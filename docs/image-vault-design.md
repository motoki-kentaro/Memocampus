# Image Vault — Slice 0 design (proposal, not implemented)

Status: Slice 0 approved by the PO (all recommendations in §9 accepted). Slice 1 (SQLite storage), Slice 2 (context-menu save over `connectNative`, see `native-host/src/image_protocol.rs` and `extension/image-*.js`) Slice 3 (Image tab, see `native-host/src/image_query.rs`, `extension/image-client.js`, `extension/image-gallery.js`) and Slice 4 (copy, export, thumbnail rebuilding, see `native-host/src/image_original.rs`, `extension/image-original.js`) are implemented; Slice 5 is not. Dogfood steps: `docs/image-vault-dogfood-slice2.md` (includes Slices 3 and 4).

### Vault identity across reconnects (Slice 2 review)

`Session::ticket()` also yields a `vaultToken`: SHA-256 of the vault file's volume serial number and file index, so it
does not depend on how the path is spelled and never reveals it. Every save reply carries it; the extension fixes it
on the first reply of a save action and sends it with every retry (same `saveId`, at most 3 attempts in total). A
retry that reaches another active vault is refused with `vault_switched` before any lookup or write, including the
resend after a lost commit answer. A connection lost before the first reply is not retried, because the save was not
yet bound to a vault.

### Image tab (Slice 3)

- `image_list` (metadata only, ≤ 60), `image_thumbnails` (≤ 16 ids) and `image_sources` (≤ 20 history rows) are
  one-shot messages; each reply is cut to a 700 KB budget (a list or history page ends early and its `next` cursor
  resumes there; thumbnails that do not fit come back as `deferred`). Thumbnails are re-checked by magic bytes on the
  host and again in the extension; only PNG/JPEG/WebP are displayed, through object URLs.
- Tiles request thumbnails when within 400 px of the viewport, at most once while loaded; tiles more than 2500 px away
  release their object URL. Every reply carries the `vaultToken`; a vault switch (here or in another window) drops all
  tiles, object URLs and late answers.
- Missing thumbnails: the image stays in the grid with its type label. Regeneration is **not** in Slice 3: it needs
  the original bytes in the extension (a chunked read of up to 32 MiB through ≤ 1 MB replies — the same `image_read`
  the copy feature of Slice 4 needs), a decode + resize, and an `image_thumbnail_put` that only inserts derived data
  (the original is never touched; `generator` versions decide replacement). Proposed for Slice 4 on top of `image_read`.

### Copy, export and thumbnail rebuilding (Slice 4)

- `image_read { blobId, sha256, vaultToken, offset }` returns `min(512 KiB, rest)` raw bytes as Base64 (about 700 KB
  per reply, 64 replies for 32 MiB), read with SQLite incremental BLOB I/O so a chunk never loads the whole original.
  The host refuses another vault (`vault_switched`) and a blob id that now names other bytes (`blob_missing`). The
  extension requests chunks strictly in order into one buffer of the declared size, checks every reply (vault, blob,
  hash, type, size, offset, exact chunk length) and accepts the bytes only after the total SHA-256 and the magic bytes
  match; any failure discards them.
- Deviation from §3: chunks are one-shot `sendNativeMessage` requests, not a port. Each request is validated on its
  own (vault token, hash), like the other Image tab queries; the cost is one host start per chunk.
- Copy: `clipboard.write([new ClipboardItem({ "image/png": promise })])` is called synchronously in the menu click;
  the promise reads the original and, unless it is already PNG, decodes it and encodes PNG (images over 50 Mpx are
  refused). Success is reported only after `write` resolves; a refused write is an error. If `ClipboardItem`
  rejects promises, the PNG is written when ready (this may lose the gesture and is then reported as an error).
- Export: `image_export { blobId, sha256, vaultToken }`. The host reads and verifies the original before the dialog
  opens, so a vault switch during the dialog cannot change what is written. Documents via `SHGetKnownFolderPath`;
  the dialog's only filter is the real type (default extension), and the Save dialog's default options include the
  overwrite prompt. The bytes go to a temporary file next to the target (`create_new`, `sync_all`), then
  `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` if the file existed when the dialog closed, otherwise a no-replace
  move (`export_file_appeared` if a file appeared meanwhile). On failure the temporary file is removed and the target
  is unchanged. Only the file name comes back; cancel is `{ ok: true, cancelled: true }`.
- Rebuilding: a tile without a usable thumbnail that comes within 400 px of the viewport is queued (one rebuild at a
  time, once per list); `makeThumbnail` (as at save time) runs on the original and `image_thumbnail_put` inserts it
  with `ON CONFLICT DO NOTHING` inside `write_for`, after re-checking blob id and hash. Nothing else is written.

## 0. Constraints found in the current system

| Fact | Source / evidence | Consequence |
|---|---|---|
| Host → Chrome message ≤ **1 MB**; Chrome → host ≤ **64 MiB** | Chrome “Native messaging” docs | Anything the host returns (thumbnails, image bytes for copy) must be chunked well below 1 MB. Uploads may be larger but are chunked anyway (PO requirement, bounded memory). |
| The host already processes **many messages per process** (`while let Some(body) = read_message()`) and rejects frames > 64 MiB | `native-host/src/main.rs` | `chrome.runtime.connectNative()` works with the existing host loop: one process per port, messages in order, process ends when the port closes. |
| `connectNative()` keeps the MV3 service worker alive (Chrome 105+); a host crash closes the port | Chrome SW lifecycle docs | An upload over a port is not cut off by SW idle termination; disconnect = abort. |
| Content scripts are bound by the page's same-origin policy even with host permissions; the SW is not (host permissions) | Chrome “Cross-origin network requests” docs | Fetch in the SW first; use page context only for what only the page can read (`blob:`, same-origin, hotlink-protected same-site images). |
| `host_permissions: ["<all_urls>"]` is already granted | `extension/manifest.json` | No new host permission needed. |
| `contextMenus.OnClickData` has `srcUrl`, `pageUrl`, `frameUrl`, `frameId`, `mediaType` | Chrome contextMenus docs | Image URL, page URL and frame are available without reading the page. |
| Writes go through `Session::write` (write lock + config re-check) | `native-host/src/vault.rs` | Image commits inherit the vault-switch guarantee: a commit lands in the vault that is still configured, or fails with `vault_switched`. |
| Vaults created by Memocampus have `user_version = 1`; legacy ones `0`; validation accepts extra tables | `vault.rs` (`SCHEMA_VERSION`, `validate_schema`) | Image tables can be added as schema v2 without breaking older hosts. |
| rfd's Windows save dialog does not override options, so `FOS_OVERWRITEPROMPT` (Windows default for save dialogs) stays on | `rfd-0.17.2/src/backend/win_cid/file_dialog/dialog_ffi.rs` | The OS asks before overwriting; still to be confirmed on a real machine. |
| `SHGetKnownFolderPath(FOLDERID_Documents)` is in `windows-sys` (`Win32_UI_Shell`) | windows-sys 0.61 | Documents folder without building `C:\Users\…`. |

## 1. Data model (schema v3; v3 added `saved_images.incognito` with no conversion of v2 vaults) — implemented in Slice 1

The authoritative definition is `IMAGE_SCHEMA_SQL` in `native-host/src/images.rs`.

| Table | Columns (in order) | Notes |
|---|---|---|
| `image_blobs` | `id`, `sha256` UNIQUE (64 hex), `mime_type`, `byte_length` > 0, `width` > 0, `height` > 0, `created_at`, `last_saved_at`, `save_count` > 0, **`data` last** | One row per distinct original. `data` is the last column so list queries never walk the BLOB's overflow pages. Index `image_blobs_recent (last_saved_at DESC, id DESC)`. |
| `saved_images` | `id`, `save_id` UNIQUE, `blob_id` → `image_blobs.id` (RESTRICT), `image_url`, `page_url`, `frame_url`, `saved_at`, `incognito` (0 or 1, required, no default) | Append-only, one row per save action. Index `saved_images_by_blob (blob_id, saved_at DESC, id DESC)`. |
| `image_thumbnails` | `blob_id` PK → `image_blobs.id` (CASCADE), `mime_type`, `width`, `height`, `generator` > 0, **`data` last** | Derived data; replaced only by a higher `generator`. |

- Times are Unix epoch **milliseconds** taken from the host clock at commit; export names use seconds.
- Dedup (`sha256`) and save-action idempotency (`save_id`) are separate: a replay of the same `save_id` returns the
  existing row and changes nothing (a different `sha256` for a known `save_id` is `save_id_conflict`); a new `save_id`
  for known bytes adds a history row and bumps `save_count` / `last_saved_at` without a new BLOB.
- The host detects the format from magic bytes (PNG, JPEG, GIF, WebP, AVIF, BMP), rejects SVG/markup
  (`svg_not_supported`) and anything else (`unsupported_format`), requires the declared MIME to match the bytes,
  compares header dimensions where the format allows (PNG, GIF, BMP, WebP, JPEG), rejects visibly truncated files
  (PNG `IEND`, GIF trailer, WebP RIFF size, BMP file size), verifies SHA-256 and length, and caps originals at
  32 MiB of raw bytes. Pixel decoding stays in the extension (`createImageBitmap`) before upload.
- URLs: `image_url` is `http(s)`, `blob:` or a bare `data:image/<type>` marker (no payload); `page_url` / `frame_url`
  are `http(s)`; all ≤ 8192 characters.
- `PRAGMA foreign_keys = ON` on every vault connection.
- Listing: keyset pages on `(last_saved_at, id)` (≤ 60 per page) with the latest source of each image; history pages
  on `(saved_at, id)`.
- Vault safety: `Session::ticket()` records the active vault when a transfer starts; `Session::write_for(ticket, …)`
  commits only if, under the write lock, both the open connection and `config.json` still name that vault
  (`vault_switched` otherwise). Validation and hashing run before the lock is taken.

### Migration

New vaults are created at `user_version = 2` with the image tables. Existing vaults (0 or 1) are migrated lazily by the
first image **write**, inside its `BEGIN IMMEDIATE` transaction: existing image tables are checked first (a foreign
table with the same name is `image_schema_mismatch`), then `CREATE TABLE/INDEX IF NOT EXISTS`, then
`user_version = 2` — all committed together with the first save, or rolled back with it. Reads, search, visits,
snapshots, status and backups never migrate. `user_version > 2` is `vault_newer` for image reads and writes. Backup
verification compares image row counts as well.

## 2. Image acquisition path (extension service worker)

1. Context menu item `Memocampusへ保存` with `contexts: ["image"]`, created in `runtime.onInstalled`.
2. On click, validate `srcUrl` scheme: `http:`/`https:`/`data:`/`blob:` only.
3. Get the bytes:
   - `data:` → decode in the SW.
   - `http(s):` → **SW `fetch(srcUrl, { credentials: "include", redirect: "follow" })`** (CORS does not apply with
     host permissions). Read the body as a stream and abort beyond the size cap; require `response.ok`.
   - If that fails (network error or HTTP 4xx such as hotlink protection) or for `blob:` → **page-context fetch**
     via `chrome.scripting.executeScript({ target: { tabId, frameIds: [frameId] } })`, which uses the page's origin,
     cookies and referrer (same-origin or CORS-enabled images only).
4. Validate: sniff the format from magic bytes (PNG, JPEG, GIF, WebP, AVIF, BMP), decode with
   `createImageBitmap` (rejects corrupt data, gives width/height), compute SHA-256 with `crypto.subtle`.
5. Thumbnail: `OffscreenCanvas` (max 320 px edge) → WebP.
6. Upload to the host (§3). Report success only after the host's `image_saved`.

Cookies/credentials: the browser attaches the user's cookies as it does when displaying the image; Memocampus never
reads, stores or forwards cookies or request headers. Only the response bytes and the URLs above are stored.

Expected coverage (to be confirmed in dogfooding; nothing is promised beyond this):

| Case | Expected |
|---|---|
| Normal public HTTPS image | SW fetch |
| X media CDN (`pbs.twimg.com`) | SW fetch; the variant shown (e.g. `name=small`) is what gets saved, not automatically `orig` |
| Futaba and similar same-site hosts | SW fetch, or page-context fallback (same origin, page referrer) |
| Authenticated images | SW fetch with the browser's cookies where SameSite / third-party cookie settings allow; else same-origin fallback; otherwise a clear error |
| `blob:` URL | page-context fetch while the page keeps the blob alive; revoked blobs fail with an error |
| `data:` URL | decoded locally |
| Redirects | followed by `fetch`; `image_url` stays the clicked `srcUrl` |
| HTTP errors / network failure | explicit error with status; **no URL-only fallback** |
| Not supported | cross-origin hotlink-protected images that need the page's `Referer` (e.g. some art sites), CSS background images and canvas-drawn images (no `srcUrl`), video frames |

## 3. Native Messaging transfer protocol

Options compared:

| | A. `connectNative` port per save, host buffers in memory (**recommended**) | B. one-shot messages + staging table in the vault | C. one-shot messages + temp files in `%LOCALAPPDATA%` |
|---|---|---|---|
| Fit with host | uses the existing multi-message loop | needs cross-process upload sessions | needs cross-process upload sessions |
| Atomicity | nothing touches SQLite until the final short transaction; disconnect/crash = nothing stored | partial rows live in the vault, need cleanup | partial files outside the vault, need cleanup |
| Resume after failure | restart the save (images are bounded) | chunk-level resume possible | chunk-level resume possible |
| Leftovers | none (memory is freed with the process) | stale sessions in the vault (also copied by backups) | stale temp files |
| Memory | ≤ size cap (+ one chunk) in the host | small | small |

Protocol (port messages, all JSON, chunks base64; protocol version 1):

```text
→ image_save_begin   { protocol: 1, saveId, sha256, byteLength, mimeType, width, height,
                       imageUrl, pageUrl, frameUrl }
← image_save_ready   { saveId, chunkBytes: 1048576, maxBytes }          // or
← image_saved        { saveId, blobId, savedImageId, deduplicated: true } // blob already stored: history row only
→ image_save_chunk   { saveId, index, data }                            // raw ≤ 1 MiB per chunk, in order
← image_save_chunk_ack { saveId, index, received }
→ image_save_commit  { saveId, thumbnail: { mimeType, width, height, data } }
← image_saved        { saveId, blobId, savedImageId, deduplicated }     // sent only after COMMIT
← image_save_error   { saveId, error }                                  // any step; the session is discarded
```

- The host checks: protocol version, declared size ≤ cap, chunk order and lengths, valid base64, total length,
  **SHA-256 of the received bytes = declared hash**, magic-byte format = declared MIME (after sniffing on both
  sides), thumbnail size cap.
- Commit = one `Session::write` transaction (blob if new, history row, counters, thumbnail). A vault switch during
  the upload yields `vault_switched`, never a write into another vault.
- Lost answer after COMMIT: the extension retries once with the **same `saveId`**; the host finds it and answers
  `image_saved` instead of inserting twice.
- Old hosts answer `{ ok: false, error: "unknown_type" }` → shown as “Native Hostの更新が必要です”.
- Images are **not** queued in `chrome.storage` like visits (10 MB quota, bytes would duplicate the vault): when
  the host or vault is unavailable the save fails visibly.

Reading for the gallery uses one-shot `sendNativeMessage` like search, with responses kept far below 1 MB:
`image_list` (metadata only, ≤ 60 items), `image_thumbnails` (≤ 16 ids, ≤ 700 KB total), `image_sources`
(history rows of one image). Full bytes (copy, thumbnail rebuilding) are read with one-shot `image_read` requests of ≤ 512 KiB raw each (Slice 4).

## 4. Thumbnails

| | Save-time thumbnail in the SW, stored as derived data (**recommended**) | Host-side generation (Rust `image` crate) | No stored thumbnails, browser cache only |
|---|---|---|---|
| Gallery cost | one small row per tile | same | full images through the 1 MB channel |
| New dependencies | none (`OffscreenCanvas`) | large decoder crates, more attack surface | none |
| Regeneration | missing/old `generator` → panel regenerates from the original and stores it back | host can regenerate | always |

The original BLOB stays the only source of truth; thumbnails can be deleted and rebuilt. The gallery loads
thumbnails lazily (IntersectionObserver), holds object URLs only for loaded pages and revokes them on vault switch.

## 5. Copy and export

- **Copy**: stream the original over a port, verify SHA-256, `createImageBitmap` → PNG via `OffscreenCanvas`
  (Chrome's Clipboard API writes `image/png` reliably), `navigator.clipboard.write([new ClipboardItem({ "image/png":
  promise })])` started inside the click so the user gesture is not lost while bytes stream. The vault is unchanged.
  Animated images copy as their first frame.
- **Export** (host side, no bytes through Native Messaging): `image_export { blobId }` → Documents via
  `SHGetKnownFolderPath(FOLDERID_Documents)` → rfd save dialog with initial name `<epoch seconds of the latest save>.<ext>`
  (`png`, `jpg`, `gif`, `webp`, `avif`, `bmp` from the detected type) → the OS overwrite prompt → write the original
  bytes to a temp file in the chosen folder and rename over the confirmed target. Cancel → `cancelled`, shown as a
  neutral message. The path comes from the OS dialog only; the extension never sends a path.
- Thumbnail actions use an in-panel context menu (right click, context-menu key, Shift+F10) with “画像をコピー” and
  “ファイルとして書き出す”.

## 6. Error handling

Every failure is shown (notification + panel when open); success only on `image_saved`. Codes:
`unsupported_url`, `fetch_failed` (with HTTP status), `fetch_blocked`, `too_large`, `unsupported_format`,
`decode_failed`, `hash_mismatch`, `transfer_interrupted`, `host_unavailable`, `host_outdated`, vault codes
(`vault_not_configured`, `vault_missing`, `vault_busy`, `vault_switched`, …), `db_error`, `clipboard_failed`,
`export_failed`; `cancelled` is not an error.

## 7. Migration of existing vaults

- Lazy: the first image operation runs `BEGIN IMMEDIATE; CREATE TABLE IF NOT EXISTS …; PRAGMA user_version = 2; COMMIT`
  when `user_version` is 0 or 1. Status/search never migrate. Existing tables and rows are untouched.
- A vault with `user_version > 2` (newer Memocampus) is not written by image operations (`vault_newer`).
- Backups (Backup API) include images automatically; backup verification adds image row counts.
- Vault growth: images make vaults large; status shows image count and bytes; no automatic VACUUM or deletion.

## 8. Test strategy

- Rust unit: migration v0/v1 → v2 (existing rows untouched, idempotent), dedup and append-only history, `save_id`
  idempotency, FK integrity, transaction atomicity (failure before COMMIT leaves nothing), size caps, magic-byte
  sniffing incl. lying MIME headers, SVG/HTML rejection, keyset pagination, response size budgets, export naming and
  Documents lookup (injected), vault switch during upload.
- Rust stdio (many messages in one process = a port): full upload, dedup short-circuit, out-of-order/oversized
  chunks, bad base64, hash mismatch, EOF before commit → nothing visible, old/unknown protocol version.
- JS: fetch strategy with fake `fetch`/`scripting` (data:, blob:, HTTP errors, redirects, oversize, fallback),
  uploader with a fake port (acks, errors, disconnect, retry with same `saveId`), gallery on the fake DOM
  (pagination, empty state, vault switch, stale answers, object URL release), copy/export result handling.
- Chrome dogfood checklist (Slice 5): the sites in §2, large images, cancel/overwrite in the dialog, clipboard paste
  into another app, vault switch while saving.

## 9. Decision points for the PO

1. **Transfer**: A (port + in-memory buffer) — recommended; B/C only if resumable uploads of very large files are needed.
2. **Size cap** for one image: proposed **32 MiB** (host memory and vault growth).
3. **Fetch credentials**: SW fetch with the browser's cookies (`credentials: "include"`) + page-context fallback —
   recommended; alternative is cookie-less SW fetch only (authenticated images then fail).
4. **New permissions**: `contextMenus` (required), `scripting` (page-context fallback), `notifications` (save result
   when the panel is closed). `clipboardWrite` only if dogfooding shows the gesture-based copy is insufficient.
5. **SVG**: reject in v1 (active content) — recommended; alternative: store and only ever render through `<img>`/canvas.
6. **Incognito**: refuse saving from incognito tabs — recommended; alternative: allow when the user enabled the
   extension in incognito.
7. **Export file name** for an image saved several times: latest save time — recommended.
