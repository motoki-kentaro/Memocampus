<#
.SYNOPSIS
  Builds the Windows setup program: the version, the channel and the extension id are all checked
  before anything is compiled, and nothing is left behind when a check or the compile fails.
.DESCRIPTION
  Channels (they never share an output name, folder or purpose):
    test     an INSTALLER TEST build: synthetic extension id, test AppId, test install folder, private
             registry keys, no Chrome Web Store link. Output: <OutDir>\test\Memocampus-Setup-x64-INSTALLER-TEST.exe
             with a NOT-FOR-DISTRIBUTION.txt next to it. Never to be given to anyone.
    product  the release candidate. Needs the real Web Store extension id recorded in
             installer\release-gate.json (or passed with -ExtensionId); while the id is null the build
             refuses. Output: <OutDir>\product\Memocampus-Setup-x64.exe.
  Both write SHA256SUMS.txt and build-info.json. Neither signs the program (there is no certificate
  yet); build-info.json says "signed": false. Nothing is published, tagged or uploaded here.
  The three executables must already be in installer\stage (installer\stage.ps1).
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$Version,
  [Parameter(Mandatory)][ValidateSet('test', 'product')][string]$Channel,
  [string]$ExtensionId,
  # When given (a release run), must be exactly v<Version>.
  [string]$Tag,
  [string]$OutDir,
  [string]$Iscc = 'C:\Program Files\Inno Setup 7\ISCC.exe',
  [string]$RequiredInno = '7.1.1',
  [string]$Commit
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path -Parent $here
if (-not $OutDir) { $OutDir = Join-Path $root 'dist' }
$TestExtensionId = 'abcdefghijklmnopabcdefghijklmnop'   # synthetic, test channel only, never a Web Store id

function Fail([string]$why) { throw "BUILD REFUSED: $why" }

# --- version: x.y.z, each part a plain number Windows version resources can hold -------------------------
if ($Version -notmatch '^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})$') {
  Fail "version '$Version' is not x.y.z (numbers only, no leading zeros, no suffix)"
}
foreach ($part in $Version.Split('.')) { if ([int]$part -gt 65535) { Fail "version part $part is above 65535" } }
if ($Tag -and $Tag -ne "v$Version") { Fail "tag '$Tag' does not match version '$Version' (expected v$Version)" }

# --- extension id by channel -----------------------------------------------------------------------------
$idPattern = '^[a-p]{32}$'
if ($Channel -eq 'test') {
  if ($ExtensionId -and $ExtensionId -ne $TestExtensionId) { Fail 'the test channel always uses its own synthetic extension id' }
  $ExtensionId = $TestExtensionId
} else {
  if (-not $ExtensionId) {
    $gate = Get-Content -LiteralPath (Join-Path $here 'release-gate.json') -Raw | ConvertFrom-Json
    $ExtensionId = $gate.extensionId
  }
  if (-not $ExtensionId) { Fail 'the release gate is closed: installer\release-gate.json has no extensionId (the Chrome Web Store item does not exist yet)' }
  if ($ExtensionId -cnotmatch $idPattern) { Fail "extension id '$ExtensionId' is not 32 letters a-p" }
  if ($ExtensionId -eq $TestExtensionId) { Fail 'that is the synthetic test extension id; a product installer cannot carry it' }
  if (@($ExtensionId.ToCharArray() | Sort-Object -Unique).Count -lt 6) { Fail 'that extension id looks made up (too few different letters)' }
}

# --- Inno Setup: the exact version ----------------------------------------------------------------------
if (-not (Test-Path -LiteralPath $Iscc -PathType Leaf)) { Fail "Inno Setup compiler not found: $Iscc" }
# (The compiler reports its engine version only while compiling; it is checked from that output below,
# and a program built by any other version is discarded.)

# --- stage: exactly three files ------------------------------------------------------------------------
$stage = Join-Path $here 'stage'
$want = @('memocampus-mcp.exe', 'memocampus-native-host.exe', 'memocampus-tray.exe')
$have = @(if (Test-Path -LiteralPath $stage) { Get-ChildItem -LiteralPath $stage -Force | ForEach-Object Name | Sort-Object })
if (($have -join '|') -ne ($want -join '|')) { Fail "installer\stage must hold exactly the three executables (found: $($have -join ', '))" }

# --- no Visual C++ runtime dependency ---------------------------------------------------------------------
# A clean Windows has no VCRUNTIME140.dll: the executables must link the C runtime statically
# (.cargo\config.toml of each crate). The import table of the real files is read; a dependency stops the build.
$node = Get-Command node -ErrorAction SilentlyContinue
if (-not $node) { Fail 'node is needed to read the executables'' import tables (scripts\pe-imports.mjs); not found' }
$peCheck = & node (Join-Path (Split-Path -Parent $here) 'scripts\pe-imports.mjs') --check (Get-ChildItem -LiteralPath $stage -File | ForEach-Object FullName) 2>&1
if ($LASTEXITCODE -ne 0) { Fail ("an executable needs the Visual C++ runtime (build with the crates' .cargo\config.toml, crt-static):`n" + ($peCheck -join "`n")) }

if (-not $Commit) { $Commit = (& git -C $root rev-parse HEAD 2>$null); if ($LASTEXITCODE -ne 0) { $Commit = 'unknown' } }
$dirty = [bool](& git -C $root status --porcelain --untracked-files=no 2>$null)

$ch = Join-Path $OutDir $Channel
if (Test-Path -LiteralPath $ch) { Remove-Item -LiteralPath $ch -Recurse -Force }
New-Item -ItemType Directory -Path $ch | Out-Null
$exeName = if ($Channel -eq 'product') { 'Memocampus-Setup-x64.exe' } else { 'Memocampus-Setup-x64-INSTALLER-TEST.exe' }

try {
  $defs = @("/DAppVersion=$Version", "/DChromeExtensionId=$ExtensionId", "/O$ch")
  if ($Channel -eq 'test') { $defs += '/DInstallerTestMode' } else { $defs += '/DStoreListing' }
  $ErrorActionPreference = 'Continue'   # the compiler writes warnings to stderr
  $log = (& $Iscc @defs (Join-Path $here 'memocampus.iss') 2>&1 | Out-String)
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) { Fail "the Inno Setup compile failed (exit $code)`n$log" }
  if ($log -notmatch 'Compiler engine version: Inno Setup ([0-9]+(\.[0-9]+)+)') { Fail 'could not read the Inno Setup version from the compile output' }
  $innoVersion = $Matches[1]
  if ($innoVersion -ne $RequiredInno) { Fail "Inno Setup $innoVersion built it, $RequiredInno is required" }
  $exe = Join-Path $ch $exeName
  if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Fail "the compile did not produce $exeName" }
  $fv = (Get-Item -LiteralPath $exe).VersionInfo
  if (-not $fv.FileVersion -or -not $fv.FileVersion.StartsWith($Version)) { Fail "the program's version resource says '$($fv.FileVersion)', not $Version" }
  $sha = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
  "$sha  $exeName" | Set-Content -LiteralPath (Join-Path $ch 'SHA256SUMS.txt') -Encoding ascii
  [ordered]@{
    product   = 'Memocampus for Windows'
    channel   = $Channel
    version   = $Version
    file      = $exeName
    sha256    = $sha
    gitCommit = $Commit
    workingTreeModified = $dirty
    innoSetup = $innoVersion
    signed    = $false
    forDistribution = ($Channel -eq 'product')
    storeLink = ($Channel -eq 'product')
  } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $ch 'build-info.json') -Encoding utf8
  if ($Channel -eq 'test') {
    "INSTALLER TEST BUILD - NOT FOR DISTRIBUTION.`nIt carries a synthetic Chrome extension id and test registry keys. Give it to nobody; it is not the Memocampus installer." |
      Set-Content -LiteralPath (Join-Path $ch 'NOT-FOR-DISTRIBUTION.txt') -Encoding ascii
  }
  "built $exeName  $Channel  $Version  sha256=$sha"
} catch {
  # No half-made or unchecked program is left to be picked up.
  if (Test-Path -LiteralPath $ch) { Remove-Item -LiteralPath $ch -Recurse -Force }
  throw
}
