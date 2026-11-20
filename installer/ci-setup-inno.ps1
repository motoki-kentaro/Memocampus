<#
.SYNOPSIS
  CI helper: makes sure Inno Setup is available and writes ISCC_PATH to the job environment.
.DESCRIPTION
  1. An existing C:\Program Files\Inno Setup 7\ISCC.exe is used as it is. (build.ps1 then refuses any
     compiler that is not exactly the required version, so a wrong one is caught there.)
  2. Otherwise the installer named by the repository variables INNO_SETUP_URL and INNO_SETUP_SHA256
     is downloaded (https only), its SHA-256 is compared, and only then is it run, silently, into a
     private folder. Without those variables the step stops: nothing is fetched from a place no one
     has pinned.
  No certificate, token or secret is involved.
#>
[CmdletBinding()]
param()
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

function Publish([string]$iscc) {
  if (-not (Test-Path -LiteralPath $iscc -PathType Leaf)) { throw "ISCC.exe not found at $iscc" }
  if ($env:GITHUB_ENV) { "ISCC_PATH=$iscc" | Add-Content -LiteralPath $env:GITHUB_ENV -Encoding utf8 }
  "Inno Setup compiler: $iscc"
}

$installed = 'C:\Program Files\Inno Setup 7\ISCC.exe'
if (Test-Path -LiteralPath $installed -PathType Leaf) { Publish $installed; return }

$url = $env:INNO_SETUP_URL
$sha = $env:INNO_SETUP_SHA256
if (-not $url -or -not $sha) {
  throw 'Inno Setup 7 is not on this runner and INNO_SETUP_URL / INNO_SETUP_SHA256 are not set. Record the pinned download address and its SHA-256 as repository variables (see installer\README.md). Nothing was downloaded.'
}
if ($url -notmatch '^https://(files\.)?jrsoftware\.org/') { throw "INNO_SETUP_URL must be an https address on jrsoftware.org: $url" }
if ($sha -notmatch '^[0-9a-fA-F]{64}$') { throw 'INNO_SETUP_SHA256 must be 64 hex digits' }
$tmp = Join-Path $env:RUNNER_TEMP 'innosetup'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$setup = Join-Path $tmp 'innosetup-installer.exe'
Invoke-WebRequest -Uri $url -OutFile $setup -UseBasicParsing
$got = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash
if ($got -ne $sha.ToUpperInvariant()) {
  Remove-Item -LiteralPath $setup -Force
  throw "The downloaded Inno Setup installer has SHA-256 $got, not the pinned $sha. It was not run."
}
$dir = Join-Path $tmp 'app'
$p = Start-Process -FilePath $setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', "/DIR=$dir" -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "Inno Setup installer exited with $($p.ExitCode)" }
Publish (Join-Path $dir 'ISCC.exe')
