<#
.SYNOPSIS
  Registers the Memocampus native messaging host for the current Windows user.

.DESCRIPTION
  Generates <HostName>.json from com.memocampus.vault.template.json, filling in this machine's host
  executable path and your extension ID, and points
  HKCU\Software\Google\Chrome\NativeMessagingHosts\<HostName> at it.

  This is the DEVELOPMENT registration. -HostName is one of:
    com.memocampus.vault              (default, the name development setups have always used)
    io.github.memocampus.vault.dev    (the new development name; the extension must use the same
                                       profile, see extension/host-config.js)
  The product name io.github.memocampus.vault belongs to the Windows installer and is refused here.
  Registrations under other names are never read, changed or removed.

  - Current user only (HKCU); no administrator rights; HKLM is never touched.
  - Your vault and settings (%LOCALAPPDATA%\Memocampus) are never read, changed or deleted.
  - Safe to run again. An existing registration that points at a different manifest is only
    changed with -Replace. If any step fails, the previous manifest and registration are restored.
  - Works in Windows PowerShell 5.1 and PowerShell 7. Supports -WhatIf.

.PARAMETER ExtensionId
  The 32-character ID that chrome://extensions shows for the loaded Memocampus extension.

.PARAMETER Replace
  Allow re-pointing an existing registration of the chosen host name at this manifest.

.PARAMETER HostName
  The development host name to register (see above). Default: com.memocampus.vault.

.PARAMETER RegistryBase
  The registry key that holds the host registrations. Only tests change it (to a private key).

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\install-native-host.ps1 -ExtensionId abcdefghijklmnopabcdefghijklmnop
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[a-pA-P]{32}$')]
  [string]$ExtensionId,
  [ValidateSet('com.memocampus.vault', 'io.github.memocampus.vault.dev')]
  [string]$HostName = 'com.memocampus.vault',
  [string]$BinaryPath,
  [string]$ManifestPath,
  [string]$TemplatePath,
  [switch]$Replace,
  [string]$RegistryBase = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts'
)

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
# Defaults are worked out here, not in the param block: Windows PowerShell 5.1 leaves $PSScriptRoot
# empty there when the script is started with -File.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $BinaryPath) { $BinaryPath = Join-Path $here 'target\release\memocampus-native-host.exe' }
if (-not $ManifestPath) { $ManifestPath = Join-Path $here "$HostName.json" }
if (-not $TemplatePath) { $TemplatePath = Join-Path $here 'com.memocampus.vault.template.json' }
# $HostName is a parameter (PowerShell variable names are not case sensitive).
$registryPath = "$RegistryBase\$HostName"

function Resolve-FullPath([string]$Path) {
  # Relative paths resolve against the current PowerShell location, without wildcard expansion.
  $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
}

# --- 1. Validate everything before changing anything -------------------------------------------
# Chrome extension IDs are lower case, and allowed_origins must match the origin exactly.
$extensionId = $ExtensionId.ToLowerInvariant()

$binary = Resolve-FullPath $BinaryPath
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
  throw "Native host executable not found: $binary`nBuild it first with 'cargo build --release' in native-host."
}

$manifestFile = Resolve-FullPath $ManifestPath
$manifestDir = Split-Path -Parent $manifestFile
if (-not (Test-Path -LiteralPath $manifestDir -PathType Container)) {
  throw "Manifest folder does not exist: $manifestDir"
}

$template = Get-Content -LiteralPath (Resolve-FullPath $TemplatePath) -Raw -Encoding UTF8 | ConvertFrom-Json
if ($template.name -ne 'com.memocampus.vault' -or $template.type -ne 'stdio') {
  throw "Unexpected template: name must be 'com.memocampus.vault' and type 'stdio'."
}

$manifest = [ordered]@{
  name            = $HostName
  description     = [string]$template.description
  path            = $binary
  type            = 'stdio'
  allowed_origins = @("chrome-extension://$extensionId/")
}
$json = ConvertTo-Json -InputObject $manifest -Depth 3

$previousRegistration = $null
if (Test-Path -LiteralPath $registryPath) {
  $previousRegistration = (Get-Item -LiteralPath $registryPath).GetValue('')
}
if ($previousRegistration -and
    -not [string]::Equals($previousRegistration, $manifestFile, [StringComparison]::OrdinalIgnoreCase) -and
    -not $Replace) {
  throw "$hostName is already registered for this user with another manifest:`n  $previousRegistration`nRe-run with -Replace to point it at:`n  $manifestFile"
}

if (-not $PSCmdlet.ShouldProcess("$registryPath -> $manifestFile", 'Register native messaging host')) {
  return
}

# --- 2. Write the manifest, then register; roll back on failure ---------------------------------
$temp = Join-Path $manifestDir ".$hostName.$([guid]::NewGuid().ToString('N')).tmp"
$backup = "$temp.previous"
$manifestExisted = Test-Path -LiteralPath $manifestFile
$createdKey = $false
try {
  # UTF-8 without BOM; [IO.File] works the same in Windows PowerShell 5.1 and PowerShell 7.
  [IO.File]::WriteAllText($temp, $json, (New-Object System.Text.UTF8Encoding($false)))
  if ($manifestExisted) {
    Copy-Item -LiteralPath $manifestFile -Destination $backup
  }
  Move-Item -LiteralPath $temp -Destination $manifestFile -Force

  if (-not (Test-Path -LiteralPath $registryPath)) {
    # -Force only creates missing parent keys here; an existing key is never recreated.
    New-Item -Path $registryPath -Force | Out-Null
    $createdKey = $true
  }
  Set-ItemProperty -LiteralPath $registryPath -Name '(default)' -Value $manifestFile
}
catch {
  if ($createdKey) {
    Remove-Item -LiteralPath $registryPath -ErrorAction SilentlyContinue
  }
  if ($manifestExisted -and (Test-Path -LiteralPath $backup)) {
    Move-Item -LiteralPath $backup -Destination $manifestFile -Force
  }
  elseif (-not $manifestExisted -and (Test-Path -LiteralPath $manifestFile)) {
    Remove-Item -LiteralPath $manifestFile -ErrorAction SilentlyContinue
  }
  throw
}
finally {
  foreach ($leftover in @($temp, $backup)) {
    if (Test-Path -LiteralPath $leftover) {
      Remove-Item -LiteralPath $leftover -ErrorAction SilentlyContinue
    }
  }
}

Write-Output "Registered $hostName for extension $extensionId (current user)."
Write-Output "Manifest: $manifestFile"
Write-Output "Binary:   $binary"
if ($previousRegistration -and $previousRegistration -ne $manifestFile) {
  Write-Output "Previous registration pointed at: $previousRegistration"
}
Write-Output 'Reload the extension in chrome://extensions to connect.'
