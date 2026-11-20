<#
.SYNOPSIS
  Static checks of installer\memocampus.iss (and of the stage folder when present).
  No compiler, no install, no registry access.
#>
[CmdletBinding()]
param()
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$iss = Join-Path $PSScriptRoot 'memocampus.iss'
$text = Get-Content -LiteralPath $iss -Raw -Encoding UTF8
$code = (Get-Content -LiteralPath $iss -Encoding UTF8 | Where-Object { $_ -notmatch '^\s*;' }) -join "`n"
$failed = 0
function Check([string]$name, [bool]$ok) {
  if ($ok) { "PASS  $name" } else { "FAIL  $name"; $script:failed++ }
}
$sections = @([regex]::Matches($code, '(?m)^\[(\w+)\]') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
Check 'sections are only Setup, Languages, Files, Icons' (($sections -join ',') -eq 'Files,Icons,Languages,Setup')
Check 'AppId is the fixed GUID' ($code -match '(?m)^AppId=\{\{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A\}\s*$')
Check 'AppName and publisher are Memocampus' (($code -match '(?m)^AppName=Memocampus\s*$') -and ($code -match '(?m)^AppPublisher=Memocampus\s*$'))
Check 'AppVersion comes from /DAppVersion and is required' (($code -match '(?m)^AppVersion=\{#AppVersion\}\s*$') -and ($text -match '#ifndef AppVersion\s+#error'))
Check 'per user, no administrator, no HKLM' (($code -match '(?m)^PrivilegesRequired=lowest\s*$') -and ($code -notmatch 'PrivilegesRequiredOverridesAllowed') -and ($code -notmatch 'HKLM'))
Check 'install dir is LOCALAPPDATA\Programs\Memocampus and fixed' (($code -match '(?m)^DefaultDirName=\{localappdata\}\\Programs\\Memocampus\s*$') -and ($code -match '(?m)^DisableDirPage=yes\s*$') -and ($code -match '(?m)^UsePreviousAppDir=no\s*$'))
Check 'x64 only' (($code -match '(?m)^ArchitecturesAllowed=x64compatible\s*$') -and ($code -match '(?m)^ArchitecturesInstallIn64BitMode=x64compatible\s*$'))
Check 'output name is Memocampus-Setup-x64' ($code -match '(?m)^OutputBaseFilename=Memocampus-Setup-x64\s*$')
Check 'running programs are not closed or restarted' (($code -match '(?m)^CloseApplications=no\s*$') -and ($code -match '(?m)^RestartApplications=no\s*$'))
$files = @([regex]::Matches($code, '(?m)^Source:\s*"([^"]+)"') | ForEach-Object { $_.Groups[1].Value } | Sort-Object)
Check 'Files lists exactly the three stage executables' (($files -join '|') -eq 'stage\memocampus-mcp.exe|stage\memocampus-native-host.exe|stage\memocampus-tray.exe')
Check 'no wildcard and no recursion in Files' (($code -notmatch '(?m)^Source:.*[\*\?]') -and ($code -notmatch 'recursesubdirs'))
$icons = @([regex]::Matches($code, '(?m)^Name:\s*"\{group\}[^"]*".*$') | ForEach-Object { $_.Value })
Check 'one Start menu icon, to the tray, no parameters' (($icons.Count -eq 1) -and ($icons[0] -match 'Filename:\s*"\{app\}\\memocampus-tray\.exe"') -and ($icons[0] -notmatch 'Parameters|WorkingDir'))
Check 'no registry, run, delete, code or environment' (($code -notmatch '(?i)^\[(Registry|Run|UninstallRun|InstallDelete|UninstallDelete|Code|Dirs)\]') -and ($code -notmatch 'MEMOCAMPUS_APP_DIR|ChangesEnvironment|--selftest|--headless|--run-seconds|--mcp-exe|--registry-root'))
Check 'nothing refers to the data folder' ($code -notmatch '(?i)localappdata\}\\Memocampus')
$stage = Join-Path $PSScriptRoot 'stage'
if (Test-Path -LiteralPath $stage) {
  $names = @(Get-ChildItem -LiteralPath $stage -Force -Recurse | ForEach-Object Name | Sort-Object)
  Check 'stage holds exactly the three executables' (($names -join '|') -eq 'memocampus-mcp.exe|memocampus-native-host.exe|memocampus-tray.exe')
} else { 'SKIP  stage folder absent (run stage.ps1)' }
if ($failed) { "$failed check(s) failed"; exit 1 }
'all checks passed'
