# Secure Vault — Slice B: encrypted vaults, DPAPI key store, recovery phrase (Issue #51)

Status: implemented on `feature/51-secure-vault-b`, **not committed**; waiting for an independent security review,
Windows dogfood and PO approval. Basis: `main` at `cca6aac` (Slice B0 merged).

Markers: **[TEST]** covered by an automated test named below, **[CODE]** read in the code, **NOT RUN** not exercised.

## 1. What changed

| Stage | Content | Where |
|---|---|---|
| B1 | SQLCipher 4.5.7 (SQLite 3.45.3) in all three crates; keyed open in the host, VisibleStore and (through it) MCP | `*/Cargo.toml`, `native-host/src/secure_vault/cipher.rs`, `vault.rs`, `visible-store/src/snapshot.rs` |
| B2 | Vault key, recovery seed and phrase, recovery envelope, DPAPI key store, recovery sidecar | `native-host/src/secure_vault/{secret,phrase,recovery,protector,key_store,sidecar}.rs` |
| B3 | Encrypted creation from the side panel: begin → phrase shown once → three words re-entered → commit, or cancel | `native-host/src/manage/encrypted.rs`, `extension/vault-panel.js`, `vault-client.js`, `sidepanel.html` |
| B4 | Opening an encrypted vault (DPAPI), recovery with the phrase, re-protection for the current Windows user | the same files |

Not in this slice: conversion between plain and encrypted, backups of encrypted vaults (refused, see §7), restore, phrase
regeneration, key rotation, non-Windows protectors.

## 2. Build and link

- `rusqlite 0.32` with feature `bundled-sqlcipher-vendored-openssl` in native-host, visible-store and mcp-server (the latter
  only as a dev-dependency; its binary gets SQLite through visible-store). `libsqlite3-sys 0.30.1` has `links = "sqlite3"`, so
  every binary links exactly one SQLite: SQLCipher. There is **no build without SQLCipher**, hence no build in which an
  encrypted path silently does nothing.
- Keying uses `sqlite3_key_v2`, declared in `cipher.rs`; it exists only in SQLCipher, so a plain-SQLite link fails. At run
  time `PRAGMA cipher_version` is checked before a key is applied (`KeyingError::Unavailable` otherwise).
- OpenSSL 3.6.3 (`openssl-src 300.6.1`) is built from source and needs a native Windows Perl (`PERL`). Measured here: see §8.
- Plain vaults are opened without a key exactly as before; the 3.46.0 → 3.45.3 SQLite downgrade broke no existing test (§8).
- Notices: `NATIVE_THIRD_PARTY_NOTICES.md`, `LICENSES/SQLCipher.txt`, `LICENSES/OpenSSL.txt`.

## 3. Keys and metadata

```
VK (32 bytes, OS CSPRNG)          ── SQLCipher raw key  x'<hex>'  via sqlite3_key_v2
seed E (16 bytes, OS CSPRNG)      ── 12 words (BIP-39 English list + checksum; no PBKDF2, no passphrase)
vaultId (16 bytes, OS CSPRNG)     ── config.json "vaultId", key store file name, sidecar, all bindings
cipherSalt = first 16 file bytes  ── written by SQLCipher, stored in the key store entry and the sidecar

key store  <app>\keys\<vaultId>.json   {version, vaultId, cipherSalt, protector:"dpapi-user", protected: DPAPI(VK), kcv}
             DPAPI user scope, UI forbidden, entropy = "memocampus/vault-key/v1\0" || vaultId
             kcv = HMAC-SHA256(HKDF(VK, "memocampus/kcv/v1"), vaultId || cipherSalt)
sidecar    <vault>.mckey               {version, format, vaultId, cipherSalt, recovery}
             recovery = { scheme "bip39-128/hkdf-sha256/xchacha20poly1305/v1", salt(32), nonce(24), wrapped(48), check(32) }
             (KEK, Kc) = HKDF-SHA256(E, salt, "memocampus/recovery/v1/kek" | ".../check")
             wrapped   = XChaCha20-Poly1305(KEK, nonce, AAD = label || vaultId || cipherSalt)(VK)
             check     = HMAC-SHA256(Kc, label || vaultId || cipherSalt)
```

- VK and E are independent random values; the phrase is **not** an encoding of VK.
- Unlocking (host, VisibleStore): the salt is read from the file, the entry must name the expected vault id and that salt,
  DPAPI must open the blob, and the kcv must match. Only then is the key applied; SQLCipher then checks page 1 (HMAC). A
  DPAPI blob that opens to another key is refused by the kcv [TEST `key_store::a_swapped_altered_or_foreign_entry_is_refused`].
- The check value tells a wrong phrase (`recovery_phrase_rejected`) from altered metadata (`recovery_metadata_damaged`)
  without a shortcut: each guess still costs one HKDF.
- Key material types (`VaultKey`, `RecoverySeed`) have no `Debug`/`Display`/`Serialize` output, no `Clone`, and are zeroed
  on drop; the hex form handed to SQLCipher, DPAPI's output buffer and phrase strings in the host are zeroed too
  (`zeroize`). Limits: the phrase in the host's JSON answer (`serde_json`) and JavaScript strings cannot be zeroed; the
  host process exits after each message, and the panel drops every reference (§6).
- A key lives as long as the connection: the host keeps one connection per process (Chrome starts one process per message);
  VisibleStore unlocks for each read and closes the connection before rows are judged.
- Nothing ever creates a key in place of a missing one: no entry → `key_unavailable` (host) / `vault_locked` (MCP).

## 4. Configuration

`config.json` v2 gains `vaultId` (32 hex digits): **required in encrypted mode, forbidden in plain mode**; anything else is
`config_invalid` [TEST `keyed::a_config_must_name_the_key_of_an_encrypted_vault_and_only_of_one`]. The publication stays bound
to the file identity (`vaultToken`), as for plain vaults (§9, difference D1). Hosts from before Slice B read an encrypted
config as `config_invalid` (fail closed; no downgrade).

## 5. Creating an encrypted vault (B3)

| State | Manifest `vault-create.json` | Files | Active vault |
|---|---|---|---|
| 1 none | — | — | unchanged |
| 2 creating | `phase: creating` (written first, `create_new`) | `<t>.pending` (keyed DB), then `keys\<id>.json.pending`, `<t>.mckey.pending` | unchanged |
| 3 awaiting confirmation | `awaiting_confirmation` | all three, each checked to give the same VK (DPAPI, envelope with E, DB keyed + `quick_check` + schema) | unchanged |
| 4 complete, not active | (commit) | promoted with no replacement: key entry, sidecar, DB | unchanged |
| 5 active | removed | `<t>`, `<t>.mckey`, `keys\<id>.json` | `<t>`, encrypted, **publication pending** |

Messages (side panel → host; the service worker relays none of them):

| Message | Fields | Answer |
|---|---|---|
| `vault_create_encrypted` | `path` \| `useDefault` \| (dialog), `approveWarnings` | `{path, recoveryPhrase: [12 words]}`; `vault_creation_pending`, `file_exists`, `sidecar_exists`, location codes, `key_protector_*`, `random_unavailable` |
| `vault_create_encrypted_commit` | `recoveryPhrase` (the phrase shown) | `{path, status}`; `recovery_phrase_invalid` (+`reason`, `position`/`count`), `recovery_phrase_rejected`, `vault_creation_not_pending`, `vault_creation_damaged`, `file_exists` |
| `vault_create_encrypted_cancel` | — | `{status}` |

- Begin refuses while a manifest exists, so an interrupted creation is never mixed with a new key; the phrase is never
  stored, so it cannot be shown again: such a creation is cancelled (the panel offers **作成途中のVaultを破棄する**).
- The panel shows the 12 words once, then hides them and asks for three random positions (`crypto.getRandomValues`,
  rejection sampling); only matching answers send the commit. The commit carries the phrase back, and the host checks that
  it opens the envelope written at begin: the phrase displayed is provably the vault's (difference D3).
- Promotion steps are idempotent: a commit retried after a crash accepts files already under their final names when they
  carry this creation's vault id and salt [TEST `a_commit_interrupted_after_promotion_completes_on_retry_with_the_same_key`].
- The commit point is the `config.json` rename (Slice B0). Before it, nothing is active; after it, the vault is active and
  pending. Cancel removes only files that carry this creation's vault id/salt and are not active.
- On failure during begin everything created is removed; only DPAPI-protected and phrase-wrapped forms of VK are ever
  written.

## 6. Opening and recovery (B4)

- `vault_open` of a file without the SQLite header: its vault id comes from `<file>.mckey` (salt must match), else from
  this user's key store by salt; neither → the plain path answers `vault_invalid` as before. Then DPAPI unlock and
  `quick_check`; on success the vault becomes active (decision kept only for the same file, B0 rule).
- No usable key (`key_unavailable`, `vault_locked`, `vault_decrypt_failed`) with a matching sidecar → the answer carries
  `recoveryAvailable: true`; the panel opens **リカバリーフレーズで復旧**. `vault_status` of an active encrypted vault that
  cannot be unlocked also carries `recoveryAvailable`.
- `vault_recover {path, recoveryPhrase, approveWarnings?}`: header → sidecar (`recovery_metadata_missing` / `_damaged` /
  `_mismatch`) → phrase (`recovery_phrase_invalid` with reason) → envelope (`recovery_phrase_rejected` /
  `recovery_metadata_damaged`) → the key opens the vault itself (`quick_check`, schema) → only then a DPAPI entry is
  written for this Windows user (new, or replacing an unusable one in one rename) → the vault becomes active with a
  **fresh pending publication**. Any failure changes nothing: no config, no entry, no vault byte [TEST
  `another_windows_user_recovers_with_the_phrase_and_only_with_it`, `a_wrong_phrase_never_replaces_a_key_entry_…`].
- The panel never stores the phrase: not in `chrome.storage`, `localStorage` or the queue; the input is cleared on success
  and cancel; there is no copy button (clipboard).

## 7. Host, VisibleStore and MCP

- Host: `open_vault_in(path, mode, vaultId, keys)`; encrypted mode checks the header (plain SQLite or < 16 bytes →
  `vault_mode_mismatch`), unlocks, keys a read-write connection, then runs every plain check (`schema::check`, read-only
  detection). A refused key is `vault_decrypt_failed`; none of the key errors proves the file unwritable, so a switch away
  uses the B0 exclusive-handle fence; an unlocked encrypted vault is fenced by its write lock [TEST
  `keyed::switching_away_from_an_unlocked_encrypted_vault_takes_its_write_lock`].
- `vault_backup` of an encrypted vault is refused (`vault_backup_encrypted_unsupported`) before any dialog or file
  [TEST `an_encrypted_vault_is_never_backed_up_in_the_clear_…`].
- VisibleStore: the gate (B0) admits `{path, mode, token, vaultId}`; the read unlocks with the app directory's key store,
  opens read-only with the key, checks `journal_mode = delete` and no `-wal`/`-shm`, then the identity (token) while SQLite
  holds the file, schema, rows, gate again. Codes: `vault_locked` (no usable key), `vault_decrypt_failed`,
  `vault_mode_mismatch`. No fallback to any other vault [TEST `visible-store tests::encrypted::*`].
- MCP gets its key the same way (same Windows user, same DPAPI): the key never crosses Native Messaging or MCP. stdio and
  Streamable HTTP share the path [TEST `http_mcp::an_encrypted_vault_answers_the_same_over_http_as_over_stdio_…`].
- The extension queue keeps events refused with key errors (only four codes are permanent, unchanged).

## 8. Verification (this branch, Windows 11, rustc 1.98.1 MSVC, SQLCipher 4.5.7 / SQLite 3.45.3)

| Suite | Before (B0) | After | New |
|---|---|---|---|
| native-host (`cargo test --locked`) | 470 | 503 | 33 (lib: secret 3, phrase 4, recovery 4, protector 1, key store 3, sidecar 1, cipher 3, fsutil 2; host unit 4; `tests/secure_vault_b.rs` 8) |
| visible-store | 111 (+2 ignored) | 115 (+2 ignored) | 4 |
| mcp-server | 56 | 57 | 1 |
| extension `npm test` | 721 (712 pass, 9 skipped) | 728 (718 pass, 10 skipped) | 6 (+1 opt-in E2E) |
| extension with `MEMOCAMPUS_TEST_HOST` + `MEMOCAMPUS_TEST_MCP` | 721 | 728 pass, 0 skipped | — |
| `cargo fmt --check`, `clippy --all-targets -D warnings`, `git diff --check` | clean | clean (3 crates) | — |
| release build (MSVC, SQLCipher, cold OpenSSL for release) | — | 14.4 min for the three crates; host 8,704,512 B, MCP 12,191,744 B | — |

Existing tests whose expectations changed: an encrypted-mode config now needs `vaultId`; an encrypted vault without a key
is `key_unavailable` / `vault_locked` (was `vault_encryption_unsupported`); a key SQLCipher refuses is
`vault_decrypt_failed`.

## 9. Differences from the Slice A design

| # | Design (architecture.md) | Slice B | Reason / safety effect |
|---|---|---|---|
| D1 | §7.2: publication identity of an encrypted vault is `vaultId` | still the file token; `vaultId` names the key | a copy of the vault file is pending again (stricter); B0 gate unchanged |
| D2 | §4: codes `key_store_missing`, `dpapi_failed`, `key_store_mismatch` | one `key_unavailable` (host), `vault_locked` (MCP); recovery codes as §6 | the user action is the same; causes kept internally (`KeyError`) |
| D3 | threat model §3: the phrase crosses extension → host only for recovery | it also crosses at the commit of a creation | proves that the phrase shown opens the stored envelope; same channel as recovery |
| D4 | §3: `PRAGMA key = "x'…'"` as the first statement | `sqlite3_key_v2` with the literal in a zeroed buffer | no key in SQL text; plain-SQLite link impossible |
| D5 | §2.1 recovery AAD = label ‖ vaultId | + cipherSalt, and a phrase check value | an envelope moved to another vault file is refused; damage told from a wrong phrase |
| D6 | §3: `temp_store = MEMORY` on every connection | on keyed connections only | plain behaviour unchanged (plain temp files are plaintext anyway) |
| D7 | §2.1 `activeVaultId` in config | `vaultId` | naming only |
| D8 | — | `vault_open` finds the vault id via the key store when the sidecar is missing | a copied vault without its sidecar still opens for the user who has its key |

## 10. Not verified (NOT RUN)

- A second Windows account and a real second PC / OS reinstall (DPAPI of another user). Simulated by a second app
  directory under the same user: the key store and config are separate, DPAPI is the same.
- Real Chrome + real MCP client dogfood (§11).
- GitHub Actions on the hosted runner (the workflow was changed, not run): Strawberry Perl path and the build time there.
- Power cut during creation/commit (only process-level interruption between steps is tested).
- Vaults larger than 1 GiB; antivirus holding `.pending` files.
- `PRAGMA cipher_memory_security` is not enabled (SQLCipher's own memory wiping); key copies inside SQLCipher/OpenSSL are
  outside Memocampus' control.

## 11. Windows dogfood (manual, after review)

Use a test `MEMOCAMPUS_APP_DIR` or a throw-away Windows account; never the real vault for steps 5–8.

1. Build the release binaries (native Perl), reinstall the host, reload the extension.
2. **Existing plain vault**: the panel shows it as before (`mode` plain), recording continues, MCP unchanged.
3. **Create encrypted** (default choice): the phrase appears once; closing the panel at that point leaves a
   "作成途中" notice and the old vault active; discard it.
4. Create again, write down the phrase, answer the three words wrongly (refused), then rightly: the new vault is active,
   **MCPへの公開: 未承認**. Browse a few pages; search finds them; the `.sqlite3` file contains no readable URL.
5. Confirm MCP publication; an MCP client (follow-active) reads the new vault; a `saveButNoMcp` rule hides a URL at once.
6. Restart Chrome and Windows: the vault opens without any password.
7. **Other user**: copy the vault and its `.mckey` to a second Windows account (or a reinstalled PC); open it → "鍵を取得できません"
   → recovery with a wrong phrase (refused, nothing changes) → the right phrase → active, pending; MCP there needs approval.
8. Delete `.mckey` and try recovery: `.mckey` missing is reported; nothing changes.
9. Backup button on the encrypted vault: refused with the explanation; no file appears.

## 12. Residual risks

- R-B1: the phrase is shown on screen and travels in Native Messaging (begin answer, commit, recovery); shoulder-surfing,
  screenshots and malware running as the user are out of scope (T7).
- R-B2: an encrypted vault whose phrase and DPAPI key are both lost cannot be opened by anyone (by design).
- R-B3: `vault_status` unlocks the key with DPAPI each time it is called (≈0.2 ms); no caching of keys across processes.
- R-B4: pending capture events in `chrome.storage.local` remain unencrypted (R1, unchanged).
- R-B5: cold builds need a native Perl and about 12 extra minutes per profile; CI time grows accordingly.
