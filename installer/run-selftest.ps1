<#
.SYNOPSIS
  Compiles and runs installer\nmh-selftest.iss: the installer's Native Messaging registration code against a
  private registry key (HKCU\Software\MemocampusInstallerTest\<id>) and a scratch folder.
.DESCRIPTION
  It is not the installer: the self-test installs nothing, creates no uninstall record and exits from
  InitializeSetup. It never touches Chrome's keys. The private key and the scratch folder are removed
  at the end. Needs Inno Setup 7 (ISCC.exe); pass -Iscc if it is not in the default place.
#>
[CmdletBinding()]
param([string]$Iscc = 'C:\Program Files\Inno Setup 7\ISCC.exe')
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not (Test-Path -LiteralPath $Iscc)) { throw "Inno Setup compiler not found: $Iscc" }
$id = [guid]::NewGuid().ToString('N')
$scratch = Join-Path ([IO.Path]::GetTempPath()) "memocampus-nmh-selftest-$id"
$rootKey = "Software\MemocampusInstallerTest\$id"
$log = Join-Path $scratch 'result.log'
New-Item -ItemType Directory -Path "$scratch\files", "$scratch\build" | Out-Null
$failed = 0
try {
  & $Iscc /Q "/O$scratch\build" /Fnmh-selftest (Join-Path $here 'nmh-selftest.iss')
  if ($LASTEXITCODE -ne 0) { throw 'the self-test did not compile' }
  # An unrelated "other" registration next to the one under test: it must come out unchanged.
  $other = "HKCU:\$rootKey\NativeMessagingHosts\other.host"
  New-Item -Path $other -Force | Out-Null
  $p = Start-Process -FilePath "$scratch\build\nmh-selftest.exe" -Wait -PassThru -ArgumentList @(
    "/SelfTestKey=$rootKey", "/SelfTestDir=$scratch\files", "/SelfTestLog=$log")
  if (-not (Test-Path -LiteralPath $log)) { throw "the self-test wrote no result (exit $($p.ExitCode))" }
  $lines = Get-Content -LiteralPath $log -Encoding UTF8
  $lines | Where-Object { $_ -notmatch '^PASS ' } | ForEach-Object { $_ }
  $pass = @($lines | Where-Object { $_ -match '^PASS ' }).Count
  $fail = @($lines | Where-Object { $_ -match '^FAIL |^EXCEPTION|^REFUSED' }).Count
  "Pascal self-test: $pass PASS, $fail FAIL"
  if ($fail -or $pass -eq 0) { $failed++ }
  # The manifests it left, read by a real JSON parser.
  foreach ($line in ($lines | Where-Object { $_ -match '^manifest ' })) {
    $m, $exe, $ext = ($line.Substring(9)) -split '\|'
    $json = [IO.File]::ReadAllBytes($m)
    $bom = ($json.Length -ge 3 -and $json[0] -eq 0xEF -and $json[1] -eq 0xBB -and $json[2] -eq 0xBF)
    $doc = [Text.Encoding]::UTF8.GetString($json) | ConvertFrom-Json
    $ok = (-not $bom) -and $doc.name -eq 'io.github.memocampus.vault' -and $doc.type -eq 'stdio' -and
      $doc.path -ceq $exe -and @($doc.allowed_origins).Count -eq 1 -and
      $doc.allowed_origins[0] -ceq "chrome-extension://$ext/"
    "{0}  JSON parse of {1}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $m
    if (-not $ok) { $failed++ }
  }
  # The unrelated registration is intact.
  $o = (Get-Item -LiteralPath $other).GetValue('')
  "{0}  unrelated registration untouched ({1})" -f ($(if ($o -eq 'C:\other\host.json') { 'PASS' } else { 'FAIL' })), $o
  if ($o -ne 'C:\other\host.json') { $failed++ }
}
finally {
  Remove-Item -LiteralPath "HKCU:\Software\MemocampusInstallerTest\$id" -Recurse -Force -ErrorAction SilentlyContinue
  if (-not (Get-ChildItem 'HKCU:\Software\MemocampusInstallerTest' -ErrorAction SilentlyContinue)) {
    Remove-Item -LiteralPath 'HKCU:\Software\MemocampusInstallerTest' -Recurse -Force -ErrorAction SilentlyContinue
  }
  Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
if ($failed) { "$failed group(s) failed"; exit 1 }
$global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
'self-test passed'
