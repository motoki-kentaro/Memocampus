# Issue #93 S2: the key of the Windows Rust CI build cache (target dir + Cargo registry).
#
# The key names everything that changes what the cached third-party build outputs (OpenSSL, SQLCipher, crates) are:
#   RUST_CACHE_VERSION  bumped by hand to drop every cache at once (e.g. one found to be bad)
#   runner OS image     ImageOS (e.g. win25), not the weekly image version: the tools it carries are named below
#   rustc               release and commit hash (rustc -Vv), host must be x86_64-pc-windows-msvc
#   MSVC toolset        the default VC tools version of the newest Visual Studio with C++ tools (what cc picks)
#   Windows SDK         the newest installed Windows 10/11 SDK
#   Perl                the version of $env:PERL (it runs OpenSSL's Configure)
#   environment         every variable that can change a build: RUSTFLAGS, CARGO_*, RUSTC*, CC/CFLAGS, OPENSSL_*, ...
#   inputs              Cargo.toml, Cargo.lock and .cargo/config.toml of each CI crate (a missing file counts too)
# Debug and release outputs share one entry: both profiles are fixed by the Cargo.toml files above.
#
# If any part cannot be read, the run is not cacheable: nothing is restored or saved and the job builds from scratch.
# Writes key, cacheable and cargo-home to $env:GITHUB_OUTPUT (or prints them when that is not set).
param(
  [string]$Root = (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)),
  [string[]]$Crates = @('native-host', 'visible-store', 'mcp-server', 'page-extract', 'tray'),
  [string]$Version = $env:RUST_CACHE_VERSION
)
$ErrorActionPreference = 'Stop'

function Get-Sha256Hex([string]$text) {
  $sha = [System.Security.Cryptography.SHA256]::Create()
  try { -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($text)) | ForEach-Object { $_.ToString('x2') }) }
  finally { $sha.Dispose() }
}
# Keeps a key part to characters that are safe in a cache key.
function Get-Part([string]$value) { ($value -replace '[^0-9A-Za-z._+-]', '_') }

$problems = New-Object System.Collections.Generic.List[string]
$parts = [ordered]@{}

if ($Version -cmatch '^v[0-9]+$') { $parts.version = $Version } else { $problems.Add('RUST_CACHE_VERSION is not v<number>') }
$parts.os = if ($env:ImageOS) { Get-Part $env:ImageOS } else { 'local' }

try {
  $rustc = (& rustc -Vv 2>$null) -join "`n"
  if ($LASTEXITCODE -ne 0) { throw 'rustc -Vv failed' }
  $release = [regex]::Match($rustc, '(?m)^release: (\S+)$').Groups[1].Value
  $commit = [regex]::Match($rustc, '(?m)^commit-hash: ([0-9a-f]{40})$').Groups[1].Value
  $hostTriple = [regex]::Match($rustc, '(?m)^host: (\S+)$').Groups[1].Value
  if (-not $release -or -not $commit) { throw 'rustc -Vv has no release or commit-hash' }
  if ($hostTriple -ne 'x86_64-pc-windows-msvc') { throw "rustc host is $hostTriple" }
  $parts.rustc = 'rustc' + (Get-Part $release) + '-' + $commit.Substring(0, 12)
} catch { $problems.Add("rustc: $($_.Exception.Message)") }

try {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  if (-not (Test-Path -LiteralPath $vswhere)) { throw 'vswhere.exe not found' }
  $vs = @(& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)[0]
  if (-not $vs) { throw 'no Visual Studio with the C++ tools' }
  $msvc = (Get-Content -LiteralPath (Join-Path $vs 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt') -Raw).Trim()
  if ($msvc -notmatch '^[0-9]+(\.[0-9]+)+$') { throw "unexpected VC tools version '$msvc'" }
  $parts.msvc = 'msvc' + $msvc
} catch { $problems.Add("MSVC: $($_.Exception.Message)") }

try {
  $include = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Include'
  $sdk = Get-ChildItem -LiteralPath $include -Directory | Where-Object { $_.Name -match '^10\.[0-9.]+$' } |
    Sort-Object { [version]$_.Name } | Select-Object -Last 1
  if (-not $sdk) { throw 'no Windows SDK' }
  $parts.sdk = 'sdk' + $sdk.Name
} catch { $problems.Add("Windows SDK: $($_.Exception.Message)") }

try {
  if (-not $env:PERL) { throw 'PERL is not set' }
  $perl = (& $env:PERL -e 'print $^V' 2>$null) -join ''
  if ($LASTEXITCODE -ne 0 -or $perl -notmatch '^v[0-9.]+$') { throw "unexpected Perl version '$perl'" }
  $parts.perl = 'perl' + $perl.TrimStart('v')
} catch { $problems.Add("Perl: $($_.Exception.Message)") }

$buildEnv = Get-ChildItem env: | Where-Object {
  $_.Name -match '^(RUSTFLAGS|RUSTDOCFLAGS|RUSTC.*|RUSTUP_TOOLCHAIN|CARGO_.*|CC|CC_.*|CXX|CXX_.*|CFLAGS.*|CXXFLAGS.*|LDFLAGS|AR|AR_.*|OPENSSL_.*|SQLCIPHER_.*|LIBSQLITE3_.*|PERL|VCPKG.*|VSLANG|LIB|INCLUDE)$'
} | Sort-Object Name | ForEach-Object { "$($_.Name)=$($_.Value)" }
$parts.env = 'env' + (Get-Sha256Hex ($buildEnv -join "`n")).Substring(0, 12)

try {
  $lines = foreach ($crate in $Crates) {
    foreach ($rel in 'Cargo.toml', 'Cargo.lock', '.cargo/config.toml') {
      $path = Join-Path (Join-Path $Root $crate) $rel
      if (Test-Path -LiteralPath $path -PathType Leaf) {
        # Line endings depend on the checkout; the content does not.
        $text = [System.IO.File]::ReadAllText($path).Replace("`r`n", "`n")
        "$crate/$rel $(Get-Sha256Hex $text)"
      } elseif ($rel -eq '.cargo/config.toml') {
        "$crate/$rel absent"
      } else { throw "$crate/$rel is missing" }
    }
  }
  $parts.inputs = 'in' + (Get-Sha256Hex ($lines -join "`n")).Substring(0, 16)
} catch { $problems.Add("inputs: $($_.Exception.Message)") }

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$cacheable = $problems.Count -eq 0
$key = if ($cacheable) { 'rust-' + ($parts.Values -join '-') } else { '' }

Write-Host 'Rust cache key parts:'
$parts.GetEnumerator() | ForEach-Object { Write-Host ("  {0,-8} {1}" -f $_.Key, $_.Value) }
Write-Host ("  build environment variables hashed: {0}" -f (($buildEnv | ForEach-Object { ($_ -split '=', 2)[0] }) -join ', '))
if ($cacheable) { Write-Host "key: $key" } else {
  Write-Host 'NOT cacheable (nothing is restored or saved; the job builds from scratch):'
  $problems | ForEach-Object { Write-Host "  - $_" }
}

$out = @("key=$key", "cacheable=$($cacheable.ToString().ToLowerInvariant())", "cargo-home=$cargoHome")
if ($env:GITHUB_OUTPUT) { $out | Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8 } else { $out | ForEach-Object { Write-Output $_ } }
