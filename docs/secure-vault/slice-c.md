# Secure Vault — Slice C: portable backups, password-protected backups, restore (Issue #51)

Status: implemented on `feature/51-secure-vault-c`, **not committed**. The independent review's findings C-01 (plaintext
work file of a C backup) and C-03 (phrase not proven when a B/D backup is made) are fixed (§3.1, §3.2); C-02 (plaintext
`.partial` while a plain vault is restored) is accepted by the PO and unchanged. Waiting for the differential review,
Windows dogfood and PO approval. Basis: `main` at `9df4082` (Slice B merged, PR #53).

Markers: **[TEST]** covered by an automated test named below, **[CODE]** read in the code, **NOT RUN** not exercised.

## 1. What changed

| Stage | Content | Where |
|---|---|---|
| C1 | Formats A/B/C/D, the container reader and writer (bounds, STREAM AEAD, Argon2id), backup engine (Backup API snapshot, verification, publication) | `native-host/src/secure_vault/container.rs`, `native-host/src/manage/backup.rs`, `vault.rs` (`snapshot_into`, `verify_copy`) |
| C2 | Restore engine: inspection, password and phrase checks, a new vault in `.partial`, verification, new key store entry (DPAPI of this user) and sidecar, promotion, activation with publication pending | `native-host/src/manage/restore.rs` |
| C3 | Native Messaging (`vault_backup` with `protection`, `vault_restore_inspect`, `vault_restore`, `vault_restore_activate`, `vault_restore_discard`), file dialogs, side panel UI | `manage.rs`, `main.rs`, `dialog.rs`, `extension/vault-panel.js`, `vault-client.js`, `search-client.js`, `sidepanel.html`, `sidepanel.css` |
| C4 | Adversarial and integration tests (format tampering, wrong secrets, truncation, fault injection at every step, large vaults, canaries, host + MCP end to end) | `container/tests.rs`, `manage/backup_restore_tests.rs`, `tests/extension/vault-restore.test.js`, `tests/extension/mcp-process.test.js` |

Not in this slice: conversion between plain and encrypted (Slice D), phrase regeneration (Slice E), key rotation, non-Windows
key protectors, concurrent writers on several PCs, cloud synchronisation. MCP can neither back up nor restore: only the
Native Host, which only the extension reaches, handles these messages.

New dependency: `argon2 0.5.3` (with `blake2 0.10.6`, `password-hash 0.5.0`, `base64ct 1.8.3`; all MIT OR Apache-2.0), and
the `stream` feature of `chacha20poly1305` (no new crate). SQLCipher/OpenSSL build unchanged.

## 2. Formats

| Format | Vault | Protection | File | Making it needs | Restore needs |
|---|---|---|---|---|---|
| A | plain | none (as before) | plain SQLite, `memocampus-backup-<UTC>.sqlite3` | nothing | nothing |
| B | encrypted | SQLCipher (the vault key) + MAC | container, `memocampus-backup-<UTC>.mcbackup` | the phrase (§3.2) | the 12-word phrase |
| C | plain | password (Argon2id + STREAM AEAD) | container, `.mcbackup` | the password twice | the password |
| D | encrypted | password around a whole B package | container, `.mcbackup` | the password twice and the phrase | the password **and** the phrase |

The file name and extension help the user; they are never used to decide what a file is. Detection reads the first 16 bytes
(`container::detect`): `SQLite format 3\0` → A (then fully validated); the container magic with version 1 and kind B/C/D →
that container; anything else (a bare SQLCipher vault, another version, an unknown kind, a short file) is refused.

### 2.1 Common prefix

```
magic 8 = 89 'M' 'C' 'V' 'B' 0D 0A 1A | version u16 LE = 1 | kind u8 'B'|'C'|'D' | flags u8 = 0
| header_len u32 LE (1..=65536) | header: strict JSON (deny_unknown_fields), non-secret values only
```

The header length is checked before it is allocated; the file length must equal exactly what the header announces (prefix +
sections + tags), so truncation and appended bytes are refused before any key is derived [TEST
`container::tests::a_format_b_package_that_is_cut_or_extended_is_refused_before_any_crypto`,
`removed_reordered_duplicated_or_cut_chunks_are_refused`]. No field names a path; the restore target is always chosen by the
user.

### 2.2 Format B

```
prefix | payload (payloadLen: a SQLCipher 4 database, ciphertext) | sealed policy copy (optional) | HMAC-SHA256 (32)
header  {payload:"sqlcipher4", schemaVersion:7, vaultId, cipherSalt (payload's), recoverySalt, recovery:{envelope},
         payloadLen, policy?:{nonce, len}}
K_mac    = HKDF-SHA256(VK, "memocampus/backup-b/v1/mac");  MAC over prefix || payload || sealed policy
K_policy = HKDF-SHA256(VK, "memocampus/backup-b/v1/policy"); XChaCha20-Poly1305, AAD = label || vaultId
```

- In the clear: the vault id, two SQLCipher salts, the payload length, the phrase-wrapped envelope (Slice B's sidecar
  envelope, 128-bit phrase level) and the sealed policy copy's nonce and length. No row, title, URL, tag, image, policy rule,
  key or phrase [TEST `an_encrypted_vault_backs_up_encrypted_and_restores_on_another_pc_with_the_phrase_only`].
- Restore: header and length checks → the phrase unwraps VK (`recovery_phrase_rejected` / `backup_damaged`) → the payload is
  streamed into `<target>.partial` while the MAC is computed → MAC → the sealed policy copy → keyed full `integrity_check`
  and exact schema.

### 2.3 Formats C and D

```
header {kdf:{alg:"argon2id", version:19, mKib, t, p, salt (32, new per backup)}, aead:"xchacha20poly1305-stream-be32",
        chunk:65536, noncePrefix (19, new per backup), check, sections:[{kind, len}]}
(K_enc, K_check) = HKDF-SHA256(prk = Argon2id(password, salt, m, t, p))
check = HMAC-SHA256(K_check, "memocampus/backup-p/v1/password-check\0" || kind)
body  = STREAM-BE32(XChaCha20-Poly1305, K_enc, noncePrefix, AAD = SHA-256(prefix)) over the sections, 64 KiB chunks
C sections: meta (JSON: schema 7, mode plain) | policy (optional) | vault-sqlite3
D sections: package-b (a whole format B file)
```

- New backups: m = 64 MiB, t = 3, p = 1 (`container::ARGON_*_DEFAULT`); 256 MiB is only the upper bound a restore accepts,
  never the cost of a new backup. A restore accepts m 19–256 MiB, t 1–10, p 1–4 and checks them (and every length,
  the section layout and the exact file length) **before Argon2 allocates anything** [TEST
  `crafted_kdf_parameters_are_refused_before_argon2_runs`: each refusal under 500 ms].
- Chunk removal, reordering, duplication and truncation fail (the chunk counter and the last-chunk flag are in the nonce, the
  total length is fixed by the authenticated header) [TEST `removed_reordered_duplicated_or_cut_chunks_are_refused`]; any
  header change changes the AAD of every chunk [TEST `a_changed_header_or_section_table_is_refused`].
- D: the password yields only a B package, which still needs the phrase; the phrase cannot open the outer layer [TEST
  `format_d_needs_both_the_password_and_the_phrase`]. The inner header (vault id, envelope) is not visible in a D file.
- Wrong password vs damage: the check value separates `backup_password_rejected` from `backup_tampered`. Trade-off: it tells
  an attacker nothing they could not learn by trying to decrypt the first chunk, and every guess still costs one Argon2id
  run; a tampered check value is reported as a wrong password (never as success).
- Password rules (new backups): at least 8 characters, at most 1024 bytes, entered twice in the panel; stored nowhere.

## 3. Backup consistency (`manage::backup`)

1. Bound to the vault the panel showed (`expectedVaultToken`, Issue #48 `read_for`): a switch before the read starts gives
   `vault_switched` and no file [TEST `a_backup_names_the_vault_shown_and_refuses_another`, `backup_race_tests`]. A switch
   after the read started does not affect the backup: it is a snapshot of the vault shown, and the answer names that vault.
2. Encrypted (B, D): the key from this user's key store, the envelope from `<vault>.mckey`, both checked against the file's
   salt and the config's vault id (`recovery_metadata_missing|damaged|mismatch` otherwise), **and the phrase the user typed
   must open that envelope to exactly that key** (§3.2).
3. Snapshot.
   - B, D: the SQLCipher **Backup API** under one read transaction on the source (committed WAL content included), into
     `<app dir>\backup-work\<uuid>.sqlite3` keyed with VK: SQLCipher ciphertext with a new salt, never plaintext. Checked:
     full `integrity_check`, exact schema, the row counts read inside the source transaction, and the first bytes.
   - C: **no work file** (§3.1). Inside one read transaction the plain vault is checked (full `integrity_check`, exact
     schema, counts) and its pages are read from its own file one at a time and encrypted as they are read.
4. The container goes to `<folder>\.memocampus-backup-<uuid>.partial` (`create_new`), is flushed (`sync_all`), then read back
   **in full** with the key or password just used: every MAC/AEAD tag must pass and the vault bytes inside must hash to the
   SHA-256 of what was written. Only then `rename_no_replace` to `memocampus-backup-<UTC>[-n].mcbackup`; nothing is ever replaced.
5. A failure removes this backup's `.partial` and its work file. Fault injection at every step (snapshot, metadata, write,
   verification, before the rename) with an I/O error and a full disk leaves no file in the folder and no work file [TEST
   `a_backup_failing_at_any_step_leaves_no_file_and_reports_no_success`]; a full disk is reported as `disk_full`.
6. A process dying (simulated: no cleanup runs) leaves only a hidden `.memocampus-backup-<uuid>.partial` (ciphertext for B, C,
   D) and, for B and D, a work file (SQLCipher ciphertext under VK); the next backup removes work files whose owner is gone
   (each has a lock file its owner holds open without sharing); a death right after the rename leaves a complete, restorable
   backup and no `.partial` [TEST `a_backup_process_dying_leaves_only_partial_files_which_never_pass_for_backups`,
   `a_format_c_backup_dying_at_any_step_leaves_only_ciphertext`].

### 3.1 Format C without a plaintext work file (review finding C-01)

Isolated PoC first (`poc-c01`, scratch crate on the production engine: SQLite 3.45.3 / SQLCipher 4.5.7, MSVC), then the
production change.

| # | Question | Result |
|---|---|---|
| P1 | Backup API from a plain connection into a keyed (SQLCipher) target — the review's proposal | **refused**: `backup is not supported with encrypted databases`; target file 0 bytes |
| P2 | `sqlite_dbpage` (raw page images through the pager) in this build | **absent**: not in `compile_options` (`libsqlite3-sys` bundles `DBSTAT`, not `DBPAGE`); `no such table` |
| P3 | Ephemeral SQLCipher snapshot: `ATTACH … KEY K_temp; sqlcipher_export` + ids, inside one read transaction | works: ciphertext (no SQLite magic, no canary), opens with K_temp only (wrong key and no key: `NOTADB`), content equal. **But** the C payload must stay plain SQLite file bytes (format compatibility), and with P1 and P2 there is no way to turn the keyed temp back into those bytes without writing a plain file or a custom VFS |
| P4 | Pages of the plain source read from its own file while one read transaction holds SHARED (rollback journal) | consistent: a concurrent writer gets `SQLITE_BUSY` until the transaction ends; bytes = `page_size × page_count`; the bytes open as a database with `integrity_check` ok and equal content; the writer commits afterwards |
| P5 | Same, source in WAL mode | **not consistent**: a committed row still in `-wal` is missing from the main file |
| P6 | Same, source over 1 GiB (281,970 pages of 4 KiB, release build) | the copy with the page at 1 GiB (SQLite's lock bytes on Windows, never data) written as zeros: `integrity_check` ok, all rows equal |

Adopted: **P4/P6, page streaming**. The ephemeral SQLCipher route (P3) is not used: it cannot produce the format C payload
here. Page streaming needs neither a work file nor a temporary key, so nothing can be left behind but the container's own
ciphertext `.partial`:

- `manage::backup::stream_plain_pages`: `BEGIN`; refuse WAL mode (`backup_wal_unsupported`); full `integrity_check`, exact
  schema, counts (this takes SHARED and pins the file until `COMMIT`); `page_size`, `page_count`; open the vault file and
  check its identity against the token the panel showed (`vault_switched` otherwise); stream pages 1..n (one page in memory,
  zeroed on drop; the lock page as zeros) into the STREAM AEAD writer; `COMMIT`.
- Writers are blocked (they wait `busy_timeout`, then the extension keeps the event queued) for the time the pages are read,
  as during the Backup API copy of A, B and D.
- Tests: no plaintext SQLite header or canary in any file except the source vault at every checkpoint of a C backup
  [TEST `a_password_backup_of_a_plain_vault_writes_no_plaintext_to_disk_at_any_step`]; a process dying at every step leaves
  only ciphertext and no work file [TEST `a_format_c_backup_dying_at_any_step_leaves_only_ciphertext`]; a concurrent writer is
  blocked and its row is absent from the backup [TEST `a_format_c_snapshot_is_one_consistent_state_while_others_write`]; WAL
  mode refused with nothing written, while format A still includes WAL content [TEST
  `a_plain_vault_in_wal_mode_is_refused_for_format_c_and_nothing_is_written`]; about 24 MiB through C, B and D [TEST
  `a_larger_vault_round_trips_through_every_container_format`].

### 3.2 The phrase is proven when a B or D backup is made (review finding C-03)

`vault_backup` of an encrypted vault needs `recoveryPhrase`. Before any file is written:

1. Syntax (12 words, word list, checksum) before the folder dialog: `recovery_phrase_invalid` with `reason`/`position`.
2. Inside the vault binding (`read_for`): the key from this user's key store (DPAPI, key check value bound to the vault id and
   the file's salt); the sidecar for the same id and salt; the phrase must open the sidecar's envelope
   (`recovery_phrase_rejected` otherwise, also for a valid phrase of another vault; `recovery_metadata_damaged` for an
   envelope that does not authenticate); the key it holds must be the key store's key (`recovery_key_mismatch`).
3. The phrase is parsed into a seed that is zeroed on drop; it is never written, logged or answered. The panel sends it in the
   one `vault_backup` request (not through the event queue) and clears the field when the request ends.

[TEST `b_and_d_backups_need_the_phrase_that_recovers_this_vault`: no phrase, three words, another vault's phrase, swapped
words, a missing, unparsable, altered, foreign and wrong-key sidecar are all refused with no file written; with the right
phrase B and D are made and restore; the phrase is in no file and no response. UI: `vault-restore.test.js`.] This proves the
phrase at the time of the backup; it does not prove that the medium keeps the backup or that a later version reads it:
restore tests remain necessary.

## 4. Restore transaction (`manage::restore`)

| Step | Message | Effect |
|---|---|---|
| 1–2 | `vault_restore_inspect {path?}` | Picks the file (dialog), detects the format, checks bounds and exact length; answers `format`, `mode`, `needsPassword`, `needsPhrase`, `policyCopy`. Writes nothing. |
| 3–6 | `vault_restore {backupPath, path?, password?, recoveryPhrase?}` | Picks the target (a new file; dialog), decrypts and verifies into `<target>.partial`, encrypted: new vault id, key store entry (DPAPI of **this** user) and sidecar; promotes everything with no replacement. The active vault and the config are unchanged. Answers counts and the policy copy. |
| 7–9 | `vault_restore_activate {expectedVaultToken}` | The user's confirmation: the restored vault becomes active, publication **pending**. |
| — | `vault_restore_discard` | Removes what the recorded restore created. Never the active vault. |

Manifest `<app dir>\vault-restore.json` (one restore at a time; `vault_restore_pending` otherwise):

| Phase | On disk | After a crash |
|---|---|---|
| `restoring` | `<target>.partial` being written (C, A: plaintext — it is the restored plain vault) | `status.restorePending` shows it; activation refused (`vault_restore_incomplete`); discard removes the `.partial` (and, once known, key store entry and sidecar of the new vault id) |
| `verified` | verified `.partial`; encrypted: `keys\<newId>.json.pending`, `<target>.mckey.pending`; the `.partial`'s file identity is recorded | activation finishes the promotion; or discard |
| `restored` | `<target>` (+ `.mckey`, key store entry) | activation or discard |

- Idempotency: a retried activation after a crash between the config write and the manifest removal is accepted (the config
  already names the vault); a second activation answers `vault_restore_not_pending` [TEST
  `a_restore_interrupted_by_a_crash_is_reported_and_can_be_discarded_or_finished`].
- Discard removes only: the `.partial` this restore reserved (before verification), files whose identity (`vault_token`,
  recorded before the rename) matches, the sidecar and key store entries naming the new vault id, and the manifest.
- Every failure path removes everything the restore created; config, active vault and policy unchanged [TEST
  `a_restore_failing_at_any_step_removes_everything_it_created` (I/O error and full disk at write, verification, key
  writing, promotion), `a_damaged_backup_is_refused_after_streaming_and_leaves_nothing`].
- Never overwrites: the target, `<target>.partial`, the sidecar names and journal names must not exist; the active vault's
  path is refused the same way [TEST `a_restore_never_overwrites_and_never_touches_the_active_vault_or_the_policy`].
- Encrypted: the vault key comes only from the backup's envelope and the phrase. No key store or DPAPI data of the original PC
  is read or copied. The restored vault gets a **new vault id**, so restoring next to the original or twice never collides
  [TEST `restoring_next_to_the_original_and_twice_never_collides`]; a fresh host process then opens it through DPAPI without the
  phrase, and the phrase still recovers it through its new sidecar.
- Location: the target goes through the same location check as a new vault (`location_needs_approval` → the panel asks).
- Format A is restored with the Backup API from the backup opened read-only; a file that is not a vault of schema 7 is refused
  (`backup_vault_invalid` / `backup_schema_mismatch`). The exact schema check also refuses extra triggers or views a crafted
  SQLite file could carry.

## 5. Privacy policy and publication

- A restored vault is always `pending`, also when the source was confirmed and the current policy is valid [TEST
  `a_restored_vault_stays_closed_to_mcp_even_with_a_confirmed_source_and_a_ready_policy`; end to end with the real host and
  MCP in `mcp-process.test.js`: refused at start with `vault_publication_pending`, served after confirmation].
- Policy copy: B/C/D carry `{format:"memocampus-policy-copy", version 1, policy, approvedDomains}` when the global policy is
  usable at backup time (else no copy; the answer says `policyCopy: false`). B seals it under VK, C/D inside the password
  layer. A has none (PO decision P2, architecture §7.3).
- A restore shows it (state `valid` with revision and content, `invalid`, `absent`, or `not_supported` for A). It is **never
  written** to `policy.json` or the approvals: the panel shows it for review; adopting it is a separate edit in the privacy
  settings [TEST: `policy.json` byte-identical, and no policy file is created on the other PC].
- MCP and VisibleStore are unchanged: they read only the active, confirmed vault; a switch during a read stays fail-closed (B0).

## 6. Differences from `architecture.md` (reported, not silently resolved)

| # | architecture.md | Slice C | Why |
|---|---|---|---|
| DC1 | B = raw ciphertext copy of the vault file (same salt) | Backup API into a keyed work file (new salt); the header records both `cipherSalt` (payload) and `recoverySalt` (envelope binding) | PO order: Backup API first (Slice A review); no 1 GiB lock-byte special case; WAL handled by SQLite |
| DC2 | B may be restored with the local key store instead of the phrase | the phrase is always required | PO order (§3 B: 「復元には正しい12単語フレーズを要求する」) |
| DC3 | — (vault id kept implicitly) | a restored encrypted vault gets a new vault id and a new envelope for it (same VK, same phrase) | restoring next to the original or twice would collide in the key store |
| DC4 | C over B is kind `C` with a `package-b` section | kind `D` | the four PO formats; the secrets needed are known before the password is entered |
| DC5 | Argon2 m up to 1 GiB | up to 256 MiB | 「過大なメモリ消費を引き起こさない」 |
| DC6 | B policy copy sealed inside the header | sealed after the payload (length in the header) | the header is limited to 64 KiB, a policy may be up to 1 MiB |
| DC7 | restore = publish the file; activation is an ordinary `vault_open` | dedicated `vault_restore_activate` after the user's confirmation, with a manifest | PO state machine (§7: 確認 → 切り替え → pending) and crash handling |
| DC8 | `.partial` leftovers listed at the next start and offered for deletion | backup work files cleaned automatically; restore leftovers via `restorePending` + discard; `.partial` files in a backup folder are documented, not listed | the host does not know every folder the user chose |
| DC9 | C over A: source bytes stream-encrypted under SHARED, no plaintext temp (§5.4) | as designed, by page reads inside the read transaction (§3.1); **a source in WAL mode is refused** for C | (first version used a plaintext work file; fixed after review C-01) |
| DC10 | B needs no phrase when made | B and D need the phrase that recovers the vault when made (§3.2) | review C-03 |

## 7. Error codes (Native Messaging, fixed; no secret, no key, no row)

`backup_password_required`, `backup_password_too_short`, `backup_password_too_long`, `backup_password_rejected`,
`backup_path_required`, `backup_unsupported_format`, `backup_unsupported_version`, `backup_malformed`, `backup_incomplete`,
`backup_tampered`, `backup_damaged`, `backup_schema_mismatch`, `backup_vault_invalid`, `recovery_phrase_invalid` (with
`reason`/`position` as in Slice B), `recovery_phrase_rejected`, `recovery_metadata_missing|damaged|mismatch`,
`vault_restore_pending`, `vault_restore_not_pending`, `vault_restore_incomplete`, `vault_restore_damaged`, `restore_failed`,
`backup_failed`, `disk_full`, `file_exists`, `parent_missing`, `vault_switched`, `invalid_request`, `backup_wal_unsupported`
(C of a vault in WAL mode), `recovery_key_mismatch` (B/D: the envelope opens to another key than the vault's).

## 8. Verification

| Suite | Before (Slice B merged) | This run | New |
|---|---|---|---|
| native-host (unit + integration) | 504 | **540 pass** | 36 (container 15, backup/restore 21) |
| visible-store | 115 + 2 ignored | 115 + 2 ignored | 0 |
| mcp-server | 57 | 57 | 0 |
| Node, standard | 728 (718 + 10 skipped) | 735: **724 pass**, 0 fail, 11 skipped (opt-in) | 7 (UI 6, end to end 1); after the review fixes the B/D UI test also covers the phrase |
| Node, with `MEMOCAMPUS_TEST_HOST` + `MEMOCAMPUS_TEST_MCP` (debug binaries) | — | **735 pass**, 0 skipped | — |

`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean in all three crates; `git diff --check` clean.
Windows MSVC release build of the three crates: 59 s incremental (OpenSSL already built); host 9,063,424 B (+358,912),
MCP 12,193,280 B. Release timings on a nearly empty vault: B backup 64 ms, D backup 423 ms (two Argon2id runs: write and
verification), B restore 67 ms, D restore 242 ms, D with a wrong password refused in 197 ms. Debug-build test of a vault
with about 24 MiB of images through C, B and D: about 60 s in total.

Changed expectations of existing tests: `secure_vault_b.rs` (an encrypted vault's backup is now format B instead of being
refused) and `vault-backup.test.js` (the success message names the format: 「（形式A）」).

Canaries (URL, title, tag, image URL, image blob) are checked absent from B, C and D backups, from crash leftovers in the
backup folder, from the restored encrypted vault and its metadata, and from every response; the password and phrase from
every file and response. Format A is plaintext by design and is not checked.

## 9. NOT RUN

- A real second Windows user, a second PC or an OS reinstall (simulated by separate app directories under the same user:
  separate config, key store and policy; DPAPI is still this user's).
- A real power cut or process kill during backup or restore (simulated by fault injection that skips all cleanup).
- Vaults over 1 GiB (largest tested: about 24 MiB of image data), a real full disk (simulated `StorageFull`), antivirus
  holding `.partial` files, network or cloud-synced backup folders.
- Real Chrome with the side panel and a real MCP client (the UI is tested against the fake host; the real host and MCP are
  tested end to end without Chrome).
- GitHub Actions (runs after the PR is opened).

## 10. Windows dogfood (PO)

Use a throw-away app directory (`MEMOCAMPUS_APP_DIR`) or a test Windows user; never the real vault.

1. Plain vault: **バックアップを作成…** (通常) → a `.sqlite3` (format A). **バックアップから復元…** → choose it → choose a new
   file → the panel shows counts and "まだ使用していません" → **このVaultを使い始める** → MCP 未承認.
2. Plain vault, **パスワードで保護する**: a short or mismatched password is refused without a request; a good one makes a
   `.mcbackup` (format C). Restore with a wrong password (refused, nothing created), then the right one.
3. Encrypted vault, normal backup → format B: the panel asks for the phrase; a wrong one (or another vault's) is refused and
   nothing is written. On another Windows user (or another PC), restore with the phrase only; check
   that the original PC's key store was not needed, that the restored vault opens after restarting Chrome without the phrase,
   and that MCP is 未承認 until confirmed.
4. Encrypted vault, password → format D. Restore needs both; try each alone.
5. Policy copy: initialize a policy before the backup; restore on a PC with a different policy; the copy is shown (改訂n,
   JSON), and the current policy is unchanged.
6. Try to restore onto an existing file and onto the active vault (refused); open a non-backup file (refused).
7. Close the side panel during a long restore, reopen: the pending restore is shown; activate or discard.
8. Delete-on-failure: fill a small USB stick or VHD and back up to it → `disk_full`, no file left.

## 11. Residual risks

| # | Risk | Mitigation / status |
|---|---|---|
| R-C1 | (fixed, C-03) the phrase is proven when B/D is made; it does not prove the medium or future readability | restore tests stay the user's proof |
| R-C2 | (fixed, C-01) no plaintext work file for C. Remaining: a plain vault in WAL mode cannot get a C backup (refused; A works); the pages read are in process memory one at a time, and Windows may page that memory out | Memocampus never sets WAL; VisibleStore already refuses WAL vaults; no claim about the page file or crash dumps |
| R-C3 | A crash while restoring A or C leaves a plaintext `<target>.partial` (it is the restored plain vault) | shown as `restorePending`, removed by discard |
| R-C4 | `.memocampus-backup-*.partial` left in a backup folder after a crash | B/C/D: ciphertext; A: plaintext as before Slice C; never named like a finished backup; not cleaned automatically |
| R-C5 | Password and phrase pass through Native Messaging and live in JS strings and serde buffers that cannot all be zeroised | same as Slice B; inputs cleared after each step; never stored or logged |
| R-C6 | Weak user password (C over A) | 8-character minimum and Argon2id only slow guessing |
| R-C7 | Two restores of one backup start with the same 16 bytes (the payload's SQLCipher salt) | different vault ids, key store entries and sidecars; harmless for opening (salt + id both checked) |
| R-C8 | A very large vault can exceed the panel's 10-minute request timeout; the host keeps working and the result appears in the status (restore) or in the folder (backup) | NOT RUN at that size |
| R-C9 | An old phrase plus an old B/D backup keeps access (no revocation) | as in architecture §9 (T10) |

## 12. PO decisions requested

1. DC1–DC10 above (in particular DC2 phrase always required, DC3 new vault id, DC8 leftover handling).
2. C-01: page streaming instead of the ephemeral SQLCipher snapshot the review proposed (§3.1: that route cannot produce
   the format C payload in this build); format C refused for a plain vault in WAL mode.
3. C-03: the phrase is mandatory for every B and D backup (no "skip" option).
4. Password policy: minimum 8 characters, no strength meter.
5. Argon2id cost: 64 MiB / t = 3 / p = 1 for new backups; restore bounds 19–256 MiB, t ≤ 10, p ≤ 4.
6. Whether to add "adopt this policy copy" (an explicit `policy_apply` from the restore view) in a later slice; today the copy
   is review-only.
