# Secure Vault — implementation plan (Issue #51)

Nothing below is approved for implementation. Every slice ends with an independent review and PO
approval. Commits, pushes and PRs happen only after that approval.

## Slice dependencies

```
A (this design + PoC) ── A' (SQLCipher PoC, blocked) ──► S0 build switch
                                                      │
                     S1 hardening (plain only) ◄──────┤
                        │                             │
                        ▼                             ▼
                     S2 encrypted vault + onboarding (Phase B)
                        │
                        ▼
                     S3 backups B/C + restore + publication gate (Phase C)
                        │
                        ▼
                     S4 mode conversion without silent loss (Phase D)
                        │
                        ▼
                     S5 phrase regeneration, rotation decision, adversarial tests, dogfood (Phase E)
```

S1 can start once A is accepted. It is useful even without encryption, because it closes HIGH-1,
HIGH-4, HIGH-5 and R8 for plain vaults. S2 and later need A' and S0.

## Slices

### A' — finish the SQLCipher PoC — **done** (see `poc-results.md` A-1, A-4.3–A-4.5, A-8.2)

- **Scope.**
  - Build `poc/secure-vault/size-cipher` with native Perl.
  - Run the A-4.3 matrix: Backup API, `sqlcipher_export` and `VACUUM INTO` across all four
    combinations, with data, `schema::check`, ids, `-journal` content, temp files and a crash midway.
  - Run the plain-vault regression on the SQLCipher build: all native-host, visible-store and
    mcp-server tests.
  - Measure the wrong-key and no-key error codes.
  - Check that the salt sits at bytes 0–15 and stays the same across a raw copy.
- **Done when:** every NOT RUN item in `poc-results.md` that depends on SQLCipher is PASS or FAIL,
  with measurements.
- **Outcome:**
  - all SQLCipher-dependent items PASS;
  - finding: `sqlcipher_export` drops `application_id`/`user_version`;
  - a crashed export leaves an openable, incomplete `.partial`.
- **Still NOT RUN:**
  - a >1 GiB vault;
  - Node/E2E on a SQLCipher host binary;
  - DPAPI with another user or on a real second PC.

### S0 — build switch (no behaviour change) — **carried out within Slice B** (`slice-b.md` §2)

- **Scope.**
  - Use `bundled-sqlcipher-vendored-openssl` in all three crates, and document the toolchain (native
    Perl or `OPENSSL_DIR`) in CI.
  - Update `THIRD_PARTY_NOTICES.md` and `LICENSES/`.
- **Done when.**
  - All existing Rust and Node suites pass (on the SQLite 3.45.3 downgrade).
  - Release size and build time are reported.
  - Existing plain vaults open unchanged.
- **Tests:** full suites; an open/backup regression on a synthetic schema 7 vault created by the old
  binary.

### S1 — hardening, plain vaults only (carried out as **Slice B0**, see `slice-b0.md`)

B0 implements this slice except for P8's test flag (not needed) and with the corrected config
replace (POSIX rename instead of `MOVEFILE_WRITE_THROUGH`). Its differences are listed in
`slice-b0.md` §8.

- **Scope.**
  - `config.json` v2 with `mode` and `publication`, migrated from v1. The current active vault is
    grandfathered as confirmed (P3).
  - Error taxonomy codes (architecture §4).
  - `proves_unwritable` restricted to plain mode, plus the exclusive-handle fence.
  - Write-through config rename.
  - VisibleStore takes a mode.
  - MCP follow-active by default, and fixed-path mode refused unless active.
  - Publication gate (`pending` → `confirmed`) with UI.
- **Done when.**
  - A switch while the old vault fails with a non-proving error uses the handle fence.
  - MCP after a switch never returns the old vault's rows.
  - A newly opened unknown vault is `pending`.
  - The global policy is never written by an open or restore.
- **Tests.**
  - Deterministic race hooks (as #48).
  - Stale-MCP E2E: start MCP, switch, then query.
  - A power-cut model for the config rename (crash-point injection).
  - A canary: no key or path secrets in responses.

### S2 — encrypted vault creation and onboarding (Phase B) — **implemented as Slice B** (`slice-b.md`; review and dogfood pending)

- **Scope.**
  - Key store (DPAPI), recovery sidecar (P1), phrase display and 3-word confirmation.
  - Keyed open in host, VisibleStore and MCP.
  - `.pending` life cycle (Q6).
- **Done when.**
  - An abandoned onboarding leaves no usable active vault.
  - A wrong or missing key gives the correct code and never initialises or overwrites.
  - The encrypted vault passes every existing feature test in encrypted mode, with the suites run in
    both modes.
- **Tests:**
  - phrase parser fuzzing;
  - DPAPI failure injection (corrupt blob, missing entry, kcv mismatch);
  - the key-store/vault swap matrix.

### S3 — backups and restore (Phase C)

> Status: implemented as Slice C (`slice-c.md`). Differences: B is a Backup API snapshot, not a raw copy (DC1); C over B is
> format D (DC4). "No plaintext temp file": met after review finding C-01 (C streams the vault's pages, no work file;
> `slice-c.md` §3.1); a plain vault in WAL mode gets no C backup. B and D need the phrase when made (C-03, §3.2).

- **Scope.**
  - Normal backups A (as today) and B (raw ciphertext copy).
  - Password backups C over A and C over B.
  - Restore of A, B and C into a new vault.
  - Policy copy in B and C, and the review UI.
  - #48 identity binding kept for every backup kind.
- **Done when.**
  - New-PC restore works from the file plus the phrase alone, simulated by a fresh app dir and a
    fresh key store; a real second PC in dogfood.
  - Every tamper, truncation and parameter case fails closed.
  - No plaintext temp file is made, checked by a directory scan during operation.
  - The restored vault stays MCP-pending until confirmed.
- **Tests:**
  - a container fuzzer (`cargo fuzz` or a proptest corpus);
  - a size-limit DoS suite;
  - a crash-point injection for each publication step;
  - a >1 GiB vault for the lock-byte page.

### S4 — mode conversion (Phase D)

- **Scope.**
  - The `maintenance` protocol, the conversion phases P0–P6, cancel and resume.
  - The extension pause UI and watchdog (M3), and progress over `connectNative`.
- **Done when (zero silent loss).**
  - Every event generated during the conversion ends in the target or stays queued.
  - The `dropped`, `navigationNotStored` and dead-letter counts are unchanged.
  - Policy evaluation succeeds throughout.
  - A crash at every phase boundary leaves exactly one active vault and an unchanged source.
  - MCP never reads the former vault after P5.
- **Tests.**
  - A Node harness generating visits at a fixed rate during the conversion.
  - A crash at each phase, via a test hook plus process kill.
  - Contention scripts: concurrent backup, `vault_open` and MCP readers.

### S5 — regeneration, rotation, hardening (Phase E)

- **Scope.**
  - Phrase regeneration (re-wrap).
  - VK rotation, if P6 says so, built on the S4 machinery.
  - An adversarial review and a security review.
- **Done when.**
  - Old backups behave as documented (old phrase only).
  - The UI wording never claims revocation.

## Windows dogfood items

1. A real second PC, or a fresh Windows account after an OS reinstall: restore B and C(B) with the
   phrase only; DPAPI re-protect.
2. A second Windows user on the same PC cannot unlock (A-2.4).
3. Vaults on OneDrive, a USB drive or a network share: location warnings plus sidecar behaviour.
4. An 8.3 short path and junction paths for the vault, the sidecar and the backup target (#48 lesson).
5. Power cut, simulated by a hard VM reset, during conversion and during backup publication.
6. A slow HDD and a large vault (more than 1 GiB of images): conversion duration against queue
   headroom.
7. Antivirus scanning and locking `.partial` files during publication.

## Independent review points

- After A' (SQLCipher evidence), before S0.
- After S1 (fence, MCP gate): security review.
- After S3 (formats frozen): crypto and format review, including the fuzzer results.
- After S4 (conversion): recoverability review with the crash matrix.
- Before release: a full security review (S5).

## PO decisions needed

| # | Question | Recommendation |
|---|---|---|
| P1 | Active encrypted vault gets a `<vault>.mckey` recovery sidecar (a two-file vault) | yes: it enables recovery after an OS reinstall with no backup |
| P2 | Format A (raw SQLite) carries no policy copy (exception E-A); B and C carry an encrypted copy | yes: the only option without cleartext rules |
| P3 | Grandfather the current active vault as MCP-confirmed at upgrade; all other or new vaults start pending | yes |
| P4 | Accept OpenSSL (Apache-2.0) and SQLCipher (BSD) notices, the native-Perl build requirement (+12 min cold build, +3 MB per binary) and the SQLite 3.46.0 → 3.45.3 downgrade (existing Rust suites pass), or upgrade rusqlite as part of S0 | accept; keep rusqlite 0.32 for S0 |
| P5 | Recovery security level 128 bits (12 words) for backups and the sidecar | accept (24 words would give 256, at a UX cost) |
| P6 | Offer VK rotation for compromise (new vault, new key); old backups remain readable with the old phrase | offer it as "re-encrypt into a new vault"; no claim of revocation |
| P7 | Conversion watchdog cancels when queue headroom falls below a threshold, rather than pausing indefinitely | yes (M3) |
| P8 | Fixed-path MCP mode refused unless the path is the active vault; an explicit flag for tests | yes |
| P9 | Pending capture queue in `chrome.storage.local` stays plaintext (R1) | accept for now, track separately |
