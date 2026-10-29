# Memocampus

Local-first web memory for Chrome on Windows.

## Slice 1

Record completed top-level page navigations as URL, title, and timestamp. The MV3 service worker queues events in `chrome.storage.local`, sends them to the Rust native host, and removes a queue item only after a successful acknowledgement. The native host stores events in SQLite with an idempotency key.

## Side panel search

Clicking the Memocampus toolbar action opens a side panel with three tabs: **URL** (search), **Image** (placeholder for a future image library) and **⚙** (vault settings). The URL tab searches the local SQLite vault (`visits` and `tab_snapshots`, aggregated per URL) by URL or title. The panel also has the “全タブを記録” button (the former toolbar popup has been removed). See `docs/issue-002-side-panel-search.md` for the `search_pages` protocol.

## Vault

The vault (SQLite file) is independent of the host executable. Its path is stored in `%LOCALAPPDATA%\Memocampus\config.json`; the default vault is `%LOCALAPPDATA%\Memocampus\memocampus.sqlite3`. On first use, open the side panel's **Vault** section and create a vault explicitly (or open an existing one). Until then visits stay queued in the extension. Manual backups use the SQLite Backup API. See `docs/issue-003-vault-location.md`.

The database previously stored next to the executable (`native-host/target/release/memocampus.sqlite3`) is no longer used and is left untouched.

This project never closes tabs automatically.

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

- After changing the host, rebuild with `cargo build --release`. The registered manifest points at `native-host\target\release\memocampus-native-host.exe`, so `cargo clean` removes the host until you rebuild; your vault lives outside `target/` and is not affected.
- To unregister: `powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\uninstall-native-host.ps1`, then remove the extension in `chrome://extensions`. Your vault and settings in `%LOCALAPPDATA%\Memocampus` are kept; delete that folder yourself if you no longer need the data.

## Tests

- Native host: `cargo test` in `native-host/` (unit tests plus a stdio protocol test against the built binary).
- Extension modules: `npm test` at the repository root (Node's built-in test runner, no dependencies).

## Layout

- `extension/` — Chrome Manifest V3 service worker and search side panel
- `native-host/` — Rust Native Messaging host and SQLite persistence
- `docs/` — issue and slice acceptance criteria

## License

The Memocampus source code is released under the MIT License (see `LICENSE`). Third-party material is not covered by it:

- `assets/settings.svg` (and its copy `extension/assets/settings.svg`) is the Google Material Symbols “settings” icon, licensed under the Apache License 2.0. The full license text is in `LICENSES/Apache-2.0.txt`; source and provenance are recorded in `THIRD_PARTY_NOTICES.md`.
- The `extension/` folder carries its own `LICENSE`, `LICENSES/Apache-2.0.txt` and `THIRD_PARTY_NOTICES.md` (identical copies), so it can be distributed on its own.

- Third-party Rust crates keep their own licenses (MIT, Apache-2.0, Unicode-3.0 and similar permissive licenses; SQLite, bundled through `libsqlite3-sys`, is in the public domain). Only `Cargo.lock` is committed here; anyone distributing built binaries must include the corresponding notices.
- The icon candidate `62c5e2a1-b459-4579-a4ab-bdba9be3c89e.jpeg` was generated with Google Gemini and carries C2PA provenance metadata. It is not covered by the MIT License statement above until its terms of use have been confirmed.
