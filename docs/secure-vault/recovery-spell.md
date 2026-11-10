# Secure Vault — Recovery Spell (ふっかつのじゅもん) (Issue #51)

Status: implemented on `feature/51-recovery-spell`, **not committed**; waiting for an independent security review and PO
approval. Basis: `main` at `9c2842b` (Slice C merged, PR #54). This document is the specification of the recovery secret.
It replaces the 12-word BIP-39 recovery phrase of Slice B/C everywhere; `slice-b.md` and `slice-c.md` keep their history and
point here.

Markers: **[TEST]** covered by an automated test named below, **[CODE]** read in the code, **NOT RUN** not exercised.

## 1. Names

| Where | Name |
|---|---|
| UI (Japanese) | **ふっかつのじゅもん** |
| UI (English) | **Recovery Spell** |
| Code | `RecoverySpell` (secret type), `secure_vault::spell` (encoding), `recovery_spell_*` (error codes), `recoverySpell` (message field), `needsSpell` (restore inspection) |

"Recovery phrase" and "BIP-39" remain only where the old scheme is named in order to refuse it (§7).

## 2. The canonical form

- 32 bytes (256 bits) from the operating system's CSPRNG: `getrandom::fill`, on Windows `ProcessPrng`
  (`secret::random_bytes`). No other generator is used, and a string the user makes up is never adopted as a spell
  [CODE `secret.rs` `RecoverySpell::generate`; `spell.rs` has no constructor from text other than `parse` of an existing
  spell].
- Written as RFC 4648 §5 base64url **without padding**: exactly **43 characters** from `A–Z a–z 0–9 - _`. `+`, `/`
  and `=` never occur.
- The last character carries 4 data bits and 2 unused bits, which are zero: it is one of `AEIMQUYcgkosw048`.

### 2.1 Parsing (host, `spell::parse`)

Strict; nothing is repaired, folded or substituted:

| Check, in order | Error (`recovery_spell_invalid` + details) |
|---|---|
| exactly 43 characters (Unicode scalar values) | `{reason: "length", count}` |
| every character in the base64url alphabet (case-sensitive; spaces, tabs, `+ / =`, full-width letters and digits, accented letters are refused) | `{reason: "character", position}` (1-based, first offending character) |
| base64url decoding (engine `URL_SAFE_NO_PAD`: no padding, no trailing bits) gives 32 bytes, **and** re-encoding them gives the input byte for byte (constant-time compare) | `{reason: "non_canonical"}` |

The input is never echoed in an error. The host does not trim: removing whitespace **around** a pasted spell is the
panel's job (`normalizeSpellInput`), whitespace inside is an error on both sides. A 43-character string passes only if
all three checks pass; length alone is never enough [TEST `spell::tests::*`: 42/44 characters, every forbidden character
class by position, `=` padding, every non-canonical last character refused and every canonical one accepted, case
sensitivity, round trip of 256 generated spells, fixed vectors].

## 3. Key derivation and domain separation

The spell's 32 bytes are **only** the input keying material of HKDF-SHA256; they are never a key, a MAC key or a
nonce. Each use has its own HKDF `info` label, and no label is shared with the 12-word scheme or with the keys derived from
VK.

```text
Recovery envelope (recovery.rs), scheme "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v2":
  salt_e         = 32 random bytes, new for every envelope (HKDF salt)
  KEK            = HKDF-SHA256(ikm = spell, salt = salt_e, info = "memocampus/recovery-spell/v2/kek")
  K_check        = HKDF-SHA256(ikm = spell, salt = salt_e, info = "memocampus/recovery-spell/v2/check")
  check          = HMAC-SHA256(K_check, "memocampus/recovery-spell/v2/spell-check\0" || vaultId || cipherSalt)
  wrapped        = XChaCha20-Poly1305(KEK, nonce = 24 random bytes,
                                      AAD = "memocampus/recovery-spell/v2/wrap\0" || vaultId || cipherSalt)(VK)

Keys derived from VK (unchanged roles; the spell reaches them only through the envelope):
  key check value = HMAC-SHA256(HKDF(VK, info "memocampus/kcv/v1"), vaultId || cipherSalt)   (key store; unchanged)
  K_mac (backup B)    = HKDF-SHA256(VK, info = "memocampus/backup-b/v2/mac")
  K_policy (backup B) = HKDF-SHA256(VK, info = "memocampus/backup-b/v2/policy"),
                        XChaCha20-Poly1305, AAD = "memocampus/backup-b/v2/policy\0" || vaultId
```

| Purpose (PO order §2.3) | Key | Derived from | Label |
|---|---|---|---|
| Key wrapping (sidecar, backup B/D header) | KEK | spell (+ per-envelope salt) | `memocampus/recovery-spell/v2/kek` |
| Telling a wrong spell from damage | K_check | spell (+ per-envelope salt) | `memocampus/recovery-spell/v2/check` |
| Backup authentication | K_mac | VK | `memocampus/backup-b/v2/mac` |
| Policy copy encryption | K_policy | VK | `memocampus/backup-b/v2/policy` |
| VK ↔ vault match (key store) | KCV key | VK | `memocampus/kcv/v1` |

Decisions, after auditing the Slice B/C design:

- **HKDF salt**: 32 random bytes per envelope (as in Slice B). Two envelopes of the same spell never share a KEK.
- **Context binding**: vault id and the vault file's SQLCipher salt are bound by the AAD and the check value (as in Slice
  B), not by the HKDF `info`: an envelope copied to another vault, or to another file of the same vault, is refused.
- **Backup MAC and policy keys stay derived from VK**, as in Slice C. The spell protects them transitively: it is needed to
  unwrap VK, and VK is needed for both. Deriving them from the spell directly would change the backup protocol (a backup
  made from the DPAPI-held VK alone could not be verified with the same keys); this is listed for the PO in §11. Their
  labels moved to `v2` with the container version, so a B package of the old scheme can never verify under the new one.
- **Key check value unchanged** (`memocampus/kcv/v1`): it binds the DPAPI key store entry to VK and has nothing to do with
  the recovery secret. Keeping it means existing encrypted vaults still open with this user's DPAPI key.
- **No slow KDF**: the spell is 256 uniformly random bits, so each offline guess costs one HKDF and one HMAC against 2²⁵⁶
  candidates. The security level of the recovery path rises from 128 bits (12 words) to 256 bits, the same as VK.
- XChaCha20-Poly1305, SQLCipher (raw 256-bit key) and DPAPI (user scope) are unchanged.

[TEST `recovery::tests`: the right spell and only it unwraps VK; every canonical single-character change is
`SpellRejected`; altered tag/ciphertext/nonce is `Damaged`; altered check or salt, another vault id, another cipher salt are
`SpellRejected`; the 12-word scheme and any other scheme are `Unsupported`; a spell equal to VK still yields KEK ≠ K_check ≠
VK and an envelope that reveals nothing; fresh salt and nonce per envelope.]

## 4. Files and formats

### 4.1 Sidecar `<vault>.mckey`, version 2

```json
{"version": 2, "format": "memocampus-vault-recovery", "vaultId": "<32 hex>", "cipherSalt": "<32 hex>",
 "recovery": {"scheme": "recovery-spell-256/hkdf-sha256/xchacha20poly1305/v2",
              "salt": "<64 hex>", "nonce": "<48 hex>", "wrapped": "<96 hex>", "check": "<64 hex>"}}
```

Reading (`sidecar::read`): the format name and version are read first. Another format or malformed JSON is `Invalid`
(`recovery_metadata_damaged`); the right format with another version (1 = 12-word phrase, or a newer one) is
**`Unsupported`** (`recovery_metadata_unsupported`), never damage and never parsed as a spell envelope; a version 2 sidecar
whose envelope names another scheme is `Invalid` [TEST `sidecar::tests`].

### 4.2 Backup containers, version 2

The common prefix is unchanged except the version field: `89 'M' 'C' 'V' 'B' 0D 0A 1A | version u16 LE = 2 | kind | 0 |
header_len | header`. The B header carries the envelope of §4.1 (`recovery`, scheme v2).

| Prefix version | Kind | Result |
|---|---|---|
| 2 | B, C, D | read |
| 1 | B, D (12-word phrase) | **`backup_legacy_recovery_phrase`**, at inspection, before any secret is asked for or any key derived |
| 1 | C | read: format C never involved the phrase; its layout, Argon2id/HKDF labels and STREAM construction are unchanged, and the version is authenticated through `AAD = SHA-256(prefix)` |
| anything else | any | `backup_unsupported_version` |

New backups of every kind are written as version 2. A version 2 D whose inner package is a version 1 B is refused as
`backup_legacy_recovery_phrase` at the inner prefix [TEST `container::tests::slice_c_phrase_containers_are_refused_as_such_before_any_key_is_derived`,
`a_slice_c_format_c_file_is_still_read_and_new_ones_are_version_2`, `manage::backup_restore_tests::files_that_are_not_backups_are_refused_and_nothing_is_created`].

Kind C keeps every Slice C invariant: pages streamed from the source file under one read transaction (no plaintext work
file, no temporary key), WAL sources refused (`backup_wal_unsupported`) [TEST, unchanged Slice C tests].

### 4.3 Native messaging

| Message | Field | Notes |
|---|---|---|
| `vault_create_encrypted` | answer `recoverySpell` (string, 43 characters) | the only message that carries a generated spell, once |
| `vault_create_encrypted_commit` | `recoverySpell` | the whole spell, as re-entered |
| `vault_recover` | `recoverySpell` | |
| `vault_backup` (B, D) | `recoverySpell` | not read for A and C |
| `vault_restore_inspect` | answer `needsSpell` | |
| `vault_restore` (B, D) | `recoverySpell` | |

Error codes: `recovery_spell_invalid` (+ `reason`, `count`/`position`), `recovery_spell_rejected`,
`recovery_metadata_unsupported`, `backup_legacy_recovery_phrase`; the others are as in Slice B/C. No status, inspection or
other answer carries a spell; there is no message that shows a spell again.

## 5. Lifecycle

### 5.1 Creation of an encrypted vault

1. `vault_create_encrypted` generates VK, the spell and the vault id; writes the pending SQLCipher file, the pending DPAPI key
   store entry and the pending sidecar (version 2); checks that all three open to the same VK; answers the spell once.
2. The panel shows **ふっかつのじゅもん（Recovery Spell）**, the PO's wording
   「あなたの記憶を取り戻すための、復活の呪文を記録してください。決して忘れないでくださいね。」 and
   「この呪文は一度しか表示されません。安全な場所に保管してください。失うとバックアップから復元できなくなる場合があります。」,
   the spell in one monospace block, a **コピー** button (clipboard only on that click), and notes on case sensitivity,
   look-alike characters and clipboard history.
3. **記録しました。確認に進む** removes the spell from the page and asks for **all 43 characters** (typed or pasted).
   Only an exact match (surrounding whitespace aside) sends the commit; partial or short confirmation does not exist. A
   mismatch is reported and can be retried; nothing becomes active. The text says that this check does not prove safe
   storage.
4. `vault_create_encrypted_commit {recoverySpell}`: the host parses it strictly and checks that it opens the envelope written
   at step 1; only then are the files promoted and the vault activated, publication **pending**. A refused commit leaves
   the creation pending for another try.
5. Cancelling, or completing, clears the spell from memory, the DOM and the input. A panel reopened during a pending
   creation cannot show the spell again; the creation can only be discarded.

DPAPI auto-unlock for this Windows user is unchanged [TEST `secure_vault_b::an_encrypted_vault_is_created_only_after_its_whole_spell_is_entered_again`
(another spell, single-character changes, 42 characters, 8 characters, case flipped, leading space, inner space, missing:
all refused, creation still pending; no file holds the spell, half of it or its standard-base64 spelling; no status
carries it), `vault-encryption.test.js`].

### 5.2 Recovery (another PC, another Windows user, reinstall)

`vault_recover {path, recoverySpell}`: header → sidecar (version 2) → spell (strict) → envelope (`recovery_spell_rejected` /
`recovery_metadata_damaged`) → the key opens the vault → only then a DPAPI entry for this user is written and the vault
becomes active, publication pending. No key store or DPAPI data of the original environment is needed [TEST
`another_windows_user_recovers_with_the_spell_and_only_with_it`, `recovery_metadata_that_is_missing_swapped_or_altered_fails_closed`
(another vault's spell refused), `a_wrong_spell_never_replaces_a_key_entry_and_a_right_one_repairs_a_damaged_entry`].

### 5.3 Backups B and D

- The spell is required for every B and D backup (review finding C-03, unchanged): syntax before the folder dialog; then it
  must open the sidecar's envelope (bound to the vault id and file salt) to exactly the VK this user's key store holds.
  Another vault's spell, a wrong spell, altered metadata, a sidecar of another vault, an envelope around another key and a
  version 1 sidecar are refused **before any file is written**. A and C never read a spell.
- The backup carries the sidecar's envelope (version 2), so the same spell restores it.
- D still needs both the password and the spell; neither alone opens it.

[TEST `b_and_d_backups_need_the_spell_that_recovers_this_vault`, `a_password_backup_of_an_encrypted_vault_needs_both_the_password_and_the_spell`,
`an_encrypted_vault_backs_up_encrypted_and_restores_on_another_pc_with_the_spell_only`, `vault-restore.test.js`.]

### 5.4 Restore

A B or D backup restores with its spell only (D: and its password) into a **new** vault in a separate app directory with
no key store: a new vault id, a new DPAPI entry, a new version 2 sidecar wrapping the same VK under the same spell, so that
spell also recovers the restored vault. Publication starts **pending**; the active vault, the policy and the original key
store are untouched until the user activates [TEST as above; E2E `mcp-process.test.js` (opt-in) with real binaries in two
app directories].

## 6. Secret handling

- Host: `RecoverySpell` has no `Debug`/`Display`/`Serialize` output (`RecoverySpell(..)`), is zeroed on drop; the encoded
  string is a `Zeroizing<String>`. The spell crosses native messaging in the creation answer and in each request that needs
  it; `serde_json` buffers and the OS pipe cannot be zeroed (residual, as Slice B R-B1).
- Panel: the spell lives in one JavaScript variable during onboarding and in input fields; it is cleared on completion,
  cancellation and success. It is never written to `chrome.storage`, `localStorage`, the event queue (owned by the service
  worker, which never receives it) or a log [TEST `vault-encryption.test.js`, `vault-restore.test.js`: no request but the
  commit/backup/restore/recover carries it, no message to the service worker does, inputs and DOM are empty afterwards].
- JavaScript strings, the Windows clipboard (and its history), screenshots and crash dumps cannot be erased by Memocampus.
  Copying happens only on the user's click, and the panel tells the user to clear the clipboard.
- MCP and VisibleStore never see the spell: they open encrypted vaults with the DPAPI key only and refuse a locked vault.

## 7. Existing 12-word vaults and backups (incompatibility)

PO decision (order §5): no compatibility layer and no migration tool.

| Existing item | Behaviour in this build |
|---|---|
| Encrypted vault with a version 1 sidecar, on the Windows user that created it | **opens and works** with the DPAPI key (key check value unchanged). Nothing is re-wrapped, rewritten or deleted. |
| Same vault: recovery, or B/D backup | refused, `recovery_metadata_unsupported` ("the old 12-word recovery file cannot be used by this version"); the sidecar is not touched |
| Same vault on another Windows user / PC | cannot be opened: `recovery_metadata_unsupported` |
| B or D backup of Slice C | refused at inspection, `backup_legacy_recovery_phrase`; nothing is created |
| C backup of Slice C | still restores (§4.2) |
| A backup | unchanged (plain SQLite) |
| Pending creation (`vault-create.json`) begun by Slice C | its sidecar is version 1: the commit fails as `vault_creation_damaged`; discarding removes all of its files, the version 1 sidecar included (matched by vault id and salt, `sidecar::identity`) [TEST `a_creation_begun_before_the_spell_cannot_complete_and_is_discarded_completely`] |

Old data is never presented as recoverable when it is not: every refusal names the old scheme instead of reporting damage
or a wrong spell [TEST `secure_vault_b::a_vault_with_a_12_word_sidecar_still_opens_here_but_is_never_read_as_a_spell`].

## 8. Relation to DPAPI, Privacy Gate and Publication Gate

- DPAPI remains the everyday unlock; the spell is needed only where DPAPI cannot help (other user, other PC, backups).
- Every vault created, recovered or restored with a spell starts with MCP publication **pending**; no recovery failure
  changes the policy, the approved domains, the publication state or the active vault. The privacy gate applies to
  encrypted vaults exactly as before [TEST E2E `mcp-process.test.js`, Slice B/C publication tests, unchanged].

## 9. Changes in this branch

| Area | Files |
|---|---|
| Spell encoding (new) | `native-host/src/secure_vault/spell.rs` (replaces `phrase.rs`, removed) |
| Secret type | `secure_vault/secret.rs` (`RecoverySeed` 16 bytes → `RecoverySpell` 32 bytes) |
| Envelope v2 | `secure_vault/recovery.rs` |
| Sidecar v2 | `secure_vault/sidecar.rs` |
| Container v2 | `secure_vault/container.rs`, `container/tests.rs` |
| Host messages | `manage.rs`, `manage/encrypted.rs`, `manage/backup.rs`, `manage/restore.rs`, `manage/backup_restore_tests.rs`, `tests/secure_vault_b.rs` |
| Dependency | `bip39` (and `bitcoin_hashes`, `hex-conservative`, `arrayvec`) removed from all three lock files; `base64` was already a dependency |
| Panel | `extension/vault-client.js`, `vault-panel.js`, `search-client.js`, `sidepanel.html`, `sidepanel.css` |
| Tests | `tests/extension/fake-dom.js`, `vault-encryption.test.js`, `vault-restore.test.js`, `mcp-process.test.js` |
| Docs | this file, `architecture.md`, `threat-model.md`, `implementation-plan.md`, `slice-b.md`, `slice-c.md`, `README.md`, `NATIVE_THIRD_PARTY_NOTICES.md` |

## 10. Residual risks

| # | Risk | Note |
|---|---|---|
| R-S1 | The spell is on screen once, in native messaging, in JS strings and possibly in the clipboard (history, cloud clipboard) | cannot be erased by Memocampus; copy is explicit and the UI says so |
| R-S2 | 43 case-sensitive characters are harder to copy by hand than 12 words; look-alike characters (0/O, 1/l/I, -/_) | full re-entry check at creation; the UI names the look-alikes; no checksum beyond the envelope's check value, so a typo is reported as "does not match", not located |
| R-S3 | Existing 12-word vaults lose portable recovery and B/D backups | by PO decision; their DPAPI unlock still works; see §11 |
| R-S4 | The re-entry check does not prove the spell was stored safely | stated in the UI |
| R-S5 | An old spell plus an old backup keeps access (no revocation) | unchanged from architecture §9 |

## 11. For the PO

1. **Backup MAC and policy keys** remain HKDF(VK) with distinct `v2` labels (§3); the spell protects them through the
   envelope. Deriving them from the spell directly would change the backup protocol; not done without a decision.
2. **Version 1 format C backups are still read** (§4.2): they never involved the phrase. Refusing them as well is a
   one-line change if preferred.
3. **Existing 12-word encrypted vaults** keep working with DPAPI but cannot be recovered elsewhere or backed up as B/D. A
   user-initiated "issue a new spell" for such a vault (unwrap with DPAPI, write a version 2 sidecar after full
   re-entry) would restore that; it was not built because the order forbids re-wrapping existing vaults.
4. The panel does not offer a "show again" or "re-issue" of a spell for spell vaults either (Slice E, regeneration).
