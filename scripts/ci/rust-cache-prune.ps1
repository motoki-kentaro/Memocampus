# Issue #93 S2: before the Windows Rust CI build cache is saved, removes from the cargo target dir everything that is
# not a third-party build output, so the cache never carries this repository's own crates, test programs or release
# executables, and a restored cache can only save the time of building dependencies (OpenSSL, SQLCipher, crates).
# Cargo rebuilds what is removed here. Handles cargo's build-dir layout (<profile>/build/<package>/<hash>/) and the older
# one (<profile>/deps, <profile>/.fingerprint).
param([Parameter(Mandatory = $true)][string]$TargetDir)
$ErrorActionPreference = 'Stop'

$TargetDir = (Resolve-Path -LiteralPath $TargetDir).Path
# Only ever a cargo target dir.
if (-not (Test-Path -LiteralPath (Join-Path $TargetDir 'CACHEDIR.TAG') -PathType Leaf)) { throw "$TargetDir is not a cargo target dir (no CACHEDIR.TAG)" }

function Get-SizeMB([string]$path) {
  $sum = (Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
  [math]::Round(($sum ?? 0) / 1MB)
}
# This repository's packages and library names (every CI crate's package is memocampus-*; page-extract's library is page_extract).
$own = '^(lib)?(memocampus[-_]|page_extract)'
$removed = 0
function Remove-Entry($item) {
  Remove-Item -LiteralPath $item.FullName -Recurse -Force
  $script:removed++
}

$before = Get-SizeMB $TargetDir
Get-ChildItem -LiteralPath $TargetDir -Directory -Force | Where-Object { $_.Name -eq 'tmp' } | ForEach-Object { Remove-Entry $_ }
foreach ($profileDir in Get-ChildItem -LiteralPath $TargetDir -Directory -Force) {
  if (-not ((Test-Path -LiteralPath (Join-Path $profileDir.FullName 'build')) -or (Test-Path -LiteralPath (Join-Path $profileDir.FullName 'deps')))) { continue }
  # Final outputs copied up into the profile dir (executables, .pdb, .rlib, .d) are this repository's.
  Get-ChildItem -LiteralPath $profileDir.FullName -File -Force | ForEach-Object { Remove-Entry $_ }
  foreach ($name in 'incremental', 'examples') {
    Get-ChildItem -LiteralPath $profileDir.FullName -Directory -Force | Where-Object { $_.Name -eq $name } | ForEach-Object { Remove-Entry $_ }
  }
  foreach ($sub in 'build', '.fingerprint') {
    $dir = Join-Path $profileDir.FullName $sub
    if (Test-Path -LiteralPath $dir) { Get-ChildItem -LiteralPath $dir -Force | Where-Object { $_.Name -match $own } | ForEach-Object { Remove-Entry $_ } }
  }
  $deps = Join-Path $profileDir.FullName 'deps'
  if (Test-Path -LiteralPath $deps) {
    # Test and binary programs are always this repository's; dependencies produce libraries only.
    $programs = @(Get-ChildItem -LiteralPath $deps -File -Force -Filter '*.exe' | ForEach-Object { $_.BaseName })
    Get-ChildItem -LiteralPath $deps -Force | Where-Object { ($_.Name -match $own) -or ($programs -contains $_.BaseName) } | ForEach-Object { Remove-Entry $_ }
  }
}
$after = Get-SizeMB $TargetDir
Write-Host ("Pruned {0}: {1} entries removed, {2} MB -> {3} MB (uncompressed)" -f $TargetDir, $removed, $before, $after)
