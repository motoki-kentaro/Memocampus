# Secure Vault — Slice E: the Recovery Spell as the only recovery secret (Issue #51)

Status: **implemented, awaiting independent security review and PO approval.** Branch `feature/51-secure-vault-e`,
based on `main` at `9e61584` (Slice D merged, PR #55). No commit, push or PR has been made.

Markers: **[TEST]** covered by the automated test named, **[POC]** measured by `native-host/tests/secure_vault_e_poc.rs`,
**[CODE]** read in the code, **NOT RUN** not exercised.

## 1. The invariant

> The encrypted vault file and its 43-character Recovery Spell are all that is needed to recover the memories it holds:
> no other file, secret or operating-system credential.

Before this slice the vault key (VK) was random and only **wrapped** under the spell, in `<vault>.mckey`; a user who kept
the spell but lost the sidecar could not recover the vault on another PC. Since this slice VK is **derived** from the spell
and a public salt that the vault file itself carries, so the file and the spell suffice.

**Security model.** The attacker has the source code, the encrypted vault file, its salt and every other public value
(vault id, key check values, backup headers), the algorithms, the KDF parameters and the file formats. The attacker does
not have the spell: 256 bits from the OS CSPRNG. Offline decryption costs one HKDF-SHA256 plus SQLCipher's page check per
guess of a 256-bit value. Nothing depends on the secrecy of the salt, the vault id or the algorithms.

The invariant is the subject of the most important test,
[TEST `tests/secure_vault_e.rs::the_vault_file_and_its_spell_alone_bring_the_memories_back`]: an encrypted vault with
visits and a 1.5 MiB image is created through the real host, **only the vault file** is copied (under another name, into
an otherwise empty folder whose name has Japanese characters, `#` and spaces) and recovered with the spell by a host whose
app directory is empty (no key store, no DPAPI entry, no config, no policy, no `.mckey` anywhere); the schema, every visit,
the image blob byte for byte and the saved image are compared, and publication is `pending`.

## 2. PoC (order §3)

All experiments are kept as tests in `native-host/tests/secure_vault_e_poc.rs`, so that an engine or crate upgrade that
changes one of them fails there first. SQLCipher 4.5.7 (`bundled-sqlcipher-vendored-openssl`), `cipher_compatibility = 4`.

| # | Question | Result |
|---|---|---|
| P1 | VK = HKDF-SHA256(spell, salt, label) deterministic; spell, salt, label each change it; usable by `cipher::open` | yes; fixed vector checked against RFC 5869 computed by hand (`p1_…`, `recovery::tests`) |
| P2 | Where does the public salt live? | SQLCipher's own salt, the file's first 16 bytes (`p2_p3_…`): readable without any key, survives copy/move/rename |
| P3 | Can the caller choose SQLCipher's salt? | yes: the raw key with explicit salt `x'<64 hex key><32 hex salt>'` on a **new** database writes exactly that salt as bytes 0–15 (`p2_p3_…`); `PRAGMA cipher_salt` reports it |
| P3 | Does SQLCipher derive anything from VK? | no: a raw key (`x'…'`) is used as the key itself (no PBKDF2); the same 64 hex digits given as a **passphrase** are a different key and never open the vault (`p2_p3_…`) |
| P3 | Reopening | the raw key alone (`x'<64 hex>'`): SQLCipher reads the salt from the file. The raw key with a **different** explicit salt is refused (`p2_p3_…`) |
| P2 | Tampering | a changed salt byte gives another VK and another HMAC key: refused, never misread; a changed page fails its HMAC when read (`p2_tampering_…`) |
| P2 | `cipher_plaintext_header_size` | **rejected**: measured, the explicit salt is not kept at all; the salt in use becomes the constant SQLite magic, the same for every vault, so a per-vault salt would need a sidecar again (`p2_alternatives_…`) |
| P2 | An app header in front of the SQLCipher file | **rejected**: the file is no longer a SQLCipher database; neither SQLCipher nor any other tool opens it (`p2_alternatives_…`) |
| P4 | Re-encryption under a new spell and salt (`sqlcipher_export`, source `main` read-only and keyed, target attached `KEY ''` then keyed with key **and** salt through the C API) | the new file starts with the new salt, holds the same schema and rows (`integrity_check` ok), opens only with the new spell; the old VK and the old spell are refused on it; the source is byte-for-byte unchanged and still opens with the old spell (`p4_reencryption_…`) |
| P4 | A restore under a new salt (so a new vault id) with the **same** spell | VK' = HKDF(spell, salt') opens it; nothing else is needed (`p4_reencryption_…`) |
| P4 | Committed WAL frames of the source | exported; the new vault is in `delete` journal mode (`p4_wal_…`) |
| P4 | An existing encrypted database attached without its key | refused by `ATTACH` itself (Slice D P2). Consequence for the refresh of a re-keyed copy: two connections, §7.2 |

Comparison of the options (order §3 P2):

| Criterion | SQLCipher salt as the public parameter (**adopted**) | `cipher_plaintext_header_size` | App header in front |
|---|---|---|---|
| No other file needed | yes | no (salt lost, measured) | yes |
| SQLCipher compatible | unchanged format | changed format, salt externally kept | not a SQLCipher file |
| Performance / safety in use | unchanged (raw key: no PBKDF2 at open) | unchanged | unchanged |
| Salt: generation / storage / reading | OS CSPRNG → raw key with salt at creation → bytes 0–15 → read before opening (`cipher::read_head`) | no per-vault salt in the file | in the header |
| Tamper detection, wrong key refused | SQLCipher page HMAC; a changed salt is another key | same, minus the salt | needs own MAC |
| Backups | the payload (a SQLCipher file) carries its own salt | needs the external salt | needs the header |
| Windows / Linux / macOS | nothing OS-specific (DPAPI only caches) | same | same |
| Other SQLCipher tools | open with `PRAGMA key = "x'<VK hex>'"` | need the salt | cannot open |
| Survives move, copy, rename | yes [TEST 12.1] | no | yes |

## 3. Key derivation (fixed)

```text
spell   = 32 bytes, OS CSPRNG; shown once as 43 base64url characters (strict canonical parser, unchanged)
salt    = 16 bytes, OS CSPRNG; SQLCipher's salt = the vault file's bytes 0–15
VK      = HKDF-SHA256(ikm = spell, salt = salt, info = "memocampus/vault-key/v3")          32 bytes
vaultId = SHA-256("memocampus/vault-id/v3\0" || salt)[0..16]                                public name only
kcv     = HMAC-SHA256(HKDF-SHA256(ikm = VK, info = "memocampus/kcv/v1"), vaultId || salt)   (unchanged)
SQLCipher key at creation: x'<VK hex><salt hex>'   at every later open: x'<VK hex>'   (sqlite3_key_v2, never SQL text)
```

[CODE `secure_vault/recovery.rs` `vault_key`, `vault_id`, `NewVault`; `cipher.rs` `apply_key_to`, `apply_new_key_to`,
`create`]. Scheme name `spell-hkdf-sha256/sqlcipher4-raw/v3` (`recovery::SCHEME`), in backup headers.

- **Salt size and role.** The 16 bytes are both SQLCipher's salt (used by SQLCipher only for its HMAC key derivation, since
  VK is a raw key) and the HKDF salt. They are one value on purpose: it is the only public value the file carries before it
  is decrypted. HKDF's security does not rest on the salt here (the input is already 256 uniform bits); the salt makes VK
  unique per vault, so one spell never yields the same key for two vaults.
- **Domain separation.** `memocampus/vault-key/v3` is used for nothing else; the keys derived *from* VK keep their own labels
  (`memocampus/kcv/v1`, `memocampus/backup-b/v3/mac`, `…/v3/policy`); the password backups' keys come from Argon2id
  (`memocampus/backup-p/v1/…`, unchanged). The labels of the earlier envelope (`memocampus/recovery-spell/v2/…`) are no
  longer used by any code.
- **Not inputs.** The vault's path, the Windows user, the machine and DPAPI play no part. The vault id is a function of the
  salt, which the vault file carries.
- **The vault id** names the vault in the config, the key store and the backup header. Two copies of one file share it
  (they are the same vault, with the same key). A new vault id always means a new salt, so a new key (§6).

## 4. `.mckey` removed (order §5)

- Nothing writes a sidecar any more: creation, D1, re-issue and restore make none [TEST `secure_vault_b.rs`,
  `convert_tests::d1_…`, `backup_restore_tests::an_encrypted_vault_…`, `tests/secure_vault_e.rs`].
- Open, recover, backup, restore and convert never read one for a key [CODE: `sidecar.rs` has no reader of envelopes].
- **Earlier vaults are unsupported, explicitly.** A vault of Slices B–D has a random VK; this build cannot derive it. Such a
  vault is recognised by a `.mckey` (version 1 or 2) beside it naming its salt, or by a version 1 key store entry naming its
  salt (`sidecar::legacy_for`, `key_store::legacy_entry_for`), and is refused as `vault_legacy_unsupported` by open,
  recover, status (state) and backup; nothing about it is changed, migrated or deleted
  [TEST `secure_vault_b.rs::a_vault_of_an_earlier_scheme_is_refused_as_unsupported_and_left_as_it_is`]. Without either
  marker it cannot be told from a vault of this scheme with a wrong spell: the answer is then `recovery_spell_rejected`,
  and it is never misread (a derived key that is not its key fails SQLCipher's check).
- A stray sidecar naming another salt beside a vault of this scheme changes nothing
  [TEST `secure_vault_b.rs::recovery_needs_the_vault_file_and_its_spell_only_…`].
- Since the first bytes of an encrypted vault are random, a file that does not start with the SQLite magic is taken for an
  encrypted vault only if its length is a whole number of 512-byte pages (`cipher::plausible_size`); any other file is
  `vault_invalid` as before [TEST `stdio_protocol::open_switches_vaults_and_rejects_invalid_files`].

## 5. DPAPI and daily use (order §6)

The key store keeps VK protected with DPAPI (user scope) so that daily opens need no spell. It is a cache: never what a
recovery depends on [TEST 12.1 and `losing_the_key_store_of_this_windows_user_loses_nothing_but_convenience`].

- Entry **version 2** (`keys\<vaultId>.json`): as before (vault id, salt, protector, blob, kcv), and the vault id must be
  `recovery::vault_id(salt)` on every read and write. Version 1 entries are `KeyError::Legacy`, never used, never removed or
  replaced [TEST `key_store::tests`].
- Open: DPAPI first; no usable entry → `key_unavailable` with `recoveryAvailable: true` for any plausible encrypted file
  (the file alone is what recovery needs).
- Recover: VK from the spell and the file's salt, proven on the vault (`open_checked_in`: keyed open + `quick_check`), then a
  DPAPI entry for this user: `New` if none, kept if it already holds this key, `Replace` (one rename) if it is unusable.
  Entries of other vaults are never touched (one entry per vault id). Publication `pending` afterwards.
- A key or kcv mismatch, a swapped entry, or an entry of another vault fails closed (unchanged tests in `key_store`).

## 6. Backups A–D and restore (order §7)

Container **version 3** (`container.rs`). Versions 1 and 2 of B and D (random VK in an envelope) are refused as
`backup_legacy_unsupported` before any secret is asked for; C of versions 1–2 is still read (its keys never involved a
spell). Any other version is `backup_unsupported_version` [TEST `container::tests`, `backup_restore_tests::files_that_are_not_backups_…`].

| Kind | Change |
|---|---|
| A | unchanged (SQLite Backup API) |
| B | header: `keyScheme`, `vaultId` (= the salt's, checked), `cipherSalt`, `kcv`, lengths, sealed policy; **no envelope**. The payload is the Backup API snapshot keyed with VK **and the vault's salt**, so it starts with that salt (checked when written and when read). Restore: VK = HKDF(spell, `cipherSalt`), checked against `kcv` (wrong spell → `recovery_spell_rejected` before anything is written), then the MAC over the whole file (`backup_tampered`). MAC and policy keys `HKDF(VK, …/v3/…)` |
| C | unchanged (streamed, no plaintext temp file, WAL refused, Argon2id parameters and checks unchanged) |
| D | unchanged outer layer around a version 3 B package: the password and the spell are both required |

Creating B or D still requires the spell of this vault (review finding C-03): it must derive, with the vault's salt, the key
this user's key store holds [TEST `backup_restore_tests::b_and_d_backups_need_the_spell_that_recovers_this_vault`].

**Restore: a new vault id with the same spell.** A restored vault gets a new vault id. Since the id is the salt's, that
means a new salt `salt'` and therefore a new key `VK' = HKDF(spell, salt')`. The restore has the spell (B and D need it), so
it can derive `VK'`: the payload is streamed into `<target>.payload.partial` (still ciphertext under the backup's VK),
verified, exported (`convert::export`, compared row by row in one transaction) into `<target>.partial` under `VK'` and
`salt'`, verified on its own, and the payload work file is removed. The condition under which "a new vault id" and "the
same spell restores it" are compatible is exactly this: **the new vault id is derived from a new salt, and the new key from
the same spell and that salt**; no stored key is reused [TEST `backup_restore_tests::an_encrypted_vault_backs_up_encrypted_and_restores_on_another_pc_with_the_spell_only`
(including the recovery of the restored file with the same spell in an empty app directory),
`restoring_next_to_the_original_and_twice_never_collides`, `a_password_backup_of_an_encrypted_vault_needs_both_…`].

Restore states (manifest version 2): `restoring` (payload work file, then `.partial`), `verified` (`.partial` +
`keys\<newId>.json.pending`), `restored`; fault injection and crashes at every step including the new `RestoreRekey`
[TEST `a_restore_failing_at_any_step_removes_everything_it_created`, `a_restore_interrupted_by_a_crash_…`].

## 7. Re-issuing the Recovery Spell (order §8)

### 7.1 Operation

`vault_convert_start {direction: "reissue", expectedVaultToken, path?}` on an **encrypted** active vault, then the Slice D
steps unchanged: `vault_convert_confirm {recoverySpell}` (the new spell in full) → `vault_convert_activate` / `_keep` /
`_discard`. It is a third conversion direction; everything Slice D guarantees holds for it (manifest, unpredictable work
file name, `create_new` / `rename_no_replace`, refusal of out-of-order and concurrent operations, discard of provable
leftovers only, the source never written).

1. The source is unlocked with this user's key store (no spell needed: a lost spell is the reason to re-issue).
2. A new spell (CSPRNG) and a new salt (CSPRNG, never the source's) are generated; the new vault id and salt are written
   to the manifest **before** any file exists; VK' follows from them.
3. `sqlcipher_export` into the work file keyed with VK' and the new salt; verification; DPAPI entry `…json.pending`.
4. The new spell is answered once (the only time it leaves the host).
5. `confirm`: the typed spell must derive, with the new salt, the pending entry's key; then promotion.
6. `activate`: under the switch's write fence the copy is refreshed from the source (§7.2) and verified; the new vault
   becomes active, publication `pending`.

[TEST `convert_tests::reissuing_the_spell_makes_a_new_vault_that_only_the_new_spell_recovers`,
`a_reissue_is_refused_for_a_plain_vault_and_can_be_discarded_…`, the fault matrix (`reissue` × copy/verify/keys ×
error/full disk/crash), E2E `mcp-process.test.js`]: new and old spells differ, new and old keys differ, only the new spell
recovers the new vault (from the file alone, in another app directory), the old spell is refused on it, the old vault is
kept and still opens with the old spell, nothing is active before confirmation, visits recorded during and after the
confirmation are in the new vault, a second re-issue is refused while one is pending, publication is `pending`.

### 7.2 Refresh of a re-keyed copy

Both ends are encrypted, so neither can be attached (`ATTACH` refuses an existing encrypted database before it could be
keyed, and `ATTACH … KEY` would put the key in SQL text). `convert::refresh` therefore uses **two connections**, each keyed
through `sqlite3_key_v2`: the source read-only in a read transaction (its lock held throughout, its identity checked under
it), the target in one `BEGIN IMMEDIATE` transaction; every table is emptied and filled with bound values, and the two are
compared (definitions, header fields, every row in primary-key order) across the connections before the target commits.

### 7.3 What re-issuing does not do

It does not revoke the old spell: the old vault file and its backups still open with it. The panel says so (§11). The PO's
"complete revocation is not guaranteed" is stated in the UI text and here; deleting the old vault and backups is the
user's decision.

## 8. Conversion (Slice D) integration (order §9)

- D1 (plain → encrypted): the new spell and salt are generated at start and recorded (salt and id) before writing; VK
  follows; no `.mckey`; confirmation and staged promotion unchanged [TEST `convert_tests::d1_…`, `tests/secure_vault_d.rs`].
- D2 (encrypted → plain): unchanged: explicit acknowledgement, no spell asked, the source key from DPAPI, nothing secret in
  the plain vault [TEST `convert_tests::d2_…`].
- Every converted vault starts `pending`.
- `slice-d.md` keeps the Slice D description; where it names `.mckey` or a random VK for D1, this section supersedes it.

## 9. Privacy, Publication and Policy (order §10)

Unchanged mechanisms, re-tested: every new, recovered, restored, converted and re-issued vault starts `pending`
(`switch_active_*` with `fresh_publication`), the source's `confirmed` is never inherited, MCP (stdio and HTTP) and
VisibleStore refuse an unconfirmed vault [TEST E2E `mcp-process.test.js` (D1, re-issue, D2), `tests/secure_vault_e.rs`
(`active_vault::admit` → `PublicationPending`)]. Policy and approved domains are not touched by any of these operations.
The spell and VK appear in no log, no file, no status, no MCP answer and no Chrome storage; the spell leaves the host only
in the one start/begin answer [TEST spell canaries in `secure_vault_b.rs`, `convert_tests`, `vault-convert.test.js`].

## 10. States and crashes (order §11)

| Operation | Crash point | State left | Handling |
|---|---|---|---|
| Creation | after the pending DB / key entry | manifest `creating`/`awaiting_confirmation`, `.pending` files | confirm (if the spell was kept) or cancel; nothing usable |
| Creation | after promotion, before the switch | `promoted` | the next confirm switches |
| Recovery | after the DPAPI entry, before the switch | entry for this key, config unchanged | a retry (or a resend after a timeout) finds the entry already holding this key and switches; never a second entry [TEST `a_recovery_that_died_after_storing_the_key_…`] |
| D1 / re-issue | copy / verify / keys | manifest `converting`, work file | discard only (the work file can never be opened: `vault_work_file`) [TEST fault matrix] |
| D1 / re-issue | before confirmation | `verified`, pending key entry | confirm with the written-down spell, or discard |
| Activation | refresh fails | rolled back, config unchanged | retry or keep/discard [TEST `activation_fails_closed_…`] |
| Restore B/D | write / verify / rekey / keys | `restoring` or `verified`, payload or `.partial` | discard; or (after `verified`) activate finishes the promotion [TEST] |
| Old manifests (version 1, an earlier build) | — | unreadable | `*_damaged`; cancel/discard removes only the manifest; their `.pending`/`.partial` files cannot be proven to be that operation's and stay (they can never become a vault) |

8.3 short paths: identity is the vault token everywhere a file is compared (unchanged); a resend after a timeout meets the
manifest (`vault_convert_pending`, `vault_restore_pending`, `vault_creation_pending`) or an idempotent step.

## 11. User interface

- `SPELL_NOTE`: the spell and **one** vault file (or its backup) recover the memories on another PC, after a reinstall or
  as another Windows user; no other file is needed; a lost spell can be replaced by re-issuing while the vault still opens.
- New fieldset **ふっかつのじゅもんを再発行する** (encrypted vaults only): what happens, and that the old spell is **not**
  revoked (it still opens the old vault and old backups). The spell step and the decision box add the same note.
- Errors: `vault_legacy_unsupported`, `backup_legacy_unsupported` (earlier key management, nothing changed);
  `recovery_spell_rejected` now also says that a damaged or altered file is reported the same way. Removed:
  `recovery_metadata_*`, `recovery_key_mismatch`, `backup_legacy_recovery_phrase`.

## 12. Changed files

- New: `docs/secure-vault/slice-e.md`, `native-host/tests/secure_vault_e.rs`, `native-host/tests/secure_vault_e_poc.rs`.
- Host: `secure_vault/{recovery,cipher,sidecar,key_store,container,convert,secret,mod}.rs`,
  `manage/{encrypted,convert,restore,backup}.rs`, `manage.rs`, `vault.rs`, `dialog.rs`.
- Tests: `secure_vault/container/tests.rs`, `manage/{convert_tests,backup_restore_tests}.rs`,
  `vault/secure_vault_tests.rs`, `tests/{secure_vault_b,secure_vault_d,secure_vault_d_poc}.rs`,
  `visible-store/src/tests.rs`, `mcp-server/tests/http_mcp.rs`.
- Extension: `vault-client.js`, `vault-panel.js`, `search-client.js`, `sidepanel.html`; tests `fake-dom.js`,
  `vault-convert.test.js`, `mcp-process.test.js`.
- Docs: `architecture.md`, `threat-model.md`, `implementation-plan.md`, `recovery-spell.md`, `slice-d.md`, `README.md`.

## 13. Residual risks

| # | Risk | Note |
|---|---|---|
| R-E1 | Earlier encrypted vaults (Slices B–D) and their B/D backups become unusable with this build | by PO decision (no migration). A user with such a vault must decrypt it (D2) **with the previous build** before upgrading, or keep the previous build to read it |
| R-E2 | A vault of an earlier scheme without its `.mckey` and without a version 1 key store entry is reported as "wrong spell" | it cannot be told apart from a wrong spell; it is never misread or changed |
| R-E3 | Re-issuing does not revoke the old spell | old vault files and backups still open with it; stated in the UI |
| R-E4 | A junk file of a whole number of 512-byte pages is offered for recovery | a spell then fails as `recovery_spell_rejected`; nothing is written |
| R-E5 | The spell alone protects every copy of the vault and every B backup | as before; no second factor for B (D adds a password) |
| R-E6 | Restore of B/D writes the backup's ciphertext payload to `<target>.payload.partial` before re-encrypting | ciphertext only; removed on success, failure and discard |
| R-E7 | Unfinished operations recorded by an earlier build (manifest version 1) leave their pending files | they can never become a vault; the user removes them by hand |

NOT RUN: GitHub Actions; the Chrome UI by eye; a real process kill or power cut during re-encryption; a second physical PC
or Windows user (simulated with separate app directories, i.e. separate key stores; DPAPI itself is the same user's).

## 14. For the PO

1. **Earlier encrypted vaults are unsupported (R-E1).** If any real vault is encrypted with Slices B–D, it should be
   converted to plain (D2) with the current `main` build before this slice is installed.
2. **Vault id = function of the salt.** Copies of one vault file share their id (and key); a new id always comes with a new
   salt and key (restore, D1, re-issue). Alternative (a random id stored inside the encrypted database) was not chosen: it
   cannot be read before the key is known, and the key store needs the id first.
3. **Spell re-issue is a conversion direction** (`reissue`), reusing Slice D's states, fence and refresh; it needs no old
   spell (DPAPI unlocks the source).
4. **B/D creation still requires the spell** (review finding C-03), now checked by derivation.
5. **Unrecognised legacy files** (R-E2) are reported as a wrong spell.
