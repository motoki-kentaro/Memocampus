<#
.SYNOPSIS
  Removes the Memocampus native messaging host registration for the current Windows user.

.DESCRIPTION
  Deletes only HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault.
  The generated manifest file, the host executable, and your vault and settings
  (%LOCALAPPDATA%\Memocampus) are left in place. Supports -WhatIf.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param()

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$registryPath = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault'

if (-not (Test-Path -LiteralPath $registryPath)) {
  Write-Output 'com.memocampus.vault is not registered for this user; nothing to remove.'
  return
}
if ($PSCmdlet.ShouldProcess($registryPath, 'Remove native messaging host registration')) {
  Remove-Item -LiteralPath $registryPath -Recurse
  Write-Output 'Removed the com.memocampus.vault registration for this user.'
  Write-Output 'Your vault and settings in %LOCALAPPDATA%\Memocampus were not touched.'
}
