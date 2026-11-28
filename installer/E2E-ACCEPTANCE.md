# Memocampus for Windows: real-installer acceptance gate (Issue #67, Slice 3B-4)

**Status: NOT RUN unless the row says otherwise** (A10 is PASS for the C runtime start only; see the rows). The
product owner ran the product installer (the v0.1.0 release candidate) in an isolated Windows 11 x64 VM and reported
what was confirmed, without recorded evidence, so no row has been recorded as PASS from that run: those reports are recorded as **PO-CONFIRMED** under
"PO-confirmed observations" below, which is not PASS and not a direct verification; the rows they touch say so and
stay NOT RUN. A3 is no longer blocked (the Chrome Web Store id `cdklofjfcjagkaclekmicliailmlnfla` is recorded in
`installer\release-gate.json`, so product builds can be made). The code has been compiled, unit-tested against private
registry keys and real scratch trays, and statically checked; the Inno Setup engine's own behaviour (what runs when,
exit codes, what `Abort` does in each event) has not been observed directly. **The v0.1.0 release rule below must be
met before the public release of v0.1.0**, the first public release of the Windows app (Issue #102), on the very Setup
file that is to be published (same SHA-256), not on the review pre-release `v0.1.0-review.1`. Nothing here may be
written up as PASS until someone has run it and kept the evidence.

## Where to run it

In an isolated Windows 11 x64 environment that is **not** a development machine (Windows 11 x64 is the platform
verified for v0.1.0; Windows 10 is not verified, and nothing here may be recorded as proving it; Windows Server is not
supported, and no result obtained on it counts): a fresh VM (snapshot
before each case), or a separate Windows user on a spare PC. **Never** on a machine whose real Chrome
profile, vault (`%LOCALAPPDATA%\Memocampus`), development Native Messaging registrations or MCP
clients matter. The test-mode installer (`/DInstallerTestMode`) does not make this safe on a real
profile: it still creates an install folder, a Start menu entry and an uninstall record, and the tray
it would start writes the real Run key. Use the VM.

Needed in the VM: Chrome (so the native messaging registration can be exercised), the release candidate
`Memocampus-Setup-x64.exe` **and** the previous one for the update cases, and `installer\e2e-snapshot.ps1`.

## Which build proves what (read before A3 and A10)

| Build | Native Messaging registration it writes | Can Chrome see it? | Proves |
|---|---|---|---|
| Product `Memocampus-Setup-x64.exe` | `HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.memocampus.vault`, `allowed_origins` = the Web Store id from `installer\release-gate.json` (`cdklofjfcjagkaclekmicliailmlnfla`) | yes, for the Web Store extension only | the installer's own product registration path (A3) |
| Test `...-INSTALLER-TEST.exe` | `HKCU\Software\MemocampusInstallerTest\NativeMessagingHosts\io.github.memocampus.vault`, `allowed_origins` = the synthetic id `abcdefghijklmnopabcdefghijklmnop` | **no, by design** | the installer's behaviour on private keys (A9 and the lifecycle cases) |

The test build's registration is isolated on purpose (`installer\README.md`, "Isolated installer test"): Chrome
never reads `Software\MemocampusInstallerTest`, the synthetic id belongs to no extension, and an unpacked
extension from this repository asks for `com.memocampus.vault` (`extension\host-config.js`, committed profile
`legacyDev`), not the product name. A Chrome log line `Can't find manifest for native messaging host
com.memocampus.vault` after installing only the test build is therefore expected, not a product defect. The
isolation is not relaxed for the acceptance run.

### Temporary development registration (disposable clean VM only, for A10)

To let Chrome start the **installed test-build executable** (whose registration Chrome cannot see, by design), the existing
development script registers it under the legacy development name. This is allowed only in a disposable clean VM
that is reverted afterwards, never on a development machine or a real profile. It writes one key,
`HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault`; the script refuses the product name
`io.github.memocampus.vault`, so the product key is never written. It does **not** exercise or prove the
installer's product registration (that is A3).

1. Install the test build (`...-INSTALLER-TEST.exe`).
2. In Chrome, `chrome://extensions` > Developer mode > Load unpacked > this repository's `extension` folder,
   unchanged (profile `legacyDev`). Copy the id Chrome shows: it is derived from the folder path, so use the real
   one, never the synthetic test id.
3. Create a work folder in the VM (for example `C:\e2e\nmh`) and register:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\install-native-host.ps1 `
     -ExtensionId <id from step 2> `
     -BinaryPath "$env:LOCALAPPDATA\Programs\MemocampusInstallerTest\memocampus-native-host.exe" `
     -ManifestPath C:\e2e\nmh\com.memocampus.vault.json
   ```
4. Reload the extension and open the side panel's **Vault** section: a status other than "Native Hostに接続できません"
   means Chrome started the installed executable. Record the host name, id, manifest path and registry value used.
5. When done, remove the registration (it is removed only if it still names that manifest), then revert the VM:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\uninstall-native-host.ps1 `
     -ManifestPath C:\e2e\nmh\com.memocampus.vault.json
   ```

## Evidence for each case

Take `e2e-snapshot.ps1 -Take before.json` before the step and `-Take after.json` after it, then
`-Compare before.json after.json`. Keep the setup log (`Memocampus-Setup-x64.exe /LOG=setup.log`), the
screenshot of the last page, and the compare output. The snapshot covers: the three Native Messaging
registrations, the Run key (and the `MemocampusTray` command), `StartupApproved\Run`, the uninstall
key, the install folder (hashes), the data folder (hashes of every file), the Start menu entry and
the running `memocampus-*` processes. It is read-only.

The result column is filled in by whoever ran the case: `PASS (date, who, evidence file)`,
`FAIL (what)` or `NOT RUN`.

**PO-CONFIRMED** is a different kind of record: the product owner's own report of what they saw, written as
`PO-CONFIRMED (date, who, environment, evidence: <file> | none, statement only)`. It always says whether evidence
exists. It is never written as PASS, never counted as a direct verification of a row, and covers only what the report
names: the rest of a row it touches stays NOT RUN.

## PO-confirmed observations (v0.1.0 release candidate)

Reported by the product owner in Issue #102 (reported 2026-11-28; the date of the VM run is to be entered by the PO),
for the release candidate `Memocampus-Setup-x64.exe`, SHA-256 `0a0bd02bd1daf4df0ed67e6f4fd20f28155088d2d145f6b1a15b6ba593146f5d` (run 46094619251,
commit a0018f2663982e85fd9a24d412a22d5b7e0e07ba), in an isolated Windows 11 x64 VM. **Evidence: none attached
(statement only)** unless a file is named here later. Nothing below is PASS.

| # | What the PO confirmed | Rows it touches (in part only) | Not confirmed by it (stays NOT RUN) |
|---|---|---|---|
| O1 | The Setup file's SHA-256 equals the release candidate's | the precondition above | |
| O2 | The installation succeeded | A1, A2 | no administrator prompt and a standard user (A1); the list of files (A2) |
| O3 | The Web Store extension connected to the installed Native Host | A3, A10 | whether the VM had no Visual C++ runtime (A10); the tray and MCP start (A10) |
| O4 | Browsing history was saved to the vault and found by search | outside these rows (Issue #69) | |
| O5 | The Native Messaging registration names the official id, and `allowed_origins` matches it | A3 | that the dev and legacy registrations are unchanged (A3) |
| O6 | The last page's checkboxes behaved as expected | A4, A6 | which choices were exercised; the texts in each language; that both start unticked (A4) |
| O7 | Offline, the installation stayed complete | A6 | |
| O8 | A silent install succeeded | A7, F8 | that neither the tray nor the browser was started (A7); the other language (F8) |
| O9 | The uninstall succeeded | C4 | the files, shortcut and uninstaller removed (C4); the tray quit (C1); the registrations (C2, C3); **the data kept (C5)**; the extension kept (C6) |

## A. New installation

| # | Check | Expected | Result |
|---|-------|----------|--------|
| A1 | Run the setup as a standard user (no administrator prompt) | Succeeds, no UAC prompt | NOT RUN (PO-CONFIRMED in part: O2) |
| A2 | Files | `%LOCALAPPDATA%\Programs\Memocampus` holds the three executables, the generated `io.github.memocampus.vault.json`, and the uninstaller; nothing under Program Files | NOT RUN (PO-CONFIRMED in part: O2) |
| A3 | Native Messaging | `HKCU\Software\Google\Chrome\NativeMessagingHosts\io.github.memocampus.vault` default value = the manifest path; manifest `path` = `memocampus-native-host.exe` in the install folder; `allowed_origins` = exactly `chrome-extension://<release id>/`, the release id being `cdklofjfcjagkaclekmicliailmlnfla` (`installer\release-gate.json`); no dev / legacy key changed. Purpose: the installer's own product registration path, with the product build and the Web Store extension (added from `https://chromewebstore.google.com/detail/memocampus/cdklofjfcjagkaclekmicliailmlnfla`; `chrome://extensions` shows that id). Not provable with the test build or with the temporary development registration | NOT RUN (no longer blocked: the id is recorded in `installer\release-gate.json` and product builds can be made; the release gate is not relaxed; PO-CONFIRMED in part: O3, O5) |
| A4 | Last page | Says Setup installed the programs and the Chrome connection, and that the extension, the vault and remembering are separate and approved in Chrome: the extension is added by the user, from the Chrome Web Store (`FinishedLabel` in `installer\i18n\en.isl` / `ja.isl`). Offers two **unticked** boxes: start Memocampus (`McLaunchTray`), and "Open Memocampus in the Chrome Web Store" / 「Chrome Web Store で Memocampus を開く」 (`McOpenStorePage`). In English and in Japanese | NOT RUN (PO-CONFIRMED in part: O6) |
| A5 | Tray box ticked | Tray starts once, as the user, no arguments; a second start (Start menu) exits quietly (already running) | NOT RUN |
| A6 | Store box ticked | Default browser opens exactly `https://chromewebstore.google.com/detail/memocampus/<release id>`, that is `https://chromewebstore.google.com/detail/memocampus/cdklofjfcjagkaclekmicliailmlnfla` (the published listing); nothing is installed in Chrome. Box left unticked: no browser opens. No default browser or no network: the error (if any) does not undo the installation, which stays listed in "Apps" and registered | NOT RUN (PO-CONFIRMED in part: O6, O7) |
| A7 | Silent install (`/VERYSILENT`) | Neither the tray nor the browser is started | NOT RUN (PO-CONFIRMED in part: O8) |
| A8 | Data | `Vault`, `config.json`, `policy.json`, `tray-settings.json` (other than created by a ticked tray start), token and keys: unchanged / absent as before; MCP publication not approved | NOT RUN |
| A9 | The test installer (`...-INSTALLER-TEST.exe`) | Has no Run section and no Store link; uses its own AppId, folder and private keys; real Chrome and Run keys untouched | NOT RUN |
| A10 | Clean Windows with no Visual C++ Redistributable | Purpose: the installed executables start without the Visual C++ runtime (C runtime linked statically). The tray starts from the Start menu with no `VCRUNTIME140.dll` error; Chrome launches the installed `memocampus-native-host.exe` and an MCP client launches `memocampus-mcp.exe` without a missing-DLL error. `node scripts\pe-imports.mjs --check` on the installed files reports no Visual C++ runtime DLL. With the test build, Chrome reaches the executable only through the temporary development registration above; that proves the executable starts, not the installer's product registration (A3, out of scope here) | PASS for the C runtime start (clean-VM run reported to the PO; Chrome connection made through the manual temporary development registration `com.memocampus.vault`; the product registration path is out of scope and stays with A3). Date, who and evidence file: to be entered from the PO's release record. Product build: PO-CONFIRMED in part (O3) |

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
| C4 | Files | Executables, shortcut and uninstaller removed | NOT RUN (PO-CONFIRMED in part: O9) |
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

## F. Languages (Issue #75)

Needs two VMs (or one, switched): a Japanese Windows 11 and an English Windows 11, plus one more display
language that Setup does not offer (for example German or French) for F3. The rows marked "development
machine" were seen on a Japanese Windows development machine with the **test** build only, without
installing anything (the wizard was left before *Install*, or stopped in *Preparing to Install* by a
refusal); they are partial evidence and are still to be run in the VM.

| # | Check | Expected | Result |
|---|-------|----------|--------|
| F1 | Japanese Windows, run Setup | The *Select Setup Language* dialog appears in Japanese with 日本語 preselected; OK gives a Japanese wizard | NOT RUN in the VM. Development machine (ja-JP UI, test build, 2026-11-24): dialog in Japanese, 日本語 preselected, Ready page in Japanese, no garbled text |
| F2 | English Windows, run Setup | The dialog preselects English; the wizard is English | NOT RUN |
| F3 | A display language Setup does not offer (for example German) | English is preselected (fallback) | NOT RUN |
| F4 | Choose the other language in the dialog, and `/LANG=english` / `/LANG=japanese` | The wizard follows the choice; with `/LANG=` no dialog appears | NOT RUN in the VM. Development machine: English chosen by hand on Japanese Windows gave an English wizard; `/LANG=english` skipped the dialog |
| F5 | In each language: the last page (A4), its two choices, and the refusals that can be provoked (D1 foreign registration, B3/B4 files in use, B5 tray that does not quit, D5 registration failure, D6 cleanup failure, B8 rollback) | Every text in the chosen language, complete, not cut off, no garbled characters; identifiers (`ROLLBACK_FAILED`, paths, host name) unchanged; the details under a Japanese message (the error or report lines) are English by design | NOT RUN in the VM. Development machine: D1 (foreign registration in the test build's private key) shown in Japanese and in English in *Preparing to Install*, nothing installed, the foreign value unchanged |
| F6 | Update: install in Japanese, then run the new Setup on English Windows (or the reverse) | The dialog preselects the language of the installed copy (`UsePreviousLanguage`); the update behaves exactly as B1 to B8 | NOT RUN |
| F7 | Uninstall after an install in each language | The uninstaller speaks the language of the last install or update, without a language dialog; its messages (C1, D6, kept entries) in that language | NOT RUN |
| F8 | Silent install (`/VERYSILENT`) on each Windows | No language dialog, no prompt; the result is as A7 | NOT RUN (PO-CONFIRMED in part: O8) |
| F9 | The behaviour does not change with the language | For the same case, the same files, registrations, exit code and log decisions (`e2e-snapshot.ps1 -Compare` of a Japanese and an English run of A1, B6, C2) | NOT RUN (the Pascal self-tests pass the same checks in both languages; that is code, not the engine) |

## Release rule

### v0.1.0 (the first public release, Issue #102)

Chosen by the product owner on the risk of a first release (2026-11-28): nobody updates from an earlier public
version, so the update cases (B) are not needed for it; the fault, engine and language cases (D, E, F) are covered for
v0.1.0 by the automated checks of the code. The release candidate is publishable only when **all** of these hold:

| # | Requirement | Met by |
|---|---|---|
| R1 | The file published is the release candidate: SHA-256, `build-info.json` (product channel, `gitCommit` = the tagged commit, working tree not modified), the official extension id | the Slice 3 record and O1 |
| R2 | A new installation on Windows 11 x64 completes, registers the host for the official id only, and the Web Store extension records and finds pages through it | PO-CONFIRMED O2, O3, O4, O5 |
| R3 | The last page's choices work, the Store page opens only when ticked, and the installation stays complete offline | PO-CONFIRMED O6, O7, with the static contract (`verify-static.ps1`: unticked, postinstall, skipifsilent, the exact listing address) |
| R4 | A silent install completes | PO-CONFIRMED O8, with the static contract (`skipifsilent`) |
| R5 | The uninstall removes the program and keeps the user's data | PO-CONFIRMED O9 for the uninstall. **That the data is kept (C5) has not been checked directly on a real machine**: it rests on the static checks (the installer refers to no vault, policy, settings, token or data folder; the uninstall removes only its own registrations) and on the uninstall having succeeded. The product owner accepts this residual risk for v0.1.0 (2026-11-28) |
| R6 | The automated gates are green and unchanged: CI (JavaScript, Rust, CI gate), `windows-dist.yml` (checks and the product build), `verify-static.ps1` with `-Negative` and `-Compile`, `verify-dist.ps1` with `-Negative`, the Pascal self-tests, and the release gate (`installer\release-gate.json`) | the release candidate's run and the CI of its commit |
| R7 | The product owner's decisions just before publishing: shipping unsigned, and the Inno Setup licence | the product owner |

Disclosed with the release (the release notes and the READMEs): the Setup program is unsigned (SmartScreen warns);
Windows 11 x64 is the verified platform, Windows 10 is not verified and Windows Server is not supported; installing
over an earlier build, the fault cases and the language detection have not been observed on a real installer; a user
of the review build `v0.1.0-review.1` uninstalls it before installing v0.1.0.

The rows not listed above stay NOT RUN; none is written as PASS. Any FAIL is a release blocker. This file is the
checklist; the evidence lives wherever the PO keeps release records.

### Later releases

There is no rule that every row must pass. For each release the cases are chosen from what changed and the risk of
it, and the choice and its reasons are recorded in that release's issue; this file is the catalogue to choose from. A
chosen case is recorded as PASS (with evidence), PO-CONFIRMED (saying whether evidence exists) or FAIL. The automated
gates of R6 are never relaxed.
