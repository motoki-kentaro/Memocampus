# Memocampus

Local-first web memory for Chrome on Windows.

## Slice 1

Record completed top-level page navigations as URL, title, and timestamp. The MV3 service worker queues events in `chrome.storage.local`, sends them to the Rust native host, and removes a queue item only after a successful acknowledgement. The native host stores events in SQLite with an idempotency key.

## Side panel search

Clicking the Memocampus toolbar action opens a side panel with three tabs: **URL** (search), **Image** (saved images, see below) and **⚙** (vault settings). The URL tab searches the local SQLite vault (`visits` and `tab_snapshots`, aggregated per URL) by URL or title. The panel also has the “全タブを記録” button (the former toolbar popup has been removed). See `docs/issue-002-side-panel-search.md` for the `search_pages` protocol.

## Vault

The vault (SQLite file) is independent of the host executable. Its path is stored in `%LOCALAPPDATA%\Memocampus\config.json`; the default vault is `%LOCALAPPDATA%\Memocampus\memocampus.sqlite3`. On first use, open the side panel's **Vault** section and create a vault explicitly (or open an existing one). Until then visits stay queued in the extension. Manual backups use the SQLite Backup API and copy the whole vault, including saved images, their save history and thumbnails. Switching or opening another vault switches the images too; nothing is copied between vaults. See `docs/issue-003-vault-location.md`.

The database previously stored next to the executable (`native-host/target/release/memocampus.sqlite3`) is no longer used and is left untouched.

This project never closes tabs automatically.

## Incognito windows

Memocampus never records browsing automatically from incognito windows, even if you allow the extension to run in
incognito in `chrome://extensions`. An image you explicitly save from an incognito window is the one exception:

- **Visits**: the tab's state is checked before anything is queued. If the tab is incognito, already closed, or its state
  cannot be determined, the visit is not recorded (no queue entry, no dead letter). A page closed the moment it
  finishes loading can therefore be missing from the history.
- **全タブを記録**: only tabs known to be normal are saved and counted; incognito tabs are left out without a trace.
- **Images**: right-clicking an image in an incognito window and choosing **Memocampusへ保存** works. The image is
  stored permanently in the normal SQLite vault and is part of every backup: incognito does not make it temporary. Each
  save-history row records whether it came from an incognito window (shown as “シークレット” in 保存履歴). The same image
  saved from both kinds of window is stored once and has a row of each kind. A save whose window type cannot be
  determined is refused. The image is fetched inside the page, or else without cookies or HTTP cache, so the normal
  profile's cookies are not sent; an image that the page may not read and that requires a login can therefore fail.
- **Future MCP access**: saving and publishing are separate operations. Nothing is published initially, and this
  attribute is kept so that a future gate can use it.
- **Native host**: as a second layer it accepts a visit or tab snapshot only if it is declared `"incognito": false`;
  anything else is refused without being stored, and the extension discards it without keeping its URL or title. The host
  cannot see the browser: it enforces the extension's declaration, and telling incognito tabs apart is the extension's job.

Queued events without that declaration (from an earlier version) are discarded, not sent. Rows already in a vault are
not changed or deleted.

## Images (Image Vault)

**Saving.** Right-click an image on a web page and choose **Memocampusへ保存**. The extension fetches the original file
(with the browser's cookies, falling back to the page itself for hotlink-protected images), checks it and sends it to
the native host in chunks; it is stored in the active vault in one transaction, so an interrupted transfer leaves
nothing behind. A notification (and, if notifications are off, a “!” badge on the toolbar button) reports the result.

- Formats: PNG, JPEG, GIF, WebP, AVIF, BMP, identified from the bytes (not the file name or server header).
  Up to **32 MiB** per image.
- Not saved: SVG (may contain scripts), `file:`/`chrome:` and other non-web URLs.
  CSS background images, canvas drawings and video frames have no image context menu.
- Each distinct file (by SHA-256) is stored once; saving it again only adds a save-history row (time, image URL,
  page URL, frame URL) and increases its save count. Cookies, tokens and response headers are never stored.

**Image tab.** Saved images, newest first, one tile per distinct file. The refresh icon (top right) reloads the list
and shows a dot when new images were saved while you were scrolled down.

- Click a tile (or press Enter) for its **保存履歴**: every save with date, image URL and page URL.
- Right-click a tile (or press the context-menu key / Shift+F10) for:
  - **画像をコピー** — puts the picture on the clipboard as PNG (other formats are converted for the copy only;
    animated images copy their first frame). The side panel must keep the focus until “コピーしました” appears.
  - **ファイルとして書き出す** — the native host opens the Windows Save dialog in **Documents**, suggesting
    `<last save time in Unix seconds>.<ext>`; you can change folder and name, Windows asks before replacing a file,
    and the stored bytes are written unchanged (through a temporary file, so a failed write leaves an existing file
    intact). The vault itself is never modified by copying or exporting.
- Images saved without a thumbnail show their type label; the thumbnail is rebuilt from the original when the tile
  comes into view and added to the vault (the original and its history are not changed).

Design and protocol: `docs/image-vault-design.md`. Manual test checklist: `docs/image-vault-dogfood-slice2.md`.

### Permissions

`contextMenus` (the save menu item), `scripting` (fetching an image inside its page when a direct fetch is refused),
`notifications` (save results), `nativeMessaging`, `storage`, `tabs`, `webNavigation`, `sidePanel`, and host access to
all sites (to fetch the right-clicked image with its site's cookies, and to record visits).

## Installation (Windows, developer mode)

Requirements: Windows 10/11, Google Chrome 116 or later, [Rust](https://rustup.rs/) (stable, MSVC toolchain).
Node.js 18+ is only needed to run the extension tests. No administrator rights are required.

1. **Build the native host** (from the repository root):

   ```powershell
   cd native-host
   cargo build --release
   cd ..
   ```

2. **Load the extension**: open `chrome://extensions`, turn on *Developer mode*, choose *Load unpacked*, and select the `extension` folder.
3. **Copy the extension ID** shown on the Memocampus card (32 letters `a`–`p`).
4. **Generate the host manifest and register it for your user** (works in Windows PowerShell 5.1 and PowerShell 7):

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\install-native-host.ps1 -ExtensionId <extension-id>
   ```

   The script fills `native-host/com.memocampus.vault.template.json` with the absolute path of *your* built `memocampus-native-host.exe` and your extension ID, writes `native-host/com.memocampus.vault.json` (git-ignored), and sets `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault` to that file. It does not touch HKLM or your vault, can be run again safely, and refuses to re-point an existing registration that uses a different manifest unless you pass `-Replace`. Add `-WhatIf` to see what it would do.
5. **Reload the extension** in `chrome://extensions`.
6. **Check the connection**: click the Memocampus toolbar button to open the side panel, then open **Vault**. A status such as “Vaultが未設定です” means Chrome reached the native host. “Native Hostに接続できません” means the registration or manifest is wrong; check it with `reg query "HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault"` and re-run step 4.
7. **Create your vault** in the **Vault** section (default location `%LOCALAPPDATA%\Memocampus\memocampus.sqlite3`, or choose another folder), or open an existing one. Visits are queued in the extension until a vault exists.

### About the extension ID

- With *Load unpacked*, Chrome derives the ID from the folder's absolute path, so it differs per machine and changes if you move the folder. Re-run step 4 with the new ID (the old registration is updated in place).
- A future Chrome Web Store release has one fixed ID; the install script would then be run with that ID (or ship with it as a default). Nothing in this repository hard-codes an ID.

### Updating and uninstalling

- After updating the source, rebuild the host with `cargo build --release` and reload the extension in `chrome://extensions` (accept new permissions if Chrome asks). An extension newer than the host reports “Native Hostが古い…” / “この操作に対応していません”; rebuilding fixes it. The host registration does not need to be repeated unless the extension ID or the folder changed.
- The host is not copied anywhere: the registered manifest points at `native-host\target\release\memocampus-native-host.exe`, so `cargo clean` removes the host until you rebuild; your vault lives outside `target/` and is not affected.
- To unregister: `powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\uninstall-native-host.ps1`, then remove the extension in `chrome://extensions`. Your vault and settings in `%LOCALAPPDATA%\Memocampus` are kept; delete that folder yourself if you no longer need the data.

## Tests

- Native host: `cargo test` in `native-host/` (unit tests plus a stdio protocol test against the built binary).
- Extension modules: `npm test` at the repository root (Node's built-in test runner, no dependencies).

## Continuous Integration

GitHub Actions (`.github/workflows/ci.yml`) runs on pull requests targeting `main`, on pushes to `main`, and on manual runs. It only has read access to the repository contents, uses no secrets, and uploads no artifacts.

- **JavaScript** (`ubuntu-latest`, Node.js 24): `npm ci`, `npm test`. The extension tests use fake Chrome APIs and need no Windows.
- **Rust** (`windows-latest`, stable MSVC toolchain): format check, tests, Clippy and a release build. The host calls Windows APIs, embeds its icon through the MSVC linker, and has tests that only run on Windows/MSVC, so a Linux run would not verify it.

Run the same checks locally:

```powershell
npm ci
npm test

cd native-host
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

The release build replaces the executable Chrome uses; close Chrome first, or add `--target-dir` with another folder to leave it in place.

CI does not cover Chrome itself: loading the extension, the host registration and the file dialogs are still checked by hand in Chrome (dogfooding).

## Layout

- `extension/` — Chrome Manifest V3 service worker (visits, image saving) and side panel (search, images, vault)
- `native-host/` — Rust Native Messaging host and SQLite persistence
- `docs/` — issue and slice acceptance criteria
- `assets/icon.png` — master of the app icon; `scripts/generate-icons.py` — regenerates the icons from it

## Icons

`assets/icon.png` (1254×1254, opaque, dark navy background) is the only source of the app icon. Every other icon file is
generated from it and committed, so builds need neither Python nor network access:

| Generated file | Sizes | Used by |
|---|---|---|
| `extension/icons/icon-16.png`, `-32`, `-48`, `-128` | 16, 32, 48, 128 px | `manifest.json` `icons` (extensions page, Web Store size) and `action.default_icon` (toolbar, 16/32) |
| `native-host/resources/memocampus.ico` | 16, 24, 32, 48, 64, 128, 256 px (PNG entries, 32-bit) | `native-host/build.rs`, which embeds it as the executable's icon (MSVC builds) |

After replacing the master, regenerate and rebuild:

```powershell
python -m pip install Pillow          # once
python scripts/generate-icons.py      # writes the files above; assets/icon.png is only read
python scripts/generate-icons.py --check   # fails if a generated file is out of date
cd native-host; cargo build --release
```

Each size is resized directly from the master with Lanczos resampling; the aspect ratio, colours and the opaque dark
background are kept (the ICO entries get a fully opaque alpha channel). The generated files carry no metadata. The
master was generated with ChatGPT (OpenAI image generation) and adopted by the project owner; it keeps its C2PA
content credentials, which record that origin.

## Known limitations

- Windows only (native host, dialogs and paths); Chrome 116 or later.
- Images: hotlink-protected images on another domain, `blob:` URLs whose page is closed, and images that need a
  login the browser's cookies do not cover can fail to save. Images above 50 megapixels are not copied or
  thumbnailed in the panel (export still works). Copying a 32 MiB original takes a few seconds (64 host requests).
- The Image tab keeps one element per loaded tile (thumbnails far off screen are released); very large libraries
  have not been optimised with a virtual list yet.
- Saved images cannot be deleted from the panel yet.

## License

The Memocampus source code is released under the MIT License (see `LICENSE`). Third-party material is not covered by it:

- `assets/settings.svg` and `assets/cached.svg` (and their copies in `extension/assets/`) are the Google Material Symbols “settings” and “cached” icons, licensed under the Apache License 2.0. The full license text is in `LICENSES/Apache-2.0.txt`; source and provenance are recorded in `THIRD_PARTY_NOTICES.md`.
- The `extension/` folder carries its own `LICENSE`, `LICENSES/Apache-2.0.txt` and `THIRD_PARTY_NOTICES.md` (identical copies), so it can be distributed on its own.

- Third-party Rust crates keep their own licenses (MIT, Apache-2.0, Unicode-3.0 and similar permissive licenses; SQLite, bundled through `libsqlite3-sys`, is in the public domain). Only `Cargo.lock` is committed here; anyone distributing built binaries must include the corresponding notices.

- The Memocampus icon was generated using OpenAI's image generation service. The icon assets are distributed under the project's MIT License.
