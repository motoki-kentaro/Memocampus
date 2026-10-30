# Image Vault — Slices 2 to 4 dogfood checklist (Chrome, Windows)

Scope: saving images from the page context menu (Slice 2), browsing them in the Image tab (Slice 3), and copying,
exporting and rebuilding thumbnails (Slice 4). Saved rows can
also be checked with the SQLite queries below. Use a **test vault** (Side panel → ⚙ → 場所を選んで新規作成…) so your real vault stays clean.

## Setup

1. `cd native-host` → `cargo build --release` (replaces the registered host executable).
2. `chrome://extensions` → reload Memocampus. Chrome asks to approve the new permissions
   (context menus, scripting, notifications); accept them.
3. Windows: Settings → System → Notifications → make sure Google Chrome may show notifications.
4. Optional inspection tool: any SQLite viewer opened **read-only** on the test vault, e.g.
   `SELECT id, mime_type, byte_length, width, height, save_count, datetime(last_saved_at/1000,'unixepoch') FROM image_blobs ORDER BY id DESC;`
   `SELECT blob_id, image_url, page_url, frame_url, datetime(saved_at/1000,'unixepoch') FROM saved_images ORDER BY id DESC;`

## Final PO check for Issue #2 (start here)

Required checks are marked **必須**; the rest are optional. Failure injection that is hard to reproduce by hand is
never required: it is covered by automated tests (column “自動テスト”). Numbers refer to the detailed tables below.

| Area | 必須 manual checks | Covered by automated tests (no manual check needed) |
|---|---|---|
| Save from a page | **#1–#4, #9, #12, #17**; real sites **#5 (X)**, **#6 (ふたば)** — record the result per site | SHA-256 of the original = stored BLOB; dedup; idempotent retries after a lost answer (`stdio_protocol`, `image_protocol::tests`) |
| Large images | **#7** and **#37** with one 10–30 MiB image (note the times) | 32 MiB upload and 64-chunk read-back byte for byte (`a_32_mib_original…`, `image-original.test.js`) |
| Limits | **#8** (> 32 MiB) | > 50 megapixels refused for copy/rebuild (`image-original.test.js`); manual check optional, needs a suitable file |
| Image tab | **#20–#24, #27, #29, #30** | paging, lazy thumbnails, object URL release, stale answers after a vault switch (`image-gallery.test.js`) |
| Refresh icon | **#23, #23b** | duplicate clicks, failure keeps the dot (`image-gallery.test.js`) |
| Keyboard / focus | **#31, #32, #34, #35** | focus fallback when a tile disappears (#33 optional) |
| Copy | **#36** (PNG, JPEG, WebP, AVIF, GIF) | conversion only for the copy, refused write is an error (`image-original.test.js`); **#38** optional |
| Export | **#39** incl. the SHA-256 comparison, **#40, #41, #42** | exact bytes, temporary file, existing file intact on failure (`image_original::tests`); #43, #44 optional |
| Thumbnail rebuild | **#45** if you have an image without a thumbnail (#26); otherwise optional | rebuilt once, only the thumbnail row is added, nothing written after a vault switch (`image_original::tests`, `image-gallery.test.js`, `stdio_protocol`) |
| Vault | **#19** (open, create, backup with images), **#28** | backup contains images, history and thumbnails; old-vault requests refused after a switch (`stdio_protocol`) |
| Notifications off | **#14** | badge logic (`image-save.test.js`) |
| Failure injection | none required | #15 (switch during a save), #16 (kill the host) are optional; covered by `image_protocol::tests` and the extension upload tests |

Report for each check: pass / fail, and for failures the page URL (if shareable), the message shown and whether rows
appeared in the vault.

## Checks

| # | Action | Expected |
|---|---|---|
| 1 | Right-click a normal image on an HTTPS page | Menu shows **Memocampusへ保存**. Right-click on text/links (not images) does not show it. |
| 2 | Save it | Notification “Memocampusへ保存しました” with the thumbnail, size and bytes. One `image_blobs` row, one `saved_images` row with image URL and page URL. |
| 3 | Save the same image again | Notification mentions “保存履歴を追加しました”. Still one blob; two history rows; `save_count` 2. |
| 4 | Save the same image from a different page | Still one blob; new history row with the other page URL. |
| 5 | X (x.com): right-click a photo in a post | Saved. The stored variant is the one displayed (e.g. `name=small` / `medium`), not automatically `orig`. |
| 6 | Futaba (`*.2chan.net`): right-click a thread image or thumbnail | Saved, either directly or through the in-page fallback. Record which images fail, if any. |
| 7 | A large image (5–30 MB, e.g. a high-resolution photo) | Saved; takes longer; no partial row appears while it is transferring. |
| 8 | An image larger than 32 MiB | Failure notification “画像が大きすぎます（上限 32 MiB）”. No new rows. |
| 9 | An SVG image (e.g. a site logo in `.svg`) | Failure “SVG画像は保存できません”. No new rows. |
| 10 | A `data:` image (e.g. an inline icon) | Saved; `image_url` is `data:image/…` without the payload. |
| 11 | An image drawn from a `blob:` URL (e.g. image previews in web apps) | Saved via the page while the page is open, or a clear failure. |
| 12 | Same image in an incognito window (allow Memocampus in incognito to see the menu) | Failure “シークレットウィンドウの画像は保存しません”. No new rows. |
| 13 | ⚙ → no vault configured (point the config to a missing vault or start fresh), then save | Failure “Vaultが未設定です…”. Toolbar badge shows “!” with the reason in its tooltip. |
| 14 | Disable Chrome notifications in Windows, cause a failure (e.g. #9) | No toast, but the toolbar badge shows “!” and the tooltip names the failure. A later successful save clears it. |
| 15 | During a large save (#7), switch the vault in ⚙ | Failure “Vaultが切り替えられました…”; the image is in neither vault. |
| 16 | During a large save, close Chrome or kill `memocampus-native-host.exe` in Task Manager | No success notification (or a failure); no partial rows in the vault. |
| 17 | A broken image link or a 404 image | Failure naming the HTTP status (e.g. “HTTP 404”). |
| 18 | An image that only loads when signed in (e.g. a private photo on a site you are logged into) | Saved if the browser's cookies allow it; otherwise a clear failure. Note the site and result. |
| 19 | Regression: browse a few pages, search them in the URL tab, use 全タブを記録, open/create/backup a vault in ⚙ | Unchanged behaviour. |

Report for each failing check: the page URL (if shareable), the notification text and whether rows appeared.

## Known limits (expected failures)

- Cross-origin images that the server only serves with the original page as `Referer` (hotlink protection on a
  different domain) can fail in both the worker and the page fallback.
- CSS background images, canvas drawings and video frames have no image context menu / `srcUrl`.
- `blob:` URLs only work while the page that created them is open and has not revoked them.
- Authenticated images depend on the site's cookie settings (SameSite, third-party cookie blocking).
- Animated images are stored as their original file; the thumbnail shows the first frame.
- Animated GIF/WebP copy as their first frame (the clipboard takes a still PNG); export keeps the animated file.
- Copying and rebuilding decode in the side panel; images above 50 megapixels are not copied or rebuilt.
- Reading a large original takes one host start per 512 KiB (about 64 for 32 MiB), so copying it takes a few seconds.

## Slice 3 — Image tab

| # | Action | Expected |
|---|---|---|
| 20 | Open the side panel on the URL tab | No image request yet; the Image tab loads only when you open it. |
| 21 | Open the **Image** tab | Saved images newest first, one tile per distinct image; tiles keep the picture's proportions. Each tile's tooltip shows type, size, last save time and save count; `×N` marks images saved several times. |
| 22 | Save a new image while the Image tab is open and scrolled to the top | It appears at the top without further action. |
| 23 | Scroll down, then save another image | The list stays where it is; the refresh icon (top right of the Image tab) gets a dot and its tooltip reads “新しい画像があります”. Clicking it scrolls to the top and loads the new image; the dot disappears and the tooltip returns to “画像一覧を更新”. |
| 23a | Click the refresh icon several times quickly | One reload; the icon is dimmed while loading. With the vault unavailable, the error is shown and the dot stays. |
| 23b | Refresh icon in light, dark and Windows high-contrast themes | Icon and dot clearly visible; the round click area is comfortably large (44 px). |
| 24 | Click a tile | “保存履歴” opens at the bottom: type, size, count, and every save with date, image URL, page URL (and frame). Web URLs open in a new tab; `data:` and other URLs are plain text. |
| 25 | With 100+ saved images, scroll to the end | More tiles load automatically (60 per page); scrolling back up shows thumbnails again. |
| 26 | An image saved without a thumbnail (rare; e.g. an AVIF the browser could not thumbnail) | Its tile shows the type label (e.g. “AVIF”) instead of a picture; it is not missing from the grid. |
| 27 | Switch the vault in ⚙ while the Image tab has images, then open the Image tab | Only the new vault's images; none from the previous vault, even briefly. |
| 28 | Point the config to no vault and open the Image tab | Error with “Vault設定を開く”, which switches to ⚙. |
| 29 | Light and dark theme, narrow side panel | Grid, tiles, history panel readable; no horizontal scrolling. |
| 30 | Regression: URL tab search, scroll position when switching tabs, ⚙ vault actions | Unchanged. |

## Slice 4 — Copy, export, thumbnail rebuilding

Keep the side panel focused while copying (click in it first). For the byte comparison of exported files use
`certutil -hashfile <file> SHA256` against `SELECT sha256 FROM image_blobs WHERE id = …`.

| # | Action | Expected |
|---|---|---|
| 31 | Image tab: Tab to a tile, press Enter | 保存履歴 opens and gets focus (outline); a screen reader announces “保存履歴”. Escape or 閉じる returns focus to the same tile. |
| 32 | Click a tile with the mouse | 保存履歴 opens; focus is not moved into it. |
| 33 | Open 保存履歴 from the keyboard, then click the refresh icon or switch the vault in ⚙ | Focus is not lost to the page: it lands on the Image tab area. |
| 34 | Right-click a tile | Menu with **画像をコピー** / **ファイルとして書き出す** at the pointer; near the right or bottom edge it stays inside the panel. Up/Down/Home/End move, Escape closes and focus returns to the tile; a click elsewhere closes it. The browser menu does not appear. |
| 35 | Focus a tile, press the context-menu key or Shift+F10 | The same menu opens below the tile, first item focused. |
| 36 | 画像をコピー on a PNG, a JPEG, a WebP, an AVIF and a GIF | Status “画像をクリップボードにコピーしました。”; paste into Paint, Word or a chat: the picture appears (GIF: first frame). |
| 37 | 画像をコピー on a large image (10–30 MB) | “コピーしています…”, then success after a few seconds (note the time). Paste works. |
| 38 | Start a copy of a large image, then click into the web page before it finishes | Either success or a clear error (“許可されませんでした…”); never a false success. Note which. |
| 39 | ファイルとして書き出す | The Windows Save dialog opens in **Documents** with the name `<epoch seconds>.<ext>` (e.g. `1730000000.jpg`) and the image's type. Save → “「…」として書き出しました。”; the file's SHA-256 equals the vault's. |
| 40 | Export, change folder and name, Save | Written exactly there under the new name. |
| 41 | Export, Cancel | “書き出しをキャンセルしました。”; no file. |
| 42 | Export onto an existing file | Windows asks to replace it. No → back in the dialog, nothing written. Yes → the file is replaced completely. |
| 43 | Export onto a read-only existing file (Properties → Read-only) | Error “ファイルを書き出せませんでした。既存のファイルは変更されていません。”; the old file is intact; no `.memocampus-tmp` file is left. |
| 44 | While the Save dialog is open, switch the vault in ⚙ of another window, then save | The file is the image you chose (from the vault it was listed in). |
| 45 | An image without a thumbnail (#26) scrolled into view | Its picture appears shortly after; reopening the tab shows it at once. `save_count`, `last_saved_at` and `saved_images` are unchanged; one `image_thumbnails` row was added. |
| 46 | Regression: save from the page context menu, URL tab search, ⚙ vault actions, left-click history | Unchanged. |
