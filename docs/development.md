# Memocampus — developer guide

[日本語の利用者向けREADME](../README.md) | [English user README](../README.en.md)

This page is for people who build Memocampus from source, run its tests or work on the repository. Users who
install the released product do not need it. Design notes and acceptance records for each issue are in
[`docs/`](.); the Windows installer and its release pipeline are described in
[`installer/README.md`](../installer/README.md).

## Layout

- `extension/` — Chrome Manifest V3 extension: service worker (visits, image and page saving) and the side panel
  (URL, Image and Page tabs, settings), the full-page History (`history.html`) and the Privacy settings page
  (`options.html`). Display text is in `extension/_locales/{en,ja}/messages.json` (Issue #77).
- `native-host/` — Rust Native Messaging host and SQLite/SQLCipher persistence (vault schema 10).
- `visible-store/` — the only read path from a vault to MCP, with the privacy gate.
- `mcp-server/` — `memocampus-mcp`, the read-only MCP server over VisibleStore (stdio, or Streamable HTTP on loopback).
- `page-extract/` — text and metadata extraction from Chrome MHTML (Issue #71). A library that no product code path calls
  yet: the extension and the host do not offer full-text search of saved pages.
- `tray/` — `memocampus-tray`, the Windows tray that runs and manages the MCP server over Streamable HTTP
  ([Slice 1](issue-067-tray-slice1.md), [Slice 2](issue-067-tray-slice2.md), [languages](issue-078-tray-i18n.md)).
- `installer/` — the Inno Setup installer for the three executables (see its README).
- `scripts/` — packaging, host profile switching, icon generation and verification tools.
- `docs/` — issue and slice acceptance criteria and design notes.
- `assets/icon.png` — master of the app icon.

## Building and loading from source (Windows, developer mode)

Requirements: Windows 10/11, Google Chrome 116 or later, [Rust](https://rustup.rs/) (stable, MSVC toolchain),
and a **native Windows Perl** (Strawberry Perl, or MSYS2 `mingw-w64-x86_64-perl`; not the Perl of Git for Windows): the
host and the MCP server embed SQLCipher, whose OpenSSL is built from source (Issue #51; the first build takes about 12
minutes more). If `perl` on `PATH` is not a native one, set `PERL` to it, e.g. `$env:PERL = "C:\Strawberry\perl\bin\perl.exe"`.
Node.js is only needed to run the extension tests (CI uses Node.js 24). No administrator rights are required.

Build each crate **from its own directory**. The three crates that produce the shipped executables (`native-host`,
`mcp-server`, `tray`) each have a `.cargo/config.toml` that links the C runtime statically (`+crt-static`). The library
`visible-store` has the same file, because it ships inside the MCP server and so shares its OpenSSL / SQLCipher build
in CI; `page-extract` has none. Cargo reads that file only when it runs inside the crate directory,
so a build started from the repository root with `--manifest-path` skips it and produces executables that need
`VCRUNTIME140.dll` (the installer build refuses them: `scripts/pe-imports.mjs --check`).

1. **Build the native host**:

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
6. **Check the connection**: click the Memocampus toolbar button to open the side panel and open the settings tab (⚙).
   A status such as "Vaultが未設定です" / "No Vault is set up" means Chrome reached the native host.
   "Native Hostに接続できません…" / "Cannot connect to the Native Host…" means the registration or manifest is wrong; check
   it with `reg query "HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault"` and re-run step 4.
7. **Create your vault and start remembering** as described in the user README ("Initial setup").

### Extension ID and host names

- With *Load unpacked*, Chrome derives the ID from the folder's absolute path, so it differs per machine and changes if you move the folder. Re-run step 4 with the new ID (the old registration is updated in place).
- The Chrome Web Store item has one fixed ID. It is recorded only in `installer/release-gate.json`, the build input of
  the product installer, whose Native Messaging manifest allows exactly that ID. The extension sources contain no ID and no
  manifest `key`; `scripts/package-extension.mjs` refuses a manifest `key`.
- **Host names.** The extension's host name is chosen in one place, `extension/host-config.js` (`HOST_PROFILE`):

  | Profile | Host name | Registered by |
  |---|---|---|
  | `legacyDev` (committed value) | `com.memocampus.vault` | `native-host/install-native-host.ps1` (default) |
  | `dev` | `io.github.memocampus.vault.dev` | `install-native-host.ps1 -HostName io.github.memocampus.vault.dev` |
  | `product` | `io.github.memocampus.vault` | the Windows installer only |

  `node scripts/set-host-profile.mjs dev` switches the working copy; reload the extension afterwards and pass the same
  `-HostName` to both scripts. The Web Store package is built from a temporary copy switched to `product`
  (`node scripts/package-extension.mjs --out <dir>`); the working tree is not rewritten. See `installer/README.md`.

### Updating and uninstalling a development setup

- After updating the source, rebuild the host with `cargo build --release` and reload the extension in `chrome://extensions` (accept new permissions if Chrome asks). An extension newer than the host offers fewer functions or reports that the Native Host is outdated; rebuilding fixes it. The host registration does not need to be repeated unless the extension ID or the folder changed.
- The host is not copied anywhere: the registered manifest points at `native-host\target\release\memocampus-native-host.exe`, so `cargo clean` removes the host until you rebuild; your vault lives outside `target/` and is not affected.
- The release build replaces the executable Chrome uses; close Chrome first, or add `--target-dir` with another folder to leave it in place.
- To unregister: `powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\uninstall-native-host.ps1`, then remove the extension in `chrome://extensions`. Your vault and settings in `%LOCALAPPDATA%\Memocampus` are kept; delete that folder yourself if you no longer need the data.
- A database stored next to the executable by very early versions (`native-host/target/release/memocampus.sqlite3`) is no longer used and is left untouched.

## MCP server from source

`memocampus-mcp.exe` serves the approved active vault over stdio by default, or with `--transport streamable-http` on
`http://127.0.0.1:8765/mcp` (loopback only, bearer token required, browser origins refused). Build, client setup, tools and
limits: [issue-008-mcp-server.md](issue-008-mcp-server.md) and [mcp-streamable-http.md](mcp-streamable-http.md).
Start-up and connection events (no URLs, titles, queries or vault paths) go to
`%LOCALAPPDATA%\Memocampus\logs\mcp-startup.log`.

## Tests

- Native host: `cargo test` in `native-host/` (unit tests plus a stdio protocol test against the built binary).
- VisibleStore: `cargo test` in `visible-store/`.
- MCP server: `cargo test` in `mcp-server/` (unit tests plus MCP sessions against the built binary, synthetic vaults only).
- MHTML extraction: `cargo test` in `page-extract/`.
- Tray: `cargo test` in `tray/` (CI runs it with `--features no-desktop`, which reports the window tests as ignored).
- Extension modules, scripts and the READMEs: `npm test` at the repository root (Node's built-in test runner, no dependencies).
- Installer: the checks listed in [`installer/README.md`](../installer/README.md).

## Continuous Integration

GitHub Actions (`.github/workflows/ci.yml`) runs on pull requests targeting `main`, on pushes to `main`, and on manual runs. It only has read access to the repository contents, uses no secrets, and uploads no artifacts.

- **JavaScript** (`ubuntu-latest`, Node.js 24): `npm ci`, `npm test`. The extension tests use fake Chrome APIs and need no Windows.
- **Rust** (`windows-latest`, stable MSVC toolchain): format check, tests and Clippy for `native-host`, `visible-store`, `mcp-server`, `page-extract` and `tray` in turn, and a release build of each of them except `page-extract`. The host calls Windows APIs (DPAPI among them), embeds its icon through the MSVC linker, and has tests that only run on Windows/MSVC, so a Linux run would not verify it. SQLCipher's OpenSSL is built with the runner's Strawberry Perl (`PERL`), and the crates share one target directory so it is built once per profile.

`.github/workflows/windows-dist.yml` builds and checks the installer and the extension package (see `installer/README.md`).

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

CI does not cover Chrome itself: loading the extension, the host registration and the file dialogs are still checked by hand in Chrome (dogfooding).

### Action dependencies (`actions.lock`)

`.github/workflows/actions.lock` records the exact commit of every action the workflows use, so a moved tag cannot silently change the code CI runs. It is generated by the official [`gh-actions-lock`](https://github.com/github/gh-actions-lock) extension (`gh extension install github/gh-actions-lock`). Never edit it by hand. It says which code runs, not that the code is safe.

After adding a workflow or changing any `uses:`:

```powershell
gh actions-lock            # regenerates the lockfile; may also rewrite uses: to a full version such as v7.0.1
gh actions-lock --verify   # read-only check of coverage and provenance; exits non-zero if invalid
```

Commit the lockfile and the workflow change in the same pull request. To move to a newer version, change `uses:` and regenerate. `gh actions-lock --relock` advances refs that moved upstream, and `--accept-moved` accepts rewritten upstream history; check upstream first, and never use the latter in automation.

Review the whole diff as a dependency update: a changed `commit` means different code will run, so check it against the action's tag and release notes. A changed `owner_id` or `repo_id` means the repository's identity changed; investigate it before merging. A move back to an older commit is not detected by the tool, so only a reviewer can catch it.

The **Require lockfile** policy is not enabled, and enabling it is a separate decision. Before that, confirm that CI passes with the lockfile on GitHub, that `gh actions-lock --verify` passes for every workflow, that a verification check runs on pull requests, and that any action allowlist matches locked commits rather than tags.

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

## Licences of built binaries

Third-party Rust crates keep their own licences (MIT, Apache-2.0, Unicode-3.0 and similar permissive licences; SQLite,
bundled through `libsqlite3-sys`, is in the public domain). Only `Cargo.lock` is committed here; anyone distributing
built binaries must include the corresponding notices (see `NATIVE_THIRD_PARTY_NOTICES.md`).
