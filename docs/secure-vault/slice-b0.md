# Secure Vault — Slice B0: foundation hardening for plain vaults (Issue #51)

Status: **implemented on `feature/51-secure-vault-b0`, not committed; waiting for independent review
and PO approval.** No SQLCipher, key generation, DPAPI, onboarding or encrypted vault creation is
part of this slice. Existing vaults are not converted; the only automatic change is the one-time
`config.json` version 1 → 2 rewrite described in §3.

B0 corresponds to Slice **S1** of `implementation-plan.md`. Differences from the Slice A design are
listed in §8.

## 1. What changed, by requirement

| Req. | Change | Where |
|---|---|---|
| B0-1 MCP follows the active vault | `memocampus-mcp` without `--vault` follows the active vault, resolved for every tool call. With `--vault <path>` it reads that file only while it is the confirmed active vault. Same gate for stdio and Streamable HTTP (one `VisibleStore`). | `mcp-server/src/main.rs`, `visible-store/src/lib.rs` |
| B0-2 Publication gate | `config.json` v2 records `publication: {vaultToken, state: pending\|confirmed, policyRevision?}` for the active vault file. Readers pass the gate before the policy is even loaded, and again after the rows were read. Confirm/revoke only through the Native Host (`vault_publication_confirm` / `_revoke`), for the vault the panel shows. Minimal Side Panel UI. | `native-host/src/active_vault.rs`, `vault.rs`, `manage.rs`, `extension/vault-panel.js` |
| B0-3 Key errors and fences | `VaultError::{KeyUnavailable, Locked, DecryptFailed, ModeMismatch, EncryptionUnsupported}`; `KeySource` trait (this version: `NoKeys`). `proves_unwritable()` is false for every encrypted-mode failure except absence. Switching away from a vault this process cannot key uses an exclusive-handle fence (share mode 0). | `native-host/src/vault.rs` |
| B0-4 VisibleStore modes | The mode comes from the config, passed explicitly to the reader. Plain: existing checks; a file without the SQLite header is `vault_mode_mismatch` (was `vault_schema_mismatch`). Encrypted: never handed to SQLite; `vault_encryption_unsupported`, or `vault_mode_mismatch` for a plain/empty file. | `visible-store/src/snapshot.rs` |
| B0-5 config durability | Temp file → write → flush → **rename by handle with POSIX semantics** (`FileRenameInfoEx`) → flush again. Fault injection at every step. | `native-host/src/vault.rs` `write_config` |
| B0-6 Non-regression | All existing suites pass (§6). | — |

## 2. `config.json` version 2

```json
{
  "version": 2,
  "activeVault": "C:\\Users\\<you>\\AppData\\Local\\Memocampus\\memocampus.sqlite3",
  "locationApproved": false,
  "mode": "plain",
  "publication": { "vaultToken": "<64 hex>", "state": "confirmed", "policyRevision": 3 }
}
```

- Strict: unknown fields, unknown `mode`, unknown `state`, a malformed token, a relative path or any
  truncation make the whole file `config_invalid`. Size is bounded (64 KiB) before parsing.
- `vaultToken` is the existing `vault_token` (SHA-256 of volume serial + file index). A copied or
  replaced file has another token, so its publication is pending.
- Only one decision is kept: the one for the active vault file. Switching away and back starts
  pending again (never reuses an earlier approval of another file).
- Version 1 (`{version, activeVault, locationApproved}`) is still read, as plain with no
  publication. Readers (MCP) treat it as pending. The host rewrites it once (§3).
- A host binary from before B0 refuses a version 2 file as `config_invalid` (downgrade is not
  supported; all binaries ship together).

## 3. State transitions

```
                       vault_create / vault_open / switch to another file
   (no config) ──────────────────────────────────────────────────────────────► pending(token)
                                                                                  │   ▲
             vault_publication_confirm(expected = token, policy loads OK)         │   │ vault_publication_revoke
                                                                                  ▼   │ (expected = token)
                                                                              confirmed(token, policyRevision)

   switch to the very same file (same token)  : decision kept
   file replaced / copied (token changes)     : readers see "pending" (the recorded token no longer matches)
   config v1 at host start                    : → v2, confirmed only if the active vault opens AND the
                                                 global policy loads; pending otherwise; publication none
                                                 if the vault is missing/invalid; deferred (v1 kept) on any
                                                 transient failure (config lock busy, vault busy/IO, policy busy/IO)
   policy initialized (policy_initialize /     : confirmed → pending (same token), BEFORE the policy is
     policy_start; only when no policy and       written; a v1 config → v2 with no decision (so no later
     no marker exist)                            start can grandfather it); see §10
   ordinary policy edit                        : unchanged (§5, decision 3)
```

- Every write of `config.json` happens under `config.lock` (`ConfigLock`), like vault switches.
- Confirm/revoke compare `expectedVaultToken` (from the `vault_status` the panel showed) with the
  active file's token **under the lock**; a mismatch is `vault_switched` and nothing is written.
- Confirm requires: management enabled (not `MEMOCAMPUS_DB`), the vault opens as a valid plain vault
  (identity taken while it is open), and `PolicyStore::load()` succeeds (initialized, valid,
  supported). The policy is never written. Revoke requires only the token.

## 4. Protocols

### 4.1 Native Messaging (extension → host), additions

| Message | Fields | Answer (`vault_result`) | Errors |
|---|---|---|---|
| `vault_status` (extended) | — | `status.mode`: `plain`/`encrypted`/null; `status.publication`: `confirmed`/`pending`/null (null: no vault, or the `MEMOCAMPUS_DB` override, which MCP never reads); `status.publicationPolicyRevision` | — |
| `vault_publication_confirm` | `expectedVaultToken` | `{status}` | `invalid_vault_token`, `management_disabled`, `vault_not_configured`, `config_invalid`, `vault_switched`, `vault_missing`/`vault_invalid`/…, `vault_encryption_unsupported`, `policy_not_initialized`/`policy_invalid`/`policy_unsupported`/`policy_busy`/`policy_unavailable`, `vault_busy` |
| `vault_publication_revoke` | `expectedVaultToken` | `{status}` | as above, without the policy and open requirements |

Only extension pages can reach the host (Native Messaging `allowed_origins`). The service worker
relays no `vault_*` message (tested). The MCP server and VisibleStore contain no code that writes
`config.json` (tested by source scans).

### 4.2 MCP

- `memocampus-mcp [--vault <absolute path>] [--transport …]`. Without `--vault`: follow the active
  vault. With it: the path must be the active vault file itself (same identity, any spelling).
- Each tool call: gate (config, identity, publication) → policy → snapshot read (the open file's
  identity must equal the admitted one, checked while SQLite holds it) → gate again → judge rows.
  A switch, revocation or replacement anywhere in between discards the read (`vault_switched`).
- New tool error codes: `vault_not_configured`, `config_invalid`, `vault_not_active`,
  `vault_publication_pending`, `vault_switched`, `vault_mode_mismatch`,
  `vault_encryption_unsupported` (reserved: `vault_locked`, `vault_decrypt_failed`).
- Start-up runs the same gate once (`check_vault`); a refused start exits 2 with the code, as
  before. Consequence: a server cannot start while the active vault is pending.

### 4.3 Error taxonomy in the host (B0-3)

| Code | Meaning | May skip the switch fence? | Fence used |
|---|---|---|---|
| `vault_missing` | file absent | yes (any mode) | none |
| `vault_invalid` / `vault_schema_mismatch` (plain) | contents prove it is not a vault | yes (plain only) | none |
| `key_unavailable` | key store missing / protector (DPAPI) failure / mismatch | **no** | exclusive handle |
| `vault_locked` | key needs the user | **no** | exclusive handle |
| `vault_decrypt_failed` | keyed open answered `NotADatabase` | **no** | exclusive handle |
| `vault_mode_mismatch` | file contradicts the recorded mode | **no** | exclusive handle |
| `vault_encryption_unsupported` | encrypted mode in this version | **no** | exclusive handle |
| `vault_busy`, `io_error`, `db_error` | transient / unexplained | no | switch refused (unchanged) |

The exclusive handle (`CreateFile`, read access, share mode 0, never creating) can only be taken
while no process has the file open, and blocks every open while held (measured in tests: plain open
→ error 32, SQLite → `SQLITE_CANTOPEN`, never `NotADatabase`). Plain vaults keep the write-lock fence
exactly as before; their existing behaviour (read-only file refused, held file refused, junk file
needs no fence) is unchanged.

## 5. Design decisions

1. **The gate lives in VisibleStore, not only in the MCP binary.** Every public constructor is
   gated (`follow_active`, `pinned`, `from_process`); the ungated constructor exists only under
   `cfg(test)` inside the crate. The mode cannot be chosen by a caller; it always comes from the
   config.
2. **No fixed-path bypass.** Instead of a development flag, `--vault` is accepted only when it is the
   confirmed active vault. Tests publish their synthetic vault in a synthetic `config.json`. So
   there is no switch that could serve an unpublished vault.
3. **Policy edits do not revoke a confirmation; a policy initialization does** (PO decision after
   review finding F-02). Every read applies the current policy; a policy that is missing or broken
   gives `policy_unavailable`. The revision at confirmation is recorded and shown. A rule set
   replaced by a more permissive one is an explicit user action in the options page and keeps the
   approval. A policy that is initialized again (after the old one was lost) is a new basis, so the
   approval is withdrawn first (§10).
4. **Migration at host start**, not in the read path: every host process tries it once without
   waiting for any lock; MCP never writes. Until a host has run, MCP answers
   `vault_publication_pending` for a version 1 config (fail closed).
5. **POSIX-semantics rename** instead of `MOVEFILE_WRITE_THROUGH` (see §8).
6. Cursors and opaque ids are not bound to a vault: a cursor from vault A used after a switch names
   a position only, read from B under B's gate and policy. Nothing of A is returned.

## 6. Verification (this branch, Windows 11, rustc 1.98.1 MSVC, plain `bundled` SQLite)

| Suite | Before (c08797a / 395e468) | After | New |
|---|---|---|---|
| native-host (`cargo test --locked`) | 430 | 470 | 40 (`active_vault` 5, `vault::secure_vault_tests` 24, `manage` 4, `tests/secure_vault_b0.rs` 7) |
| visible-store | 102 (+2 ignored) | 111 (+2 ignored) | 9 |
| mcp-server | 52 | 56 | 4 |
| extension (`npm test`) | 708 (700 pass, 8 skipped) | 721 (712 pass, 9 skipped) | 13 |
| extension with `MEMOCAMPUS_TEST_HOST` | — | 721 (718 pass, 3 skipped) | — |
| extension with `MEMOCAMPUS_TEST_HOST` + `MEMOCAMPUS_TEST_MCP` | hung (F-01) | 721 (721 pass), about 21 s | — |
| `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `git diff --check` | — | clean (3 crates) | — |

Existing tests whose expectations changed on purpose: header-less files are `vault_mode_mismatch`
(VisibleStore 3, MCP 1); MCP start-up and replaced-file cases now meet the publication gate first
(MCP 3). Measured, not only asserted:

- **Rename experiment** (scratch program, 2 runs × 4 writers × 300 replaces, 3 readers):
  `std::fs::rename` 0 failed writes; `MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)` **956 and 967 of
  1200 writes failed with `ERROR_ACCESS_DENIED`** while readers had the file open; POSIX rename by
  handle 0 failed writes, 0 reader errors. The first B0 implementation used `MoveFileExW` and the
  multi-process host test caught `io_error` switches and a reader seeing no config at all.

## 7. Not verified (NOT RUN)

- Power cut or hard VM reset during a config write (only crash points simulated in-process).
  Whether the disk honours the flush is outside the program's control; no guarantee is claimed.
- File systems without POSIX rename semantics (FAT32, exFAT): the fallback to `std::fs::rename`
  exists but is untested here (vaults on such volumes are already refused or warned about).
- Real Chrome + real MCP client dogfood (§9).
- SQLCipher: the keyed path (`DecryptFailed` end to end) needs Slice S0/S2; only its classification
  is unit-tested.
- Non-Windows: the exclusive-handle fence is not available and refuses (R10).

## 8. Differences from the Slice A design

| Slice A text | B0 | Reason |
|---|---|---|
| architecture §8.2: use `MoveFileExW(MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH)` | `SetFileInformationByHandle(FileRenameInfoEx, REPLACE_IF_EXISTS \| POSIX_SEMANTICS)` + flush | measured: the `MoveFileExW` variant fails under concurrent readers; `WRITE_THROUGH` only affects copy-and-delete moves |
| §7.2: `--vault` accepted only while active and confirmed; "explicit flag for tests" (P8) | no flag at all | not needed: tests publish synthetic vaults |
| §7.2 VisibleStore: "the caller passes the mode" | mode passed internally from the config; no public mode parameter | a caller-chosen mode would contradict I4 |
| §4: `key_store_missing`, `dpapi_failed`, `key_store_mismatch` as codes | one code `key_unavailable` (cause kept internally as `KeyError::{Missing, ProtectorFailed, Mismatch}`) | the user action is the same; finer codes can be added in S2 |
| §7.2: publication keyed by `vaultId` for encrypted vaults | only `vaultToken` (plain only in B0) | encrypted vaults and `vaultId` arrive in S2 |

## 9. Windows dogfood (manual, after review)

Use a throw-away Windows account or a test `MEMOCAMPUS_APP_DIR`; never the real vault for steps 4–6.

1. Build the three release binaries; install the host as usual.
2. **Upgrade**: with an existing v1 `config.json` and an initialized policy, open the side panel.
   Expect `config.json` `"version": 2`, `publication.state = "confirmed"`, panel "MCPへの公開: 承認済み";
   an MCP client keeps working without changes. Keep a copy of the old `config.json` first.
3. **Follow-active**: register the MCP server **without** `--vault`. Create a second vault in the
   panel → panel shows "未承認"; the MCP client gets `vault_publication_pending`. Confirm in the
   panel (review step) → the client reads the new vault; the old one is never read.
4. **Pinned**: a client registered with `--vault <old path>` → after the switch every tool answers
   `vault_not_active`.
5. **Revoke**: "MCPへの公開を停止" → the client gets `vault_publication_pending` on the next call.
6. **Replace**: copy a vault file over the active path (with the panel closed and Chrome stopped)
   → the panel shows "未承認"; MCP refuses until confirmed again.
7. **Concurrency**: keep an MCP client polling (HTTP mode) while switching vaults several times in
   the panel: no `io_error` on switches; every MCP answer is either the new vault's or an error.
8. **8.3 / junction path** for the vault: the panel's token and the MCP gate agree (identity, not
   path text).
9. **Policy lost and initialized again** (test `MEMOCAMPUS_APP_DIR` only): with a confirmed vault,
   move `policy.json` and `policy.initialized` aside → MCP answers `policy_unavailable`; initialize
   the policy from the extension → panel shows "未承認", MCP answers `vault_publication_pending`
   until confirmed again. An ordinary rule edit in the options page leaves "承認済み".

## 10. Review findings F-01 and F-02 (after the independent review)

### F-01: opt-in end-to-end tests hung once the gate refused MCP's start

The E2E tests (`vault-items.test.js`, `navigation.test.js`, `MEMOCAMPUS_TEST_HOST` +
`MEMOCAMPUS_TEST_MCP`) created a vault (now `pending`) and started MCP, which exits 2 with
`vault refused: vault_publication_pending`; their RPC promise had no exit or time limit and waited
forever.

- The tests now confirm the new vault as the side panel does (`vault_status` →
  `vault_publication_confirm` with its `expectedVaultToken`) and check that a new vault is `pending`.
  The gate is not disabled or bypassed; `--vault` is still the path of the confirmed active vault.
- `tests/extension/real-processes.js` (shared, not a test) starts MCP with requests that always end:
  `ProcessFailure.kind` is `spawn_failed`, `exited` (with exit code, signal and stderr), `timeout`
  (15 s default), `protocol_error`, or `response_error` (`request` only; `rpc` still returns the raw
  message as before).
- `tests/extension/mcp-process.test.js`: stand-in servers that exit at start, crash mid-request,
  never answer, answer with a JSON-RPC error, or cannot be spawned (always run); and with the real
  binaries, MCP refused while pending (with and without `--vault`), served after confirmation, and
  refused on the next call after revocation (opt-in).

### F-02: a policy initialized again kept the old approval

The only operations that create a policy are `policy_initialize` (default policy) and
`policy_start` (first-run draft). Both write only when neither `policy.json` nor `policy.initialized`
exists, i.e. on a new install or after the policy was lost or removed outside Memocampus. There is
no reset message, and none was added.

| Operation | Publication |
|---|---|
| Rule edits: `policy_apply`, `policy_item_apply`, `policy_migrate`, domain approvals | kept |
| `policy_initialize` / `policy_start` that writes a policy | `confirmed` → `pending` (same token) |
| `policy_initialize` on an initialized policy (`policy_already_initialized`) | kept (nothing written) |
| Policy file lost or damaged | kept, but every MCP read fails (`policy_unavailable`) |
| Vault switch | as before (§3) |

Order (in `PolicyStore::initialize_with_then`, under the policy's exclusive lock, after every check):

1. withdraw (`vault::withdraw_publication`, under `config.lock`, waiting up to 5 s):
   `confirmed` → `pending` keeping the recorded token; a version 1 config → version 2 with no
   decision; nothing published (no config, pending, invalid config, `MEMOCAMPUS_DB`) → no write;
2. write the marker, then the policy.

- Withdrawal fails (config lock busy → `policy_busy`; config write fails → `policy_unavailable`):
  nothing is written. The publication may still say `confirmed`, but there is no policy, so MCP
  reads nothing; a retry withdraws first again.
- Policy write fails after the withdrawal: the vault stays `pending` (re-approval needed), never the
  reverse.
- A reader admitted under the old approval while this happens finds no policy (refused), or loads
  the new one and is discarded by the gate check after the read (`vault_switched`).
- Only one publication is stored, the active vault's; its token is kept as recorded, so no other
  vault's decision exists to be touched. A→B→A after the re-initialization is pending (§3).
- Lock order: policy lock (non-blocking) then config lock (waits). Confirmation holds the config lock
  and only tries the policy lock (fails as `policy_busy`), so the two cannot deadlock.

Not covered: policy files replaced by hand outside Memocampus with another valid policy are loaded
like any policy and are not detected as a re-initialization; missing, invalid or unsupported files
stay fail-closed (MCP reads nothing). External file changes are outside what the host can observe,
as before.
