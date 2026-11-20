<#
.SYNOPSIS
  Fills installer\stage with exactly the three release executables the installer ships.
.DESCRIPTION
  Copies three named files, nothing else. Stops if one is missing (no placeholder is made) and
  stops if the stage folder holds anything but the three. Never reads or writes the vault,
  settings or registry, and never copies a build folder as a whole.
#>
[CmdletBinding()]
param([string]$Root)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
if (-not $Root) { $Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path) }
$sources = [ordered]@{
  'memocampus-native-host.exe' = 'native-host\target\release\memocampus-native-host.exe'
  'memocampus-mcp.exe'         = 'mcp-server\target\release\memocampus-mcp.exe'
  'memocampus-tray.exe'        = 'tray\target\release\memocampus-tray.exe'
}
foreach ($name in $sources.Keys) {
  $from = Join-Path $Root $sources[$name]
  if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
    throw "Missing build output: $from`nBuild it first (see installer\README.md)."
  }
}
$stage = Join-Path $PSScriptRoot 'stage'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
foreach ($name in $sources.Keys) {
  Copy-Item -LiteralPath (Join-Path $Root $sources[$name]) -Destination (Join-Path $stage $name)
}
$found = @(Get-ChildItem -LiteralPath $stage -Force | ForEach-Object Name | Sort-Object)
$want = @($sources.Keys | Sort-Object)
if (($found -join '|') -ne ($want -join '|')) {
  throw "Stage content is not exactly the three executables: $($found -join ', ')"
}
Get-ChildItem -LiteralPath $stage | ForEach-Object { '{0}  {1:N0} bytes' -f $_.Name, $_.Length }
