# Secure Vault — technical architecture (Issue #51, Slice A)

Status: **design proposal.** Basis: `main` at `c08797a`. Implemented so far: Slice B0 (`slice-b0.md`) and Slice B
(`slice-b.md`: SQLCipher, DPAPI key store, recovery sidecar, encrypted creation and recovery) and Slice C
(`slice-c.md`: backups A–D and restore; its §6 lists differences DC1–DC9 from §5–§6 here). Where the
implementation differs from this text, the slice documents list the difference (`slice-b.md` §9).

> **Recovery Spell.** The 12-word BIP-39 recovery phrase of this proposal and of Slices B/C has been replaced by the
> **Recovery Spell (ふっかつのじゅもん)**: 256 CSPRNG bits written as 43 base64url characters. Its specification — format,
> parsing, key derivation and domain separation, sidecar version 2, container version 2, handling of 12-word files — is
> `recovery-spell.md`, which takes precedence over §2, §2.2, §4 error codes, §5–§6 and §9 below where they differ.
Each statement is tagged:

- **[CODE]** — verified against the current source (file:line given);
- **[POC]** — measured in `poc/secure-vault` (see `poc-results.md`);
- **[DOC]** — taken from upstream documentation or crate source, not executed here;
- **[DESIGN]** — a proposal, not yet verified.

SQLCipher 4.5.7 was built (MSVC, vendored OpenSSL, native Perl) and measured in A' (see
`poc-results.md`, A-1, A-4.3 to A-4.5, A-8.2). Statements about SQLCipher marked [POC] were measured
there; the rest stay [DOC] or [DESIGN].

---

## 1. Current state relevant to Secure Vault

| Area | Today | Source |
|---|---|---|
| SQLite | `rusqlite 0.32.1` + `libsqlite3-sys 0.30.1` `bundled` (SQLite 3.46.0) in native-host, visible-store, mcp-server (three separate workspaces) | [CODE] `*/Cargo.toml`, `*/Cargo.lock`; [POC] `sqlite_version()` |
| Vault format | schema 7, `application_id` = `0x4D434D50`, `user_version` = 7, byte-exact `schema::check` | [CODE] `native-host/src/schema.rs:16-18,312` |
| Journal | rollback journal (`delete`), WAL refused by VisibleStore | [CODE] `visible-store/src/snapshot.rs:76-121` |
| Active vault | `config.json` `{version, activeVault, locationApproved}`, atomic temp+rename, `config.lock` (share mode 0) serialises management | [CODE] `native-host/src/vault.rs:112-215` |
| Switch fence | old vault `BEGIN IMMEDIATE` while config is rewritten; **skipped** when the old vault "proves unwritable", which includes SQLite `NotADatabase` | [CODE] `vault.rs:302-325,442-481` |
| Write binding | `write_checked`: write lock, then config re-check, else `Switched` | [CODE] `vault.rs:730-766` |
| Identity | `vault_token` = SHA-256(volume serial, file index) | [CODE] `vault.rs:519-548` |
| Backup | Backup API into `.partial`, verify, `rename_no_replace`; bound to the vault the panel showed (#48) | [CODE] `vault.rs:824-900`, `manage.rs` `backup()` |
| Policy | global: `%LOCALAPPDATA%\Memocampus\policy.json` + `settings.sqlite3` (PSL approvals), not in any vault | [CODE] `policy_store.rs:72`, `domain_approvals.rs:1-20` |
| Per-image MCP permission | `mcpPublic` flag stored **inside the vault** | [CODE] `image_publication.rs:1-10` |
| MCP | `memocampus-mcp --vault <path>`: path fixed for the process lifetime | [CODE] `mcp-server/src/main.rs:27,253` |
| VisibleStore | requires the `SQLite format 3\0` magic before opening | [CODE] `visible-store/src/snapshot.rs:89` |
| Capture queue | `chrome.storage.local`, `MAX_PENDING = 5000`; on overflow, enqueue fails and `dropped` is counted | [CODE] `extension/event-queue.js:37,154,166-171`, `service-worker.js:79-99` |
| Pre-enqueue policy check | a visit is judged by the host **before** it is queued; with no answer it is not recorded and **not counted** | [CODE] `service-worker.js:158-160` |
| Policy evaluation | `policy_evaluate_batch` uses only the policy gate, never the vault session | [CODE] `native-host/src/main.rs:456-458` |

## 2. Key hierarchy

```
                          ┌──────────────────────────── VK (256-bit, BCryptGenRandom) ───────────────┐
                          │                                                                           │
   SQLCipher raw key ◄────┤  PRAGMA key = "x'<VK hex>'"  (no SQLCipher PBKDF2; raw-key mode) [DOC]    │
                          │                                                                           │
   local unlock ◄─────────┤  DPAPI(VK, entropy = "memocampus/vault-key/v1\0" || vaultId),             │
                          │  user scope, CRYPTPROTECT_UI_FORBIDDEN            [POC A-2.2]             │
                          │                                                                           │
   recovery ◄─────────────┤  KEK = HKDF-SHA256(ikm = S, salt = 32 random bytes,                       │
                          │                    info = "memocampus/recovery-spell/v2/kek")             │
                          │  wrap = XChaCha20-Poly1305(KEK, nonce 24 random,                          │
                          │                            AAD = label || vaultId || cipherSalt)          │
                          │  S = 256-bit Recovery Spell (43 base64url chars)  [recovery-spell.md]     │
                          │                                                                           │
   backup MAC key ◄───────┤  HKDF-SHA256(VK, info = "memocampus/backup-b/v2/mac")                     │
   policy-copy key ◄──────┤  HKDF-SHA256(VK, info = "memocampus/backup-b/v2/policy")                  │
   key-check value ◄──────┘  HMAC-SHA256(HKDF(VK, "memocampus/kcv/v1"), vaultId)                      │
                                                                                                     ─┘
   backup password P ─► Argon2id(P, salt, m=64 MiB, t=3, p=1) ─► HKDF ─► K_enc (STREAM), K_check  [POC A-5.2]
```

- **No circular dependency.**
  - The DB is encrypted under VK.
  - VK is recovered from data *outside* the DB: the DPAPI blob in the key store, or the recovery
    envelope in a sidecar or backup header.
  - Nothing needed to unlock is stored only inside the encrypted DB.
- **Security level (stated, not inflated).**
  - The DB key is 256-bit.
  - The recovery path is bounded by the spell's entropy, **256 bits** (the 12-word phrase it replaced
    gave 128). Anyone holding a backup can try spells offline, at one HKDF plus one HMAC per guess,
    against 2²⁵⁶ candidates.
- **Why HKDF, not a slow KDF, for the spell.**
  - The spell is uniformly random at 256 bits, so stretching adds nothing meaningful.
  - The backup *password* is user-chosen, so it gets Argon2id with bounded parameters.
  - Both are deliberate and different.
- **Password never wraps VK** (PO decision A).
  - Format C encrypts the whole format B package.
  - The spell-wrapped VK inside it stays spell-wrapped.

### 2.1 Vault identity

- `vaultId` is 128 random bits, created with the encrypted vault. It is recorded in:
  - the key store entry;
  - the recovery sidecar;
  - every format B header;
  - `config.json` (`activeVaultId`).
- **File ↔ vaultId binding [DESIGN, needs SQLCipher PoC].**
  - SQLCipher stores a 16-byte random salt in the clear as the first 16 bytes of the file [DOC].
  - The key store and the sidecar record it as `cipherSalt`.
  - The host refuses a vault whose salt differs: `key_store_mismatch`, before keying.
  - After keying, a successful page-1 HMAC proves the key [DOC].
  - The key-check value proves the key store entry belongs to the `vaultId`. The DPAPI blob is
    partly malleable ([POC A-2.2]), although it never decrypted to a different key.
- **The schema is unchanged.**
  - An encrypted vault holds exactly schema 7.
  - `schema::check` runs after keying, so conversions copy rows without any schema change.

### 2.2 Where key material lives

| Item | Location | Contents | Secret? |
|---|---|---|---|
| Key store entry | `%LOCALAPPDATA%\Memocampus\keys\<vaultId>.json` | `{version, vaultId, cipherSalt, dpapi: base64, kcv}` | DPAPI blob (user scope) |
| Recovery sidecar **[PO decision P1]** | `<vault path>.mckey` | `{version: 2, vaultId, cipherSalt, recovery envelope}` | no (spell-wrapped) |
| Backup B header | inside the backup file | `vaultId`, recovery envelope, sealed policy copy | no (spell-wrapped / VK-sealed) |
| Recovery Spell | **nowhere**; shown once, then zeroised | 43 base64url characters (256 bits) | yes |
| Backup password | **nowhere** | — | yes |

**P1** decides whether an Active Vault may be recovered after an OS reinstall from the vault file
plus spell, with no backup:

- **With the sidecar, yes.** The vault becomes a two-file unit; moving one without the other is
  detected by `vaultId` and `cipherSalt`.
- **Without it, only a backup can be restored on a new PC.** This still meets the PO requirement,
  "backup + spell".

Recommendation: the sidecar, because an OS reinstall without a fresh backup is a likely data-loss
case.

## 3. SQLCipher policy

- **Build.**
  - Use `rusqlite` feature `bundled-sqlcipher-vendored-openssl` in all three crates.
  - `libsqlite3-sys 0.30.1` bundles **SQLCipher 4.5.7 on SQLite 3.45.3** [DOC: crate source
    `sqlcipher/sqlite3.c:499,106612`].
  - This is a **downgrade from SQLite 3.46.0**. All production Rust suites still pass on it
    (430 + 102 + 52) [POC A-1.5]. A Node/E2E run on a SQLCipher host remains for S0.
- **Coexistence.**
  - `libsqlite3-sys` has `links = "sqlite3"`, and `bundled-sqlcipher` switches the bundled source to
    `sqlcipher/` [DOC: `build.rs`].
  - So one binary cannot link both plain SQLite and SQLCipher: **all three crates switch together.**
  - SQLCipher with no key reads plain databases, so plain vaults keep working: a vault written by
    the plain build opens, checks and stays plain after writes [POC A-1.3].
- **Flags.**
  - The crate sets `SQLITE_HAS_CODEC` and `SQLITE_TEMP_STORE=2` [DOC: `build.rs:144`; confirmed in
    `PRAGMA compile_options` POC A-1.2].
  - `SQLITE_TEMP_STORE=2` means memory by default, but `PRAGMA temp_store` can override it. The host
    sets `PRAGMA temp_store = MEMORY` on every connection, so temporary b-trees (sorts, `VACUUM`,
    exports) never reach disk in plaintext.
- **Keying.**
  - `PRAGMA key = "x'<64 hex>'"` (raw key) is the first statement on a new connection.
  - `PRAGMA cipher_compatibility = 4` pins the format.
  - The key never appears in SQL logs or error text: the key PRAGMA is never formatted into an
    error message.
- **Windows build dependency.**
  - The vendored OpenSSL build needs a *native* Perl. Git-for-Windows MSYS Perl fails
    (`Locale::Maketext::Simple` missing); MSYS2 `mingw-w64-x86_64-perl` works [POC A-1.2].
  - CI and developer machines therefore need native Perl, or a prebuilt MSVC OpenSSL through
    `OPENSSL_DIR` (PO/maintainer decision P4).
  - Cost [POC A-1.2]: about +2.97 MB per binary (native-host 5.39 → 8.37 MB, mcp 9.11 → 12.09 MB),
    and about 12 minutes more for a cold release build.
- **Licence [DOC].**
  - SQLCipher Community Edition: BSD-style (Zetetic) with an attribution clause.
  - OpenSSL 3.x: Apache-2.0.
  - Both go into `THIRD_PARTY_NOTICES.md`. The vendored `openssl-src` licence text must ship with
    the binaries.

## 4. Strict open, error taxonomy and fences (HIGH-4)

`config.json` v2 records the mode of the active vault (`"mode": "plain" | "encrypted"`). The host
never guesses the mode from the file. A file that contradicts the recorded mode is an error, not a
reason to switch modes.

| Code | Meaning | Detected by | May skip the switch fence? |
|---|---|---|---|
| `key_store_missing` | no key store entry for `vaultId` | file absent | no |
| `dpapi_failed` | `CryptUnprotectData` failed (other user, damaged profile, damaged blob) | Win32 error (13, 0x8009000B, 0xD000000D seen [POC]) | no |
| `key_store_mismatch` | entry or kcv or `cipherSalt` disagrees with the vault | kcv / salt compare | no |
| `recovery_spell_invalid` | not 43 characters, a character outside base64url (position reported) or a non-canonical last character | `spell::parse` | n/a |
| `recovery_spell_rejected` | well-formed spell, but the envelope's check value does not match | HMAC | n/a |
| `recovery_metadata_unsupported` | the sidecar is of another version (1 = 12-word phrase) | sidecar version | n/a |
| `vault_decrypt_failed` | key accepted (kcv ok) but SQLCipher refuses page 1 | `NotADatabase` **while keyed** (no key and wrong key both give `NotADatabase`/26 [POC A-8.2]) | **no** |
| `vault_corrupt` | keyed open ok, `integrity_check`/`quick_check` fails; or plain-mode `NotADatabase`/`Corrupt` | SQLite | plain mode only (as today) |
| `vault_mode_mismatch` | plain mode but no SQLite magic, or encrypted mode but SQLite magic | first 16 bytes | no |
| `unsupported_format` / `unsupported_version` | unknown container, kind, version or KDF | container reader [POC A-5] | n/a |
| `vault_switched` | identity changed during an operation (#48) | `vault_token` / `vaultId` | n/a |
| `backup_tampered` | MAC/AEAD failure after the key was accepted | [POC A-5.1/A-5.2] | n/a |
| `backup_password_rejected` | password check value mismatch | [POC A-5.2] | n/a |
| `restore_target_exists` | target file exists | `create_new` / `rename_no_replace` | n/a |
| `vault_maintenance` | conversion in progress (temporary) | `config.json` `maintenance` | n/a |

**Fence rule.**

- `proves_unwritable` stays true only when the failing open used exactly the keying every writer
  uses, and the vault is in plain mode. In encrypted mode, no open failure proves anything about
  other processes, which may hold the key.
- When the active vault cannot be keyed in this process (for example `dpapi_failed`), the switch uses
  an **exclusive-handle fence** instead: `CreateFile` with share mode 0 on the vault file, held while
  `config.json` is rewritten.
  - It needs no key.
  - It fails with `ERROR_SHARING_VIOLATION` (32) while any connection is open.
  - A writer opening meanwhile gets `CannotOpen` (temporary), never `NotADatabase` [POC A-7.1].
- So a user can always switch away from a vault this process cannot unlock, and no writer can commit
  to it during the switch.

## 5. Backup formats (A-5)

> Implemented in Slice C with differences DC1–DC9 (`slice-c.md` §6): B is a Backup API snapshot with a new salt, C over B
> is kind `D`, the B policy copy follows the payload, Argon2 m is bounded at 256 MiB.

Detection is by the first 16 bytes [POC A-5]. Recognition never relies only on the SQLite magic:

| First bytes | Format | Next step |
|---|---|---|
| `SQLite format 3\0` | A (plain SQLite) | full validation: `quick_check`, `schema::check`, ids |
| `89 4D 43 56 42 0D 0A 1A` + version 2 + kind `B` | B | header bounds, then the spell (version 1 B/D: `backup_legacy_recovery_phrase`) |
| same + kind `C` | C | header bounds, then password |
| anything else | refused `unsupported_format` (this includes a bare SQLCipher file: it is not a backup) | — |

### 5.1 Common container prefix [POC]

```
magic 8 | version u16 LE = 1 | kind u8 ('B'|'C') | flags u8 = 0 | header_len u32 LE (1..=65536) | header JSON
```

The header is strict JSON with `deny_unknown_fields`, holding non-secret values only, and is bounded
in size before parsing.

### 5.2 Format A — plain vault, normal backup

- Unchanged: today's raw SQLite file (Backup API, verify, atomic `rename_no_replace`, #48 identity).
- **No policy copy** (exception E-A, PO decision P2), because:
  - appending anything breaks the raw-SQLite requirement;
  - adding a table breaks the byte-exact schema;
  - storing `neverSave`/`saveButNoMcp` rules in the clear is forbidden by the PO.
- The restore flow states that this backup carries no policy, and the restored vault starts closed to
  MCP (§7).

### 5.3 Format B — encrypted vault, normal backup

```
prefix(kind 'B', header{payload:"sqlcipher4", vaultId, payloadLen, recovery:{scheme,salt,nonce,wrapped},
                        policy:{nonce, ct}?})
payload  = SQLCipher file bytes (ciphertext)
trailer  = HMAC-SHA256(K_mac, prefix || payload)
```

- **Snapshot without plaintext.**
  - The ciphertext bytes of the vault file are copied while one read transaction holds SHARED.
  - In rollback-journal mode no writer can commit meanwhile, so the bytes form one consistent
    snapshot [POC A-4.2 on plain SQLite].
  - The copy is ciphertext, so no plaintext temp file exists. Measured on SQLCipher: same salt,
    opens with VK only, all checks pass [POC A-4.4].
  - The Backup API would also work here (cipher → cipher, same or new key [POC A-4.3]), but the raw
    copy keeps the same salt and needs no second keyed database.
  - Windows: SQLite's lock byte range sits at offset 1 GiB, and `LockFileEx` locks are mandatory. A
    raw reader must not read that range of a vault larger than 1 GiB; the lock-byte page is never
    used for data, so it is written as zeros. [DESIGN, needs a >1 GiB PoC]
- **Verification before publishing.**
  - The copied payload, in `.partial` (ciphertext), is opened with VK.
  - `integrity_check`, `schema::check`, ids and counts must match the counts read under the same
    SHARED lock.
  - The container is written as `.partial`, flushed and renamed with no replacement (as #48).
  - Every step was measured except the >1 GiB case [POC A-4.3, A-4.4].
- **Restore.**
  1. Check the exact length (header, `payloadLen`, 32) before any crypto.
  2. Unwrap VK with the spell (Slice C: always the spell, DC2).
  3. Stream the payload into the target `.partial` while computing the MAC.
  4. Verify the MAC, then keyed `integrity_check` and `schema::check`.
  5. Write a new key store entry (DPAPI on this PC).
  6. `rename_no_replace`.

  Tamper, truncation, appended bytes and a wrong spell all fail closed [POC A-5.1: 3,100 flipped
  positions, none accepted].

### 5.4 Format C — password-protected backup

```
prefix(kind 'C', header{kdf:{argon2id, v19, mKiB, t, p, salt}, aead:"xchacha20poly1305-stream-be32",
                        chunk:65536, noncePrefix, check, sections:[{kind,len}...]})
body = STREAM_BE32(K_enc, AAD = SHA-256(prefix)) over concat(sections)
sections: "policy" (JSON copy of policy + PSL approvals), then "vault-a" (raw SQLite) or "package-b" (whole B file)
```

- **C over A: the password is enough.**
  - The plaintext SQLite exists only in the restore target `.partial`, which is the restored plain
    vault itself.
  - When *creating* C over A, the source bytes are stream-encrypted under the SHARED lock (as in
    §5.3), so no plaintext temp copy is ever written.
- **C over B (format D): password AND spell** (PO decision A) [POC A-5.3]:
  - the password alone yields a format B package, which is still SQLCipher with a spell-wrapped VK;
  - the spell alone cannot open a D file.
- **Bounds checked before Argon2 allocates** [POC A-5.2: refused in under 50 ms]:
  - `mKiB` from 19,456 to 1,048,576;
  - `t` from 1 to 10;
  - `p` from 1 to 4;
  - at most 4 sections;
  - header at most 64 KiB;
  - exact body length.
- **The password check value separates a wrong password from damage.** Without it, a tampered first
  chunk looked like a wrong password ([POC] found and fixed). It gives no shortcut to guessing: each
  guess still costs one Argon2id run.
- Truncation, chunk reordering, header changes and flipped bits all fail [POC A-5.2].

### 5.5 Publication and leftovers

- Every artifact goes to `.partial` (created with `create_new`), is fsynced, and is renamed with no
  replacement.
- Failure removes only the `.partial` this operation created.
- A crash can leave a `.partial` behind. For C over A on restore this is a **plaintext** file. It is
  listed and offered for deletion at the next start, never deleted silently (existing F2/F5 class;
  residual risk R6).

## 6. Restore (separate vault only)

> Implemented in Slice C (`slice-c.md` §4): the spell is always required for B and D (DC2), a restored encrypted vault
> gets a new vault id (DC3), and activation is a separate confirmed step with a manifest (DC7).

1. Pick the file, then detect the format (§5).
2. Ask only for the secrets that format needs:
   - A: none;
   - B: the spell;
   - C: password; D: password and the spell.
3. Pick the target. It must not exist and must not be the active vault (identity check), and its
   parent must exist. Never in place.
4. Stream into the target `.partial`, verify (§5), and publish with no replacement.
5. Encrypted only: write the key store entry for this PC (DPAPI) and the sidecar (P1).
6. **No change to `config.json` or the active vault until the user chooses to activate.** Activation
   is an ordinary `vault_open` and goes through the publication gate (§7).
7. A failure at any step leaves `config.json`, the key store and the active vault unchanged.

## 7. Privacy Gate, MCP and restored vaults (HIGH-1, HIGH-3, HIGH-5)

### 7.1 Invariants

- **I1** — MCP in normal use reads the vault `config.json` names as active, resolved at every call.
  After a switch or conversion it never reads the former vault, except for one in-flight snapshot
  read started before the switch.
- **I2** — A vault is published to MCP only if its identity is in `config.json` `publication` as
  `confirmed`.
  - A restored, newly opened or converted vault starts `pending`.
  - `pending` makes MCP answer `vault_publication_pending`, with no data.
- **I3** — Restoring never writes `policy.json` or `settings.sqlite3`.
  - The backup's policy copy is shown for review only.
  - Adopting it is a separate, explicit `policy_apply`, with the existing validation and revision
    check.
- **I4** — The vault mode comes from `config.json`, never from sniffing. An encrypted vault is never
  opened, or reported, as a plain one (`vault_mode_mismatch`), and a locked vault is reported as
  locked, never as a schema mismatch.
- **I5** — Key material, spell, password and DPAPI blobs never enter MCP responses, native-messaging
  responses, logs or diagnostics.
  - VisibleStore receives only an opened connection, or an opaque key handle.
  - Error values are fixed codes.
  - A canary test searches every response for the key's hex and base64 forms.

### 7.2 Mechanism

- **MCP modes.**
  - `memocampus-mcp` without `--vault` (the default) follows the active vault.
  - `--vault <path>` (diagnostics and tests) is accepted only while that path's identity equals the
    active identity and is confirmed; otherwise `vault_not_active`.
  - Stale servers therefore fail closed after a conversion (PO HIGH-1 disposition; P8 for an
    explicit override flag).
- **The identity behind `publication`.**
  - Encrypted vault: `vaultId`.
  - Plain vault: `vault_token`. A copied plain file has a new token, so it is pending again; this
    fails closed.
- **Upgrade.**
  - The active vault at upgrade time is entered as confirmed, so existing MCP users are not cut off
    (PO decision P3).
  - Other vaults start pending.
- **Confirmation UI.**
  - It shows the current global policy revision, and the backup's policy copy if one exists (B or C).
  - It shows a summary of the vault's `mcpPublic` image flags, which are vault-scoped and travel with
    the vault [CODE].
  - It shows the difference between them.
  - The user confirms (records `{state: confirmed, policyRevision}`) or keeps the vault pending.
- **Policy edits and policy initialization** (Slice B0, review finding F-02; `slice-b0.md` §10).
  - Ordinary edits of the global policy keep a confirmation; every read applies the current policy.
  - Initializing a policy (`policy_initialize`, `policy_start`, possible only when no policy and
    no marker exist, e.g. after a loss) first sets the active vault's publication back to
    `pending`, then writes the policy. A failed withdrawal writes no policy; a failed policy write
    after it leaves `pending`. A later restore (S3) that brings a policy copy must follow the same
    order.
- **Why the gate is needed.**
  - Rows saved under `saveButNoMcp` and image `mcpPublic` flags travel inside the vault.
  - On a new PC the global policy may be missing or more permissive, so without the gate those rows
    could become MCP-visible with no decision by anyone.
  - VisibleStore already fails closed when the policy is missing (`PolicyUnavailable`) [CODE]. The
    gate also covers a *present but different* policy.
- **VisibleStore (HIGH-5).**
  - Today it refuses any file without the SQLite magic as `vault_schema_mismatch` [POC A-8.1].
  - Proposed: the caller passes the mode.
    - Plain: the existing header and WAL checks.
    - Encrypted: no magic check; `cipherSalt` must match the key store; keyed read-only open, then
      `PRAGMA journal_mode` = `delete`, no `-wal`/`-shm`, then `schema::check`.
  - New codes: `vault_locked` (no key), `vault_decrypt_failed`.

### 7.3 Policy copy in backups: options compared (A-6)

| Option | A raw SQLite kept | 1 file | no cleartext rules | Verdict |
|---|---|---|---|---|
| a. Table inside the A backup | yes | yes | **no** (cleartext), breaks byte-exact schema | rejected |
| b. Trailer appended to the A file | no (SQLite ignores the tail, but the "raw" requirement and our length checks break) | yes | no | rejected |
| c. A has no policy; B and C carry an encrypted copy | yes | yes | yes | **recommended**, needs PO acceptance of E-A (P2) |
| d. Make every "normal backup" of a plain vault a container | **no** (violates PO) | yes | needs a key the plain vault lacks | rejected |

Conclusion: having both "format A is raw SQLite" and "every backup carries the policy" is not
possible without cleartext rules. This is reported as a PO decision (P2), not resolved silently.
Under option c the publication gate (I2) still protects a restored A vault.

## 8. Encryption-mode conversion (A-7)

### 8.1 Loss conditions in today's capture pipeline [CODE]

| # | Condition | Outcome today | Visible? |
|---|---|---|---|
| L1 | pending queue would exceed `MAX_PENDING` (5000) | event not queued, `dropped += 1` | counted, but lost |
| L2 | `chrome.storage.local` write fails (quota: no `unlimitedStorage` in the manifest) | retried without page metadata, then `dropped += 1` | counted, but lost |
| L3 | `policy_evaluate_batch` gives no answer (host unreachable, timeout, `policy_busy`) | visit not recorded | **not counted (silent)** |
| L4 | image save (context menu) while the vault refuses writes | explicit error to the user | yes |
| L5 | host refuses a write: `vault_busy`, `vault_switched`, unknown codes | event stays queued, retried | n/a (no loss) |

Conversion must keep every refused write on path L5 and must never cause L1, L2 or L3:

- L3 needs no vault (`policy_evaluate_batch` uses only the gate [CODE]). Conversion must therefore
  never take `policy.lock`, and must never block host start-up. This is an acceptance test (S4).
- L1 and L2 are bounded by quiescence duration multiplied by capture rate, against queue headroom.
  This is handled by the watchdog (§8.3).

### 8.2 Algorithm (both directions)

Preconditions:

- `ConfigLock` is held by the conversion host for the whole operation.
- The target path was chosen by the user and does not exist.
- Free space is at least `size(source) × 1.1 + 64 MiB`.
- For plain to encrypted: the spell was shown **and re-entered in full first** (Q6). VK, the key store entry
  and the sidecar are prepared as `.pending` files and are not active.

| Phase | Action | Durable state after a crash here |
|---|---|---|
| P0 | Write `config.json` `maintenance = {op, id, sourceToken, target, startedAt}`, atomic and write-through | source active; writers answer `vault_maintenance` (temporary, so events stay queued) |
| P1 | Open source; `BEGIN IMMEDIATE` (fence; also waits for writers in flight) | same |
| P2 | Export to `target.partial`. Plain to encrypted: `ATTACH target KEY x'VK'; SELECT sqlcipher_export('t')`. Encrypted to plain: `ATTACH target KEY ''; sqlcipher_export`. **Then `PRAGMA t.application_id`/`t.user_version` from the source: `sqlcipher_export` does not copy them [POC A-4.3].** The Backup API cannot cross modes (refused) [POC A-4.3]. `temp_store = MEMORY` | partial target, which may be *openable but incomplete* after a crash [POC A-4.5]; source untouched |
| P3 | Verify target: keyed `integrity_check`, `schema::check`, ids, per-table row counts and content digests equal to the source, read under the same fence | same |
| P4 | fsync target; `rename_no_replace(target.partial, target)`; promote the key store entry and sidecar from `.pending` | complete target exists but is not active |
| P5 | **Commit point**: `config.json` = `{activeVault: target, mode, activeVaultId, publication: pending→confirmed only if the source was confirmed (same data), maintenance: none}`, atomic (POSIX rename by handle + flush, see below) | target active |
| P6 | `ROLLBACK` the source fence; report; the extension flushes its queue into the target | done; source kept unchanged |

- **Crash recovery.**
  - A host that finds `maintenance` set and can acquire `ConfigLock` knows the converter is gone.
    This is safe because the OS releases the lock at process exit [CODE].
  - It clears `maintenance`, so the source stays active, and reports `conversion_interrupted` with the
    leftover paths.
  - Leftovers are never deleted automatically.
- **The commit point is one atomic rename.**
  - Today `write_config` uses `fs::rename` after `sync_all` of the temp file [CODE], without
    write-through on the rename and without flushing the directory.
  - After a power cut the old config can reappear. Writes made to the target after the switch would
    then be hidden, though not lost.
  - ~~**Finding**: use `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` for P0 and P5.~~
    **Corrected in Slice B0** (`slice-b0.md` §6, §8): that call fails with `ERROR_ACCESS_DENIED` while
    readers hold `config.json` open (most replaces failed in a measurement), and `WRITE_THROUGH` only
    affects moves done as copy and delete. `write_config` now renames the flushed temp file through
    its own handle with POSIX semantics (`FileRenameInfoEx`) and flushes it again. Survival of a
    real power cut is not verified.
- **Concurrent actors.**

  | Actor | During conversion |
  |---|---|
  | Writers (visits, snapshots, images) | refused with `vault_maintenance` (temporary) |
  | MCP | reads the source snapshot until P5, then follows the target (I1) |
  | Backup | allowed on the source until P5 (shared lock); after that it fails with `vault_switched` (#48) |
  | `vault_open`/`vault_create` | `ConfigLock` busy, so they fail with `vault_busy` |
  | A second conversion | same |

- **Rows written after P1** cannot exist in the source, because the fence blocks commits. So the
  target equals the source as of P1, and every later event waits in the extension queue.

### 8.3 Pause and resume without silent loss: options

| Option | Mechanism | Pros | Cons |
|---|---|---|---|
| M1 host journal | the host ACKs events into an app-dir journal during maintenance, replays after P5 | no extension limits | a new plaintext file for an encrypted target (violates at-rest) unless sealed under VK; new replay code |
| M2 queue only | rely on the extension queue (L5) | no new code | bounded by L1 and L2; no feedback |
| **M3 queue + watchdog (recommended)** | the side panel starts the conversion over a `connectNative` port with progress; the extension watches `pendingCount` and stored bytes; when headroom is below a threshold (for example 20% of `MAX_PENDING` or 2 MB) it sends `vault_convert_cancel`, and the converter rolls back to P0 state (source active) | no silent loss; bounded pause; explicit UI ("記録を一時停止中: n件待機") | needs a cancel path at every phase before P5, and a long-lived port |

Acceptance criteria (for S4), all automated:

- events generated throughout the conversion all end up in the target or remain queued;
- `dropped`, `navigationNotStored` and the dead letters are unchanged;
- L3 never occurs, because policy evaluation succeeds while maintenance is held;
- a power cut at every phase boundary (P0–P6) ends with exactly one active vault, an unchanged
  source, and a target that is either complete or only `.partial`.

## 9. Recovery secret specification

The 12-word BIP-39 profile proposed here (A-3) and used by Slices B/C has been **replaced** by the Recovery Spell; the
full specification is `recovery-spell.md`. In short:

- **Form.** 32 bytes from the OS CSPRNG, written as 43 base64url characters without padding; parsed strictly (exact length,
  alphabet, canonical last character; case-sensitive; nothing trimmed or substituted by the host).
- **Derivation.** HKDF-SHA256 with the spell as input keying material, a 32-byte random salt per envelope and distinct
  `info` labels for the KEK and the check key (`memocampus/recovery-spell/v2/...`); the envelope binds vault id and cipher
  salt (AAD and check value). Backup MAC and policy keys stay HKDF(VK) with `v2` labels; the key check value is unchanged.
- **Onboarding (Q6).** The vault file, key store entry and sidecar are `.pending` until the user re-enters **all 43
  characters**; only then is everything promoted and activated. Abandoning onboarding leaves no active vault.
- **Old 12-word files** are recognised and refused as unsupported (sidecar version 1, container version 1 B/D), never read
  as spell envelopes; an existing 12-word vault still opens with DPAPI for its Windows user.
- **Regeneration (HIGH-2, later slice)**, while VK is available through DPAPI: a new spell, a new envelope over the **same
  VK**, sidecar and key store copy replaced atomically; old backups keep their old envelope and still need the old spell.
  **This is not revocation.**
- **Compromise (revocation).** Requires a new VK (re-encrypting into a new vault). Old backups stay decryptable by whoever
  holds the old spell; this cannot be revoked and is stated in the UI (P6).

## 10. Crash recovery summary

| Operation | Commit point | Crash before | Crash after |
|---|---|---|---|
| Create encrypted vault | `config.json` rename | `.pending` files listed; nothing active | active, complete |
| Backup B/C | `rename_no_replace` of `.partial` | `.partial` left (B: ciphertext; C over A on create: ciphertext; C over A on restore: **plaintext**, R6) | complete file |
| Restore | `rename_no_replace` of the target | `.partial` (same as above) | complete, not active |
| Conversion | `config.json` rename (P5) | `maintenance` cleared on next start; source active | target active |
| Spell regeneration | sidecar rename and key store rename (two files; the key store copy is the authority, the sidecar is re-derived on mismatch) | old spell still valid | new spell valid |
| Key store write (new PC) | key store rename | spell needed again | DPAPI unlock |
