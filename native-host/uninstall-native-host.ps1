<#
.SYNOPSIS
  Removes the Memocampus native messaging host registration for the current Windows user.

.DESCRIPTION
  Deletes HKCU\Software\Google\Chrome\NativeMessagingHosts\<HostName>, but only when its value names
  the manifest this repository's install-native-host.ps1 generated for that host name (or -ManifestPath).
  A registration that points anywhere else (another checkout, the Windows installer's product
  registration, another tool) is left alone and reported. The manifest file, the host executable, and
  your vault and settings (%LOCALAPPDATA%\Memocampus) are left in place. Supports -WhatIf.

  -HostName is com.memocampus.vault (default) or io.github.memocampus.vault.dev. The product name
  io.github.memocampus.vault is the installer's and is refused here.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [ValidateSet('com.memocampus.vault', 'io.github.memocampus.vault.dev')]
  [string]$HostName = 'com.memocampus.vault',
  [string]$ManifestPath,
  [string]$RegistryBase = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts'
)

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
if (-not $ManifestPath) {
  $ManifestPath = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "$HostName.json"
}
$registryPath = "$RegistryBase\$HostName"

if (-not (Test-Path -LiteralPath $registryPath)) {
  Write-Output "$HostName is not registered for this user; nothing to remove."
  return
}
$expected = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ManifestPath)
$current = (Get-Item -LiteralPath $registryPath).GetValue('')
if (-not ($current -is [string]) -or -not [string]::Equals($current, $expected, [StringComparison]::OrdinalIgnoreCase)) {
  Write-Output "$HostName is registered for another manifest, so it was NOT removed:"
  Write-Output "  registered: $current"
  Write-Output "  this repo:  $expected"
  Write-Output 'Remove it yourself, or pass -ManifestPath to name the manifest you registered.'
  exit 1
}
if ($PSCmdlet.ShouldProcess($registryPath, 'Remove native messaging host registration')) {
  Remove-Item -LiteralPath $registryPath -Recurse
  Write-Output "Removed the $HostName registration for this user."
  Write-Output 'Your vault and settings in %LOCALAPPDATA%\Memocampus were not touched.'
}
