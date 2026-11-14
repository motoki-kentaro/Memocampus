# Measures the real Page receiver + incremental SQLite COMMIT using only its synthetic temp-Vault
# test. Build first: cargo test --release --bin memocampus-native-host --no-run (native-host).
param([Parameter(Mandatory = $true)][string]$TestBinary)
$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $TestBinary).Path
$workspacePath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetPath = Join-Path $workspacePath 'native-host\target'
if (-not $binaryPath.StartsWith($targetPath + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Use the built test harness under native-host/target only.' }
$testName = 'page_protocol::tests::exact_128_mib_transfer_prepare_commit_and_streaming_verify'
$listing = & $binaryPath --list
if ($LASTEXITCODE -ne 0 -or -not ($listing -contains "${testName}: test")) { throw 'Not the Page transfer test harness.' }
$stdoutPath = Join-Path $workspacePath '.tmp-6-memory-stdout.log'
$stderrPath = Join-Path $workspacePath '.tmp-6-memory-stderr.log'
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
if ($measured.ExitCode -ne 0) { throw 'Synthetic Page transfer failed.' }
[pscustomobject]@{ PeakWorkingSetBytes = $peak; PeakWorkingSetMiB = [Math]::Round($peak / 1MB, 2); WallSeconds = [Math]::Round($started.Elapsed.TotalSeconds, 3); Scope = 'one synthetic 128MiB Host transfer/store/verify; excludes Chrome and Native Messaging JSON framing' }
