# Secure Vault — Slice D: converting a vault between plain and encrypted (Issue #51)

Status: **implemented, awaiting independent review and PO approval.** Branch `feature/51-secure-vault-d`, based on
`feature/51-recovery-spell` (`1e87209`): the Recovery Spell was not merged into `main` when this slice started, so the
slice is built on it rather than on `main` (which would lack the spell). No commit, push or PR has been made.

The slice adds two operations on the **active vault the side panel shows**:

- **D1 (`encrypt`)**: a plain vault is copied into a **new** SQLCipher vault with a new vault key (VK), a new vault id, a
  new DPAPI key store entry, a new `.mckey` (version 2) and a new Recovery Spell, shown once and re-entered in full.
- **D2 (`decrypt`)**: an encrypted vault is copied into a **new** plain SQLite vault, after the user acknowledged that
  everything in it will be stored unencrypted on disk.

In both directions the source vault is never written, moved, re-keyed or removed, the active vault does not change until
the user activates the converted vault, and the converted vault's MCP publication starts `pending`.

---

## 1. Invariants and where they are enforced

| # | Invariant (order §3) | Mechanism | Test |
|---|---|---|---|
| 1 | The source is not changed | the source is opened **read-only** (`file:` URI, `mode=ro`) by every connection of the conversion; no other code path of the conversion opens it for writing | `secure_vault_d_poc::p1`, `p2` (writes refused with `SQLITE_READONLY`), `e1`, `convert_tests::d1_…`, `d2_…` (file hashes, sidecar, key store unchanged) |
| 2 | The source is not deleted | `remove_converted` skips any path equal to the source or with the source's file identity (`vault_token`) and the active vault | `convert_tests::one_operation_at_a_time…`, `a_failed_conversion…` |
| 3 | The active vault is not switched implicitly | only `vault_convert_activate` switches (config lock, write fence) | every D1/D2 test checks the status before activation |
| 4 | No existing file is overwritten | target must not exist (nor its `.mckey`, `.mckey.pending`, `-journal`, `-wal`, `-shm`); all files `create_new`; promotion `rename_no_replace` | `bad_requests_and_targets_are_refused_without_side_effects` |
| 5 | Nothing unfinished is published as a vault | the copy is written to a work file with an unpredictable name, promoted only after verification; `vault_open`/`vault_recover` refuse `.partial`/`.pending` names and the pending conversion's target | `…kept_leaves_the_active_vault…`, `one_operation_at_a_time…`, `a_failed_conversion…` |
| 6 | No switch before the target is verified | activation re-copies under the fence and runs `compare` + full `integrity_check` + schema check before the config is written | `activation_fails_closed…` |
| 7 | The source stays usable after failure, cancel or crash | the source is only read; every failure path leaves it active | `a_failed_conversion…` (re-opened in a new session) |
| 8 | Publication of the new vault starts `pending` | `switch_active_fenced(…, fresh_publication = true)` | D1/D2 tests, `secure_vault_d.rs`, E2E |
| 9 | The policy is never relaxed or overwritten | the conversion never opens `policy.json` | (no code path) |
| 10 | Existing DPAPI entries are not damaged | D1 writes only `keys\<newId>.json(.pending)` (`create_new`); D2 writes no key; discard removes only the new id's entries | `d2_…` (key store byte-equal), `a_failed_conversion…` |
| 11 | No old spell is presented as recovering the new vault | D1 always generates a new spell; D2's plain vault has no sidecar; the UI says so | `the_spell_of_an_earlier_vault_never_recovers_a_converted_one`, `vault-convert.test.js` |
| 12 | MCP/VisibleStore cannot read an unapproved vault | the converted vault is not active until activation and then `pending`; readers go through `active_vault::admit` | `admit` asserted in D1/D2 tests, `secure_vault_d.rs`, E2E `mcp-process.test.js` |

## 2. Conversion method and PoC (order §4)

The experiments are kept as tests in `native-host/tests/secure_vault_d_poc.rs` (SQLCipher 4.5.7 community, the bundled
build of `rusqlite 0.32`, MSVC, Windows 11, temporary directories).

| # | Experiment | Result |
|---|---|---|
| P1 | plain → encrypted with `sqlcipher_export`: keyed target as `main`, plain source attached read-only with `KEY ''`, in one transaction | **works.** Target starts with a salt (no SQLite magic, no canary in the bytes), opens with its key only (no key / wrong key: `SQLITE_NOTADB`), `integrity_check` ok, rows equal. **`user_version` and `application_id` are not copied** (both 0; as Slice A [POC A-4.3]); they must be set from the source. A write to the attached source fails with `SQLITE_READONLY`. |
| P2 | encrypted → plain with the key never in SQL | `ATTACH` of an encrypted file **without** a key fails at once (`SQLITE_NOTADB`: `ATTACH` reads the schema), so the attached database cannot be keyed afterwards, and `ATTACH … KEY x'…'` would put the key into SQL text. **Works** with the encrypted source as `main`, opened read-only (`mode=ro` URI) and keyed through `sqlite3_key_v2`, and the plain target attached with `KEY ''`: the attached target stays writable, `main` refuses writes. Rows equal, header fields set from the source, source file bytes unchanged. |
| P2' | unkeyed or wrongly keyed encrypted source | refused with `SQLITE_NOTADB` before anything is written; the target stays 0 bytes |
| P3 | a connection opened with `SQLITE_OPEN_READONLY` cannot write an attached target ("attempt to write a readonly database") | hence the `mode=ro` URI for the source rather than a read-only connection |
| P4 | source in WAL mode, last commits only in its `-wal` (autocheckpoint off) | **included** in the export (the export reads through SQLite, unlike Kind C's page streaming); the target is in `delete` journal mode |
| P5 | a writer of the source while the export's transaction is open (rollback journal) | the writer cannot commit (`SQLITE_BUSY`) until the export commits; the export is the snapshot before it; the write then lands in the source only |
| P6 | Backup API plain → keyed and keyed → plain | refused in both directions (as Slice C §3.1 P1) |
| P7 | `DELETE` + `INSERT … SELECT` of every table, in one transaction, into a target that already holds the schema | same rows as the source; the target keeps its salt (needed to refresh a converted vault whose salt is bound into its `.mckey`, §4) |
| E1 | the engine (`secure_vault::convert`) both ways under a folder named `記憶 #1 %` | rows equal both ways (plain → encrypted → plain), sources byte-equal, no other file in the folder |
| E2 | source file identity changed (`vault_token`) | `SourceChanged` before anything is copied |
| E3 | `refresh` after inserts, a deleted tag link (its trigger removes the unused tag) and an update in the source | `compare` detects the difference first; after `refresh` the rows are equal and the salt is unchanged |
| E4 | source with a hot journal (a writer died mid-transaction: pages half-written, journal present) | the ordinary read-write open the host does before every conversion (`Session::ticket`) rolls it back; the copy is the last committed state |
| E5 | source held by another process without sharing | refused; nothing usable is written |

**Crash state** (not a separate experiment): the copy is one SQLite transaction into the work file. A process that dies
during it leaves the work file (and its `-journal`) under its unpredictable name, recorded in the manifest; the next open
of that file would roll back to empty. It is never opened as a vault (§3) and is removed by discard.

**Plaintext outside the target (D2):** `temp_store = MEMORY` on the converting connection; the only files written are the
work file, its `-journal` (same folder) and the manifest (paths and ids only). D1 writes ciphertext only (SQLCipher
encrypts the journal pages too); the spell and the VK are in no file (`convert_tests::d1_…` scans every file).

### 2.1 Decision

- **First copy: `sqlcipher_export`** (the order's first candidate; P1, P2, P4 pass), followed by `user_version` and
  `application_id` from the source, inside the same transaction that holds the source's read lock.
- **Refresh at activation: `DELETE` + `INSERT … SELECT`** of every table in one transaction (P7), because re-exporting into
  a new file would give a new salt, and the D1 `.mckey` envelope is bound to the salt (re-wrapping would need the spell,
  which the host no longer has).
- Both end with **`compare`** inside the transaction: every object's SQL text, both header fields, and every row of every
  table read in primary-key order from both ends at once (one row in memory at a time; BLOBs compared byte for byte). A
  difference rolls the transaction back. A finished copy is then opened on its own: expected file kind (salt / SQLite
  magic), the key, full `integrity_check`, exact schema (`schema::check`).
- **Not used:** the Backup API (P6), `ATTACH … KEY` with the key as SQL text (P2), page streaming (it produces a plain file
  image, which is what Kind C needs, not a keyed database).

Engine: `native-host/src/secure_vault/convert.rs`; keying of a schema through `sqlite3_key_v2`:
`cipher::apply_key_to` (`cipher.rs`).

## 3. Messages and states

| Message | Fields | Answer |
|---|---|---|
| `vault_convert_start` | `direction` (`encrypt`/`decrypt`), `expectedVaultToken` (the shown vault), `path?` (dialog otherwise), `approveWarnings?`, `acknowledgePlaintext` (`true`, required for `decrypt`) | `path`, `sourcePath`, `direction`, `phase`, `vaultToken` (of the copy), `counts`; D1 also `recoverySpell` (once) |
| `vault_convert_confirm` | `recoverySpell` (D1) | `path`, `vaultToken`, `phase` |
| `vault_convert_activate` | `expectedVaultToken` (of the copy) | `path`, `status` |
| `vault_convert_keep` | — | `path`, `status` |
| `vault_convert_discard` | — | `status` |

`vault_status` gains `conversionPending`: `{direction, phase, path, sourcePath, vaultToken}` or null (`{path: null, phase:
"damaged"}` for an unreadable manifest, which can be discarded).

The manifest `<app dir>\vault-convert.json` records one conversion (`deny_unknown_fields`, consistency-checked on every
read: absolute paths, work file next to the target and named `.<target name>.<32 hex>.partial`, ids per direction and
phase):

| Phase | On disk | Allowed next |
|---|---|---|
| (start) | — | the manifest is written **before** the work file is created |
| `converting` | work file being written; D1: its salt is recorded as soon as it exists | discard |
| `verified` | work file verified; D1: `keys\<newId>.json.pending`, `<target>.mckey.pending`, waiting for the spell | D1: confirm, discard. D2: activate or keep (both promote first), discard |
| `converted` | `<target>` (D1: + `<target>.mckey`, `keys\<newId>.json`) | activate, keep, discard |
| none | activated (config names the target, publication `pending`) or kept (an ordinary vault) | — |

Refusals of out-of-order requests: `vault_convert_not_pending`, `vault_convert_incomplete` (still `converting`),
`vault_convert_spell_unconfirmed` (D1 activate/keep before the spell), `vault_convert_invalid_state` (confirm for D2),
`vault_switched` (a token that is not the copy's). Other codes: `vault_convert_pending`, `vault_convert_plaintext_unacknowledged`,
`vault_convert_mode_mismatch`, `vault_convert_same_as_source` (the target is the source under another spelling, by
`vault_token`), `vault_convert_source_changed`, `vault_convert_verify_failed`, `vault_convert_failed`,
`vault_convert_damaged`, `vault_work_file`, and the existing `file_exists`, `disk_full`, `vault_busy`, location codes.

**One operation at a time.** While a conversion is pending no other conversion, encrypted creation
(`vault_create_encrypted`) or restore (`vault_restore`) starts (`vault_convert_pending`), and a conversion does not start
while a creation or a restore is pending. A conversion that already became active (crash before its manifest was
removed) counts as finished.

**Crash and restart** (Chrome starts a host process per message, so every step is already a restart). Simulated deaths at
every step (`Fault::Crash`: no clean-up runs) and full failures (`Fault::Error`, `Fault::DiskFull`):

- failure: everything this conversion created is removed; the manifest too; the source and the key store are as before;
- death while `converting`: the conversion is shown, its work file cannot be opened as a vault, activation and confirm are
  refused (`vault_convert_incomplete`), discard removes everything;
- death after verification (D2) or during the promotion (D1 confirm): activation (D2) or a repeated confirm (D1) finishes
  the promotion, idempotently (`promote_file` accepts a step already done for the same file identity).

[TEST `a_failed_conversion_leaves_nothing_and_a_crashed_one_is_never_usable_until_finished` (2 directions × 3 steps × 3
faults), `a_crash_while_promoting_the_confirmed_vault_is_finished_by_the_next_confirmation`.]

## 4. Writes to the source during a conversion (order §3, last paragraph)

Recording continues while the user converts. Three windows:

1. **The copy** (`vault_convert_start`): the conversion holds the source's read lock for the length of the copy. A writer
   of a rollback-journal vault waits (busy timeout) and is answered `vault_busy`, which keeps the event in the extension
   queue for retry (architecture §8.1, L5); nothing is lost. WAL vaults keep accepting writes; the copy is a snapshot.
2. **Between the copy and activation** (e.g. while the user writes down the spell): writes land in the source, which is
   still the active vault.
3. **Activation**: `vault::switch_active_fenced` takes the usual switch fence (`BEGIN IMMEDIATE` on the old active
   vault: every writer is waited for, then blocked) and, **inside the fence**, `refresh` copies the source into the
   converted vault again and verifies it; only then is the config written. Every write committed to the source before
   the fence is in the new vault; every write after it sees the new config (`Session::write` re-checks under the write
   lock) and goes to the new vault. If the refresh fails (source replaced: `vault_convert_source_changed`; I/O; busy), the
   config is not written and the copy is unchanged (its transaction rolls back).

[TEST `activation_fails_closed_when_the_source_was_replaced_or_the_last_copy_fails`,
`d1_…`/`d2_…` (a visit added after the conversion is in the activated vault), `secure_vault_d.rs` (visits recorded
through the ordinary `visit` message before, during and after the conversion, one process per message, all in the
final vault), E2E `mcp-process.test.js`.]

**Difference from the Slice A design (architecture §8.2).** That design held `ConfigLock` and the write fence for the whole
conversion, answered writers `vault_maintenance`, and needed a queue watchdog (M3) to cancel before the extension queue
overflowed. This slice holds no lock while the user reads and records the spell (which may take minutes), never takes the
config lock during the copy, and needs no new writer error code: the long exclusive window of §8.2 is replaced by (1) a
read lock during the first copy and (2) a write fence during the refresh at activation. Both still block writers of a
rollback-journal source for the length of one copy; events wait in the extension queue, bounded by its limits (L1, L2;
R-D2). The watchdog, the long-lived port and progress reporting were not built (PO item 2).

**Switching to another vault meanwhile** is allowed: the conversion keeps its source (by path and file identity); at
activation the fence is over whatever vault is active then, and the refresh still copies the conversion's own source
[TEST `a_switch_of_the_active_vault_during_a_conversion_neither_mixes_vaults_nor_loses_the_source`].

**Keep instead of activate**: the copy stays a snapshot of the source at conversion time (no refresh); later writes stay
in the source. The panel says so before the choice.

## 5. Ownership and identity

| | Source | Converted vault |
|---|---|---|
| File | kept, never written (read-only connections only) | a new file at a path the user chose (dialog starts next to the source, `<name>-encrypted.sqlite3` / `<name>-plain.sqlite3`) |
| Identity | its `vault_token` and (encrypted) vault id are unchanged | a new file identity (`vault_token`); D1: a new vault id |
| Keys | D2: its key store entry and `.mckey` are unchanged and still recover it with its own spell | D1: a new VK, a new DPAPI entry, a new `.mckey` v2 with a new spell; D2: no key, no `.mckey` |
| Publication | unchanged (it stays as it was; the decision is per file identity) | `pending`, never inherited |
| Removal | never by the conversion; the user may delete it later, outside Memocampus | removed by discard (any phase before activation/keep); after keep or activation an ordinary vault |

A plain vault has no vault id in the config (Slice B design: only encrypted vaults carry one); it is identified by its file
identity, which is new for every converted file. "New and old ids differ" therefore means: D1 a new vault id (and a new
token), D2 a new token (the plain vault has no id) [TEST `d1_…`, `d2_…`, `the_spell_of_an_earlier_vault…`].

## 6. Recovery Spell lifecycle

- **D1**: VK, spell (256 CSPRNG bits) and vault id are generated in memory at start. After the copy is verified, the VK is
  stored DPAPI-protected (pending) and wrapped under the spell in `<target>.mckey.pending` (version 2, bound to the new
  vault id and the copy's salt); the host checks that the DPAPI entry, the envelope opened with the spell and the copy
  itself all agree. The spell then leaves the host **once**, in the start answer. Confirm needs all 43 characters; a wrong
  or malformed spell changes nothing and may be retried. The spell is never written to a file, the manifest, a log,
  `chrome.storage` or the service worker [TEST `d1_…` scans every file; `vault-convert.test.js`: carriers and worker
  messages; `secure_vault_d.rs`: the spell occurs once in the host's stdout of the whole run].
- **A D1 conversion interrupted before confirmation** (panel closed, host restart) is never shown its spell again. If the
  user recorded it, `vault_convert_confirm` still completes it; the panel offers only discard (order §5: discard and
  start again instead of persisting a re-displayable secret).
- **D2** creates no spell. The source's spell keeps recovering the source only; the plain copy has no `.mckey`.
- **Old spells**: the spell of an earlier vault (e.g. the encrypted vault a plain one was decrypted from) never opens a
  vault encrypted again by D1 [TEST `the_spell_of_an_earlier_vault_never_recovers_a_converted_one`].

## 7. Privacy and Publication Gate

- The converted vault's publication is `pending` after activation whatever the source's was (the source may be
  `confirmed`); MCP and VisibleStore refuse it (`vault_publication_pending`) until the user confirms it in the panel
  [TEST E2E `mcp-process.test.js`: source confirmed, converted vault refused until confirmed, then served with the privacy
  gate applied (a never-show row stays hidden)].
- The conversion never reads or writes the policy, the approved domains or any publication record of the source.
- Until activation the active vault (and its publication) is the source's; a failed or cancelled conversion leaves it so.

## 8. Relation to backups

Conversion is not a backup and does not use the backup formats. Kinds A–D are unchanged (no code change in
`backup.rs` besides new fault-injection steps); C still writes no plaintext work file. A converted vault is an ordinary
vault: B/D backups of a D1 vault need its new spell; A/C of a D2 vault need none [covered by the unchanged backup tests on
vaults of each mode; a D1 vault is recovered with its spell on "another PC" in `d1_…`].

## 9. User interface

Side panel, Vault section (`#vault-convert`, shown for a ready vault when the host reports `conversionPending`):

- plain vault: **このVaultを暗号化する** — 「現在のVaultの内容を、新しい暗号化Vaultへコピーします。元のVaultは削除されません。」;
  the button opens the save dialog. The spell step reuses the creation's onboarding with the official title, lead and
  caution, plus the target, the source and 「これは新しい暗号化Vaultのふっかつのじゅもんです。以前に記録した呪文…では、このVaultを
  復旧できません。」; the confirm button reads 「確認して変換を完了する」, cancel discards the copy.
- encrypted vault: **このVaultの暗号化を解除する** — 「新しい平文Vaultを作成します。閲覧履歴や保存画像などが暗号化されずにディスクへ
  保存されます。元の暗号化Vaultは削除されません。」; a checkbox 「…暗号化されずにディスクへ保存されることを理解しました」 must be
  checked for the button to work, and is cleared after each request (the host requires `acknowledgePlaintext: true` too).
- the decision box (`#vault-converted`) shows target, source, the verified counts, what activation does (catch-up copy,
  publication pending) and what keeping does; buttons: use it / keep without switching / discard. After a reopen it shows
  the recorded conversion (`describeConversionPending`); an unconfirmed D1 offers discard only.
- double execution is prevented (`busy`, one conversion; restore disabled meanwhile); failures are shown in Japanese
  (`search-client.js`); secrets are not stored in `chrome.storage` or logs.

[TEST `tests/extension/vault-convert.test.js` (5 tests).]

## 10. Changed files

| Area | Files |
|---|---|
| Engine | `native-host/src/secure_vault/convert.rs` (new), `secure_vault/cipher.rs` (`apply_key_to`), `secure_vault/mod.rs` |
| Host messages | `native-host/src/manage/convert.rs` (new), `manage.rs` (dispatch, status, `refuse_work_file`), `manage/encrypted.rs`, `manage/restore.rs` (mutual exclusion, work-file refusal), `manage/backup.rs` (fault steps), `main.rs` (routing), `vault.rs` (`switch_active_fenced`, `key_kind`), `dialog.rs` (`choose_convert_target`) |
| Host tests | `native-host/src/manage/convert_tests.rs` (new, 11), `tests/secure_vault_d.rs` (new, real binary), `tests/secure_vault_d_poc.rs` (new, PoC + engine, 13) |
| Panel | `extension/vault-panel.js`, `vault-client.js`, `search-client.js`, `sidepanel.html`, `sidepanel.css` |
| Extension tests | `tests/extension/vault-convert.test.js` (new), `fake-dom.js`, `mcp-process.test.js` (opt-in E2E) |
| Docs | this file, `architecture.md`, `threat-model.md`, `implementation-plan.md`, `recovery-spell.md`, `README.md` |

## 11. Residual risks and constraints

| # | Risk | Note |
|---|---|---|
| R-D1 | D2 writes the whole vault (history, images) in plaintext to the chosen folder; the encrypted source stays | by the user's acknowledged choice; old plaintext/encrypted copies are never deleted automatically (threat model R2) |
| R-D2 | Writers of a rollback-journal source are blocked during the first copy and during the refresh at activation (each as long as one full copy); events wait in the extension queue (L5) and could hit its limits (L1/L2) for a very large vault | no watchdog or progress (PO item 2) |
| R-D3 | A very large vault may exceed the panel's 10-minute request timeout; the host continues and the result appears as `conversionPending` | no progress reporting |
| R-D4 | Free space is not checked in advance; a full disk fails the copy (`disk_full`) and removes the work file | the source is never affected |
| R-D5 | A "kept" conversion is a snapshot; writes after it remain only in the source | stated in the panel |
| R-D6 | The D1 spell crosses native messaging once and lives in the panel until confirmed (as `recovery-spell.md` R-S1) | as for creation |
| R-D7 | The `.partial`/`.pending` refusal of `vault_open` is by file name; a user may rename a work file | the work file of a pending conversion is also refused by identity; a renamed, finished copy is a valid vault |
| R-D8 | A real process kill in the middle of a large copy, a real power cut and a second PC were not exercised | simulated deaths at every step (`Fault::Crash`), one process per message in `secure_vault_d.rs` |
| R-D9 | The base branch (Recovery Spell) is not in `main` | merging order matters (§0) |

## 12. For the PO

1. **Base branch.** `main` does not contain the Recovery Spell (`feature/51-recovery-spell`, not merged); this slice is
   based on it. It must be merged after, or together with, the Recovery Spell.
2. **No maintenance mode / watchdog** (architecture §8.2–§8.3, plan S4): replaced by a copy without locks held between
   requests and a fenced refresh at activation (§4). Writers are blocked (not refused) for the length of one copy, twice.
   A watchdog with progress over a long-lived port is not built.
3. **Publication is not inherited** (order §6), superseding architecture §8.2 P5 ("confirmed only if the source was
   confirmed").
4. **Keep without activating** (`vault_convert_keep`) is an addition: it ends the conversion and leaves the copy as an
   ordinary vault. It can be removed if only activate/discard is wanted.
5. **D1 after a restart**: the host accepts `vault_convert_confirm` for a `verified` conversion if the user recorded the
   spell; the panel offers only discard. Either can be narrowed to discard-only.
6. **Plain vaults have no vault id**; "a new id" for D2 means a new file identity (§5).
