# Measures the real Page export (incremental BLOB read, temp file, sync, by-handle install, read-back
# verification) using only its synthetic temp-Vault
# test. Build first: cargo test --release --bin memocampus-native-host --no-run (native-host).
param([Parameter(Mandatory = $true)][string]$TestBinary)
$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $TestBinary).Path
$workspacePath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetPath = Join-Path $workspacePath 'native-host\target'
if (-not $binaryPath.StartsWith($targetPath + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Use the built test harness under native-host/target only.' }
$testName = 'page_manage::export::tests::exact_128_mib_original_streams_to_an_identical_file'
$listing = & $binaryPath --list
if ($LASTEXITCODE -ne 0 -or -not ($listing -contains "${testName}: test")) { throw 'Not the Page export test harness.' }
$stdoutPath = Join-Path $workspacePath '.tmp-6-s3-export-stdout.log'
$stderrPath = Join-Path $workspacePath '.tmp-6-s3-export-stderr.log'
$measured = Start-Process -FilePath $binaryPath -ArgumentList @('--exact', $testName, '--nocapture', '--test-threads=1') -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
# Windows PowerShell 5.1 needs the handle cached before exit to retain the real ExitCode.
$null = $measured.Handle
$peak = 0L
$started = [Diagnostics.Stopwatch]::StartNew()
while (-not $measured.HasExited) {
    $measured.Refresh()
    $peak = [Math]::Max($peak, $measured.PeakWorkingSet64)
    Start-Sleep -Milliseconds 20
}
$measured.WaitForExit()
$measured.Refresh()
$started.Stop()
Get-Content -LiteralPath $stdoutPath
Get-Content -LiteralPath $stderrPath
if ($measured.ExitCode -ne 0) { throw 'Synthetic Page export failed.' }
[pscustomobject]@{ PeakWorkingSetBytes = $peak; PeakWorkingSetMiB = [Math]::Round($peak / 1MB, 2); WallSeconds = [Math]::Round($started.Elapsed.TotalSeconds, 3); Scope = 'one synthetic 128MiB seed (chunked) + export + verify in the test harness; excludes the OS dialog' }
