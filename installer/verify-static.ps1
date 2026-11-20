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
  $all = $iss + "`n" + $pas

  # --- Slice 3B-1 contract (unchanged) -------------------------------------------------------------
  $sections = @([regex]::Matches($iss, '(?m)^\[(\w+)\]') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
  Check 'sections are only Setup, Languages, Files, Icons, UninstallDelete, Code' (($sections -join ',') -eq 'Code,Files,Icons,Languages,Setup,UninstallDelete')
  Check 'no [Registry], [Run], [UninstallRun], [InstallDelete], [Dirs] section' ($iss -notmatch '(?im)^\[(Registry|Run|UninstallRun|InstallDelete|Dirs)\]')
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
  Check 'no [Run] and no Run key: the installer neither starts programs nor registers logon start' ($all -notmatch '(?i)CurrentVersion\\Run|StartupApproved|\bExec\s*\(|\bShellExec\s*\(')

  # --- Host names -------------------------------------------------------------------------------------
  Check 'product host name is io.github.memocampus.vault (one define)' (([regex]::Matches($iss, '(?m)^\s*#define ProductHostName "([^"]+)"') | ForEach-Object { $_.Groups[1].Value }) -join ',' -eq 'io.github.memocampus.vault')
  Check 'the host name used in code comes from that define' ($iss -match "(?m)^\s*HostName = '\{#ProductHostName\}';")
  Check 'the installer never names the development or legacy host' ($all -notmatch 'com\.memocampus\.vault|memocampus\.vault\.dev')
  Check 'manifest file is {app}\<host name>.json, only there' (($iss -match "ExpandConstant\('\{app\}\\' \+ HostName \+ '\.json'\)") -and ($iss -match '(?m)^Type: files; Name: "\{app\}\\\{#ProductHostName\}\.json"\s*$'))
  Check 'UninstallDelete removes only that manifest' (@([regex]::Matches($iss, '(?m)^Type:.*$')).Count -eq 1)

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
  Check 'the installer does not run the extension or Chrome' ($all -notmatch '(?i)chrome\.exe|webstore|chromewebstore')

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
  $registered = [regex]::Match($iss, "NmhRegister\(HostKeyPath, ManifestFile, HostName, ExpandConstant\('\{app\}\\([^'\\]+)'\), BuiltInExtensionId, Err\)")
  Check 'NmhRegister is given {app}\memocampus-native-host.exe' ($registered.Success -and ($registered.Groups[1].Value -ceq 'memocampus-native-host.exe'))
  $placed = @([regex]::Matches($iss, '(?m)^Source:\s*"stage\\([^"]+)";\s*DestDir:\s*"\{app\}"') | ForEach-Object { $_.Groups[1].Value })
  Check 'the executable given to NmhRegister is one that [Files] places in {app}' ($registered.Success -and ($placed -ccontains $registered.Groups[1].Value))

  # --- Ownership and failure handling ----------------------------------------------------------------
  Check 'an existing registration is checked before install and again before writing' (($iss -match 'NmhCheckRegistration\(HostKeyPath, ManifestFile, Why\)') -and ($pas -match 'if not NmhCheckRegistration\(KeyPath, ManifestPath, Why\) then begin'))
  Check 'a foreign registration is refused, not overwritten' (($pas -match 'NmhSamePath\(Current, ManifestPath\)') -and ($pas -match 'will not replace it'))
  Check 'rollback re-checks ownership before touching the registry' ($pas -match 'if Has and not NmhSamePath\(Current, ManifestPath\) and not \(Snap\.HadValue and NmhSamePath\(Current, Snap\.Value\)\) then')
  Check 'a failed rollback is reported' ($pas -match "' ROLLBACK_FAILED: ' \+ RollbackErr")
  Check 'registry deletes exist only in the rollback function' ((([regex]::Match($pas, '(?s)function NmhRollback.*?\nend;')).Value -match 'RegDeleteValue') -and (@([regex]::Matches(($pas -replace '(?s)function NmhRollback.*?\nend;', ''), 'RegDelete')).Count -eq 0))
  Check 'the installer does not unregister (uninstall removal is slice 3B-3)' (($iss -notmatch 'CurUninstallStepChanged|InitializeUninstall|usUninstall|usPostUninstall'))
  Check 'test hooks are not used by the installer' ($iss -notmatch 'NmhFailAt|NmhFailRollback')

  # --- Data the installer must never touch -----------------------------------------------------------------
  Check 'nothing refers to vault, policy, settings, token or the data folder' ($all -notmatch '(?i)config\.json|policy\.json|tray-settings|mcp-http-token|\.sqlite|\{localappdata\}\\Memocampus\\|\\Memocampus\\logs|DeleteDir|RemoveDir|DelTree')
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
  } elseif (-not $quiet) { 'SKIP  stage folder absent (run stage.ps1)' }
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
    @{ n = 'a [Registry] section'; f = 'memocampus.iss'; a = '[UninstallDelete]'; b = "[Registry]`nRoot: HKCU; Subkey: ""Software\X""; Flags: uninsdeletekey`n[UninstallDelete]" },
    @{ n = 'unregistering at uninstall (3B-3 scope)'; f = 'memocampus.iss'; a = 'procedure CurStepChanged(CurStep: TSetupStep);'; b = "procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep); begin end;`nprocedure CurStepChanged(CurStep: TSetupStep);" },
    @{ n = 'vault/config reference'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Cfg = 'config.json';`nfunction NmhOwnsRegistration" },
    @{ n = 'dev host named in the installer'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Dev = 'com.memocampus.vault';`nfunction NmhOwnsRegistration" },
    @{ n = 'test mode uses Chrome''s key'; f = 'memocampus.iss'; a = '#define NmhKeyPath "Software\MemocampusInstallerTest\NativeMessagingHosts\" + ProductHostName'; b = '#define NmhKeyPath "Software\Google\Chrome\NativeMessagingHosts\" + ProductHostName' },
    @{ n = 'program started by the installer'; f = 'memocampus.iss'; a = '[UninstallDelete]'; b = "[Run]`nFilename: ""{app}\memocampus-tray.exe""`n[UninstallDelete]" },
    @{ n = 'registered executable path renamed'; f = 'memocampus.iss'; a = "ExpandConstant('{app}\memocampus-native-host.exe'), BuiltInExtensionId"; b = "ExpandConstant('{app}\memocampus-nativehost.exe'), BuiltInExtensionId" },
    @{ n = 'registered executable moved out of {app}'; f = 'memocampus.iss'; a = "ExpandConstant('{app}\memocampus-native-host.exe'), BuiltInExtensionId"; b = "ExpandConstant('{app}\bin\memocampus-native-host.exe'), BuiltInExtensionId" },
    @{ n = 'a fake extension id literal'; f = 'nmh.pas'; a = 'function NmhOwnsRegistration'; b = "const Fake = 'abcdefghijklmnopabcdefghijklmnop';`nfunction NmhOwnsRegistration" }
  )
  $scratch = Join-Path ([IO.Path]::GetTempPath()) ("memocampus-verify-neg-" + [guid]::NewGuid().ToString('N'))
  $missed = 0
  try {
    foreach ($m in $mutations) {
      $copy = Join-Path $scratch ([guid]::NewGuid().ToString('N'))
      New-Item -ItemType Directory -Path $copy | Out-Null
      Copy-Item -LiteralPath (Join-Path $here 'memocampus.iss'), (Join-Path $here 'nmh.pas') -Destination $copy
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
if ($bad) { "$bad check(s) failed"; exit 1 }
'all checks passed'
