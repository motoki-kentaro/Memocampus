<#
.SYNOPSIS
  Static checks of the installer sources (memocampus.iss, nmh.pas) and of the stage folder, and the
  extension's host-name constants they must agree with. No compiler, no install, no registry access.
.DESCRIPTION
  -Negative  applies deliberate wrong changes to a scratch copy of the sources and requires that
             each one is reported as a failure (the checks must not pass an unsafe installer).
  -Compile   also runs the Inno Setup compiler (scratch output only, never installer\out): the
             extension id contract (missing, empty, short, long, upper case, bad letter, wildcard must
             all fail to compile; a well-formed id must compile, in product and test mode).
#>
[CmdletBinding()]
param(
  [switch]$Negative,
  [switch]$Compile,
  [string]$Iscc = 'C:\Program Files\Inno Setup 7\ISCC.exe'
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here

function Read-Code([string]$path) {
  # Full-line comments (";" in the script, "//" in Pascal) are not code.
  (Get-Content -LiteralPath $path -Encoding UTF8 | Where-Object { $_ -notmatch '^\s*(;|//)' }) -join "`n"
}

# The calls inside LcUninstallCleanup must come in this order (text positions in the function body).
function Test-CleanupOrder([string]$lcText) {
  $f = [regex]::Match($lcText, '(?s)function LcUninstallCleanup.*?\nend;').Value
  $a = $f.IndexOf('NmhUnregister(')
  $b = $f.IndexOf('LcRemoveRunValue(')
  $c = $f.IndexOf('NmhOwnsRegistration(NmhKeyPath, ManifestPath)')
  $d = $f.IndexOf('NmhRemoveOwnManifest(')
  return (($a -ge 0) -and ($a -lt $b) -and ($b -lt $c) -and ($c -lt $d))
}

# Returns the names of the failed checks (and prints PASS/FAIL lines unless -Quiet).
function Invoke-Checks([string]$dir, [bool]$quiet) {
  $failed = New-Object System.Collections.Generic.List[string]
  function Check([string]$name, [bool]$ok) {
    if (-not $ok) { $failed.Add($name) }
    if (-not $quiet) { Write-Host ("{0}  {1}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $name) }
  }
  $issText = Get-Content -LiteralPath (Join-Path $dir 'memocampus.iss') -Raw -Encoding UTF8
  $iss = Read-Code (Join-Path $dir 'memocampus.iss')
  $pas = Read-Code (Join-Path $dir 'nmh.pas')
  $lc = Read-Code (Join-Path $dir 'lifecycle.pas')
  $all = $iss + "`n" + $pas + "`n" + $lc

  # --- Slice 3B-1 contract (unchanged) -------------------------------------------------------------
  $sections = @([regex]::Matches($iss, '(?m)^\[(\w+)\]') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
  Check 'sections are only Setup, Languages, Files, Icons, Messages, Run, Code' (($sections -join ',') -eq 'Code,Files,Icons,Languages,Messages,Run,Setup')
  Check 'no [Registry], [UninstallRun], [InstallDelete], [Dirs] section' ($iss -notmatch '(?im)^\[(Registry|UninstallRun|InstallDelete|Dirs)\]')
  Check 'product AppId is the fixed GUID' ($iss -match '(?m)^\s*#define SetupAppId "\{\{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A\}"')
  Check 'AppId comes from SetupAppId' ($iss -match '(?m)^AppId=\{#SetupAppId\}\s*$')
  Check 'test AppId differs from the product AppId' (($iss -match '#define SetupAppId "\{\{([0-9A-F-]+)\}"[\s\S]*?#define SetupAppId "\{\{([0-9A-F-]+)\}"') -and ($Matches[1] -ne $Matches[2]))
  Check 'AppVersion comes from /DAppVersion and is required' (($iss -match '(?m)^AppVersion=\{#AppVersion\}\s*$') -and ($issText -match '#ifndef AppVersion\s+#error'))
  Check 'per user, no administrator' (($iss -match '(?m)^PrivilegesRequired=lowest\s*$') -and ($iss -notmatch 'PrivilegesRequiredOverridesAllowed'))
  Check 'product install dir is LOCALAPPDATA\Programs\Memocampus and fixed' (($iss -match '(?m)^\s*#define SetupDir "\{localappdata\}\\Programs\\Memocampus"') -and ($iss -match '(?m)^DefaultDirName=\{#SetupDir\}\s*$') -and ($iss -match '(?m)^DisableDirPage=yes\s*$') -and ($iss -match '(?m)^UsePreviousAppDir=no\s*$'))
  Check 'x64 only' (($iss -match '(?m)^ArchitecturesAllowed=x64compatible\s*$') -and ($iss -match '(?m)^ArchitecturesInstallIn64BitMode=x64compatible\s*$'))
  Check 'product output name is Memocampus-Setup-x64 in out' (($iss -match '(?m)^\s*#define SetupOutputName "Memocampus-Setup-x64"') -and ($iss -match '(?m)^\s*#define SetupOutputDir "out"') -and ($iss -match '(?m)^OutputBaseFilename=\{#SetupOutputName\}\s*$'))
  Check 'running programs are not closed or restarted' (($iss -match '(?m)^CloseApplications=no\s*$') -and ($iss -match '(?m)^RestartApplications=no\s*$'))
  $files = @([regex]::Matches($iss, '(?m)^Source:\s*"([^"]+)"') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
  Check 'Files lists exactly the three stage executables' (($files -join '|') -eq 'stage\memocampus-mcp.exe|stage\memocampus-native-host.exe|stage\memocampus-tray.exe')
  Check 'no wildcard and no recursion in Files' (($iss -notmatch '(?m)^Source:.*[\*\?]') -and ($iss -notmatch 'recursesubdirs'))
  $icons = @([regex]::Matches($iss, '(?m)^Name:\s*"\{group\}[^"]*".*$') | ForEach-Object { $_.Value })
  Check 'one Start menu icon, to the tray, no parameters' (($icons.Count -eq 1) -and ($icons[0] -match 'Filename:\s*"\{app\}\\memocampus-tray\.exe"') -and ($icons[0] -notmatch 'Parameters|WorkingDir'))
  Check 'no tray options or environment in the installer' ($all -notmatch 'MEMOCAMPUS_APP_DIR|ChangesEnvironment|--selftest|--headless|--run-seconds|--mcp-exe|--registry-root')
  Check 'the installer neither starts programs nor reads or writes StartupApproved' ($all -notmatch '(?i)StartupApproved|\bExec\s*\(|\bShellExec\s*\(|\bExecAsOriginalUser|\[UninstallRun\]')
  Check 'the Run key path appears once, as RunKeyPath in the script header' ((@([regex]::Matches($all, '(?i)CurrentVersion\\Run')).Count -eq 1) -and ($iss -match '(?s)#else\s+#define NmhKeyPath[^\n]*\n\s*#define RunKeyPath "Software\\Microsoft\\Windows\\CurrentVersion\\Run"'))
  Check 'the Run value is only read, or deleted by name after an ownership check; never written' (($lc -notmatch 'RegWrite|RegCreate|RegDeleteKey') -and (@([regex]::Matches($lc, 'RegDeleteValue\(')).Count -eq 1) -and (([regex]::Match($lc, '(?s)function LcRemoveRunValue.*?\nend;')).Value -match 'RegDeleteValue\(HKCU, RunKeyPath, ValueName\)') -and ((([regex]::Match($lc, '(?s)function LcRemoveRunValue.*?\nend;')).Value -split 'LcRunValueIsOurs\(RunKeyPath, ValueName, TrayExe, Why\)').Count -ge 3))

  # --- Host names -------------------------------------------------------------------------------------
  Check 'product host name is io.github.memocampus.vault (one define)' (([regex]::Matches($iss, '(?m)^\s*#define ProductHostName "([^"]+)"') | ForEach-Object { $_.Groups[1].Value }) -join ',' -eq 'io.github.memocampus.vault')
  Check 'the host name used in code comes from that define' ($iss -match "(?m)^\s*HostName = '\{#ProductHostName\}';")
  Check 'the installer never names the development or legacy host' ($all -notmatch 'com\.memocampus\.vault|memocampus\.vault\.dev')
  Check 'manifest file is {app}\<host name>.json, only there' ($iss -match "ExpandConstant\('\{app\}\\' \+ HostName \+ '\.json'\)")
  Check 'no unconditional delete directive (no [UninstallDelete], no [InstallDelete])' ($iss -notmatch '(?im)^\[(UninstallDelete|InstallDelete)\]')

  # --- Registry scope -----------------------------------------------------------------------------------
  Check 'product registry key is HKCU Chrome NativeMessagingHosts\<product host>' ($iss -match '(?s)#else\s+#define NmhKeyPath "Software\\Google\\Chrome\\NativeMessagingHosts\\" \+ ProductHostName')
  Check 'test mode key is a private one, not Chrome''s' ($iss -match '(?s)#ifdef InstallerTestMode\s+#define NmhKeyPath "Software\\MemocampusInstallerTest\\NativeMessagingHosts\\" \+ ProductHostName')
  Check 'only HKCU is used; no HKLM, HKA, HKCR, HKEY_*' (($all -match '\bHKCU\b') -and ($all -notmatch '\b(HKLM|HKA|HKCR|HKU|HKCC|HKEY_LOCAL_MACHINE|HKLM32|HKLM64|HKCU32|HKCU64)\b'))
  $regCalls = @([regex]::Matches($pas, '\b(Reg[A-Za-z]+)\s*\(') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
  Check 'registry functions are only the five allowed ones' ((@($regCalls | Where-Object { $_ -notin 'RegKeyExists', 'RegQueryStringValue', 'RegWriteStringValue', 'RegDeleteValue', 'RegDeleteKeyIfEmpty' }).Count -eq 0) -and ($iss -notmatch '\bReg[A-Z][A-Za-z]+\s*\('))
  Check 'every registry call uses HKCU and the passed key path' (@([regex]::Matches($pas, '\bReg[A-Za-z]+\s*\((?!HKCU,\s*KeyPath\b)')).Count -eq 0)
  Check 'default values only: writes and deletes use the empty value name' ((@([regex]::Matches($pas, "RegWriteStringValue\(HKCU, KeyPath, '[^']")).Count -eq 0) -and (@([regex]::Matches($pas, "RegDeleteValue\(HKCU, KeyPath, '[^']")).Count -eq 0))
  Check 'no key is deleted with its subkeys' ($all -notmatch 'IncludingSubkeys')
  Check 'no other host, no Enterprise Policy, no force-install' ($all -notmatch '(?i)Policies|ExtensionInstallForcelist|ExtensionSettings|ExternalExtensions|Software\\Google\\Chrome\\(?!NativeMessagingHosts\\)|Microsoft\\Edge')
  Check 'the installer does not run the extension or Chrome (the one Web Store address is a [Run] link, not code)' (($pas + $lc + ($iss -replace '(?m)^Filename: "https://chromewebstore\.google\.com/detail/\{#ChromeExtensionId\}"[^\n]*$', '')) -notmatch '(?i)chrome\.exe|webstore')

  # --- Extension id contract (build time) -----------------------------------------------------------
  Check 'ChromeExtensionId is required, never defaulted' (($issText -match '#ifndef ChromeExtensionId\s+#error') -and ($issText -notmatch '(?m)^\s*#define\s+ChromeExtensionId\b'))
  Check 'ChromeExtensionId length and letter checks exist' (($issText -match '#if Len\(ChromeExtensionId\) != 32\s+#error') -and ($issText -match '#if Pos\(Copy\(ChromeExtensionId, ExtIdPos, 1\), "abcdefghijklmnop"\) == 0\s+#error') -and ($issText -match '#for \{ExtIdPos = 1; ExtIdPos <= 32; ExtIdPos\+\+\} CheckExtIdChar'))
  Check 'no extension id literal in the installer sources' ($all -notmatch '[a-p]{32}')
  Check 'the built-in id is checked again at install time' ($iss -match 'NmhIsValidExtensionId\(BuiltInExtensionId\)')
  Check 'the id is injected only from the define' ($iss -match "(?m)^\s*BuiltInExtensionId = '\{#ChromeExtensionId\}';")

  # --- Manifest content ---------------------------------------------------------------------------------
  Check 'allowed_origins has exactly one entry, the build-time id, no wildcard' ((@([regex]::Matches($pas, "chrome-extension://")).Count -eq 1) -and ($pas -match "NmhJsonString\('chrome-extension://' \+ ExtensionId \+ '/'\)") -and ($pas -notmatch "chrome-extension://\*|allowed_origins[^\r\n]*\*"))
  Check 'JSON strings escape backslash, quote and control characters' (($pas -match 'Result := Result \+ ''\\"''') -and ($pas -match 'Result := Result \+ ''\\\\''') -and ($pas -match '''\\u'' \+ NmhHex4\(C\)'))
  Check 'manifest is written as UTF-8 without BOM, and read back' (($pas -match 'SaveStringsToUTF8FileWithoutBOM') -and ($pas -match 'NmhUtf8Decode\(NmhRawOf\(ManifestPath\)\) <> Written'))
  Check 'path is an absolute local .exe path, checked before writing' ($pas -match 'NmhIsSafeExePath\(ExePath\)')
  # The executable the manifest points at: the one NmhRegister is given, and the one [Files] puts in {app}.
  $registered = [regex]::Match($iss, "NmhRegister\(HostKeyPath, ManifestFile, HostName, App \+ '\\([^'\\]+)', BuiltInExtensionId, Err\)")
  Check 'NmhRegister is given {app}\memocampus-native-host.exe' ($registered.Success -and ($registered.Groups[1].Value -ceq 'memocampus-native-host.exe') -and ($iss -match "App := ExpandConstant\('\{app\}'\);"))
  $placed = @([regex]::Matches($iss, '(?m)^Source:\s*"stage\\([^"]+)";\s*DestDir:\s*"\{app\}"') | ForEach-Object { $_.Groups[1].Value })
  Check 'the executable given to NmhRegister is one that [Files] places in {app}' ($registered.Success -and ($placed -ccontains $registered.Groups[1].Value))

  # --- Ownership and failure handling ----------------------------------------------------------------
  Check 'an existing registration is checked before install and again before writing' (($iss -match 'NmhCheckRegistration\(HostKeyPath, ManifestFile, Why\)') -and ($pas -match 'if not NmhCheckRegistration\(KeyPath, ManifestPath, Why\) then begin'))
  Check 'a foreign registration is refused, not overwritten' (($pas -match 'NmhSamePath\(Current, ManifestPath\)') -and ($pas -match 'will not replace it'))
  Check 'rollback re-checks ownership before touching the registry' ($pas -match 'if Has and not NmhSamePath\(Current, ManifestPath\) and not \(Snap\.HadValue and NmhSamePath\(Current, Snap\.Value\)\) then')
  Check 'a failed rollback is reported' ($pas -match "' ROLLBACK_FAILED: ' \+ RollbackErr")
  Check 'registry deletes exist only in the rollback and the ownership-checked removal' ((([regex]::Match($pas, '(?s)function NmhRollback.*?\nend;')).Value -match 'RegDeleteValue') -and (([regex]::Match($pas, '(?s)function NmhUnregister.*?\nend;')).Value -match 'RegDeleteValue') -and (@([regex]::Matches((($pas -replace '(?s)function NmhRollback.*?\nend;', '') -replace '(?s)function NmhUnregister.*?\nend;', ''), 'RegDelete')).Count -eq 0))
  Check 'removal of a registration is guarded by ownership and the default value only' ((([regex]::Match($pas, '(?s)function NmhUnregister.*?\nend;')).Value -match 'NmhSamePath\(Current, ManifestPath\)') -and (([regex]::Match($pas, '(?s)function NmhUnregister.*?\nend;')).Value -match 'NmhOwnsRegistration\(KeyPath, ManifestPath\)') -and (([regex]::Match($pas, '(?s)function NmhUnregister.*?\nend;')).Value -match "RegDeleteValue\(HKCU, KeyPath, ''\)") -and (([regex]::Match($pas, '(?s)function NmhUnregister.*?\nend;')).Value -match 'RegDeleteKeyIfEmpty'))
  Check 'a manifest is deleted only when its bytes are what this installer writes' ((([regex]::Match($pas, '(?s)function NmhRemoveOwnManifest.*?\nend;')).Value -match 'NmhUtf8Decode\(NmhRawOf\(ManifestPath\)\) <> Expected') -and (@([regex]::Matches($all, 'DeleteFile\(ManifestPath\)')).Count -ge 2))
  Check 'lifecycle routines contain no key deletion, no HKLM, only HKCU Run-value reads and deletes' (($lc -notmatch 'RegDeleteKey|HKLM|HKCR') -and (@($lc | Select-String -Pattern '\bReg[A-Za-z]+\s*\(' -AllMatches | ForEach-Object { $_.Matches } | ForEach-Object { $_.Value -replace '\s*\($', '' } | Sort-Object -Unique | Where-Object { $_ -notin 'RegValueExists', 'RegQueryStringValue', 'RegDeleteValue' }).Count -eq 0) -and (@([regex]::Matches($lc, '\bReg[A-Za-z]+\s*\((?!HKCU,\s*RunKeyPath\b)')).Count -eq 0))
  Check 'the tray is only asked with WM_CLOSE (16), never WM_ENDSESSION, WM_QUIT or a kill' (($lc -match 'LcWmClose = 16;') -and ($lc -match 'PostMessageW\(Wnds\[I\], LcWmClose, 0, 0\)') -and ($all -notmatch '(?i)TerminateProcess|TerminateThread|NtTerminate|taskkill|Stop-Process|wmic|WM_ENDSESSION|WM_QUERYENDSESSION|WM_QUIT|PostQuitMessage|ExitProcess|DebugActiveProcess|SendMessage(W|A|Timeout)'))
  Check 'a window is messaged only after its owner is verified: path, session, and again just before' (($lc -match 'NmhSamePath\(Path, TrayExe\)') -and ($lc -match 'ProcessIdToSessionId\(Pid, Theirs\)') -and ($lc -match '(?s)Alive := IsWindow\(Wnd\);.*?Pid2 <> Pid') -and ([regex]::Match($lc, '(?s)function LcCloseTrays.*?\nend;').Value -match '(?s)LcVerifyOwner\(LcWindows\[I\], TrayExe.*?PostMessageW'))
  Check 'the process handle used to wait is the one that was verified (opened by pid after the path check)' (($lc -match 'OpenProcess\(LcProcessQueryLimitedInformation or LcSynchronize, False, Pid\)') -and ($lc -match 'QueryFullProcessImageNameW\(Proc, 0, Buf, Size\)'))
  Check 'every wait is finite: no INFINITE, no unbounded loop' (($lc -notmatch '(?i)INFINITE|\$FFFFFFFF|4294967295|while\s+True|repeat') -and ($lc -notmatch '(?i)\bwhile\b') -and ($lc -match 'LcTrayWaitMs = 45000;') -and ($lc -match 'for Spin := 1 to Spins do begin') -and (@([regex]::Matches($lc, 'WaitForSingleObject\([^)]*\)')).Count -ge 1) -and (@([regex]::Matches($lc, 'WaitForSingleObject\(\w+(\[\w+\])?,(?!\s*0\))')).Count -eq 0))
  Check 'lock detection opens existing files only (OPEN_EXISTING), never creates or truncates' (($lc -match 'CreateFileW\(Path, \$40000000, 0, 0, 3, \$80, 0\)'))
  Check 'both Quiesce users stop on failure: install (PrepareToInstall) and uninstall (InitializeUninstall)' (($iss -match 'Result := Quiesce;') -and ($iss -match '(?s)function InitializeUninstall: Boolean;.*?Why := Quiesce;\s*Result := Why = ''''') -and ($iss -match '(?s)function Quiesce: String;.*?if \(R\.TimedOut > 0\) or \(R\.Unclosable > 0\) then begin.*?Exit;') -and ($iss -match 'if Locked <> '''' then'))
  Check 'only the product tray path is closed: {app}\memocampus-tray.exe' (($iss -match "Result := ExpandConstant\('\{app\}\\memocampus-tray\.exe'\);") -and ($iss -match 'LcCloseTrays\(TrayExe, LcTrayWaitMs\)'))
  Check 'native messaging is registered in ssInstall (before the files), never in ssPostInstall' (($iss -match '(?s)if CurStep = ssInstall then begin.*?NmhRegister\(.*?end else if CurStep = ssPostInstall then\s+InstallCompleted := True;') -and ($iss -notmatch '(?s)ssPostInstall.*NmhRegister'))
  Check 'an incomplete setup restores the registration it made' ($iss -match '(?s)procedure DeinitializeSetup;.*?if Registered and not InstallCompleted then begin.*?NmhRollback\(HostKeyPath, ManifestFile, PreSnap, Err\)')
  Check 'uninstall: the cleanup runs in usUninstall and a failure aborts before files are removed' (($iss -match '(?s)if CurUninstallStep = usUninstall then begin\s+if not LcUninstallCleanup\(HostKeyPath, ManifestFile, HostName, ExpandConstant\(''\{app\}\\memocampus-native-host\.exe''\),\s+BuiltInExtensionId, RunKeyPath, TrayExe, Report\) then begin.*?Abort;'))
  Check 'cleanup order: registration, Run value, then the manifest; the manifest only when no longer registered' ((@([regex]::Matches($lc, 'NmhRemoveOwnManifest\(')).Count -eq 1) -and (Test-CleanupOrder $lc))
  Check 'a Run value is ours only if the command names exactly this tray path' ((([regex]::Match($lc, '(?s)function LcRunValueIsOurs.*?\nend;')).Value -match "(?s)if not NmhSamePath\(V, TrayExe\) then begin.*?Exit;\s*end;\s*Result := True;") -and (([regex]::Match($lc, '(?s)function LcRunValueIsOurs.*?\nend;')).Value -match "RegQueryStringValue\(HKCU, RunKeyPath, ValueName, V\)"))
  # --- Slice 3B-4: what the last page offers -----------------------------------------------------------------
  $runBody = [regex]::Match($iss, '(?s)\[Run\](.*?)(?=\n\[|\z)').Groups[1].Value
  $runLines = @($runBody -split "`n" | Where-Object { $_ -match '^Filename:' })
  $trayLine = @($runLines | Where-Object { $_ -match '^Filename:\s*"\{app\}\\memocampus-tray\.exe"' })
  $storeLine = @($runLines | Where-Object { $_ -match '^Filename:\s*"https://' })
  $flagsOf = { param($l) ((([regex]::Match($l, 'Flags:\s*([^;]*)$').Groups[1].Value).Trim() -split '\s+') | Sort-Object) -join ' ' }
  Check 'the last page offers exactly two things: the tray and the Web Store page' (($runLines.Count -eq 2) -and ($trayLine.Count -eq 1) -and ($storeLine.Count -eq 1))
  Check 'the tray is started only by an unticked, post-install, non-silent choice: nowait postinstall skipifsilent unchecked' (($trayLine.Count -eq 1) -and ((& $flagsOf $trayLine[0]) -ceq 'nowait postinstall skipifsilent unchecked'))
  Check 'the tray is started with no parameters, working directory, verb or environment, and not as another user' (($trayLine.Count -eq 1) -and ($trayLine[0] -notmatch '(?i)Parameters|WorkingDir|Verb|runasoriginaluser|runascurrentuser|waituntil') -and ($all -notmatch 'MEMOCAMPUS_APP_DIR'))
  Check 'the Web Store page is opened only by an unticked, post-install, non-silent shellexec choice' (($storeLine.Count -eq 1) -and ((& $flagsOf $storeLine[0]) -ceq 'postinstall shellexec skipifsilent unchecked'))
  Check 'the Web Store address is https, on chromewebstore.google.com, built from the build-time extension id only' (($storeLine.Count -eq 1) -and ($storeLine[0] -match '^Filename:\s*"https://chromewebstore\.google\.com/detail/\{#ChromeExtensionId\}";') -and ($iss -notmatch '(?i)http://') -and ($iss -notmatch '(?i)https://(?!chromewebstore\.google\.com/detail/\{#ChromeExtensionId\}")'))
  Check 'the whole [Run] section is absent from an installer test build' ($iss -match '(?s)#ifndef InstallerTestMode\s+\[Run\].*?#endif\s+#endif\s+\[Code\]')
  Check 'the Web Store link exists only in a build with /DStoreListing' ($iss -match '(?s)#ifdef StoreListing\s+(;[^\n]*\n)*Filename: "https://chromewebstore\.google\.com/detail/\{#ChromeExtensionId\}"[^\n]*\n#endif\s+#endif')
  Check 'StoreListing together with InstallerTestMode stops the compile' ($issText -match '(?s)#ifdef StoreListing\s+#ifdef InstallerTestMode\s+#error')
  Check 'the finished page says only what Setup did, and that the extension, vault and remembering are separate' (($iss -match '(?m)^FinishedLabel=.*is added by you in Chrome.*only when you approve') -and ($iss -match '(?m)^FinishedLabelNoIcons=.*is added by you in Chrome.*only when you approve'))
  Check 'no code path starts programs, shells out or creates processes' (($pas -notmatch '(?i)\bExec\(|ShellExec') -and ($lc -notmatch '(?i)\bExec\(|ShellExec') -and ($all -notmatch '(?i)CreateProcess|WinExec|ShellExecute'))
  Check 'the Run value name is the tray''s own: MemocampusTray' ($lc -match "LcRunValueName = 'MemocampusTray';")
  Check 'test hooks are not used by the installer' ($iss -notmatch 'NmhFailAt|NmhFailRollback|LcFailAt')

  # --- Data the installer must never touch -----------------------------------------------------------------
  Check 'nothing refers to vault, policy, settings, token or the data folder' ($all -notmatch '(?i)config\.json|policy\.json|tray-settings|mcp-http-token|\.sqlite|\{localappdata\}\\Memocampus\\|\\Memocampus\\logs|DeleteDir|DelTree')
  Check 'RemoveDir is only used on the install folder, which only Setup created itself (it removes empty folders only)' (@([regex]::Matches($all, 'RemoveDir\((?!App\)|ExpandConstant\(''\{app\}''\)\))')).Count -eq 0)
  Check 'no file is created outside {app}' (($pas -notmatch "(?i)\{tmp\}|\{userdocs\}|\{commonappdata\}") -and ($iss -notmatch '(?i)DestDir:\s*"(?!\{app\}")'))

  # --- Agreement with the extension and the development scripts -----------------------------------------
  $hostConfig = Join-Path $repo 'extension\host-config.js'
  if (Test-Path -LiteralPath $hostConfig) {
    $hc = Get-Content -LiteralPath $hostConfig -Raw -Encoding UTF8
    Check 'extension product host name equals the installer''s' ($hc -match 'product:\s*"io\.github\.memocampus\.vault"')
    Check 'extension dev and legacy host names are different from the product one' (($hc -match 'dev:\s*"io\.github\.memocampus\.vault\.dev"') -and ($hc -match 'legacyDev:\s*"com\.memocampus\.vault"'))
  }
  $devInstall = Join-Path $repo 'native-host\install-native-host.ps1'
  if (Test-Path -LiteralPath $devInstall) {
    $di = Get-Content -LiteralPath $devInstall -Raw -Encoding UTF8
    $du = Get-Content -LiteralPath (Join-Path $repo 'native-host\uninstall-native-host.ps1') -Raw -Encoding UTF8
    Check 'development scripts accept only the legacy and dev host names' (($di -match "ValidateSet\('com\.memocampus\.vault', 'io\.github\.memocampus\.vault\.dev'\)") -and ($du -match "ValidateSet\('com\.memocampus\.vault', 'io\.github\.memocampus\.vault\.dev'\)"))
    Check 'development uninstall removes only a registration that names its own manifest' ($du -match 'registered for another manifest, so it was NOT removed')
  }

  # --- Stage ---------------------------------------------------------------------------------------------
  $stage = Join-Path $dir 'stage'
  if (Test-Path -LiteralPath $stage) {
    $names = @(Get-ChildItem -LiteralPath $stage -Force -Recurse | ForEach-Object Name | Sort-Object)
    Check 'stage holds exactly the three executables' (($names -join '|') -eq 'memocampus-mcp.exe|memocampus-native-host.exe|memocampus-tray.exe')
  } elseif (-not $quiet) { Write-Host 'SKIP  stage folder absent (run stage.ps1; the stage is a build output, not source)' }
  return ,$failed
}

# --- -Negative: deliberate wrong changes must be caught --------------------------------------------------
if ($Negative) {
  $mutations = @(
    @{ n = 'wrong product host name'; f = 'memocampus.iss'; a = '#define ProductHostName "io.github.memocampus.vault"'; b = '#define ProductHostName "com.memocampus.vault"' },
    @{ n = 'development id in the manifest'; f = 'nmh.pas'; a = "Result[6] := '    ' + NmhJsonString('chrome-extension://' + ExtensionId + '/');"; b = "Result[6] := '    ' + NmhJsonString('chrome-extension://iljhlinoceimhkoamjedlkooaencaeoj/');" },
    @{ n = 'a second origin'; f = 'nmh.pas'; a = "'  ]';"; b = "'    ,""chrome-extension://abcdefghijklmnopabcdefghijklmnop/""  ]';" },
    @{ n = 'wildcard origin'; f = 'nmh.pas'; a = "'chrome-extension://' + ExtensionId + '/'"; b = "'chrome-extension://*/'" },
    @{ n = 'id defaulted at build time'; f = 'memocampus.iss'; a = '#ifndef ChromeExtensionId'; b = "#ifndef ChromeExtensionId`n  #define ChromeExtensionId `"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"`n#endif`n#ifdef NeverDefined" },
    @{ n = 'length check removed'; f = 'memocampus.iss'; a = '#if Len(ChromeExtensionId) != 32'; b = '#if Len(ChromeExtensionId) != 33' },
    @{ n = 'letter check removed'; f = 'memocampus.iss'; a = '#for {ExtIdPos = 1; ExtIdPos <= 32; ExtIdPos++} CheckExtIdChar'; b = '' },
    @{ n = 'HKLM write'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "procedure NmhX; begin RegWriteStringValue(HKLM, 'Software\X', '', 'y'); end;`nfunction NmhOwnsRegistration" },
    @{ n = 'another host key written'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "procedure NmhX; begin RegWriteStringValue(HKCU, 'Software\Google\Chrome\NativeMessagingHosts\other', '', 'y'); end;`nfunction NmhOwnsRegistration" },
    @{ n = 'key deleted with subkeys'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "procedure NmhX; begin RegDeleteKeyIncludingSubkeys(HKCU, KeyPath); end;`nfunction NmhOwnsRegistration" },
    @{ n = 'unconditional delete outside rollback'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "procedure NmhX; begin RegDeleteValue(HKCU, KeyPath, ''); end;`nfunction NmhOwnsRegistration" },
    @{ n = 'pre-install conflict check removed'; f = 'memocampus.iss'; a = 'else if not NmhCheckRegistration(HostKeyPath, ManifestFile, Why) then'; b = 'else if False then' },
    @{ n = 'conflict check removed from register'; f = 'nmh.pas'; a = 'if not NmhCheckRegistration(KeyPath, ManifestPath, Why) then begin'; b = 'if False then begin' },
    @{ n = 'rollback ownership check removed'; f = 'nmh.pas'; a = 'if Has and not NmhSamePath(Current, ManifestPath) and not (Snap.HadValue and NmhSamePath(Current, Snap.Value)) then begin'; b = 'if False then begin' },
    @{ n = 'rollback failure hidden'; f = 'nmh.pas'; a = "' ROLLBACK_FAILED: ' + RollbackErr"; b = "RollbackErr" },
    @{ n = 'JSON backslash escape removed'; f = 'nmh.pas'; a = "Result := Result + '\\'"; b = "Result := Result + '\'" },
    @{ n = 'BOM written'; f = 'nmh.pas'; a = 'SaveStringsToUTF8FileWithoutBOM'; b = 'SaveStringsToUTF8File' },
    @{ n = 'registry read-back removed'; f = 'nmh.pas'; a = 'NmhUtf8Decode(NmhRawOf(ManifestPath)) <> Written'; b = 'False' },
    @{ n = 'Run key added'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';`nfunction NmhOwnsRegistration" },
    @{ n = 'Enterprise Policy key'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Pol = 'Software\Policies\Google\Chrome';`nfunction NmhOwnsRegistration" },
    @{ n = 'extension force-install'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Force = 'ExtensionInstallForcelist';`nfunction NmhOwnsRegistration" },
    @{ n = 'test hook used by the installer'; f = 'memocampus.iss'; a = 'function ManifestFile: String;'; b = "var Dummy: Integer;`nprocedure Hook; begin NmhFailAt := 3; end;`nfunction ManifestFile: String;" },
    @{ n = 'a [Registry] section'; f = 'memocampus.iss'; a = '[Code]'; b = "[Registry]`nRoot: HKCU; Subkey: ""Software\X""; Flags: uninsdeletekey`n[Code]" },
    @{ n = 'uninstall cleanup call removed'; f = 'memocampus.iss'; a = 'if not LcUninstallCleanup(HostKeyPath'; b = 'if not LcUninstallClean(HostKeyPath' },
    @{ n = 'vault/config reference'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Cfg = 'config.json';`nfunction NmhOwnsRegistration" },
    @{ n = 'dev host named in the installer'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Dev = 'com.memocampus.vault';`nfunction NmhOwnsRegistration" },
    @{ n = 'test mode uses Chrome''s key'; f = 'memocampus.iss'; a = '#define NmhKeyPath "Software\MemocampusInstallerTest\NativeMessagingHosts\" + ProductHostName'; b = '#define NmhKeyPath "Software\Google\Chrome\NativeMessagingHosts\" + ProductHostName' },
    @{ n = 'program started by the installer'; f = 'memocampus.iss'; a = '[Code]'; b = "[Run]`nFilename: ""{app}\memocampus-tray.exe""`n[Code]" },
    @{ n = 'registered executable path renamed'; f = 'memocampus.iss'; a = "App + '\memocampus-native-host.exe', BuiltInExtensionId"; b = "App + '\memocampus-nativehost.exe', BuiltInExtensionId" },
    @{ n = 'registered executable moved out of {app}'; f = 'memocampus.iss'; a = "App + '\memocampus-native-host.exe', BuiltInExtensionId"; b = "App + '\bin\memocampus-native-host.exe', BuiltInExtensionId" },
    @{ n = 'Run value deleted by name only'; f = 'lifecycle.pas'; a = 'function LcRemoveRunValue'; b = "procedure LcX(const K: String); begin RegDeleteValue(HKCU, RunKeyPath, LcRunValueName); end;`nfunction LcRemoveRunValue" },
    @{ n = 'foreign Run value accepted (path check removed)'; f = 'lifecycle.pas'; a = 'if not NmhSamePath(V, TrayExe) then begin'; b = 'if False then begin' },
    @{ n = 'Run value re-check just before the delete removed'; f = 'lifecycle.pas'; a = "if not LcRunValueIsOurs(RunKeyPath, ValueName, TrayExe, Why) then begin`n    Report := 'logon start (Run value): KEPT (it changed just before removal)';"; b = "if False then begin`n    Report := 'logon start (Run value): KEPT (it changed just before removal)';" },
    @{ n = 'native messaging registration deleted without ownership'; f = 'nmh.pas'; a = 'if not NmhSamePath(Current, ManifestPath) then begin'; b = 'if False then begin' },
    @{ n = 'recursive registry delete in the lifecycle code'; f = 'lifecycle.pas'; a = 'function LcRemoveRunValue'; b = "procedure LcY; begin RegDeleteKeyIncludingSubkeys(HKCU, RunKeyPath); end;`nfunction LcRemoveRunValue" },
    @{ n = 'StartupApproved touched'; f = 'lifecycle.pas'; a = 'function LcRemoveRunValue'; b = "const Sa = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';`nfunction LcRemoveRunValue" },
    @{ n = 'forced termination imported'; f = 'lifecycle.pas'; a = 'function GetLastError: DWORD;'; b = "function TerminateProcess(h: NativeInt; c: UINT): BOOL; external 'TerminateProcess@kernel32.dll stdcall';`nfunction GetLastError: DWORD;" },
    @{ n = 'kill by process name'; f = 'memocampus.iss'; a = 'function ManifestFile: String;'; b = "procedure Kill; var C: Integer; begin Exec('taskkill', '/F /IM memocampus-tray.exe', '', 0, ewWaitUntilTerminated, C); end;`nfunction ManifestFile: String;" },
    @{ n = 'unbounded wait'; f = 'lifecycle.pas'; a = 'for Spin := 1 to Spins do begin'; b = 'while True do begin' },
    @{ n = 'INFINITE wait'; f = 'lifecycle.pas'; a = 'function GetLastError: DWORD;'; b = "const Forever = `$FFFFFFFF;`nfunction GetLastError: DWORD;" },
    @{ n = 'owner path verification removed'; f = 'lifecycle.pas'; a = 'if not NmhSamePath(Path, TrayExe) then begin'; b = 'if False then begin' },
    @{ n = 'session check removed'; f = 'lifecycle.pas'; a = 'if not ProcessIdToSessionId(Pid, Theirs) then'; b = 'if not ProcessIdToSessionIdX(Pid, Theirs) then' },
    @{ n = 'close request is WM_ENDSESSION'; f = 'lifecycle.pas'; a = 'LcWmClose = 16;'; b = 'LcWmClose = 22;' },
    @{ n = 'a tray of another folder is asked'; f = 'memocampus.iss'; a = "Result := ExpandConstant('{app}\memocampus-tray.exe');"; b = "Result := ExpandConstant('{localappdata}\Programs\Other\memocampus-tray.exe');" },
    @{ n = 'continue although the tray did not quit'; f = 'memocampus.iss'; a = 'if (R.TimedOut > 0) or (R.Unclosable > 0) then begin'; b = 'if False then begin' },
    @{ n = 'locked files ignored'; f = 'memocampus.iss'; a = "if Locked <> '' then"; b = "if False then" },
    @{ n = 'uninstall does not wait for the tray'; f = 'memocampus.iss'; a = 'Why := Quiesce;'; b = "Why := '';" },
    @{ n = 'uninstall ownership check before manifest removal removed'; f = 'lifecycle.pas'; a = 'if NmhOwnsRegistration(NmhKeyPath, ManifestPath) then'; b = 'if False then' },
    @{ n = 'manifest deleted without content check'; f = 'nmh.pas'; a = 'if NmhUtf8Decode(NmhRawOf(ManifestPath)) <> Expected then begin'; b = 'if False then begin' },
    @{ n = 'registration moved back to ssPostInstall'; f = 'memocampus.iss'; a = 'if CurStep = ssInstall then begin'; b = 'if CurStep = ssPostInstall then begin' },
    @{ n = 'no rollback of an incomplete setup'; f = 'memocampus.iss'; a = 'if Registered and not InstallCompleted then begin'; b = 'if False then begin' },
    @{ n = 'tray started after setup'; f = 'memocampus.iss'; a = 'procedure DeinitializeSetup;'; b = "procedure Start; var C: Integer; begin Exec(ExpandConstant('{app}\memocampus-tray.exe'), '', '', 1, ewNoWait, C); end;`nprocedure DeinitializeSetup;" },
    @{ n = 'cleanup order swapped (manifest before registration)'; f = 'lifecycle.pas'; a = 'Code := NmhUnregister(NmhKeyPath, ManifestPath, Line);'; b = "Code := NmhRemoveOwnManifest(ManifestPath, HostName, ExePath, ExtensionId, Line);`n  Code := NmhUnregister(NmhKeyPath, ManifestPath, Line);" },
    @{ n = 'tray start ticked by default'; f = 'memocampus.iss'; a = 'Flags: nowait postinstall skipifsilent unchecked'; b = 'Flags: nowait postinstall skipifsilent' },
    @{ n = 'tray start also in a silent install'; f = 'memocampus.iss'; a = 'Flags: nowait postinstall skipifsilent unchecked'; b = 'Flags: nowait postinstall unchecked' },
    @{ n = 'tray started with an argument'; f = 'memocampus.iss'; a = 'memocampus-tray.exe"; Description: "Start'; b = 'memocampus-tray.exe"; Parameters: "--headless"; Description: "Start' },
    @{ n = 'tray started with the app dir variable'; f = 'memocampus.iss'; a = 'memocampus-tray.exe"; Description: "Start'; b = 'memocampus-tray.exe"; Description: "MEMOCAMPUS_APP_DIR Start' },
    @{ n = 'tray start offered in the installer test build'; f = 'memocampus.iss'; a = "#ifndef InstallerTestMode`n[Run]"; b = "#if 1`n[Run]" },
    @{ n = 'tray start waits for the tray to end'; f = 'memocampus.iss'; a = 'Flags: nowait postinstall skipifsilent unchecked'; b = 'Flags: postinstall skipifsilent unchecked' },
    @{ n = 'a third thing offered on the last page'; f = 'memocampus.iss'; a = "#ifdef StoreListing`n; Opens"; b = "Filename: ""{app}\memocampus-mcp.exe""; Flags: postinstall nowait skipifsilent unchecked`n#ifdef StoreListing`n; Opens" },
    @{ n = 'Web Store link over plain http'; f = 'memocampus.iss'; a = 'Filename: "https://chromewebstore.google.com/detail/'; b = 'Filename: "http://chromewebstore.google.com/detail/' },
    @{ n = 'Web Store link to another site'; f = 'memocampus.iss'; a = 'Filename: "https://chromewebstore.google.com/detail/'; b = 'Filename: "https://example.com/detail/' },
    @{ n = 'Web Store link with a fixed extension id'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/bcdefghijklmnopabcdefghijklmnopa"; Description' },
    @{ n = 'Web Store link ticked by default'; f = 'memocampus.iss'; a = 'Flags: shellexec postinstall skipifsilent unchecked'; b = 'Flags: shellexec postinstall skipifsilent' },
    @{ n = 'Web Store link without the StoreListing guard'; f = 'memocampus.iss'; a = "#ifdef StoreListing`n; Opens"; b = "#if 1`n; Opens" },
    @{ n = 'StoreListing allowed in the installer test build'; f = 'memocampus.iss'; a = '#error StoreListing is for the product installer only'; b = '#pragma message StoreListing is for the product installer only' },
    @{ n = 'finished page claims the extension is installed'; f = 'memocampus.iss'; a = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome'; b = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is installed (and vaults chosen) in Chrome' },
    @{ n = 'a fake extension id literal'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Fake = 'abcdefghijklmnopabcdefghijklmnop';`nfunction NmhOwnsRegistration" }
  )
  $scratch = Join-Path ([IO.Path]::GetTempPath()) ("memocampus-verify-neg-" + [guid]::NewGuid().ToString('N'))
  $missed = 0
  try {
    foreach ($m in $mutations) {
      $copy = Join-Path $scratch ([guid]::NewGuid().ToString('N'))
      New-Item -ItemType Directory -Path $copy | Out-Null
      Copy-Item -LiteralPath (Join-Path $here 'memocampus.iss'), (Join-Path $here 'nmh.pas'), (Join-Path $here 'lifecycle.pas') -Destination $copy
      $p = Join-Path $copy $m.f
      $t = (Get-Content -LiteralPath $p -Raw -Encoding UTF8).Replace("`r`n", "`n")
      if ($t.Split([string[]]@($m.a), [StringSplitOptions]::None).Count -ne 2) { "FAIL  mutation does not apply exactly once: $($m.n)"; $missed++; continue }
      [IO.File]::WriteAllText($p, $t.Replace($m.a, $m.b), (New-Object Text.UTF8Encoding($false)))
      $got = Invoke-Checks $copy $true
      if ($got.Count -gt 0) { "PASS  caught: {0}  ({1})" -f $m.n, ($got[0]) } else { "FAIL  NOT CAUGHT: $($m.n)"; $missed++ }
    }
  } finally { Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue }
  if ($missed) { "$missed mutation(s) not caught"; exit 1 }
  "all $($mutations.Count) mutations caught"
  return
}

$result = Invoke-Checks $here $false
$bad = $result.Count

# --- -Compile: the extension id contract, through the real compiler -----------------------------------------
if ($Compile) {
  if (-not (Test-Path -LiteralPath $Iscc)) { "FAIL  Inno Setup compiler not found: $Iscc"; $bad++ }
  else {
    $out = Join-Path ([IO.Path]::GetTempPath()) ("memocampus-verify-compile-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $out | Out-Null
    $iss = Join-Path $here 'memocampus.iss'
    $good = 'abcdefghijklmnopabcdefghijklmnop'
    function Compile-With([string[]]$defs) {
      # The compiler reports its errors on stderr; here only its exit code matters.
      $ErrorActionPreference = 'Continue'
      & $Iscc /Q "/O$out" /Fverify $defs $iss *> $null
      return $LASTEXITCODE
    }
    $cases = @(
      @{ n = 'no extension id'; d = @('/DAppVersion=0.0.1'); fail = $true },
      @{ n = 'no version'; d = @("/DChromeExtensionId=$good"); fail = $true },
      @{ n = 'empty id'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId='); fail = $true },
      @{ n = '31 characters'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmno'); fail = $true },
      @{ n = '33 characters'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmnopa'); fail = $true },
      @{ n = 'upper case'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=Abcdefghijklmnopabcdefghijklmnop'); fail = $true },
      @{ n = 'letter q'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmnoq'); fail = $true },
      @{ n = 'digit'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmno1'); fail = $true },
      @{ n = 'wildcard'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=*bcdefghijklmnopabcdefghijklmnop'); fail = $true },
      @{ n = 'Web Store link in an installer test build'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DInstallerTestMode', '/DStoreListing'); fail = $true },
      @{ n = 'well-formed id with the Web Store link (product build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DStoreListing'); fail = $false },
      @{ n = 'well-formed id (product build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good"); fail = $false },
      @{ n = 'well-formed id (isolated test build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DInstallerTestMode'); fail = $false }
    )
    try {
      foreach ($c in $cases) {
        $code = Compile-With $c.d
        $ok = if ($c.fail) { $code -ne 0 } else { $code -eq 0 }
        if (-not $ok) { $bad++ }
        "{0}  compile: {1} (exit {2})" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $c.n, $code
      }
    } finally { Remove-Item -LiteralPath $out -Recurse -Force -ErrorAction SilentlyContinue }
  }
}
if ($bad) {
  "$bad check(s) failed"
  foreach ($n in $result) { Write-Host "FAILED  $n" }
  exit 1
}
'all checks passed'
