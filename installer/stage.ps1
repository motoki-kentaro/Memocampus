<#
.SYNOPSIS
  Fills installer\stage with exactly the three release executables the installer ships.
.DESCRIPTION
  Copies three named files, nothing else. Stops if one is missing (no placeholder is made), stops if
  one was last written before -NotOlderThan (a stale build is never staged), and stops if the stage
  folder holds anything but the three. Never reads or writes the vault, settings or registry, and
  never copies a build folder as a whole.
  -BuildDir  one shared cargo target directory holding release\<name>.exe (how CI builds the three
             crates). Without it each crate's own target\release folder is used.
#>
[CmdletBinding()]
param(
  [string]$Root,
  [string]$BuildDir,
  [datetime]$NotOlderThan = [datetime]::MinValue
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
if (-not $Root) { $Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path) }
$crateDirs = [ordered]@{
  'memocampus-native-host.exe' = 'native-host'
  'memocampus-mcp.exe'         = 'mcp-server'
  'memocampus-tray.exe'        = 'tray'
}
$sources = [ordered]@{}
foreach ($name in $crateDirs.Keys) {
  $from = if ($BuildDir) { Join-Path (Join-Path $BuildDir 'release') $name } else { Join-Path $Root (Join-Path $crateDirs[$name] "target\release\$name") }
  if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
    throw "Missing build output: $from`nBuild it first (see installer\README.md)."
  }
  if ((Get-Item -LiteralPath $from).LastWriteTimeUtc -lt $NotOlderThan.ToUniversalTime()) {
    throw "Stale build output (older than this build started): $from"
  }
  $sources[$name] = $from
}
$stage = Join-Path $PSScriptRoot 'stage'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
foreach ($name in $sources.Keys) {
  Copy-Item -LiteralPath $sources[$name] -Destination (Join-Path $stage $name)
}
$found = @(Get-ChildItem -LiteralPath $stage -Force | ForEach-Object Name | Sort-Object)
$want = @($sources.Keys | Sort-Object)
if (($found -join '|') -ne ($want -join '|')) {
  throw "Stage content is not exactly the three executables: $($found -join ', ')"
}
Get-ChildItem -LiteralPath $stage | ForEach-Object { '{0}  {1:N0} bytes' -f $_.Name, $_.Length }
