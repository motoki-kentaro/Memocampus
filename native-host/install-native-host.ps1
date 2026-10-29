<#
.SYNOPSIS
  Registers the Memocampus native messaging host for the current Windows user.

.DESCRIPTION
  Generates com.memocampus.vault.json from com.memocampus.vault.template.json, filling in this
  machine's host executable path and your extension ID, and points
  HKCU\Software\Google\Chrome\NativeMessagingHosts\com.memocampus.vault at it.

  - Current user only (HKCU); no administrator rights; HKLM is never touched.
  - Your vault and settings (%LOCALAPPDATA%\Memocampus) are never read, changed or deleted.
  - Safe to run again. An existing registration that points at a different manifest is only
    changed with -Replace. If any step fails, the previous manifest and registration are restored.
  - Works in Windows PowerShell 5.1 and PowerShell 7. Supports -WhatIf.

.PARAMETER ExtensionId
  The 32-character ID that chrome://extensions shows for the loaded Memocampus extension.

.PARAMETER Replace
  Allow re-pointing an existing com.memocampus.vault registration at this manifest.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File .\native-host\install-native-host.ps1 -ExtensionId abcdefghijklmnopabcdefghijklmnop
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[a-pA-P]{32}$')]
  [string]$ExtensionId,
  [string]$BinaryPath = (Join-Path $PSScriptRoot 'target\release\memocampus-native-host.exe'),
  [string]$ManifestPath = (Join-Path $PSScriptRoot 'com.memocampus.vault.json'),
  [string]$TemplatePath = (Join-Path $PSScriptRoot 'com.memocampus.vault.template.json'),
  [switch]$Replace
)

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$hostName = 'com.memocampus.vault'
$registryPath = "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName"

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
if ($template.name -ne $hostName -or $template.type -ne 'stdio') {
  throw "Unexpected template: name must be '$hostName' and type 'stdio'."
}

$manifest = [ordered]@{
  name            = $hostName
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
