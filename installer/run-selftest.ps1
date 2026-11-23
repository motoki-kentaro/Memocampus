<#
.SYNOPSIS
  Compiles and runs installer\nmh-selftest.iss: the installer's Native Messaging registration code against a
  private registry key (HKCU\Software\MemocampusInstallerTest\<id>) and a scratch folder.
.DESCRIPTION
  It is not the installer: the self-test installs nothing, creates no uninstall record and exits from
  InitializeSetup. It never touches Chrome's keys. The private key and the scratch folder are removed
  at the end. Needs Inno Setup 7 (ISCC.exe); pass -Iscc if it is not in the default place.

  It runs once per installer language (/LANG=english, /LANG=japanese; Issue #75): the same checks must
  pass, with the same names, in both; every custom message of i18n\en.isl must resolve at run time in both;
  and the Japanese texts must really be the Japanese ones (not English, and in Japanese script).
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
New-Item -ItemType Directory -Path "$scratch\build" | Out-Null
$failed = 0
try {
  & $Iscc /Q "/O$scratch\build" /Fnmh-selftest (Join-Path $here 'nmh-selftest.iss')
  if ($LASTEXITCODE -ne 0) { throw 'the self-test did not compile' }
  # The custom message keys, from the English file (verify-static.ps1 checks that ja.isl has the same ones).
  $isl = Get-Content -LiteralPath (Join-Path $here 'i18n\en.isl') -Encoding UTF8
  $inCustom = $false
  $keys = foreach ($l in $isl) {
    if ($l -match '^\[(\w+)\]') { $inCustom = ($Matches[1] -eq 'CustomMessages'); continue }
    if ($inCustom -and $l -match '^(\w+)=') { $Matches[1] }
  }
  if (@($keys).Count -eq 0) { throw 'no custom messages found in i18n\en.isl' }
  $runs = @{}
  foreach ($lang in 'english', 'japanese') {
    # Each language run starts from nothing: its own scratch folder, and a private key that holds only the
    # unrelated "other" registration, which must come out unchanged.
    Remove-Item -LiteralPath "HKCU:\$rootKey" -Recurse -Force -ErrorAction SilentlyContinue
    $other = "HKCU:\$rootKey\NativeMessagingHosts\other.host"
    New-Item -Path $other -Force | Out-Null
    $files = Join-Path $scratch "files-$lang"
    New-Item -ItemType Directory -Path $files | Out-Null
    $langLog = Join-Path $scratch "result-$lang.log"
    $p = Start-Process -FilePath "$scratch\build\nmh-selftest.exe" -Wait -PassThru -ArgumentList @(
      "/LANG=$lang", "/SelfTestKey=$rootKey", "/SelfTestDir=$files", "/SelfTestLog=$langLog", "/Keys=$($keys -join ',')")
    if (-not (Test-Path -LiteralPath $langLog)) { throw "the self-test wrote no result in $lang (exit $($p.ExitCode))" }
    $lines = Get-Content -LiteralPath $langLog -Encoding UTF8
    "--- $lang"
    # The rendered texts are compared below, not printed; everything else that is not a PASS is printed.
    $lines | Where-Object { $_ -notmatch '^(PASS |msg )' } | ForEach-Object { $_ }
    $pass = @($lines | Where-Object { $_ -match '^PASS ' }).Count
    $fail = @($lines | Where-Object { $_ -match '^FAIL |^EXCEPTION|^REFUSED' }).Count
    "Pascal self-test ($lang): $pass PASS, $fail FAIL"
    if ($fail -or $pass -eq 0) { $failed++ }
    $active = @($lines | Where-Object { $_ -ceq "language $lang" }).Count -eq 1
    "{0}  the run used the language it was given ({1})" -f ($(if ($active) { 'PASS' } else { 'FAIL' })), $lang
    if (-not $active) { $failed++ }
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
    $msgs = @{}
    foreach ($l in ($lines | Where-Object { $_ -match '^msg ' })) { $k, $v = $l.Substring(4) -split '=', 2; $msgs[$k] = $v }
    $runs[$lang] = @{ lines = $lines; msgs = $msgs }
  }
  '--- both languages'
  # The language changes the texts only: the same checks, by name, pass in both languages.
  $passEn = @($runs.english.lines | Where-Object { $_ -match '^PASS ' }) -join "`n"
  $passJa = @($runs.japanese.lines | Where-Object { $_ -match '^PASS ' }) -join "`n"
  "{0}  the same checks pass in English and in Japanese" -f ($(if ($passEn -ceq $passJa) { 'PASS' } else { 'FAIL' }))
  if ($passEn -cne $passJa) { $failed++ }
  # Every text is translated: present in both runs, different, and the Japanese one in Japanese script. The
  # English one has no Japanese, except the tray's Japanese-only menu item named as shown with its gloss (F-75-01).
  $japanese = '[\u3040-\u30ff\u4e00-\u9fff]'
  $untranslated = @(foreach ($k in @($keys) + 'FinishedLabel') {
    $en = $runs.english.msgs[$k]; $ja = $runs.japanese.msgs[$k]
    if (-not ($en -and $ja -and ($en -cne $ja) -and ($ja -match $japanese) -and (($en -replace '"\u7d42\u4e86" \(Quit\)', '') -notmatch $japanese))) { $k }
  })
  "{0}  every text ({1} custom messages and FinishedLabel) differs between the languages, in Japanese script in Japanese{2}" -f `
    ($(if ($untranslated.Count -eq 0) { 'PASS' } else { 'FAIL' })), @($keys).Count, $(if ($untranslated.Count) { ': not translated: ' + ($untranslated -join ', ') } else { '' })
  if ($untranslated.Count) { $failed++ }
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
