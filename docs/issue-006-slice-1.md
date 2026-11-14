# Issue #6 Slice 1 — Page capture/storage handoff

Status: implementation candidate; **not overall PASS** until PO Chrome/Windows dogfood and
independent review. No Page tab, deletion UI, export, extraction/FTS, or Page MCP publication.

## Scope and baseline

- `feature/6-page-vault`, based on `origin/main` `9fe97eb852292bd0ea8327b48252237a6b64efa0`.
- The pre-existing `.gitignore` edit and deferred `poc/mcp-prompt-injection/` and
  `poc/release-projection/` are preserved. No commit/push/PR.
- All automated fixtures use synthetic bytes and temp APP_DIR/Vaults. No real history,
  production Native Messaging registration, or user Chrome profiles are used.

## Boundaries

`page-save.js` fixes the context-menu event's tab ID (never activeTab/linkUrl/frameUrl), URL and
title before awaiting anything. Main-frame documentId and in-memory navigation invalidations
are checked before capture, after capture and before COMMIT. Reload, main-frame commit/error,
history-state/fragment navigation, tab URL/loading updates, removal and replacement invalidate
the operation. iframe clicks mean the containing **whole tab**. HTTP(S) only; incognito refused.
Page capture is explicit and not gated by Privacy Policy; it does not mutate policy.

This is an observed-document guarantee, not a frozen DOM: dynamic content can change within
the same document without a navigation signal. Chrome's MHTML representation/reproducibility,
inactive-tab capture, actual frame permissions and event ordering need real Chrome validation.
MHTML can include scripts and authenticated content. Nothing parses/renders/executes it here.

`page-upload.js` reads Blob slices <=1MiB, not the whole Blob. No persistent queue, IndexedDB,
storage.local, external fetch, plaintext spool or automatic capture/COMMIT retry is added.
Worker/process exit cannot generate a successful notification.

## Schema 8 (creation only)

- `page_blobs`: rowid PK, unique lowercase SHA-256, length 1..134217728, fixed `mhtml` /
  `multipart/related`, exact-length BLOB. No text conversion/normalization.
- `saved_pages`: rowid event ID, unique UUID save ID, RESTRICT FK to original, original URL and
  title, capture/Host-save epoch milliseconds, incognito constrained to 0, URL/time and blob indexes.
- Metadata bounds: URL 32768 UTF-8 bytes, title 65536 UTF-8 bytes; NUL refused in metadata, not
  in MHTML. Dates limited to the JavaScript Date range. No URL/title normalization or masking.
- Schema 7 (including a valid original schema-7 shape) is explicitly refused without migration,
  deletion, recreation or relabeling. User explicitly creates a new schema-8 Vault.
- Strict sqlite_master definition validation remains. No tables join Page saves into visits,
  tab_snapshots or page_metadata. No Page publication bit or MCP API is added. VisibleStore
  search/image/domain counts remain independent of Page canaries. #14 A / ADR-0000 unchanged.

## Native protocol v1

One persistent connectNative port:

```text
page_save_open {protocol:1,saveId,metadata:{url,title,capturedAt,incognito:false}}
  -> page_save_opened {vaultToken,generation,chunkBytes:1048576,maxBytes:134217728}
Chrome capture (no DB transaction)
page_save_begin {byteLength} -> page_save_ready
page_save_chunk {index,data:base64} -> page_save_chunk_ack {index,received}
page_save_prepare -> page_save_prepared {sha256,byteLength}  (not durable success)
page_save_commit {sha256} -> page_saved {savedPageId,blobId,sha256,byteLength,...}
```

Every post-open request/reply also carries the same saveId/vaultToken/generation. Index starts
at 0, all chunks except the last exactly 1MiB; wrong/duplicate/excess/missing chunks, invalid
Base64/lengths/IDs/state discard the upload. Original SHA is incremental during transfer and
rechecked on insert. Host allocates <=128MiB receiver capacity (fallible reserve), zeroizes
received bytes on drop, uses one chunk-sized temporary decode, and does not spool plaintext.
Page-mode frame length is capped at 2MiB **before allocation**; the first request retains the
existing global Native Messaging 64MiB limit because the protocol is not known yet. Replies
are small; no original bytes go back through Native Messaging. Image v2 is not changed.

An OS file lock (`page-upload.lock`) permits one Page transfer per APP_DIR, including separate
Chrome profiles/processes. It releases on error/abort/port/process death. Native input is read
one permitted frame at a time on a reader thread, so an idle Page operation expires even if
stdin remains open. Idle and Chrome capture/step timeout: 120s; total Host operation: 15min.

## Vault selection and COMMIT

The opaque Vault file token remains the identity boundary. Config's optional
`selectionGeneration` UUID is additionally fresh on each new active selection, including
same-file reselect and A→B→A. Existing non-Page readers need no new behavior. A legacy config
without a generation cannot start Page saving until an explicit reselect.

Each Page step briefly acquires existing ConfigLock (same config→SQLite ordering as management),
compares generation, and uses a fresh `Session::read_for` to verify file identity, mode, key and
schema. COMMIT uses `Session::write_for` while ConfigLock is still held. No config lock or write
transaction is held while Chrome captures or while the port waits for chunks. Product selection,
recovery/reinitialization cannot silently change the destination. External malicious editing of
config/Vault by the OS user is not an authentication guarantee.

BLOB insertion uses zeroblob + SQLite incremental BLOB writes; BLOB handle closes before
COMMIT. BLOB and event insert share the existing immediate transaction. Same SHA references
one original; a distinct save ID creates another event. Same ID requires exact original SHA,
length, URL, title, capture time and incognito match; otherwise `save_id_conflict`.

Only a post-COMMIT `page_saved` yields success. Lost/malformed/interrupted COMMIT replies cause
one read-only `page_save_status` on a new port, bound to the original Vault/generation and full
metadata/SHA/length. No matching receipt, a switched Vault, unavailable Host or invalid reply
means **保存状態未確定**. No automatic re-COMMIT/recapture, including when the record disappeared.
Status deliberately does not disclose original bytes or metadata in its response. Navigation/
Vault cancellation after dispatch of COMMIT can still report a confirmed committed save; it
cannot undo a transaction that already completed.

## Backup / restore

Existing SQLite Backup API paths remain. ContentCounts and local UI add Page event/original
counts. Target verification and restore verification stream every Page BLOB to recompute SHA
and length and check Page FK presence. Backup A/B/C/D fixtures include binary MHTML canaries;
roundtrip dumps compare raw BLOB bytes and event metadata. No cryptographic format changes.

## Verification and memory

Full verification: JS default 886 passed / 13 opt-in skipped; with release Host+MCP supplied,
899 passed / 0 skipped. Rust native-host 603 passed / 0 ignored, VisibleStore 116 passed /
2 pre-existing ignored benchmarks, MCP server 57 passed / 0 ignored. Three crates pass fmt,
clippy --all-targets -- -D warnings and release build. git diff --check passes. Initial DPAPI
failures in the sandbox were rerun successfully with the normal Windows identity against temp
Vaults only. OpenSSL linker LNK4099 missing-PDB warnings remain (not Rust/Clippy failures).
After the final schema-error ordering/error-reflection guard, all 8 Page protocol tests were
rerun successfully (including schema 7 with an old generation-less config), then Native Host
clippy/release and all three fmt checks were rerun. No product files changed after those checks.
New coverage includes protocol malformed/duplicate/
missing/out-of-order chunks, max-size storage, same-save replay/conflicts, event/BLOB rollback,
actual deferred COMMIT failure, secure key loss, all transfer phases across A→B and A→B→A,
OS transfer exclusion/release, actual Native Messaging framing/port death/lost COMMIT ACK,
manual neverSave save vs unchanged automatic rejection, and VisibleStore Page isolation.

`scripts/measure-page-transfer.ps1` runs the synthetic 128MiB protocol fixture in a separate
hidden Windows test process and samples its peak working set. Initial debug-profile result:
152862720 bytes (145.78MiB), protocol transfer + prepare + COMMIT + streaming verification
8.804s (wall 8.92s). Only one 1MiB input chunk is generated, not another 128MiB fixture buffer.
This is **not** Chrome process/Blob memory or real Native Messaging JSON framing measurement;
physical-memory/RSS includes SQLite/allocator and is not limited to 128MiB. No low-memory
availability guarantee is claimed. Real near-limit Chrome capture remains PO validation.

## PO isolated Windows/Chrome checklist

1. Use a new test Chrome profile and isolated loopback Host bridge / APP_DIR. Do not change
   production Native Messaging registration. Create a fresh plain and secure schema-8 Vault.
   Keep any schema-7 fixture unchanged and confirm it is refused explicitly.
2. Reload the unpacked extension; inspect the new pageCapture permission. On synthetic local
   HTTP(S), save from page, iframe and link context menus; verify the click-origin whole tab,
   URL/title, binary SHA and COMMIT-only notification, not the link target or another active tab.
3. Save identical bytes under different actions; confirm one BLOB/two events. Note real Chrome
   may generate different MHTML metadata each capture, so visual equality is not byte equality.
4. During delayed capture/transfer, reload, same-URL reload, SPA A→B→A, remove/replace tab,
   switch Vault A→B and A→B→A, or remove key access. Confirm no partial event/BLOB or wrong-Vault
   write. Repeat with inactive tabs; confirm unsupported pages/incognito are explained.
5. Terminate Worker/Host before COMMIT and reconnect; no success or automatic persistent replay.
   Drop only the COMMIT reply using the isolated bridge after observing SQLite COMMIT: status
   may confirm exactly one event. Disconnect status too: 保存状態未確定, no automatic second save.
6. Use inert near-128MiB fixtures and monitor Chrome+Host memory, capture duration and total
   timeout. Empty/over-limit capture must never persist. Do not use real authenticated data.
7. Back up/restore plain and secure temporary Vaults; compare every original byte/SHA and Page
   event/BLOB counts. Confirm Page saves do not appear in existing MCP tools even if Vault MCP
   publication is separately approved. Never enable raw Page publication.

Independent review: generation/config-lock/write-lock ordering; all Vault management paths;
main-loop idle/length framing; no ambiguous COMMIT retry; full metadata idempotency; incremental
BLOB rollback/close behavior; secure backup roundtrip; navigation observation gaps. Review
untracked Page source/tests too. General memory pressure, actual Chrome MHTML/temp-file behavior,
power-loss durability and PO UI interaction remain outside automated proof.

## Changed-file inventory (excluding the user's existing .gitignore change)

- Extension: `manifest.json`, `page-save.js` (new), `page-upload.js` (new),
  `service-worker.js`, `sidepanel.js` (cancellation broadcast only), `vault-client.js` (counts).
- Host: `active_vault.rs` (selection generation), `schema.rs`, `pages.rs` (new),
  `page_protocol.rs` and `page_protocol/tests.rs` (new), `main.rs`, `vault.rs`,
  `manage.rs`, `manage/restore.rs`, `manage/convert.rs` (Page counts).
- Rust tests: `src/images/tests.rs`, `src/manage/backup_restore_tests.rs`,
  `src/manage/convert_tests.rs`, `tests/page_vault.rs` (new), `tests/stdio_protocol.rs`;
  `visible-store/src/tests.rs`, `mcp-server/tests/stdio_mcp.rs`.
- JS tests: `tests/extension/page-save.test.js` (new), `image-save.test.js`,
  `navigation.test.js`, `options.test.js`, `page-metadata.test.js`.
- Evidence: this file and `scripts/measure-page-transfer.ps1` (new). Ignored `.tmp-6-*.log`
  files retain suite/build outputs and memory-test stdout/stderr for local review.
