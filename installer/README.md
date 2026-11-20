# Memocampus Windows installer (Issue #67, Slices 3B-1 to 3B-3)

Installs the three Memocampus executables for the **current Windows user**, adds one Start menu entry
(3B-1), registers the product Native Messaging host for Chrome (3B-2), and manages the update and uninstall
lifecycle: a running product tray is asked to quit first, and only this installation's own registrations are
removed (3B-3).

## Status

Compiled with Inno Setup 7.1.1. The registration and lifecycle code is exercised by self-tests (below) against
private registry keys and, for the tray routine, against real scratch tray processes. **The installer itself has not
been run** (no isolated Windows user or VM was available), and no Chrome Web Store extension id exists yet, so no
installer built here is a release build.

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
| Update / uninstall | a running product tray is asked to quit first (see "Lifecycle"); the uninstaller then removes only this installation's own registration, Run value and manifest, and Inno removes the files and the Start menu entry |
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

Since 3B-3 the registration is made in `CurStepChanged(ssInstall)`, **before** the files are copied. Inno's
documentation (Pascal Scripting: Abort) says `Abort` ends Setup only from `InitializeSetup`, `InitializeWizard`,
`CurStepChanged(ssInstall)`, `InitializeUninstall` and the uninstall steps `usAppMutexCheck` / `usUninstall`, not from
`ssPostInstall` (where 3B-2 registered, so a failure could not stop Setup and the copied files stayed: FINDING-67-07).
Now a failed registration shows the reason and aborts with nothing installed; and if the file copy then fails or is
cancelled, `DeinitializeSetup` (called whenever Setup ends) puts the registration and the manifest back as they were.
Both paths are written from the documented contract and tested in their parts (registration, rollback); the Inno
engine behaviour around them has **not been observed** (needs the isolated-VM test below). If a failure leaves a
state you doubt, run Setup again: every step is idempotent.

## Lifecycle: update and uninstall (3B-3)

### Inno events, in order, and what each one does

| When | Event | Action | On failure |
|---|---|---|---|
| install / update | `PrepareToInstall` | the 3B-2 registration check; then `Quiesce`: ask verified product trays to quit and wait (45 s at most); then check that the installed executables are not held by anything else | returns a message: Setup stops on the "Preparing to Install" page, **nothing changed** (exit code 7) |
| install / update | `CurStepChanged(ssInstall)` | create `{app}` if needed, snapshot, write the manifest and register it | message + `Abort`: nothing installed |
| install / update | Inno copies the files, creates the Start menu entry | Inno's own | Inno's Abort/Retry/Ignore; then `DeinitializeSetup` undoes our registration |
| install / update | `CurStepChanged(ssPostInstall)` | marks the install as complete | – |
| install / update | `DeinitializeSetup` | only if registered and not complete: restore the previous registration and manifest | `ROLLBACK_FAILED` message; run Setup again |
| uninstall | `InitializeUninstall` | `Quiesce` as above | returns False: **nothing changed** |
| uninstall | `CurUninstallStepChanged(usUninstall)` | `LcUninstallCleanup`: 1 our native messaging registration, 2 our Run value, 3 our manifest | message + `Abort` before any file is removed; run the uninstall again |
| uninstall | Inno | removes the files, the Start menu entry and `{app}` if empty | Inno's |

`CurUninstallStepChanged` alone cannot stop an uninstall before it starts; that is why the process check is in
`InitializeUninstall` (whose False aborts) and the registry cleanup is in `usUninstall` (where `Abort` ends the
uninstall, documented as above). Whether `usUninstall` comes before any file is removed is **assumed from Inno's
design, not observed**; the cleanup is ordered so that a failure there leaves everything retryable either way.

### How the tray is asked to quit, and how it is identified

`lifecycle.pas` (`LcCloseTrays`):

1. `EnumWindows` lists top-level windows of the class `MemocampusTrayHost` (hidden, created by the tray: `tray/src/ui.rs`).
2. **The class name alone is never enough.** For each window the owning process is opened by pid with
   `PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE`; its image path (`QueryFullProcessImageNameW` on that handle)
   must equal `{app}\memocampus-tray.exe` (case and slash direction ignored); it must be in the installer's session;
   and the window must still belong to that pid after the open (window ended or handle value reused in between).
   The open handle pins the process, so the pid cannot be reused while it is held, and it is the handle the wait uses.
3. Only then `PostMessageW(WM_CLOSE)`, which the tray handles as the menu's Quit: its operations thread finishes the
   write it accepted, the server it owns is stopped (Job Object), exit code 0 (7 = quit incomplete). It is **never**
   `WM_ENDSESSION`, `TerminateProcess`, `taskkill` or a kill by name. A post is non-blocking, so a frozen tray cannot hang Setup.
4. The wait is 45 s at most, polled in 100 ms slices (`LcTrayWaitMs`): the tray's own quit waits up to 30 s for its
   operations thread (`CONTROLLER_WAIT`, `tray/src/quit.rs`) and then stops the server; 15 s remain for that and for the
   process to end. The installer window does not repaint during the wait (Pascal Script has no message pump).
5. A tray still running afterwards is reported and Setup stops: it is **not** killed.
6. Other trays (a development tray from `target\release`, a copy in another folder, another user's) are only listed as
   "left alone". An exit code of 7 is logged ("ended, but its quit was incomplete") and does not block: the process is
   gone, and refusing would not repair the data.

Limits: the user is identified by the profile-specific image path and the session, not by a token SID comparison.

### Native Host and MCP Server

- The tray's server child is stopped by the tray's own quit (and, if the tray died, by the Job Object's
  `KILL_ON_JOB_CLOSE`, `tray/src/winproc.rs`).
- **Nothing else is ever terminated.** An AI client's own `memocampus-mcp.exe` and the Native Host that Chrome starts
  (it owns its lifetime) are not the installer's. If they hold an installed file, `LcLockedFiles` finds it (opening the
  existing file for write with no sharing; nothing is created or truncated, ~5 s of retries after a tray quit) and Setup
  stops with: close the AI client / Chrome windows using Memocampus, then run Setup again. Inno's own in-use handling is
  off (`CloseApplications=no`).

### What uninstall removes, and what it keeps

| Item | Removed when | Otherwise |
|---|---|---|
| `HKCU\...\NativeMessagingHosts\io.github.memocampus.vault` default value | it is a string equal to this installation's manifest path, checked again right before the delete | kept, reported "KEPT" |
| that key | only when it is then empty (`RegDeleteKeyIfEmpty`) | kept with its other values/subkeys; **no recursive delete exists** |
| `{app}\io.github.memocampus.vault.json` | its bytes equal what this installer writes (rebuilt from the same constants) **and** the registration no longer points at it | kept: a file in `{app}` is not ours just by location. Inno's file log knows only `[Files]`; this file is ours by content, not by Inno's record |
| `HKCU\...\Run` value `MemocampusTray` (REG_SZ, `"<path>"`, written by the tray) | a string that, with one pair of quotes removed, names exactly `{app}\memocampus-tray.exe`, checked twice | kept: another path (a development exe), arguments, odd quoting, another type, another value name |
| `StartupApproved\Run` | **never read or written** | stays |
| `com.memocampus.vault`, `...vault.dev`, other hosts, HKLM, Chrome policy | never touched | – |
| data folder, vault, token, keys, settings, logs, backups, Chrome profile, the extension | never touched | – |

If another program replaced the registration, the report says so ("KEPT (it belongs to another program: ...)"), also
logged to the Setup log (`/LOG`). The registry has no compare-and-delete, so a change by another program in the
instant between the last check and the delete is not excluded; the window is a few instructions wide.

`StartupApproved`: the tray never writes there; Windows keeps the user's on/off record for `MemocampusTray` after the
Run value is gone. After a reinstall, if the user later turns logon start on in the tray, Windows still applies its
earlier choice (shown by the tray as "disabled in Windows"). That preserves the user's decision; nothing here resets it.

### Update keeps the user's state, and does not restart anything

Update changes only the three executables, the manifest and the registration. It never writes `tray-settings.json`,
the Run value or `StartupApproved`, and never starts the tray or the server: the tray that was closed stays closed until
the user starts it (Start menu) or the next logon starts it through the user's own Run value. Starting after setup is
slice 3B-4 (explicit user choice on the last page).

### New install versus update

There is no "is it an update" switch. The close routine acts on a running process whose image path is exactly
`{app}\memocampus-tray.exe`: on a first install there is none (not an error); on an update there is; a development tray
or a copy elsewhere is never a candidate; an executable placed by hand into `{app}` and running is, correctly, ours.

### Failure states and recovery

| Case | State afterwards | Retry |
|---|---|---|
| install: a tray won't quit / a file is held / foreign registration | nothing changed, reason shown | fix and run Setup again |
| install: registration fails | nothing installed (Abort in `ssInstall`); a failed undo says `ROLLBACK_FAILED` | run Setup again |
| install: file copy fails after registration | `DeinitializeSetup` restores the previous registration and manifest | run Setup again |
| update: new files partly copied | **Inno does not roll back files.** Executables may be a mix of old and new; the registration is restored to the previous one | run Setup again (idempotent); the data is outside `{app}` and unaffected |
| update: old product unusable | as above; reinstalling the same or the previous Setup.exe repairs it | |
| uninstall: tray / held files | nothing changed | quit it and run again |
| uninstall: registration or Run value cannot be removed | `Abort` before files are removed; what was already removed stays removed | run the uninstall again: each step skips what is gone |
| uninstall: manifest cannot be deleted | uninstall continues (nothing points at it); a stray file stays in `{app}` | delete it by hand |
| uninstall: EXE removal fails / Windows restarts midway | Inno's own behaviour (not changed here); registry entries are already removed | run the uninstaller again, or delete the folder |
| uninstall run twice | the second run finds nothing of ours and removes only what is left | |

Not provided: a transaction across registry and files, and any rollback the Inno engine does not offer. Not observed
without a VM: the Inno behaviours named above.

### Reinstall after uninstall, and logon start

`tray-settings.json` stays (data). The tray registers logon start itself **only when that file does not exist**
(`Controller::first_run`, `tray/src/controller.rs`). So after uninstall + reinstall the file exists, the tray does not
register again, and logon start stays off until the user turns it on in the tray; it is not silently revived (and the
tray's menu shows the registry's truth, not the old setting). Whether a reinstall should offer to re-enable it is a
product question for 3B-4 or the tray, not decided here.

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
| `installer\verify-static.ps1` | the sources `memocampus.iss`, `nmh.pas`, `lifecycle.pas` (74 rules): AppId, per-user, fixed folder, x64, exactly three files, one icon, the Run key path appears once and is only read or deleted by name after an ownership check, no `StartupApproved`, only HKCU and the two keys, no HKLM, no other host, no policy, no forced extension, no key deletion with subkeys, the id contract, one origin, escaping, ownership checks, WM_CLOSE only (never ENDSESSION/kill/taskkill), owner verification before any message, every wait finite, cleanup order, the registration in `ssInstall`, rollback in `DeinitializeSetup`, nothing started after setup, no data-folder reference, agreement with `extension\host-config.js` and the development scripts. **Limits: it is regular expressions over the text. It cannot prove behaviour, and a determined rewrite that keeps the matched text can pass; the logic and process tests below exist for that** |
| `... -Negative` | 54 deliberate wrong changes to a scratch copy, each of which must be reported: the 31 of 3B-2 (one of them, "unregistering at uninstall", now reads "uninstall cleanup call removed" because unregistering is the intended behaviour) plus 23 for 3B-3: Run value deleted by name only, foreign Run value accepted, native messaging deleted without ownership, recursive delete, StartupApproved touched, `TerminateProcess` imported, kill by name, unbounded and INFINITE waits, owner path / session check removed, WM_ENDSESSION as the request, another folder's tray asked, continuing after a tray did not quit, locked files ignored, uninstall without the tray wait, cleanup order swapped, manifest deleted without content check, registration back in `ssPostInstall`, no rollback of an incomplete setup, tray started after setup, ... |
| `... -Compile` | the real compiler: missing, empty, 31, 33 characters, upper case, `q`, digit, wildcard must fail; a well-formed id must compile (product and test mode); output goes to a scratch folder, never to `installer\out` |
| `installer\run-selftest.ps1` | compiles `nmh-selftest.iss` (the real `nmh.pas`) and runs it against a private key `HKCU\Software\MemocampusInstallerTest\<random>` and a scratch folder: id and path validation, JSON text and escaping, UTF-8 without BOM, a path with Japanese characters parsed back by `ConvertFrom-Json`, fresh registration, update, conflicts, a key without a default value, a failure injected at each of four steps on a fresh and on an existing registration, a failing rollback reported, an unrelated registration unchanged, and rollback refusing to remove a foreign value. It is not the installer: it installs nothing, makes no uninstall record, never touches Chrome's keys, and removes its key and folder. |
| `installer\run-lifecycle-test.ps1` | compiles `lifecycle-selftest.iss` (the real `nmh.pas` and `lifecycle.pas`). **Registry part (76 checks, private keys only):** removal of a registration (own, own with other values or subkeys, foreign, no default value, non-string default, spelling), manifest removal (own bytes, foreign bytes, another id), Run value ownership (own quoted/unquoted/capitals/trailing blanks; development exe, same name in another folder, arguments, odd quoting, another program, empty, REG_DWORD, another value name; neighbours untouched), the whole cleanup, a failure in each of its two steps and the retry, running it twice. **Tray part, against real `memocampus-tray.exe` copies** started from scratch folders in the tray's isolated mode: T0/T1 no tray of ours (others "left alone"), T2 quit through the tray's own path with exit code 0 and `quit_done ... controller=completed`, T3 same-named tray at another path left running, T4 two candidates at the same path, T5 a suspended (unresponsive) tray: reported after a bounded wait, **not terminated**, then quits normally once resumed, T6 a file held open by another process is reported and unchanged. The trays already running on the machine are recorded first and must still run at the end |
| mutations of the above | the registry part was run against 10 deliberate breaks (unregister ownership, recursive key delete, Run path check, string check, manifest content check, cleanup order and error handling, wrong value name): 8 are caught; 2 survive and are equivalent (the quote check is redundant with the path comparison, and the "still registered" guard in the cleanup is unreachable without a race). The process mutations were **not** run: a broken owner check would act on a real tray |
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

The test build also uses the private Run key `HKCU\Software\MemocampusInstallerTest\Run`, and its tray path is its
own install folder, so it can only ever ask a tray that runs from that folder. Isolation by AppId and folder is not
enough on its own: the check that it cannot touch a running tray of another folder is the owner verification above,
proven with a tray at another path in `run-lifecycle-test.ps1` (T3) and with the machine's own running tray (T0, and
"still running" at the end).

To do there: install; check the three files, the manifest, the private key, the Start menu entry; start the tray from the
Start menu; install again (update): the tray quits, the files are replaced, the tray is not restarted, `tray-settings.json`
unchanged; pre-create the key with another path and confirm Setup stops before installing; hold `memocampus-mcp.exe` open
from another process and confirm Setup stops with the guidance; make the install folder read-only and watch the failure;
uninstall and confirm: the manifest, the registration and the files go, a foreign value in the Run key and a foreign
registration stay, `StartupApproved` and `%LOCALAPPDATA%\Memocampus` are unchanged; and observe the three Inno behaviours
named above (Abort in `ssInstall`, `DeinitializeSetup` after a failed copy, `usUninstall` before file removal).

## Not in these slices (later)

- The setup page that offers to start the tray (an explicit choice), the Web Store link, and CI building (3B-4). Restarting a tray that an update closed is part of that choice: nothing restarts it unasked.
- Code signing.
- Changing the committed `HOST_PROFILE` from `legacyDev` (a decision after the development setups have moved).

## User data is kept

The installer never names `%LOCALAPPDATA%\Memocampus`, a vault, a backup, a key, the policy, the settings, the
bearer token or the Chrome profile. Uninstalling leaves all of them.

A user who has set `MEMOCAMPUS_APP_DIR` themselves keeps it: the tray started from the Start menu then runs in its
read-only verification mode (no logon registration) and uses that folder. The installer neither sets nor reads it.
