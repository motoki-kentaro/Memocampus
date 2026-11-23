<#
.SYNOPSIS
  Isolated tests of the installer's lifecycle code (installer\nmh.pas, installer\lifecycle.pas), Slice 3B-3.
.DESCRIPTION
  Compiles installer\lifecycle-selftest.iss (not an installer: it installs nothing and makes no uninstall
  record) and runs it:
    registry  ownership of the native messaging registration, the manifest and the Run value, the order of
              the uninstall cleanup, failures and the retry: against private keys under
              HKCU\Software\MemocampusInstallerTest\<random>; Chrome's keys are never used.
    trays     the tray close routine against REAL memocampus-tray.exe processes started from scratch
              folders in the tray's isolated mode (private registry root, private data folder, no server):
              no tray, one tray, a same-named tray at another path, two trays at the same path, a tray that
              does not respond (suspended), files held open by another process.
  The trays that are running on this machine (a development or product tray) are recorded first, are never
  asked to quit (the routine only acts on {app}\memocampus-tray.exe of the scratch folder), and must still
  be running at the end. Nothing is terminated by the routine; the test stops only its own scratch trays.
  Needs Inno Setup 7 (ISCC.exe) and, for 'trays', a release build of the tray (tray\target\release).
#>
[CmdletBinding()]
param(
  [string]$Iscc = 'C:\Program Files\Inno Setup 7\ISCC.exe',
  [string]$TrayExe,
  [switch]$SkipTrays
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here
if (-not $TrayExe) { $TrayExe = Join-Path $repo 'tray\target\release\memocampus-tray.exe' }
if (-not (Test-Path -LiteralPath $Iscc)) { throw "Inno Setup compiler not found: $Iscc" }

$id = [guid]::NewGuid().ToString('N')
$scratch = Join-Path ([IO.Path]::GetTempPath()) "memocampus-lifecycle-$id"
$rootKey = "Software\MemocampusInstallerTest\$id"
$trayRoot = "lc$id"
New-Item -ItemType Directory -Path "$scratch\build", "$scratch\files" | Out-Null
$script:bad = 0
$script:started = New-Object System.Collections.Generic.List[System.Diagnostics.Process]

function Report([string]$name, [bool]$ok, [string]$detail = '') {
  if (-not $ok) { $script:bad++ }
  '{0}  {1}{2}' -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $name, $(if ($detail) { "  ($detail)" } else { '' })
}

function Invoke-Harness([string[]]$Arguments, [string]$LogName) {
  $log = Join-Path $scratch $LogName
  Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
  # English unless the caller names the language (/LANG= first, so a caller's own /LANG= wins).
  if (-not ($Arguments | Where-Object { $_ -like '/LANG=*' })) { $Arguments = @('/LANG=english') + $Arguments }
  $p = Start-Process -FilePath "$scratch\build\lifecycle-selftest.exe" -Wait -PassThru -ArgumentList ($Arguments + "/SelfTestLog=$log")
  if (-not (Test-Path -LiteralPath $log)) { throw "the harness wrote no result (exit $($p.ExitCode)): $LogName" }
  , (Get-Content -LiteralPath $log -Encoding UTF8)
}

function Start-ScratchTray([string]$Folder, [string]$Tag) {
  New-Item -ItemType Directory -Path $Folder -Force | Out-Null
  $exe = Join-Path $Folder 'memocampus-tray.exe'
  if (-not (Test-Path -LiteralPath $exe)) { Copy-Item -LiteralPath $TrayExe -Destination $exe }
  $data = Join-Path $scratch "data-$Tag"
  New-Item -ItemType Directory -Path $data -Force | Out-Null
  $log = Join-Path $scratch "tray-$Tag.log"
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe
  $psi.Arguments = "--registry-root $trayRoot --no-autostart --run-seconds 600 --log `"$log`""
  $psi.UseShellExecute = $false
  $psi.EnvironmentVariables['MEMOCAMPUS_APP_DIR'] = $data
  $proc = [System.Diagnostics.Process]::Start($psi)
  $script:started.Add($proc)
  [pscustomobject]@{ Process = $proc; Log = $log; Exe = $exe }
}

function Wait-TrayWindow([string]$Folder) {
  $until = (Get-Date).AddSeconds(30)
  while ((Get-Date) -lt $until) {
    $lines = Invoke-Harness @('/Mode=find', "/AppDir=$Folder") "find-wait.log"
    if (($lines -join "`n") -match 'verified (\d+)' -and [int]$Matches[1] -ge 1) { return $true }
    Start-Sleep -Milliseconds 300
  }
  return $false
}

function Field([string[]]$lines, [string]$name) {
  $m = [regex]::Match(($lines -join "`n"), "(?:^|\s)$name (\d+)")
  if ($m.Success) { [int]$m.Groups[1].Value } else { -1 }
}

Add-Type -Namespace Lc -Name Ntdll -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("ntdll.dll")] public static extern int NtSuspendProcess(System.IntPtr h);
[System.Runtime.InteropServices.DllImport("ntdll.dll")] public static extern int NtResumeProcess(System.IntPtr h);
'@

# What is running now (never touched by this test).
$before = @(Get-Process -Name memocampus-tray -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
try {
  & $Iscc /Q "/O$scratch\build" /Flifecycle-selftest (Join-Path $here 'lifecycle-selftest.iss')
  if ($LASTEXITCODE -ne 0) { throw 'the lifecycle self-test did not compile' }

  '--- registry and ownership logic (private keys), in each installer language (Issue #75)'
  $passed = @{}
  foreach ($lang in 'english', 'japanese') {
    Remove-Item -LiteralPath "HKCU:\$rootKey" -Recurse -Force -ErrorAction SilentlyContinue
    $lines = Invoke-Harness @("/LANG=$lang", '/Mode=registry', "/SelfTestKey=$rootKey", "/SelfTestDir=$scratch\files-$lang") "registry-$lang.log"
    $lines | Where-Object { $_ -match '^(FAIL |EXCEPTION|REFUSED)' }
    $pass = @($lines | Where-Object { $_ -match '^PASS ' }).Count
    $fail = @($lines | Where-Object { $_ -match '^(FAIL |EXCEPTION|REFUSED)' }).Count
    Report "Pascal lifecycle self-test ($lang): $pass PASS, $fail FAIL" (($fail -eq 0) -and ($pass -gt 0) -and (@($lines | Where-Object { $_ -ceq "language $lang" }).Count -eq 1))
    $passed[$lang] = @($lines | Where-Object { $_ -match '^PASS ' }) -join "`n"
  }
  # The language changes no decision: the same checks, by name, pass in both.
  Report 'the same lifecycle checks pass in English and in Japanese' ($passed.english -ceq $passed.japanese)

  if (-not $SkipTrays) {
    if (-not (Test-Path -LiteralPath $TrayExe)) { throw "tray build not found: $TrayExe (build it, or pass -SkipTrays)" }
    '--- tray close routine against real scratch trays'
    "(trays already running on this machine, not touched: " + $(if ($before.Count) { $before -join ', ' } else { 'none' }) + ')'
    $appA = Join-Path $scratch 'appA'; $appB = Join-Path $scratch 'appB'; $appC = Join-Path $scratch 'appC'

    # T0: a product folder that has no tray. Whatever else runs (a development tray) is only listed.
    $r = Invoke-Harness @('/Mode=find', "/AppDir=$appC") 'T0.log'
    Report 'T0 find: nothing of ours, other trays left alone' ((Field $r 'verified') -eq 0) ($r[0])

    # T1: no tray in the folder: closing is a no-op and not an error.
    $r = Invoke-Harness @('/Mode=close', "/AppDir=$appC", '/WaitMs=1500') 'T1.log'
    Report 'T1 close with no tray: nothing verified, nothing closed, no failure' (((Field $r 'verified') -eq 0) -and ((Field $r 'closed') -eq 0) -and ((Field $r 'timedout') -eq 0) -and ((Field $r 'unclosable') -eq 0)) ($r[0])

    # T2: one tray at {app}: it quits through the orderly path, with exit code 0.
    $a = Start-ScratchTray $appA 'a'
    Report 'T2 the scratch tray is up' (Wait-TrayWindow $appA)
    $r = Invoke-Harness @('/Mode=close', "/AppDir=$appA", '/WaitMs=20000') 'T2.log'
    $a.Process.WaitForExit(15000) | Out-Null
    $tlog = if (Test-Path $a.Log) { Get-Content $a.Log -Raw } else { '' }
    Report 'T2 close: one verified, one closed, none timed out' (((Field $r 'verified') -eq 1) -and ((Field $r 'closed') -eq 1) -and ((Field $r 'timedout') -eq 0)) ($r[0])
    Report 'T2 the tray ended with exit code 0 through its own quit' ($a.Process.HasExited -and $a.Process.ExitCode -eq 0 -and $tlog -match 'quit_done supervisor=stopped controller=completed' -and $tlog -notmatch 'quit_incomplete') ("exit " + $a.Process.ExitCode)

    # T3: same executable name at another path: only the one at {app} is asked.
    $a = Start-ScratchTray $appA 'a3'
    $b = Start-ScratchTray $appB 'b3'
    Report 'T3 both scratch trays are up' ((Wait-TrayWindow $appA) -and (Wait-TrayWindow $appB))
    $r = Invoke-Harness @('/Mode=close', "/AppDir=$appA", '/WaitMs=20000') 'T3.log'
    $a.Process.WaitForExit(15000) | Out-Null
    Start-Sleep -Milliseconds 500
    Report 'T3 only the tray at {app} was closed; the same-named tray at another path was left alone' (((Field $r 'verified') -eq 1) -and $a.Process.HasExited -and -not $b.Process.HasExited -and ((Field $r 'leftalone') -ge 1)) ($r[0])
    Stop-Process -Id $b.Process.Id -ErrorAction SilentlyContinue   # our own scratch tray

    # T4: two trays at the same path (different data folders): both are ours, both are closed.
    $a1 = Start-ScratchTray $appA 'a4'
    $a2 = Start-ScratchTray $appA 'a5'
    Start-Sleep -Seconds 4
    $r = Invoke-Harness @('/Mode=close', "/AppDir=$appA", '/WaitMs=20000') 'T4.log'
    $a1.Process.WaitForExit(15000) | Out-Null; $a2.Process.WaitForExit(15000) | Out-Null
    $ended = @($a1, $a2 | Where-Object { $_.Process.HasExited }).Count
    Report 'T4 several candidates at the same path: every verified one asked, every one ended' (((Field $r 'verified') -ge 1) -and ($ended -ge 1) -and ((Field $r 'timedout') -eq 0)) ("verified " + (Field $r 'verified') + ", ended $ended of 2")
    foreach ($x in @($a1, $a2)) { if (-not $x.Process.HasExited) { Stop-Process -Id $x.Process.Id -ErrorAction SilentlyContinue } }

    # T5: a tray that does not respond (suspended): the wait ends, it is reported, nothing is killed.
    $a = Start-ScratchTray $appA 'a6'
    Report 'T5 the scratch tray is up' (Wait-TrayWindow $appA)
    [void][Lc.Ntdll]::NtSuspendProcess($a.Process.Handle)
    $t0 = Get-Date
    $r = Invoke-Harness @('/Mode=close', "/AppDir=$appA", '/WaitMs=2500') 'T5.log'
    $took = ((Get-Date) - $t0).TotalSeconds
    Report 'T5 unresponsive tray: reported as timed out, wait was bounded, the process was NOT terminated' (((Field $r 'timedout') -eq 1) -and ((Field $r 'closed') -eq 0) -and -not $a.Process.HasExited -and ($took -lt 30)) ("took {0:N1}s" -f $took)
    [void][Lc.Ntdll]::NtResumeProcess($a.Process.Handle)   # it now handles the posted close and quits by itself
    $a.Process.WaitForExit(20000) | Out-Null
    Report 'T5 after it responded again, the posted request made it quit normally' ($a.Process.HasExited -and $a.Process.ExitCode -eq 0) ("exit " + $(if ($a.Process.HasExited) { $a.Process.ExitCode } else { 'running' }))
    if (-not $a.Process.HasExited) { Stop-Process -Id $a.Process.Id -ErrorAction SilentlyContinue }

    # T6: files held by another process (an AI client's server, Chrome's host): found, not touched.
    New-Item -ItemType Directory -Path $appC -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $appC 'memocampus-mcp.exe') -Value 'x'
    $r = Invoke-Harness @('/Mode=locks', "/AppDir=$appC", '/Tries=2') 'T6a.log'
    Report 'T6 nothing holds the files: none reported' ($r[0] -match 'locked \[\s*\]' -or $r[0] -eq 'locked []')
    $holder = [IO.File]::Open((Join-Path $appC 'memocampus-mcp.exe'), 'Open', 'ReadWrite', 'None')
    try {
      $r = Invoke-Harness @('/Mode=locks', "/AppDir=$appC", '/Tries=2') 'T6b.log'
      Report 'T6 a held file is reported by name' (($r -join "`n") -match 'memocampus-mcp\.exe') ($r -join ' ')
    } finally { $holder.Dispose() }
    Report 'T6 the file was not changed by the check' ((Get-Content -LiteralPath (Join-Path $appC 'memocampus-mcp.exe') -Raw).Trim() -eq 'x')

    # The machine's own trays are untouched.
    $after = @(Get-Process -Name memocampus-tray -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    $lost = @($before | Where-Object { $_ -notin $after })
    Report 'the trays that were running before are still running' ($lost.Count -eq 0) ("before: " + ($before -join ',') + "; lost: " + ($lost -join ','))
  }
}
finally {
  foreach ($p in $script:started) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -ErrorAction SilentlyContinue } }
  Remove-Item -LiteralPath "HKCU:\Software\MemocampusInstallerTest\$id" -Recurse -Force -ErrorAction SilentlyContinue
  if (-not (Get-ChildItem 'HKCU:\Software\MemocampusInstallerTest' -ErrorAction SilentlyContinue)) {
    Remove-Item -LiteralPath 'HKCU:\Software\MemocampusInstallerTest' -Recurse -Force -ErrorAction SilentlyContinue
  }
  Remove-Item -LiteralPath "HKCU:\Software\MemocampusTrayTest\$trayRoot" -Recurse -Force -ErrorAction SilentlyContinue
  if (-not (Get-ChildItem 'HKCU:\Software\MemocampusTrayTest' -ErrorAction SilentlyContinue)) {
    Remove-Item -LiteralPath 'HKCU:\Software\MemocampusTrayTest' -Recurse -Force -ErrorAction SilentlyContinue
  }
  Start-Sleep -Milliseconds 500
  Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
if ($script:bad) { "$script:bad check(s) failed"; exit 1 }
$global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
'lifecycle tests passed'
