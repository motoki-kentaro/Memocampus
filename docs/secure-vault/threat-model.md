# Secure Vault — threat model (Issue #51, Slice A)

## 1. Assets

| Asset | Where | Sensitivity |
|---|---|---|
| Browsing history, titles, page metadata, navigation | vault | high |
| Saved images and their sources | vault | high |
| Per-image MCP permission (`mcpPublic`) | vault | medium (it decides exposure) |
| Privacy policy (`neverSave`/`saveButNoMcp` rules), PSL approvals | `policy.json`, `settings.sqlite3` (global) | high: the rules themselves reveal sensitive domains |
| Vault key VK | memory; DPAPI blob in the key store; phrase envelopes | critical |
| Recovery phrase | user's paper or password manager; memory during onboarding | critical |
| Backup password | memory only | critical (format C) |
| Backups A, B, C | user-chosen folders, cloud, USB | high |
| Pending capture queue | `chrome.storage.local` (Chrome profile) | high, **not covered by Secure Vault** |
| MCP HTTP token | token file | medium |

## 2. Attackers and capabilities

| # | Attacker | Capability | In scope? |
|---|---|---|---|
| T1 | Device or disk thief | reads the disk offline; no Windows password | **yes** |
| T2 | Backup leak (cloud folder, USB stick, mis-shared file) | holds a backup file, unlimited offline time | **yes** |
| T3 | Sync/remote-copy exposure of the active vault folder | holds the vault file, maybe its sidecar | **yes** |
| T4 | Another local, non-admin Windows user | reads files their ACLs allow | **yes** (DPAPI user scope) |
| T5 | Crafted backup file supplied to restore | arbitrary bytes, huge lengths or KDF parameters | **yes** (parser safety) |
| T6 | MCP client or LLM | calls MCP tools | **yes** (data minimisation, no secrets) |
| T7 | Malware running as the logged-in user; compromised extension or Native Host | everything the user can do | **no** (PO: out of scope) |
| T8 | Admin/SYSTEM, kernel, cold-boot or DMA attacks | memory and keys | **no** |
| T9 | Attacker who knows the user's Windows password (or NT hash) plus the disk | can decrypt DPAPI offline [DOC] | **no** (equivalent to T7 for DPAPI) |
| T10 | Holder of an old phrase together with an old backup | decrypts that backup | accepted (no revocation; §5) |

## 3. Trust boundaries

1. **Chrome extension ↔ Native Host** (native messaging, same user).
   - The host is the only process that unlocks.
   - Secrets never cross into the extension, except the phrase entered by the user during recovery,
     which goes extension → host once.
2. **Native Host / MCP ↔ file system** (vault, sidecar, key store, config, backups).
3. **MCP server ↔ MCP client** (stdio, or localhost HTTP with a token). Only VisibleStore results
   cross this boundary.
4. **Restore parser ↔ untrusted backup file.**
5. **User ↔ screen.** The phrase is shown once; shoulder-surfing and screenshots are out of scope,
   but the UI does not keep the phrase in the DOM after confirmation.

## 4. Attacks considered and defences

| Attack | Defence | Status |
|---|---|---|
| T1/T3 read a stolen encrypted vault | SQLCipher AES-256 + HMAC per page under a random 256-bit VK; VK only in DPAPI and phrase envelopes | design; SQLCipher [DOC], DPAPI [POC] |
| T3 steal the vault plus `.mckey` sidecar | the sidecar is phrase-wrapped (128-bit); not enough without the phrase | [POC A-3] |
| T2 brute-force a B backup | 128-bit phrase; HKDF + AEAD per guess | accepted level (P5) |
| T2 brute-force a C backup | Argon2id (64 MiB, t=3) per guess; C over B still needs the phrase | [POC A-5]; weak passwords stay weak (R3) |
| T2/T5 tamper with a backup (header, payload, truncation, chunk reordering) | B: exact length + HMAC under a VK-derived key; C: STREAM AEAD with the header as AAD, password check, exact length | [POC A-5.1/A-5.2] |
| T5 resource exhaustion (header length, Argon2 parameters, section count) | bounds checked before allocation | [POC A-5.2] |
| T5 restore overwriting the active vault or config | always a new target; `create_new`/`rename_no_replace`; config untouched until explicit activation | design (§6) |
| T4 another user reads the key store | DPAPI user scope (`%LOCALAPPDATA%` ACLs too) | [DOC]; cross-user **NOT RUN** |
| Key store entry swapped or altered | vault id in the DPAPI entropy; kcv; `cipherSalt` match | [POC A-2.2] for the DPAPI part |
| Wrong key mistaken for "not a database", so the write fence is skipped (HIGH-4) | mode recorded in config; keyed failures never prove unwritable; key-independent exclusive-handle fence | [POC A-7.1/A-8.1] |
| Encrypted vault misread as plain (HIGH-5) | mode from config; VisibleStore mode-aware | design |
| Stale MCP keeps reading the old plain vault after conversion (HIGH-1) | follow-active resolution per call; fixed-path mode refused unless active | design |
| Restored vault exposed to MCP under a different or empty policy (HIGH-3) | publication gate: `pending` until confirmed; never auto-overwrite global policy | design |
| Approval given under one policy carried over to a policy initialized again after a loss (review F-02) | `policy_initialize`/`policy_start` withdraw the active vault's publication (`confirmed` → `pending`) under the policy lock before writing; a failed withdrawal writes no policy; a failed policy write leaves `pending`. Ordinary rule edits keep the approval; each read applies the current policy | [TEST] `slice-b0.md` §10; hand-made valid policy files are not detected |
| Secrets leak through logs, IPC, MCP or errors | fixed error codes; key never formatted; canary tests | design |
| Plaintext temp files during secure backup | raw ciphertext copy under SHARED; stream encryption for C; `temp_store = MEMORY` | [POC A-4.2] (plain); SQLCipher [DOC] |
| Silent loss of capture during conversion | `vault_maintenance` is temporary, so events stay queued; watchdog cancels before overflow; policy evaluation unaffected | design (§8) |

## 5. Out of scope (explicit)

- T7, T8 and T9.
- Old plaintext vault files and old format A backups. They stay readable; the UI states this when
  converting to encrypted.
- Revocation of distributed backups: anyone with an old backup and its phrase keeps access.
- Traffic analysis of file sizes and page counts. SQLCipher hides content, not size; the
  `cipherSalt` is public.
- Hibernation file and pagefile copies of VK. Windows BitLocker is the mitigation, outside the app.
- Screenshots or photos of the phrase.

## 6. Impact if compromised

| Compromise | Impact |
|---|---|
| VK | all data of that vault and all its backups B/C, past and future, until rotation (new VK, new vault) |
| Phrase | same as VK for every artifact carrying that phrase's envelope (the sidecar, backups made before regeneration) |
| Backup password | C over A: that backup fully; C over B: nothing without the phrase |
| DPAPI (T7/T9) | VK of every vault in that user's key store |
| Policy copy in a backup | readable by anyone holding VK/phrase (B) or the password (C); not by T2 alone |

## 7. Residual risks

| # | Risk | Note |
|---|---|---|
| R1 | Pending capture events (URLs, titles, page metadata) sit unencrypted in `chrome.storage.local` | present today; Secure Vault does not change it. Needs a PO decision whether to accept it or to minimise the queue. |
| R2 | Old plaintext vaults and A backups after converting to encrypted | disclosed in the UI; never deleted automatically (PO) |
| R3 | Weak backup password in C over A | Argon2id only slows guessing; offer a strength hint |
| R4 | Recovery level is 128 bits, below the 256-bit DB key | stated; PO acceptance (P5) |
| R5 | DPAPI accepted 128/2,096 single-bit-modified blobs (same plaintext) | mitigated by kcv and vault-id entropy |
| R6 | A crash during restore of C over A leaves a plaintext `.partial` | listed at the next start; deletion only with the user's consent |
| R7 | SQLCipher, OpenSSL and the SQLite 3.45.3 downgrade: Rust suites pass on it [POC A-1.5]; Node/E2E and dogfood still open | Slice S0 gate |
| R8 | The config rename is not write-through; after a power cut the previous config may reappear | Slice B0: POSIX rename by handle + flush after the rename (`slice-b0.md`); crash points simulated, a real power cut NOT VERIFIED; a disk ignoring flushes stays a residual risk |
| R9 | L3 (pre-enqueue policy check failure) silently skips a visit today | pre-existing; conversion must not trigger it. Fixing it in general is a separate issue. |
| R10 | Non-Windows | design isolates DPAPI behind a key-protector interface; no non-Windows implementation is designed or verified |
