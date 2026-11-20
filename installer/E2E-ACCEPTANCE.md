# Memocampus for Windows: real-installer acceptance gate (Issue #67, Slice 3B-4)

**Status of every item below: NOT RUN.** No real installer has been executed on any machine for
Slices 3B-1 to 3B-4. The code has been compiled, unit-tested against private registry keys and real
scratch trays, and statically checked, but the Inno Setup engine's own behaviour (what runs when,
exit codes, what `Abort` does in each event) has not been observed. **This gate must be passed before a
public release of v0.8.0.** Nothing here may be written up as PASS until someone has run it and kept the
evidence.

## Where to run it

In an isolated Windows 11 x64 environment that is **not** a development machine: a fresh VM (snapshot
before each case), or a separate Windows user on a spare PC. **Never** on a machine whose real Chrome
profile, vault (`%LOCALAPPDATA%\Memocampus`), development Native Messaging registrations or MCP
clients matter. The test-mode installer (`/DInstallerTestMode`) does not make this safe on a real
profile: it still creates an install folder, a Start menu entry and an uninstall record, and the tray
it would start writes the real Run key. Use the VM.

Needed in the VM: Chrome (so the native messaging registration can be exercised), the release candidate
`Memocampus-Setup-x64.exe` **and** the previous one for the update cases, and `installer\e2e-snapshot.ps1`.

## Evidence for each case

Take `e2e-snapshot.ps1 -Take before.json` before the step and `-Take after.json` after it, then
`-Compare before.json after.json`. Keep the setup log (`Memocampus-Setup-x64.exe /LOG=setup.log`), the
screenshot of the last page, and the compare output. The snapshot covers: the three Native Messaging
registrations, the Run key (and the `MemocampusTray` command), `StartupApproved\Run`, the uninstall
key, the install folder (hashes), the data folder (hashes of every file), the Start menu entry and
the running `memocampus-*` processes. It is read-only.

The result column is filled in by whoever ran the case: `PASS (date, who, evidence file)`,
`FAIL (what)` or `NOT RUN`.

## A. New installation

| # | Check | Expected | Result |
|---|-------|----------|--------|
| A1 | Run the setup as a standard user (no administrator prompt) | Succeeds, no UAC prompt | NOT RUN |
| A2 | Files | `%LOCALAPPDATA%\Programs\Memocampus` holds the three executables, the generated `io.github.memocampus.vault.json`, and the uninstaller; nothing under Program Files | NOT RUN |
| A3 | Native Messaging | `HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.memocampus.vault` default value = the manifest path; manifest `path` = `memocampus-native-host.exe` in the install folder; `allowed_origins` = exactly `chrome-extension://<release id>/`; no dev / legacy key changed | NOT RUN |
| A4 | Last page | Says Setup installed the programs and the Chrome connection, and that the extension, the vault and remembering are separate and approved in Chrome. Offers two **unticked** boxes: start Memocampus, open the Web Store page | NOT RUN |
| A5 | Tray box ticked | Tray starts once, as the user, no arguments; a second start (Start menu) exits quietly (already running) | NOT RUN |
| A6 | Store box ticked | Default browser opens `https://chromewebstore.google.com/detail/<release id>`; nothing is installed in Chrome | NOT RUN |
| A7 | Silent install (`/VERYSILENT`) | Neither the tray nor the browser is started | NOT RUN |
| A8 | Data | `Vault`, `config.json`, `policy.json`, `tray-settings.json` (other than created by a ticked tray start), token and keys: unchanged / absent as before; MCP publication not approved | NOT RUN |
| A9 | The test installer (`...-INSTALLER-TEST.exe`) | Has no Run section and no Store link; uses its own AppId, folder and private keys; real Chrome and Run keys untouched | NOT RUN |

## B. Update (old setup installed, then the new one)

| # | Check | Expected | Result |
|---|-------|----------|--------|
| B1 | Tray running at `{app}` | Setup quits it through its own quit (exit code 0), waits, proceeds. The tray's MCP child ends with it | NOT RUN |
| B2 | A tray at another path, a dev tray | Not touched | NOT RUN |
| B3 | Chrome running with the extension (Native Host process alive) | Setup does **not** kill it. If it holds `memocampus-native-host.exe`: Setup stops before changing anything and says what to close | NOT RUN |
| B4 | An AI client's MCP server holds `memocampus-mcp.exe` | Same: not killed, Setup stops with guidance, nothing changed | NOT RUN |
| B5 | Tray that does not quit (suspend it) | Setup stops after the bounded wait (about 45 s) with nothing changed; the tray is not killed | NOT RUN |
| B6 | Update completes | Three executables replaced; registration unchanged and valid; the tray is **not** restarted unless the box is ticked | NOT RUN |
| B7 | Settings | Vault, `tray-settings.json`, token, keys, policy unchanged; `Run` value and `StartupApproved` unchanged | NOT RUN |
| B8 | Update interrupted midway (copy fails, e.g. a file locked by another program after the check) | Registration restored as before; running the setup again completes | NOT RUN |

## C. Uninstall

| # | Check | Expected | Result |
|---|-------|----------|--------|
| C1 | Tray running | Quit cooperatively, then removal proceeds | NOT RUN |
| C2 | Native Messaging | Our registration value removed; key removed only if empty; manifest deleted only if it is exactly what the installer wrote | NOT RUN |
| C3 | Run value | `MemocampusTray` removed only if it names exactly `{app}\memocampus-tray.exe`; `StartupApproved` untouched | NOT RUN |
| C4 | Files | Executables, shortcut and uninstaller removed | NOT RUN |
| C5 | Data | Vault, settings, keys, token, backups, logs kept (uninstall removes the program, not the memory) | NOT RUN |
| C6 | Chrome | Extension not removed; dev and legacy registrations unchanged | NOT RUN |

## D. Fault injection

| # | Check | Expected | Result |
|---|-------|----------|--------|
| D1 | A different owner's value in `...\NativeMessagingHosts\io.github.memocampus.vault` before install | Setup refuses, nothing changed | NOT RUN |
| D2 | `MemocampusTray` Run value pointing elsewhere before uninstall | Kept; uninstall continues | NOT RUN |
| D3 | A file held open in the install folder during uninstall (after `InitializeUninstall`) | Inno's own prompt / stop; re-run completes | NOT RUN |
| D4 | The setup stopped at the "Preparing to Install" page (exit code 7) in each blocked case above | Exit code 7, no change | NOT RUN |
| D5 | Registration step failing (read-only registry key by ACL) | Setup aborts in `ssInstall`; nothing installed | NOT RUN |
| D6 | Cleanup step failing at uninstall | Uninstall aborts before files are removed; re-run completes | NOT RUN |
| D7 | Reinstall after uninstall, then log off and on | The tray does not come back at logon until the user turns it on in the tray (settings file persisted) | NOT RUN |

## E. Inno Setup behaviours that are assumed, not yet observed

These are the documented contracts the design relies on. Each must be seen once:

1. `Abort` inside `CurStepChanged(ssInstall)` ends Setup and runs `DeinitializeSetup`.
2. A failed file copy leads to `DeinitializeSetup` with the registration restored.
3. `usUninstall` runs before the files are removed, and `Abort` there leaves the files.
4. `{app}` expands in `InitializeUninstall`.
5. A non-empty `PrepareToInstall` result stops Setup at the "Preparing to Install" page with exit code 7.
6. `postinstall` entries run only after a successful install, never with `/SILENT` or `/VERYSILENT`
   (`skipifsilent`), and never when unticked.
7. The tray started by the last page runs as the original user without arguments.

## Release rule

The release candidate is publishable only when **every row of A to D is PASS with evidence, section E
is observed, and nobody has had to edit a result**. Any FAIL is a release blocker. This file is the
checklist; the evidence lives wherever the PO keeps release records.
