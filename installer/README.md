# Memocampus Windows installer (Issue #67, Slice 3B-1)

Installs the three Memocampus executables for the **current Windows user** and adds one Start menu
entry. Nothing else.

## Status of this slice

`memocampus.iss` has **not been compiled** yet: no Inno Setup compiler was available on the machine
it was written on. What was checked is described under "Verification". A compile and an install in
an isolated Windows user or VM are still to be done.

## What the installer does

| | |
|---|---|
| Output | `installer\out\Memocampus-Setup-x64.exe` |
| Install folder | `%LOCALAPPDATA%\Programs\Memocampus` (fixed: there is no folder page) |
| Rights | current user only, no administrator prompt, nothing under HKLM or Program Files |
| Windows | x64 (`ArchitecturesAllowed=x64compatible`) |
| Files | `memocampus-native-host.exe`, `memocampus-mcp.exe`, `memocampus-tray.exe` |
| Start menu | `Memocampus\Memocampus` → `memocampus-tray.exe`, **no arguments**, no environment variable |
| Uninstall | Inno Setup's standard uninstaller (Settings > Apps) removes the three files and the Start menu entry |
| AppId | `{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A}` (never change it) |

The only registry entry is Inno Setup's own uninstall record, under `HKCU` because the install is per user.

## Not in this slice (later slices)

- Native Messaging Host registration and manifest (3B-2). The Host name `com.memocampus.vault` is not touched.
- The Run key (logon start) and closing a running tray on update/uninstall (3B-3). The tray manages its own Run entry.
- Starting the tray after setup, and the Chrome Web Store hint (3B-4), and the CI build of the setup (3B-4).
- Code signing.
- The Chrome extension. The installer never installs it.

The installer never creates a vault, a policy, a bearer token or any file under `%LOCALAPPDATA%\Memocampus`,
never approves MCP access, and starts neither the tray nor the server.

## User data is kept

The installer has no `[Registry]`, `[Run]`, `[Dirs]`, `[InstallDelete]`, `[UninstallDelete]` or `[Code]`
section and never names `%LOCALAPPDATA%\Memocampus`. Uninstalling leaves, untouched: that folder (settings,
`config.json`, `policy.json`, `tray-settings.json`, `mcp-http-token`, logs), every vault and backup, the
encryption keys and the Chrome profile. The tray's own Run value, if the user's tray created one, is also
left (removing it conditionally is slice 3B-3).

## Tools needed

- Rust stable MSVC, and a native Perl for the Native Host's OpenSSL build (see `.github/workflows/ci.yml`).
- PowerShell 5.1 or 7.
- **Inno Setup 6.4 or later** (6.3 is the first with `x64compatible`). Official site: <https://jrsoftware.org/isinfo.php>.
  The exact version to pin is not decided; confirm the current release there. Installing it changes the
  machine, so it is not done by these scripts. Per-user installation of the compiler is enough.

## Build

From the repository root:

```powershell
# 1. the three release executables (each crate has its own workspace)
cargo build --release --locked --manifest-path native-host\Cargo.toml
cargo build --release --locked --manifest-path mcp-server\Cargo.toml
cargo build --release --locked --manifest-path tray\Cargo.toml

# 2. exactly those three files into installer\stage (stops if one is missing; no placeholder)
powershell -NoProfile -ExecutionPolicy Bypass -File installer\stage.ps1

# 3. static checks of the script and the stage folder (no compiler needed)
powershell -NoProfile -ExecutionPolicy Bypass -File installer\verify-static.ps1

# 4. compile (the version is required)
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.0 installer\memocampus.iss
# (or "C:\Program Files (x86)\Inno Setup 6\ISCC.exe", whichever the installation used)
```

`installer\stage\` and `installer\out\` are git-ignored. `target\release` is never packaged as a folder:
it may hold an old database, keys, settings or logs.

## Verification

Static (`verify-static.ps1`, no install): fixed AppId; per-user and no administrator; fixed folder; x64 only;
exactly three `Source:` lines, no wildcard; one Start menu icon to the tray without parameters; no
registry, run, delete or code section; no reference to the data folder; the stage folder holds exactly
three files.

Isolated install test (**not** on a development machine's real profile; use a separate Windows user or a VM):

1. Sign in as a standard user with no `MEMOCAMPUS_APP_DIR` set. Record `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
   `HKCU\Software\Google\Chrome\NativeMessagingHosts` and the list of files under `%LOCALAPPDATA%\Memocampus`.
2. Run `Memocampus-Setup-x64.exe`. No UAC prompt must appear; no folder page is shown.
3. Check the three files in `%LOCALAPPDATA%\Programs\Memocampus`, the Start menu entry, its target and empty arguments.
4. The tray must not be running after setup. Start it from the Start menu; confirm the icon.
5. Compare the registry and the data folder with step 1: the only differences allowed are Inno's uninstall
   record, and what the tray itself did when the user started it.
6. Uninstall from Settings > Apps. The three files and the Start menu entry go; the data folder stays.
7. Running programs: with the tray running, start the uninstall and note what Setup does (the script sets
   `CloseApplications=no`, so the standard "file in use" prompt is expected, and nothing is ended by force).
   This behaviour is **expected from Inno's documentation but not yet observed**.

A user who has set `MEMOCAMPUS_APP_DIR` themselves keeps it: the tray started from the Start menu then runs
in its read-only verification mode (no logon registration) and uses that folder. The installer neither sets
nor reads that variable.
