# Memocampus Windows installer (Issue #67, Slices 3B-1 and 3B-2)

Installs the three Memocampus executables for the **current Windows user**, adds one Start menu entry
(3B-1) and registers the product Native Messaging host for Chrome (3B-2).

## Status

Compiled with Inno Setup 7.1.1. The registration code is exercised by a self-test (below) against a private
registry key. **The installer itself has not been run** (no isolated Windows user or VM was available), and
no Chrome Web Store extension id exists yet, so no installer built here is a release build.

## What the installer does

| | |
|---|---|
| Output | `installer\out\Memocampus-Setup-x64.exe` |
| Install folder | `%LOCALAPPDATA%\Programs\Memocampus` (fixed: there is no folder page) |
| Rights | current user only, no administrator prompt, nothing under HKLM or Program Files |
| Windows | x64 (`ArchitecturesAllowed=x64compatible`) |
| Files | `memocampus-native-host.exe`, `memocampus-mcp.exe`, `memocampus-tray.exe` |
| Generated file | `{app}\io.github.memocampus.vault.json` (the Native Messaging manifest) |
| Start menu | `Memocampus\Memocampus` → `memocampus-tray.exe`, **no arguments**, no environment variable |
| Registry | one value: the default value of `HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.memocampus.vault` (the manifest path), plus Inno's own per-user uninstall record |
| Uninstall | Inno's standard uninstaller removes the three files, the manifest and the Start menu entry |
| AppId | `{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A}` (never change it) |

The installer never installs or forces the Chrome extension, never touches Chrome Enterprise policy or HKLM,
never creates a vault or a policy, never approves MCP access, never creates a bearer token, and starts neither
the tray nor the server. It does not touch `%LOCALAPPDATA%\Memocampus`.

## Host names (three separate registrations)

| Use | Host name | Registered by | Registry key under `HKCU\Software\Google\Chrome\NativeMessagingHosts` |
|---|---|---|---|
| Product (Web Store extension) | `io.github.memocampus.vault` | this installer | `io.github.memocampus.vault` |
| Development, new | `io.github.memocampus.vault.dev` | `native-host\install-native-host.ps1 -HostName io.github.memocampus.vault.dev` | `io.github.memocampus.vault.dev` |
| Development, legacy | `com.memocampus.vault` | `native-host\install-native-host.ps1` (default) | `com.memocampus.vault` |

The names are Chrome identifiers, not DNS names. Each registration is its own key, so none can overwrite
another. The installer never reads, changes or removes the development or legacy keys. The development scripts
refuse the product name.

### Which host the extension talks to

`extension\host-config.js` is the only place that names a host. Its one constant `HOST_PROFILE` is `product`,
`dev` or `legacyDev`; a missing or unknown value is an error at load time and **there is no fallback** from one
host to another. Every `connectNative` / `sendNativeMessage` call uses `HOST_NAME` from that file (a test in
`tests/extension/host-config.test.js` enforces both).

The committed value is `legacyDev`, so an unpacked development extension keeps connecting to the registration it
already has. To switch (the extension must be reloaded in `chrome://extensions`; its id does not change):

```powershell
node scripts\set-host-profile.mjs dev        # io.github.memocampus.vault.dev (register it with -HostName ...dev)
node scripts\set-host-profile.mjs legacyDev  # back to com.memocampus.vault
node scripts\set-host-profile.mjs --check legacyDev   # exit code 1 unless the file says so
```

**Web Store package:** build it from a copy of `extension\` and run
`node scripts\set-host-profile.mjs product --file <copy>\host-config.js`, then
`node scripts\set-host-profile.mjs --check product --file <copy>\host-config.js` before zipping. A package built
without that step would use `com.memocampus.vault` and fail to connect (it fails closed; it never reaches another host).

### Development scripts

```powershell
# register / remove the NEW development host (an extension id from chrome://extensions)
powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\install-native-host.ps1 -ExtensionId <id> -HostName io.github.memocampus.vault.dev
powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\uninstall-native-host.ps1 -HostName io.github.memocampus.vault.dev
# without -HostName both scripts act on com.memocampus.vault exactly as before
```

The manifest is `native-host\<host name>.json` (git-ignored). The uninstall script now removes a registration only
when its value names this repository's manifest for that host name (or the `-ManifestPath` you pass); anything
else, such as the installer's product registration, is reported and left alone. `-RegistryBase` exists only so tests
can use a private key.

## Chrome extension id (build time)

The Web Store extension id is **not yet known**. It is a required build input and is never defaulted:

```powershell
ISCC.exe /DAppVersion=0.1.0 /DChromeExtensionId=<32 letters a-p> installer\memocampus.iss
```

The compile stops when the id is missing, empty, not 32 characters, or has any character outside lower case `a`-`p`.
The same check runs again when Setup starts. The id is the manifest's only `allowed_origins` entry
(`chrome-extension://<id>/`); no development id, no wildcard. If the id changes in a later release, rebuild the
installer with the new id: every install or update regenerates the manifest.

## Registration rules and failure behaviour

Before anything is installed (`PrepareToInstall`) and again right before writing:

| State of the product key | Result |
|---|---|
| absent | create it |
| default value = this installation's manifest path (case and slash direction ignored) | update it (re-install, update, new extension id) |
| default value = any other path | **Setup stops before installing anything**, naming that path |
| key present with no default value, or not a string | **Setup stops**: it cannot tell whose it is |

The choice made here: a conflict **cancels the installation** rather than installing without a working Chrome
connection and showing a success page. There is no "continue anyway". The user removes the other registration (or
uninstalls the other Memocampus) and runs Setup again.

During the write: the previous manifest is copied aside, the new one is written to `.tmp` as UTF-8 **without BOM**,
read back and compared, put in place and read back again; then the registry value is written and read back. Any
failure restores the previous manifest and the exact previous registry value (or removes what Setup created). The
restore re-checks that the registry value is still this installation's before touching it: a value another program
wrote in the meantime is left alone. If the restore itself fails, the message contains `ROLLBACK_FAILED:` and the
reason; nothing is reported as success.

If the registration fails after the files were copied, `CurStepChanged(ssPostInstall)` raises an error. **What Setup
then does with the already copied files (it is expected to roll them back or stop with the message) has not been
observed**; it needs the isolated-VM test below.

At uninstall, the manifest is deleted but the **registry value is left** (removing it, only when it is still this
installation's own, is slice 3B-3; `NmhOwnsRegistration` is ready for it). Until then an uninstalled setup leaves a
registration pointing at a missing manifest, which Chrome ignores.

## Build

From the repository root (the three crates have their own workspaces):

```powershell
cargo build --release --locked --manifest-path native-host\Cargo.toml
cargo build --release --locked --manifest-path mcp-server\Cargo.toml
cargo build --release --locked --manifest-path tray\Cargo.toml
powershell -NoProfile -ExecutionPolicy Bypass -File installer\stage.ps1          # exactly the three executables
powershell -NoProfile -ExecutionPolicy Bypass -File installer\verify-static.ps1 -Compile -Negative
& "C:\Program Files\Inno Setup 7\ISCC.exe" /DAppVersion=0.1.0 /DChromeExtensionId=<id> installer\memocampus.iss
```

`installer\stage\`, `installer\out\` and `installer\out-test\` are git-ignored. `target\release` is never
packaged as a folder (it may hold an old database, keys, settings or logs). Needs Inno Setup 7.1.1 (`ISCC.exe`).

## Checks that exist

| Command | What it checks |
|---|---|
| `installer\verify-static.ps1` | the sources: AppId, per-user, fixed folder, x64, exactly three files, one icon, no Run key, only HKCU and only the one key, no other host, no policy, no forced extension, the id contract, one origin, escaping, ownership checks, no data-folder reference, agreement with `extension\host-config.js` and the development scripts |
| `... -Negative` | 29 deliberate wrong changes to a scratch copy (wrong host name, development id, second origin, wildcard, defaulted id, HKLM write, other host, subkey delete, unconditional overwrite or delete, removed rollback check, hidden rollback failure, BOM, JSON escape, Run key, policy, force-install, test hook, `[Registry]`, `[Run]`, uninstall removal, data-file reference, test mode using Chrome's key, fake id...) each of which must be reported |
| `... -Compile` | the real compiler: missing, empty, 31, 33 characters, upper case, `q`, digit, wildcard must fail; a well-formed id must compile (product and test mode); output goes to a scratch folder, never to `installer\out` |
| `installer\run-selftest.ps1` | compiles `nmh-selftest.iss` (the real `nmh.pas`) and runs it against a private key `HKCU\Software\MemocampusInstallerTest\<random>` and a scratch folder: id and path validation, JSON text and escaping, UTF-8 without BOM, a path with Japanese characters parsed back by `ConvertFrom-Json`, fresh registration, update, conflicts, a key without a default value, a failure injected at each of four steps on a fresh and on an existing registration, a failing rollback reported, an unrelated registration unchanged, and rollback refusing to remove a foreign value. It is not the installer: it installs nothing, makes no uninstall record, never touches Chrome's keys, and removes its key and folder. |
| `npm test` | includes `tests/extension/host-config.test.js` |

## Isolated installer test (not done yet)

Use a separate Windows user or a VM, never a development profile. Build the **test build**, which is a different
program from the product installer: another AppId, folder `%LOCALAPPDATA%\Programs\MemocampusInstallerTest`,
output `installer\out-test\Memocampus-Setup-x64-INSTALLER-TEST.exe`, and the private registry key
`HKCU\Software\MemocampusInstallerTest\NativeMessagingHosts\io.github.memocampus.vault` instead of Chrome's. Chrome
cannot see that registration; the test verifies the installer's behaviour, not a Chrome connection.

```powershell
ISCC.exe /DAppVersion=0.0.0 /DChromeExtensionId=<a test id> /DInstallerTestMode installer\memocampus.iss
```

The id you pass is a test value and must never be used for a release. The product compile has no test code and
never touches the private key (`verify-static.ps1` checks the key of each mode).

To do there: install; check the three files, the manifest, the private key, the Start menu entry; install again
(update, with another id); pre-create the key with another path and confirm Setup stops before installing;
make the install folder read-only and watch the failure; uninstall and confirm the manifest and files go, the key
stays (3B-3), and `%LOCALAPPDATA%\Memocampus` is unchanged.

## Not in these slices (later)

- Removing the registration at uninstall, the Run key, starting or closing the tray (3B-3); the setup page that offers to start the tray and the Web Store link, and CI building (3B-4).
- Code signing.
- Changing the committed `HOST_PROFILE` from `legacyDev` (a decision after the development setups have moved).

## User data is kept

The installer never names `%LOCALAPPDATA%\Memocampus`, a vault, a backup, a key, the policy, the settings, the
bearer token or the Chrome profile. Uninstalling leaves all of them.

A user who has set `MEMOCAMPUS_APP_DIR` themselves keeps it: the tray started from the Start menu then runs in its
read-only verification mode (no logon registration) and uses that folder. The installer neither sets nor reads it.
