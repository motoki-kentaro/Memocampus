<#
.SYNOPSIS
  Static checks of the installer sources (memocampus.iss, nmh.pas, lifecycle.pas, the language files
  i18n\en.isl and i18n\ja.isl) and of the stage folder, and the extension's host-name constants they must
  agree with. No compiler, no install, no registry access.
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

# An installer language file (i18n\*.isl): its sections and key=value entries, in file order. Comment
# (";") and blank lines are skipped; any other line that is not a section or an entry is listed as bad.
function Read-Isl([string]$path) {
  $r = @{ exists = (Test-Path -LiteralPath $path -PathType Leaf); bom = $false; utf8 = $false; sections = @(); entries = @(); bad = @() }
  if (-not $r.exists) { return $r }
  $bytes = [IO.File]::ReadAllBytes($path)
  $r.bom = ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)
  $text = ''
  try { $text = (New-Object Text.UTF8Encoding($false, $true)).GetString($bytes); $r.utf8 = $true } catch { $r.utf8 = $false }
  $section = ''
  foreach ($line in ($text -split "`r?`n")) {
    if ($line -match '^\s*(;|$)') { continue }
    if ($line -match '^\[(\w+)\]\s*$') { $section = $Matches[1]; $r.sections += $section; continue }
    if ($line -match '^([A-Za-z][A-Za-z0-9_]*)=(.*)$') { $r.entries += [pscustomobject]@{ section = $section; key = $Matches[1]; value = $Matches[2] }; continue }
    $r.bad += $line
  }
  return $r
}

# The %1..%9 arguments a text takes, as a sorted, de-duplicated list ("%%" is a literal percent sign).
function Get-MessageArgs([string]$text) {
  (@([regex]::Matches(($text -replace '%%', ''), '%([1-9])') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)) -join ','
}

# The preprocessor #if nesting depth at the first line that matches $pattern (-1 when no line matches).
function Get-IfDepth([string]$text, [string]$pattern) {
  $depth = 0
  foreach ($line in ($text -split "`r?`n")) {
    if ($line -match $pattern) { return $depth }
    if ($line -match '^\s*#\s*if(n?def)?\b') { $depth++ }
    elseif ($line -match '^\s*#\s*endif\b') { $depth-- }
  }
  return -1
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
  $en = Read-Isl (Join-Path $dir 'i18n\en.isl')
  $ja = Read-Isl (Join-Path $dir 'i18n\ja.isl')
  $enText = @{}; foreach ($e in $en.entries) { $enText[$e.key] = $e.value }
  $jaText = @{}; foreach ($e in $ja.entries) { $jaText[$e.key] = $e.value }
  # Japanese phrases the texts must keep (escaped, so that this script stays ASCII):
  $jaDone = '\u30bb\u30c3\u30c8\u30a2\u30c3\u30d7\u304c\u884c\u3046\u306e\u306f\u3053\u3053\u307e\u3067\u3067\u3059'   # setup does only this much
  $jaYouAdd = '\u3054\u81ea\u8eab\u3067 Chrome \u306b\u8ffd\u52a0'                                                       # you add it to Chrome yourself
  $jaFromStore = 'Chrome Web Store \u304b\u3089\u3054\u81ea\u8eab\u3067 Chrome \u306b\u8ffd\u52a0'                                 # from the Chrome Web Store, you add it to Chrome yourself
  $jaOpenStore = 'Chrome Web Store \u3067 Memocampus \u3092\u958b\u304f'                                                    # open Memocampus in the Chrome Web Store
  $jaOnlyApproved ='\u627f\u8a8d\u3057\u305f\u3068\u304d\u306b\u3060\u3051'                                              # only when you approve
  $jaNotReplace = '\u7f6e\u304d\u63db\u3048\u307e\u305b\u3093'                                                            # will not replace
  $jaScript = '[\u3040-\u30ff\u4e00-\u9fff]'
  # The one Japanese an English text may hold: the tray's Japanese-only menu item, as shown, with its gloss (F-75-01).
  $trayQuit = '"\u7d42\u4e86" \(Quit\)'

  # --- Slice 3B-1 contract (the texts moved to i18n\*.isl with Issue #75) ---------------------------------
  $sections = @([regex]::Matches($iss, '(?m)^\[(\w+)\]') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
  Check 'sections are only Setup, Languages, Files, Icons, Run, Code (the texts are in i18n\*.isl)' (($sections -join ',') -eq 'Code,Files,Icons,Languages,Run,Setup')
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
  Check 'the installer does not run the extension or Chrome (the one Web Store address is a [Run] link, not code)' (($pas + $lc + ($iss -replace '(?m)^Filename: "https://chromewebstore\.google\.com/detail/memocampus/\{#ChromeExtensionId\}"[^\n]*$', '')) -notmatch '(?i)chrome\.exe|webstore')

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
  Check 'a foreign registration is refused, not overwritten' (($pas -match 'NmhSamePath\(Current, ManifestPath\)') -and ($pas -match "Why := MsgText\('McNmhForeignRegistration', \[Current\]\)") -and ($enText['McNmhForeignRegistration'] -match 'will not replace it') -and ($jaText['McNmhForeignRegistration'] -match $jaNotReplace))
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
  # Issue #100: the published listing's address form, detail/memocampus/<id>; the id still comes only from the build-time define
  # (tests/docs/readme.test.js fills in the release gate's id and compares the whole address with the READMEs' link).
  Check 'the Web Store address is the published listing: https, chromewebstore.google.com/detail/memocampus/, then the build-time extension id only' (($storeLine.Count -eq 1) -and ($storeLine[0] -cmatch '^Filename:\s*"https://chromewebstore\.google\.com/detail/memocampus/\{#ChromeExtensionId\}";') -and ($iss -notmatch '(?i)http://') -and ($iss -notmatch '(?i)https://(?!chromewebstore\.google\.com/detail/memocampus/\{#ChromeExtensionId\}")'))
  Check 'the whole [Run] section is absent from an installer test build' ($iss -match '(?s)#ifndef InstallerTestMode\s+\[Run\].*?#endif\s+#endif\s+\[Code\]')
  Check 'the Web Store link exists only in a build with /DStoreListing' ($iss -match '(?s)#ifdef StoreListing\s+(;[^\n]*\n)*Filename: "https://chromewebstore\.google\.com/detail/memocampus/\{#ChromeExtensionId\}"[^\n]*\n#endif\s+#endif')
  Check 'StoreListing together with InstallerTestMode stops the compile' ($issText -match '(?s)#ifdef StoreListing\s+#ifdef InstallerTestMode\s+#error')
  Check 'the finished page says only what Setup did, and that the extension, vault and remembering are separate (English)' (($enText['FinishedLabel'] -match 'is added by you in Chrome.*only when you approve') -and ($enText['FinishedLabelNoIcons'] -match 'is added by you in Chrome.*only when you approve'))
  Check 'the finished page says only what Setup did, and that the extension, vault and remembering are separate (Japanese)' (($jaText['FinishedLabel'] -match ($jaDone + '.*' + $jaYouAdd + '.*' + $jaOnlyApproved)) -and ($jaText['FinishedLabelNoIcons'] -match ($jaDone + '.*' + $jaYouAdd + '.*' + $jaOnlyApproved)))
  Check 'the last page''s choices say the extension is added by the user (both languages)' (($enText['McOpenStorePage'] -match 'you add the extension yourself') -and ($jaText['McOpenStorePage'] -match $jaYouAdd))
  # Issue #100: Setup installs the Windows side; the extension comes from the Chrome Web Store, added by the user.
  Check 'the finished page and the Store choice name the Chrome Web Store as where the extension comes from (both languages)' (($enText['FinishedLabel'] -cmatch 'added by you in Chrome, from the Chrome Web Store') -and ($enText['FinishedLabelNoIcons'] -cmatch 'added by you in Chrome, from the Chrome Web Store') -and ($jaText['FinishedLabel'] -cmatch $jaFromStore) -and ($jaText['FinishedLabelNoIcons'] -cmatch $jaFromStore) -and ($enText['McOpenStorePage'] -cmatch '^Open Memocampus in the Chrome Web Store ') -and ($jaText['McOpenStorePage'] -cmatch ('^' + $jaOpenStore)))
  Check 'no code path starts programs, shells out or creates processes' (($pas -notmatch '(?i)\bExec\(|ShellExec') -and ($lc -notmatch '(?i)\bExec\(|ShellExec') -and ($all -notmatch '(?i)CreateProcess|WinExec|ShellExecute'))
  Check 'the Run value name is the tray''s own: MemocampusTray' ($lc -match "LcRunValueName = 'MemocampusTray';")
  Check 'test hooks are not used by the installer' ($iss -notmatch 'NmhFailAt|NmhFailRollback|LcFailAt')

  # --- Issue #75: languages ----------------------------------------------------------------------------------
  $langs = @([regex]::Matches($iss, '(?m)^Name:\s*"(\w+)";\s*MessagesFile:\s*"([^"]+)"\s*$') | ForEach-Object { $_.Groups[1].Value + '=' + $_.Groups[2].Value })
  Check 'languages: English first (the fallback for any other UI language), then Japanese; official base files, then i18n\*.isl' (($langs -join '|') -ceq 'english=compiler:Default.isl,i18n\en.isl|japanese=compiler:Languages\Japanese.isl,i18n\ja.isl')
  Check 'language: the Windows UI language by default, the dialog always offered, an update keeps the installed language' (($iss -match '(?m)^ShowLanguageDialog=yes\s*$') -and ($iss -match '(?m)^LanguageDetectionMethod=uilanguage\s*$') -and ($iss -match '(?m)^UsePreviousLanguage=yes\s*$') -and (@([regex]::Matches($iss, '(?m)^(ShowLanguageDialog|LanguageDetectionMethod|UsePreviousLanguage)=')).Count -eq 3))
  Check 'the test and the product build have the same languages (nothing of it inside #if)' (((Get-IfDepth $issText '^\[Languages\]') -eq 0) -and ((Get-IfDepth $issText '^ShowLanguageDialog=') -eq 0) -and ((Get-IfDepth $issText '^LanguageDetectionMethod=') -eq 0) -and ((Get-IfDepth $issText '^UsePreviousLanguage=') -eq 0) -and ([regex]::Match($issText, '(?s)\[Languages\](.*?)\n\[').Groups[1].Value -notmatch '(?m)^\s*#'))
  Check 'no code branches on the language (the language changes texts only)' ($all -notmatch '(?i)ActiveLanguage|\{language\}|GetUILanguage|GetUserDefaultUILanguage|GetUserDefaultLangID|LanguageID')
  Check 'language files: present, valid UTF-8 without a BOM, no preprocessor or unknown lines' ($en.exists -and $ja.exists -and $en.utf8 -and $ja.utf8 -and -not $en.bom -and -not $ja.bom -and ($en.bad.Count -eq 0) -and ($ja.bad.Count -eq 0))
  Check 'language files hold only [Messages] and [CustomMessages], once each' ((($en.sections -join ',') -ceq 'Messages,CustomMessages') -and (($ja.sections -join ',') -ceq 'Messages,CustomMessages'))
  $enKeys = @($en.entries | ForEach-Object { $_.section + '.' + $_.key })
  $jaKeys = @($ja.entries | ForEach-Object { $_.section + '.' + $_.key })
  Check 'no text is defined twice and none is empty' (($enKeys.Count -gt 0) -and (@($enKeys | Sort-Object -Unique).Count -eq $enKeys.Count) -and (@($jaKeys | Sort-Object -Unique).Count -eq $jaKeys.Count) -and (@($en.entries + $ja.entries | Where-Object { $_.value.Trim() -eq '' }).Count -eq 0))
  Check 'English and Japanese define exactly the same texts' ((($enKeys | Sort-Object) -join '|') -ceq (($jaKeys | Sort-Object) -join '|'))
  Check 'every text takes the same arguments (%1..%9) in both languages' (@($en.entries | Where-Object { (Get-MessageArgs $_.value) -cne (Get-MessageArgs $jaText[$_.key]) }).Count -eq 0)
  Check 'every Japanese text is Japanese, every English text is not (except the tray''s Japanese-only menu item, named as shown with its gloss (Quit))' ((@($ja.entries | Where-Object { $_.value -notmatch $jaScript }).Count -eq 0) -and (@($en.entries | Where-Object { ($_.value -replace $trayQuit, '') -match $jaScript }).Count -eq 0))
  Check 'identifiers are not translated (ROLLBACK_FAILED and any other upper-case identifier of an English text is in the Japanese one)' (@($en.entries | ForEach-Object { $k = $_.key; [regex]::Matches($_.value, '\b[A-Z][A-Z0-9]*_[A-Z0-9_]+\b') | Where-Object { $jaText[$k] -cnotmatch [regex]::Escape($_.Value) } }).Count -eq 0)
  Check 'the [Messages] the installer overrides are only the finished page' ((@($en.entries | Where-Object { $_.section -eq 'Messages' } | ForEach-Object key | Sort-Object) -join ',') -ceq 'FinishedLabel,FinishedLabelNoIcons')
  $custom = @($en.entries | Where-Object { $_.section -eq 'CustomMessages' } | ForEach-Object key)
  $named = @((@([regex]::Matches($all, "MsgText\('(\w+)'")) + @([regex]::Matches($iss, '\{cm:(\w+)'))) | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
  Check 'every text the code names is defined, in both languages' ((@($named | Where-Object { -not $enText.ContainsKey($_) -or -not $jaText.ContainsKey($_) }).Count -eq 0) -and ($named.Count -gt 0))
  Check 'every custom text is used (no stale text)' (@($custom | Where-Object { $named -notcontains $_ }).Count -eq 0)
  Check 'custom texts are read only through MsgText (with its arguments) or {cm:...}' ((@([regex]::Matches($all, '\bCustomMessage\(')).Count -eq 1) -and ($pas -match "Result := FmtMessage\(CustomMessage\(Name\), Args\);") -and ($all -notmatch '\bExpandConstant\(''\{cm:'))
  Check 'no literal text is shown to the user: every message box (a MsgText or a variable holding one), refusal and [Run] description comes from the language files' ((@([regex]::Matches($iss, '\b(Suppressible)?MsgBox\((?!MsgText\(|\w+,)')).Count -eq 0) -and ($iss -notmatch "(?m)^\s*Result := '[^']") -and ($pas -notmatch "Why := '[^']") -and (@($runLines | Where-Object { $_ -notmatch 'Description:\s*"\{cm:\w+\}"' }).Count -eq 0))
  Check 'the Run descriptions are the two texts for the tray and the Web Store page' (($trayLine.Count -eq 1) -and ($trayLine[0] -match 'Description:\s*"\{cm:McLaunchTray\}"') -and ($storeLine.Count -eq 1) -and ($storeLine[0] -match 'Description:\s*"\{cm:McOpenStorePage\}"'))
  Check 'the install failure keeps ROLLBACK_FAILED in what it shows (the error, untranslated, and the instruction)' (($iss -match "MsgText\('McRegisterFailed', \[HostName, Err\]\)") -and ($enText['McRegisterFailed'] -cmatch 'ROLLBACK_FAILED') -and ($jaText['McRegisterFailed'] -cmatch 'ROLLBACK_FAILED') -and ((Get-MessageArgs $enText['McRegisterFailed']) -eq '1,2'))
  Check 'the uninstall decision reads the untranslated report (KEPT / FAILED), not a translated text' ($iss -match "if \(Pos\('KEPT', Report\) > 0\) or \(Pos\('FAILED', Report\) > 0\) then\s+SuppressibleMsgBox\(MsgText\('McUninstallKept', \[Report\]\)")
  Check 'no extension id literal in the language files' ((@($en.entries + $ja.entries | Where-Object { $_.value -match '[a-p]{32}' }).Count -eq 0))

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
    @{ n = 'tray started with an argument'; f = 'memocampus.iss'; a = 'memocampus-tray.exe"; Description: "{cm:'; b = 'memocampus-tray.exe"; Parameters: "--headless"; Description: "{cm:' },
    @{ n = 'tray started with the app dir variable'; f = 'memocampus.iss'; a = 'memocampus-tray.exe"; Description: "{cm:'; b = 'memocampus-tray.exe"; Description: "MEMOCAMPUS_APP_DIR {cm:' },
    @{ n = 'tray start offered in the installer test build'; f = 'memocampus.iss'; a = "#ifndef InstallerTestMode`n[Run]"; b = "#if 1`n[Run]" },
    @{ n = 'tray start waits for the tray to end'; f = 'memocampus.iss'; a = 'Flags: nowait postinstall skipifsilent unchecked'; b = 'Flags: postinstall skipifsilent unchecked' },
    @{ n = 'a third thing offered on the last page'; f = 'memocampus.iss'; a = "#ifdef StoreListing`n; Opens"; b = "Filename: ""{app}\memocampus-mcp.exe""; Flags: postinstall nowait skipifsilent unchecked`n#ifdef StoreListing`n; Opens" },
    @{ n = 'Web Store link over plain http'; f = 'memocampus.iss'; a = 'Filename: "https://chromewebstore.google.com/detail/'; b = 'Filename: "http://chromewebstore.google.com/detail/' },
    @{ n = 'Web Store link to another site'; f = 'memocampus.iss'; a = 'Filename: "https://chromewebstore.google.com/detail/'; b = 'Filename: "https://example.com/detail/' },
    @{ n = 'Web Store link with a fixed extension id'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/memocampus/bcdefghijklmnopabcdefghijklmnopa"; Description' },
    # Issue #100: the published listing's address, and how it is opened.
    @{ n = 'Web Store link in the old form without the listing name'; c = 'the published listing'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/{#ChromeExtensionId}"; Description' },
    @{ n = 'Web Store link with another listing name'; c = 'the published listing'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/memocampus-vault/{#ChromeExtensionId}"; Description' },
    @{ n = 'Web Store link with the listing name in capitals'; c = 'the published listing'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/Memocampus/{#ChromeExtensionId}"; Description' },
    @{ n = 'Web Store link with a query added'; c = 'the published listing'; f = 'memocampus.iss'; a = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}"; Description'; b = 'chromewebstore.google.com/detail/memocampus/{#ChromeExtensionId}?hl=ja"; Description' },
    @{ n = 'Web Store link opened with the installer''s rights (runascurrentuser)'; c = 'unticked, post-install, non-silent shellexec'; f = 'memocampus.iss'; a = 'Flags: shellexec postinstall skipifsilent unchecked'; b = 'Flags: shellexec postinstall skipifsilent unchecked runascurrentuser' },
    @{ n = 'Web Store link waits for the browser'; c = 'unticked, post-install, non-silent shellexec'; f = 'memocampus.iss'; a = 'Flags: shellexec postinstall skipifsilent unchecked'; b = 'Flags: shellexec postinstall skipifsilent unchecked waituntilterminated' },
    @{ n = 'Web Store link opened during the install, not on the last page'; c = 'unticked, post-install, non-silent shellexec'; f = 'memocampus.iss'; a = 'Flags: shellexec postinstall skipifsilent unchecked'; b = 'Flags: shellexec skipifsilent' },
    @{ n = 'English finished page no longer names the Web Store'; c = 'name the Chrome Web Store'; f = 'i18n\en.isl'; a = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome, from the Chrome Web Store,'; b = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome,' },
    @{ n = 'Japanese Store choice no longer says what it opens'; c = 'name the Chrome Web Store'; f = 'i18n\ja.isl'; u = $true; a = 'McOpenStorePage=Chrome Web Store \u3067 Memocampus \u3092\u958b\u304f'; b = 'McOpenStorePage=Memocampus \u306e\u30da\u30fc\u30b8\u3092\u958b\u304f' },
    @{ n = 'Web Store link ticked by default'; f = 'memocampus.iss'; a = 'Flags: shellexec postinstall skipifsilent unchecked'; b = 'Flags: shellexec postinstall skipifsilent' },
    @{ n = 'Web Store link without the StoreListing guard'; f = 'memocampus.iss'; a = "#ifdef StoreListing`n; Opens"; b = "#if 1`n; Opens" },
    @{ n = 'StoreListing allowed in the installer test build'; f = 'memocampus.iss'; a = '#error StoreListing is for the product installer only'; b = '#pragma message StoreListing is for the product installer only' },
    @{ n = 'finished page claims the extension is installed'; c = 'separate (English)'; f = 'i18n\en.isl'; a = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is added by you in Chrome'; b = 'FinishedLabel=Setup has installed the [name] programs and its connection to Chrome on this computer.%n%nThat is all Setup does. The Chrome extension is installed (and vaults chosen) in Chrome' },
    @{ n = 'a fake extension id literal'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Fake = 'abcdefghijklmnopabcdefghijklmnop';`nfunction NmhOwnsRegistration" },
    # Issue #75: languages and texts.
    @{ n = 'Japanese finished page claims the extension is installed'; c = 'separate (Japanese)'; f = 'i18n\ja.isl'; u = $true; a = 'FinishedLabel=[name] \u306e\u30d7\u30ed\u30b0\u30e9\u30e0\u3068\u3001Chrome \u3068\u306e\u63a5\u7d9a\u3092\u3053\u306e\u30b3\u30f3\u30d4\u30e5\u30fc\u30bf\u30fc\u306b\u30a4\u30f3\u30b9\u30c8\u30fc\u30eb\u3057\u307e\u3057\u305f\u3002%n%n\u30bb\u30c3\u30c8\u30a2\u30c3\u30d7\u304c\u884c\u3046\u306e\u306f\u3053\u3053\u307e\u3067\u3067\u3059\u3002Chrome \u62e1\u5f35\u6a5f\u80fd\u306f\u3001Chrome Web Store \u304b\u3089\u3054\u81ea\u8eab\u3067 Chrome \u306b\u8ffd\u52a0\u3057\u3066\u304f\u3060\u3055\u3044\u3002'; b = 'FinishedLabel=[name] \u306e\u30d7\u30ed\u30b0\u30e9\u30e0\u3068\u3001Chrome \u3068\u306e\u63a5\u7d9a\u3092\u3053\u306e\u30b3\u30f3\u30d4\u30e5\u30fc\u30bf\u30fc\u306b\u30a4\u30f3\u30b9\u30c8\u30fc\u30eb\u3057\u307e\u3057\u305f\u3002%n%n\u30bb\u30c3\u30c8\u30a2\u30c3\u30d7\u304c\u884c\u3046\u306e\u306f\u3053\u3053\u307e\u3067\u3067\u3059\u3002Chrome \u62e1\u5f35\u6a5f\u80fd\u3082\u30a4\u30f3\u30b9\u30c8\u30fc\u30eb\u3057\u307e\u3057\u305f\u3002' },
    @{ n = 'Japanese finished page (no icons) says remembering starts without approval'; c = 'separate (Japanese)'; f = 'i18n\ja.isl'; u = $true; a = '\u627f\u8a8d\u3057\u305f\u3068\u304d\u306b\u3060\u3051\u884c\u308f\u308c\u307e\u3059\u3002\n\n[CustomMessages]'; b = '\u627f\u8a8d\u306a\u3057\u3067\u81ea\u52d5\u7684\u306b\u884c\u308f\u308c\u307e\u3059\u3002\n\n[CustomMessages]' },
    @{ n = 'Japanese Store choice no longer says the user adds the extension'; c = 'added by the user'; f = 'i18n\ja.isl'; u = $true; a = '\uff08\u62e1\u5f35\u6a5f\u80fd\u306f\u3054\u81ea\u8eab\u3067 Chrome \u306b\u8ffd\u52a0\u3057\u3066\u304f\u3060\u3055\u3044\uff09'; b = '' },
    @{ n = 'a Japanese text missing'; c = 'define exactly the same texts'; f = 'i18n\ja.isl'; a = 'McTrayStillRunning='; b = 'McTrayStillRunningX=' },
    @{ n = 'an English text missing'; c = 'define exactly the same texts'; f = 'i18n\en.isl'; a = 'McInvalidExtensionId='; b = '; McInvalidExtensionId=' },
    @{ n = 'a text defined twice'; c = 'defined twice'; f = 'i18n\en.isl'; a = '[CustomMessages]'; b = "[CustomMessages]`nMcLaunchTray=Start" },
    @{ n = 'an empty Japanese text'; c = 'none is empty'; f = 'i18n\ja.isl'; a = 'McInvalidExtensionId='; b = "McInvalidExtensionId=`nMcInvalidExtensionIdOld=" },
    @{ n = 'an argument dropped in Japanese'; c = 'same arguments'; f = 'i18n\ja.isl'; a = ':%n%n%1%n'; b = ':%n%n%n' },
    @{ n = 'ROLLBACK_FAILED translated in Japanese'; c = 'identifiers are not translated'; f = 'i18n\ja.isl'; a = ' ROLLBACK_FAILED '; b = ' ROLLBACK FAILED ' },
    @{ n = 'Japanese in an English text beyond the tray''s menu item'; c = 'every Japanese text is Japanese'; f = 'i18n\en.isl'; u = $true; a = 'McInvalidExtensionId=This installer was built with an invalid'; b = 'McInvalidExtensionId=This installer was built with an \u7121\u52b9 invalid' },
    @{ n = 'the tray''s menu item named without its English gloss'; c = 'every Japanese text is Japanese'; f = 'i18n\en.isl'; u = $true; a = '"\u7d42\u4e86" (Quit)'; b = '"\u7d42\u4e86"' },
    @{ n = 'a Japanese text left in English'; c = 'every Japanese text is Japanese'; f = 'i18n\ja.isl'; u = $true; a = 'McInvalidExtensionId=\u3053\u306e\u30a4\u30f3\u30b9\u30c8\u30fc\u30e9\u30fc\u306f\u3001\u7121\u52b9\u306a Chrome \u62e1\u5f35\u6a5f\u80fd ID \u3067\u30d3\u30eb\u30c9\u3055\u308c\u3066\u3044\u307e\u3059\u3002'; b = 'McInvalidExtensionId=This installer was built with an invalid Chrome extension id.' },
    @{ n = 'a language file with a preprocessor line'; c = 'no preprocessor'; f = 'i18n\en.isl'; a = '[Messages]'; b = "#define X 1`n[Messages]" },
    @{ n = 'a language file overrides another standard message'; c = 'only the finished page'; f = 'i18n\en.isl'; a = '[CustomMessages]'; b = "WelcomeLabel1=Hi`n[CustomMessages]" },
    @{ n = 'a stale text no code uses'; c = 'no stale text'; f = 'i18n\en.isl'; a = '[CustomMessages]'; b = "[CustomMessages]`nMcUnused=x" },
    @{ n = 'English is no longer the fallback (first)'; c = 'English first'; f = 'memocampus.iss'; a = "Name: ""english""; MessagesFile: ""compiler:Default.isl,i18n\en.isl""`nName: ""japanese""; MessagesFile: ""compiler:Languages\Japanese.isl,i18n\ja.isl"""; b = "Name: ""japanese""; MessagesFile: ""compiler:Languages\Japanese.isl,i18n\ja.isl""`nName: ""english""; MessagesFile: ""compiler:Default.isl,i18n\en.isl""" },
    @{ n = 'Japanese without the custom texts'; c = 'English first'; f = 'memocampus.iss'; a = 'compiler:Languages\Japanese.isl,i18n\ja.isl'; b = 'compiler:Languages\Japanese.isl' },
    @{ n = 'language dialog turned off'; c = 'the dialog always offered'; f = 'memocampus.iss'; a = 'ShowLanguageDialog=yes'; b = 'ShowLanguageDialog=no' },
    @{ n = 'language from the locale, not the UI language'; c = 'the dialog always offered'; f = 'memocampus.iss'; a = 'LanguageDetectionMethod=uilanguage'; b = 'LanguageDetectionMethod=locale' },
    @{ n = 'an update ignores the installed language'; c = 'the dialog always offered'; f = 'memocampus.iss'; a = 'UsePreviousLanguage=yes'; b = 'UsePreviousLanguage=no' },
    @{ n = 'languages only in the product build'; c = 'same languages'; f = 'memocampus.iss'; a = "[Languages]`n"; b = "#ifndef InstallerTestMode`n[Languages]`n#endif`n" },
    @{ n = 'code branches on the language'; c = 'no code branches on the language'; f = 'memocampus.iss'; a = 'Why := Quiesce;'; b = "if ActiveLanguage = 'japanese' then Why := '' else Why := Quiesce;" },
    @{ n = 'a message box with a literal text'; c = 'no literal text is shown'; f = 'memocampus.iss'; a = 'procedure DeinitializeSetup;'; b = "procedure Warn; begin SuppressibleMsgBox('Setup did not finish.', mbError, MB_OK, IDOK); end;`nprocedure DeinitializeSetup;" },
    @{ n = 'a refusal with a literal text'; c = 'no literal text is shown'; f = 'memocampus.iss'; a = "Result := MsgText('McTrayStillRunning', []);"; b = "Result := MsgText('McTrayStillRunning', []);`n    if R.Found > 99 then`n      Result := 'Memocampus is still running.';" },
    @{ n = 'a registration conflict with a literal text'; c = 'no literal text is shown'; f = 'nmh.pas'; a = "Why := MsgText('McNmhUnknownKey', [KeyPath]);"; b = "Why := MsgText('McNmhUnknownKey', [KeyPath]);`n  if Why = '' then Why := 'Unknown key ' + KeyPath;" },
    @{ n = 'a Run description with literal text added'; c = 'no literal text is shown'; f = 'memocampus.iss'; a = 'Description: "{cm:McLaunchTray}"'; b = 'Description: "{cm:McLaunchTray} now"' },    @{ n = 'an undefined text named by the code'; c = 'every text the code names is defined'; f = 'memocampus.iss'; a = "MsgText('McTrayStillRunning', [])"; b = "MsgText('McTrayStillRuning', [])" },
    @{ n = 'a custom message read without MsgText'; c = 'read only through MsgText'; f = 'memocampus.iss'; a = "Result := MsgText('McInvalidExtensionId', [])"; b = "Result := MsgText('McInvalidExtensionId', []) + CustomMessage('McInvalidExtensionId')" },
    @{ n = 'the install failure no longer shows the error'; c = 'keeps ROLLBACK_FAILED'; f = 'memocampus.iss'; a = "MsgText('McRegisterFailed', [HostName, Err])"; b = "MsgText('McRegisterFailed', [HostName, ''])" },
    @{ n = 'the uninstall decision reads a translated text'; c = 'untranslated report'; f = 'memocampus.iss'; a = "if (Pos('KEPT', Report) > 0) or (Pos('FAILED', Report) > 0) then"; b = "if Pos(MsgText('McUninstallKept', ['']), Report) > 0 then" }
  )
  $scratch = Join-Path ([IO.Path]::GetTempPath()) ("memocampus-verify-neg-" + [guid]::NewGuid().ToString('N'))
  $missed = 0
  try {
    foreach ($m in $mutations) {
      $copy = Join-Path $scratch ([guid]::NewGuid().ToString('N'))
      New-Item -ItemType Directory -Path $copy | Out-Null
      Copy-Item -LiteralPath (Join-Path $here 'memocampus.iss'), (Join-Path $here 'nmh.pas'), (Join-Path $here 'lifecycle.pas') -Destination $copy
      Copy-Item -LiteralPath (Join-Path $here 'i18n') -Destination $copy -Recurse
      $p = Join-Path $copy $m.f
      $t = (Get-Content -LiteralPath $p -Raw -Encoding UTF8).Replace("`r`n", "`n")
      # u: the texts are written with \uXXXX escapes (Japanese), so that this script stays ASCII.
      $a = $m.a; $b = $m.b
      if ($m.ContainsKey('u') -and $m.u) { $a = [regex]::Unescape($a); $b = [regex]::Unescape($b) }
      if ($t.Split([string[]]@($a), [StringSplitOptions]::None).Count -ne 2) { "FAIL  mutation does not apply exactly once: $($m.n)"; $missed++; continue }
      [IO.File]::WriteAllText($p, $t.Replace($a, $b), (New-Object Text.UTF8Encoding($false)))
      $got = Invoke-Checks $copy $true
      # c: the check that must catch it (not merely some other check that happens to fail too).
      $meant = (-not $m.ContainsKey('c')) -or (@($got | Where-Object { $_ -like "*$($m.c)*" }).Count -gt 0)
      if (($got.Count -gt 0) -and $meant) { "PASS  caught: {0}  ({1})" -f $m.n, $(if ($m.ContainsKey('c')) { @($got | Where-Object { $_ -like "*$($m.c)*" })[0] } else { $got[0] }) } elseif ($got.Count -gt 0) { "FAIL  caught only by another check: $($m.n) ($($got -join '; '))"; $missed++ } else { "FAIL  NOT CAUGHT: $($m.n)"; $missed++ }
    }
  } finally { Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue }
  if ($missed) { "$missed mutation(s) not caught"; exit 1 }
  $global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
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
    # The contract is about the sources, so it is compiled from an ISOLATED copy of the installer sources
    # next to three throw-away placeholder files in a scratch stage\. Nothing here is a product input: the
    # scratch copy and its output live under the temp folder and are deleted, and installer\stage and
    # installer\out are never touched. (The real three executables are used only by build.ps1.)
    $src = Join-Path $out 'src'
    New-Item -ItemType Directory -Path (Join-Path $src 'stage') | Out-Null
    foreach ($f in 'memocampus.iss', 'nmh.pas', 'lifecycle.pas') { Copy-Item -LiteralPath (Join-Path $here $f) -Destination $src }
    Copy-Item -LiteralPath (Join-Path $here 'i18n') -Destination $src -Recurse
    foreach ($f in 'memocampus-native-host.exe', 'memocampus-mcp.exe', 'memocampus-tray.exe') { [IO.File]::WriteAllBytes((Join-Path $src "stage\$f"), [byte[]](0x4D, 0x5A)) }
    $iss = Join-Path $src 'memocampus.iss'
    $good = 'abcdefghijklmnopabcdefghijklmnop'
    function Compile-With([string[]]$defs) {
      # The compiler reports its errors on stderr: capture both streams, keep the text for the reason check.
      $ErrorActionPreference = 'Continue'
      $text = (& $Iscc /Q "/O$out" /Fverify $defs $iss 2>&1 | ForEach-Object { "$_" }) -join "`n"
      return @{ code = $LASTEXITCODE; text = $text }
    }
    $cases = @(
      @{ n = 'no extension id'; d = @('/DAppVersion=0.0.1'); fail = $true; why = 'ChromeExtensionId is required' },
      @{ n = 'no version'; d = @("/DChromeExtensionId=$good"); fail = $true; why = 'AppVersion is required' },
      @{ n = 'empty id'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId='); fail = $true; why = 'ChromeExtensionId is required|exactly 32' },
      @{ n = '31 characters'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmno'); fail = $true; why = 'exactly 32 characters' },
      @{ n = '33 characters'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmnopa'); fail = $true; why = 'exactly 32 characters' },
      @{ n = 'upper case'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=Abcdefghijklmnopabcdefghijklmnop'); fail = $true; why = 'lower case letters a to p' },
      @{ n = 'letter q'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmnoq'); fail = $true; why = 'lower case letters a to p' },
      @{ n = 'digit'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=abcdefghijklmnopabcdefghijklmno1'); fail = $true; why = 'lower case letters a to p' },
      @{ n = 'wildcard'; d = @('/DAppVersion=0.0.1', '/DChromeExtensionId=*bcdefghijklmnopabcdefghijklmnop'); fail = $true; why = 'lower case letters a to p' },
      @{ n = 'Web Store link in an installer test build'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DInstallerTestMode', '/DStoreListing'); fail = $true; why = 'StoreListing is for the product installer only' },
      @{ n = 'well-formed id with the Web Store link (product build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DStoreListing'); fail = $false },
      @{ n = 'well-formed id (product build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good"); fail = $false },
      @{ n = 'well-formed id (isolated test build)'; d = @('/DAppVersion=0.0.1', "/DChromeExtensionId=$good", '/DInstallerTestMode'); fail = $false }
    )
    try {
      foreach ($c in $cases) {
        $r = Compile-With $c.d
        $why = ''
        if ($c.fail) {
          # Refused AND for the stated reason: a compiler that fails for another reason (a missing input,
          # a broken script) must not pass as "the rule works".
          $ok = ($r.code -ne 0) -and ($r.text -match $c.why)
          if (-not $ok) { $why = "expected a refusal matching '$($c.why)'" }
        } else {
          $ok = ($r.code -eq 0)
          if (-not $ok) { $why = 'expected the compile to succeed' }
        }
        if (-not $ok) { $bad++ }
        "{0}  compile: {1} (exit {2})" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $c.n, $r.code
        if (-not $ok) { "      $why"; "      case: $($c.d -join ' ')"; "      compiler output:"; ($r.text -split "`n" | ForEach-Object { "        $_" }) }
      }
    } finally { Remove-Item -LiteralPath $out -Recurse -Force -ErrorAction SilentlyContinue }
  }
}
if ($bad) {
  "$bad check(s) failed"
  foreach ($n in $result) { Write-Host "FAILED  $n" }
  exit 1
}
$global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
'all checks passed'
