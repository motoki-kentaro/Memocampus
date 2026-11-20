<#
.SYNOPSIS
  Static checks of the distribution pipeline: the Windows workflow, build.ps1, stage.ps1, the release
  gate and the Inno Setup bootstrap. No network, no compiler, no registry.
.DESCRIPTION
  -Negative  applies deliberate wrong changes to a scratch copy and requires each one to be caught.
  The checks read text only. They can show that a rule is written down; they cannot show that GitHub
  runs the workflow as written (that needs a real run).
#>
[CmdletBinding()]
param([switch]$Negative)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here

$files = @(
  '.github/workflows/windows-dist.yml',
  '.github/workflows/actions.lock',
  'installer/build.ps1',
  'installer/stage.ps1',
  'installer/release-gate.json',
  'installer/ci-setup-inno.ps1',
  'extension/manifest.json',
  'scripts/package-extension.mjs'
)

function Get-Text([string]$root, [string]$rel) {
  (Get-Content -LiteralPath (Join-Path $root $rel) -Raw -Encoding UTF8).Replace("`r`n", "`n")
}

function Invoke-DistChecks([string]$root, [bool]$quiet) {
  $failed = New-Object System.Collections.Generic.List[string]
  function Check([string]$name, [bool]$ok) {
    if (-not $ok) { $failed.Add($name) }
    if (-not $quiet) { Write-Host ("{0}  {1}" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $name) }
  }
  $wf = Get-Text $root '.github/workflows/windows-dist.yml'
  $lock = Get-Text $root '.github/workflows/actions.lock'
  $build = Get-Text $root 'installer/build.ps1'
  $stage = Get-Text $root 'installer/stage.ps1'
  $gateText = Get-Text $root 'installer/release-gate.json'
  $inno = Get-Text $root 'installer/ci-setup-inno.ps1'
  $manifestText = Get-Text $root 'extension/manifest.json'
  $pkg = Get-Text $root 'scripts/package-extension.mjs'
  $wfCode = ($wf -split "`n" | Where-Object { $_ -notmatch '^\s*#' }) -join "`n"

  # --- Workflow: permissions, triggers, secrets ----------------------------------------------------------
  Check 'workflow permissions are exactly contents: read, at the top level' (($wfCode -match '(?m)^permissions:\n  contents: read\n') -and (@([regex]::Matches($wfCode, '(?m)^\s*permissions:')).Count -eq 1))
  Check 'no job or step asks for write, id-token or any other permission' (($wfCode -notmatch '(?i)id-token|:\s*write\s*$') -and (@([regex]::Matches($wfCode, '(?m)^\s+[\w-]+:\s*(read|write|none)\s*$')).Count -eq 1))
  Check 'no secret is read' ($wfCode -notmatch '(?i)secrets\.|secrets:')
  Check 'only pull_request and workflow_dispatch trigger it (no push, no schedule, no pull_request_target, no release or tag trigger)' (($wfCode -match '(?m)^  pull_request:') -and ($wfCode -match '(?m)^  workflow_dispatch:') -and ($wfCode -notmatch '(?m)^  (push|schedule|pull_request_target|release|create|workflow_run|repository_dispatch):'))
  Check 'nothing creates a tag, a release or a Web Store upload, and nothing pushes' ($wfCode -notmatch '(?i)gh release|action-gh-release|softprops|create-release|upload-release-asset|git tag|git push|gh api|chrome-webstore|webstore.*upload|publish')
  Check 'the version input has no default (nothing is assumed)' ($wfCode -match '(?s)\n      version:\n(?:(?!\n      \w).)*required: true' -and ($wfCode -notmatch '(?s)\n      version:\n(?:(?!\n      \w).)*default:'))
  Check 'the channel input is test or product, default test' ($wfCode -match '(?s)channel:.*?options: \[test, product\]\n\s+default: test')

  # --- Workflow: pinned tools ------------------------------------------------------------------------------
  $uses = @([regex]::Matches($wfCode, '(?m)^\s*-?\s*uses:\s*(\S+)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
  Check 'every action is pinned to an exact x.y.z version' ((@($uses | Where-Object { $_ -notmatch '^[\w./-]+@v\d+\.\d+\.\d+$' }).Count -eq 0) -and ($uses.Count -ge 3))
  $lockWf = [regex]::Match($lock, "(?s)'\.github/workflows/windows-dist\.yml':\n((?:\s+- '[^']+'\n)+)").Groups[1].Value
  $lockUses = @([regex]::Matches($lockWf, "- '([^']+)'") | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
  Check 'the lock file lists exactly the actions this workflow uses, each with a commit' ((($uses -join '|') -eq ($lockUses -join '|')) -and (@($uses | Where-Object { $lock -notmatch ("(?s)'" + [regex]::Escape($_) + "':\n\s+ref: '[^']+'\n\s+commit: 'sha1-[0-9a-f]{40}'") }).Count -eq 0))
  Check 'every checkout drops credentials' ((@([regex]::Matches($wfCode, 'actions/checkout@')).Count -ge 2) -and (@([regex]::Matches($wfCode, 'persist-credentials: false')).Count -eq @([regex]::Matches($wfCode, 'actions/checkout@')).Count))
  Check 'no cache is restored (a stale build can never be staged)' (($wfCode -notmatch '(?i)actions/cache|Swatinem|sccache|cache-dependency') -and ($wfCode -notmatch '(?m)^\s*cache:\s') -and (@([regex]::Matches($wfCode, 'package-manager-cache: false')).Count -eq @([regex]::Matches($wfCode, 'actions/setup-node@')).Count))
  Check 'the Windows runner is hosted windows-latest only' ((@([regex]::Matches($wfCode, 'runs-on:\s*(\S+)') | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique) -join ',') -eq 'windows-latest')

  # --- Workflow: untrusted text never becomes script text -----------------------------------------------
  $inRun = $false; $leak = $false
  foreach ($line in ($wfCode -split "`n")) {
    if ($line -match '^\s*(-\s+)?run:\s*(\|.*|>.*)?$') { $inRun = $true; if ($line -match '\$\{\{') { $leak = $true }; continue }
    if ($line -match '^\s*(-\s+)?run:\s*\S' -and $line -match '\$\{\{\s*(inputs|github\.event|github\.head_ref)') { $leak = $true }
    if ($inRun -and $line -match '^\s{0,9}\S' -and $line -notmatch '^\s{10,}') { $inRun = $false }
    if ($inRun -and $line -match '\$\{\{') { $leak = $true }
  }
  Check 'no ${{ }} expression inside a run: script (inputs reach scripts only through env:)' (-not $leak)

  # --- Workflow: jobs and artifacts ------------------------------------------------------------------------
  Check 'the build job needs the checks job and runs only on a manual dispatch' (($wfCode -match '(?s)\n  build:\n(?:(?!\n  \w).)*if: github\.event_name == ''workflow_dispatch''') -and ($wfCode -match '(?s)\n  build:\n(?:(?!\n  \w).)*needs: checks'))
  Check 'the executables are built in the job: fmt, test, clippy and release for native-host, mcp-server and tray' ((@([regex]::Matches($wfCode, 'cargo build --release --locked')).Count -eq 3) -and (@([regex]::Matches($wfCode, 'cargo test --locked')).Count -eq 3) -and (@([regex]::Matches($wfCode, 'cargo clippy --locked --all-targets -- -D warnings')).Count -eq 3) -and (@([regex]::Matches($wfCode, 'cargo fmt --all -- --check')).Count -eq 3))
  Check 'tray tests run with no-desktop, so window tests are ignored, not passed' ($wfCode -match 'cargo test --locked --features no-desktop')
  Check 'staging happens after the builds, with the shared target dir and a not-older-than time' ((($wfCode.IndexOf('stage.ps1')) -gt ($wfCode.LastIndexOf('cargo build --release --locked'))) -and ($wfCode -match 'stage\.ps1 -BuildDir \$env:CARGO_TARGET_DIR -NotOlderThan'))
  Check 'the compile follows staging and does not pass an extension id of its own' ((($wfCode.IndexOf('build.ps1')) -gt ($wfCode.IndexOf('stage.ps1'))) -and ($wfCode -notmatch '(?i)ExtensionId\s*=|-ExtensionId'))
  Check 'the setup program is built from the dispatch inputs through env (version, channel, tag)' (($wfCode -match 'APP_VERSION: \$\{\{ inputs\.version \}\}') -and ($wfCode -match 'CHANNEL: \$\{\{ inputs\.channel \}\}') -and ($wfCode -match 'Version = \$env:APP_VERSION; Channel = \$env:CHANNEL'))
  $up = @([regex]::Matches($wfCode, '(?s)- name: (Upload[^\n]*)\n\s+if: \$\{\{ inputs\.channel == ''(\w+)'' \}\}\n\s+uses: actions/upload-artifact@[^\n]*\n\s+with:\n\s+name: ([^\n]+)\n\s+path: ([^\n]+)'))
  $testUp = @($up | Where-Object { $_.Groups[2].Value -eq 'test' })
  $prodUp = @($up | Where-Object { $_.Groups[2].Value -eq 'product' })
  Check 'the test build is uploaded only for the test channel, named INSTALLER-TEST-NOT-FOR-DISTRIBUTION, from dist/test/' (($testUp.Count -eq 1) -and ($testUp[0].Groups[3].Value -ceq 'INSTALLER-TEST-NOT-FOR-DISTRIBUTION') -and ($testUp[0].Groups[4].Value -ceq 'dist/test/'))
  Check 'the release candidate is uploaded only for the product channel, under another name, from dist/product/' (($prodUp.Count -eq 1) -and ($prodUp[0].Groups[3].Value -ceq 'memocampus-windows-release-candidate') -and ($prodUp[0].Groups[4].Value -ceq 'dist/product/'))
  Check 'the extension ZIP is its own artifact; the setup program is not in it' (($wfCode -match 'name: chrome-extension-store-package\n\s+path: dist/extension/') )
  Check 'the extension package step uses the isolated-copy packager and only an optional exact version' (($wfCode -match 'scripts/package-extension\.mjs') -and ($wfCode -notmatch 'set-host-profile\.mjs product'))
  Check 'the summary states unsigned, tray window tests not run, real installer E2E not run' (($wfCode -match 'UNSIGNED') -and ($wfCode -match 'NOT_RUN here') -and ($wfCode -match 'install / update / uninstall: NOT_RUN'))
  Check 'the workflow runs the static, negative, compile, Native Messaging and lifecycle checks' (($wfCode -match 'verify-static\.ps1\n') -and ($wfCode -match 'verify-static\.ps1 -Negative') -and ($wfCode -match 'verify-static\.ps1 -Compile') -and ($wfCode -match 'run-selftest\.ps1') -and ($wfCode -match 'run-lifecycle-test\.ps1 .*-SkipTrays') -and ($wfCode -match 'verify-dist\.ps1 -Negative') -and ($wfCode -match 'npm test'))

  # --- build.ps1 -----------------------------------------------------------------------------------------------
  Check 'build.ps1: the version must be x.y.z, numbers only, each at most 65535' (($build -match "\^\(0\|\[1-9\]\[0-9\]\{0,4\}\)\\\.\(0\|\[1-9\]\[0-9\]\{0,4\}\)\\\.\(0\|\[1-9\]\[0-9\]\{0,4\}\)\$") -and ($build -match '-gt 65535'))
  Check 'build.ps1: a tag must be exactly v<version>' ($build -match '\$Tag -ne "v\$Version"')
  Check 'build.ps1: the product channel needs a real id from the release gate, never the synthetic one' (($build -match 'if \(-not \$ExtensionId\) \{ Fail ''the release gate is closed') -and ($build -match 'if \(\$ExtensionId -eq \$TestExtensionId\) \{ Fail ''that is the synthetic') -and ($build -match '-cnotmatch \$idPattern') -and ($build -match '#?\$TestExtensionId = ''abcdefghijklmnopabcdefghijklmnop'''))
  Check 'build.ps1: the test channel always uses the synthetic id' ($build -match "\`$ExtensionId = \`$TestExtensionId")
  Check 'build.ps1: the compiler must be exactly the required Inno Setup version, read from the compile output' (($build -match "RequiredInno = '7\.1\.1'") -and ($build -match 'Compiler engine version: Inno Setup') -and ($build -match '\$innoVersion -ne \$RequiredInno'))
  Check 'build.ps1: the stage must hold exactly the three executables' ($build -match "memocampus-mcp\.exe', 'memocampus-native-host\.exe', 'memocampus-tray\.exe'")
  Check 'build.ps1: test = /DInstallerTestMode and no store link; product = /DStoreListing and no test mode' (($build -match "if \(\`$Channel -eq 'test'\) \{ \`$defs \+= '/DInstallerTestMode' \} else \{ \`$defs \+= '/DStoreListing' \}") -and (@([regex]::Matches($build, 'DInstallerTestMode')).Count -eq 1) -and (@([regex]::Matches($build, 'DStoreListing')).Count -eq 1))
  Check 'build.ps1: the two channels have different output folders and names' (($build -match "Join-Path \`$OutDir \`$Channel") -and ($build -match "'Memocampus-Setup-x64\.exe' \} else \{ 'Memocampus-Setup-x64-INSTALLER-TEST\.exe'") -and ($build -match 'NOT-FOR-DISTRIBUTION\.txt'))
  Check 'build.ps1: the program''s version resource is checked against the version' ($build -match 'FileVersion\.StartsWith\(\$Version\)')
  Check 'build.ps1: SHA-256, commit and "signed": false are recorded' (($build -match 'Get-FileHash') -and ($build -match 'SHA256SUMS\.txt') -and ($build -match 'gitCommit') -and ($build -match 'signed\s+= \$false'))
  Check 'build.ps1: a failure removes the output folder' ($build -match '(?s)catch \{.*?Remove-Item -LiteralPath \$ch -Recurse -Force.*?throw')
  Check 'build.ps1: no signing, no network, no upload, no publish' ($build -notmatch '(?i)signtool|SignTool|Invoke-WebRequest|Invoke-RestMethod|gh release|git push|git tag|Set-AuthenticodeSignature')

  # --- stage.ps1 ------------------------------------------------------------------------------------------------
  Check 'stage.ps1: refuses an executable older than the build start, and a stage that is not exactly three files' (($stage -match 'Stale build output') -and ($stage -match 'LastWriteTimeUtc -lt \$NotOlderThan'))
  Check 'stage.ps1 copies three named files only' (($stage -match "'memocampus-native-host\.exe'") -and ($stage -match "'memocampus-mcp\.exe'") -and ($stage -match "'memocampus-tray\.exe'") -and ($stage -notmatch '(?i)Copy-Item[^\n]*\*|-Recurse -Force \$from'))

  # --- Inno Setup bootstrap ---------------------------------------------------------------------------------------
  Check 'ci-setup-inno.ps1: downloads only from jrsoftware.org over https, only with a pinned SHA-256, and runs nothing that failed the hash' (($inno -match "\^https://\(files\\\.\)\?jrsoftware\\\.org/") -and ($inno -match 'Get-FileHash') -and ($inno -match '-ne \$sha\.ToUpperInvariant\(\)') -and ($inno -match '(?s)ToUpperInvariant\(\)\) \{.*?throw') -and ($inno.IndexOf('Get-FileHash') -lt $inno.IndexOf('Start-Process')))
  # Behavioural: the allow-list is exercised through the script's own -CheckUrl hook (nothing is downloaded).
  $innoScript = Join-Path $root 'installer/ci-setup-inno.ps1'
  function Test-InnoUrl([string]$u) {
    $ErrorActionPreference = 'Continue'
    $null = & powershell -NoProfile -ExecutionPolicy Bypass -File $innoScript -CheckUrl $u 2>&1
    $LASTEXITCODE -eq 0
  }
  $okUrls = @('https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe', 'https://jrsoftware.org/download.php/is.exe', 'https://files.jrsoftware.org/is/7/innosetup-7.1.1.exe')
  $badUrls = @(
    'https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe?x=1',
    'https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe/',
    'https://github.com/jrsoftware/issrc/releases/download/is-7_1_0/innosetup-7.1.0-x64.exe',
    'https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/other.exe',
    'https://github.com/evil/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe',
    'https://github.com/jrsoftware/issrc/archive/refs/heads/main.zip',
    'https://github.com/jrsoftware/innosetup-7.1.1-x64.exe',
    'https://GITHUB.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe',
    'http://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe',
    'https://github.com.evil.example/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe',
    'https://evil.example/https://jrsoftware.org/x.exe',
    'https://jrsoftware.org.evil.example/x.exe',
    'https://github.com/')
  # Static twin (also runs in the mutation pass): exactly one GitHub address, compared whole, no GitHub pattern.
  Check 'ci-setup-inno.ps1: the GitHub source is one exact address (-ceq), never a pattern' (($inno -match "-ceq 'https://github\.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7\.1\.1-x64\.exe'") -and ($inno -notmatch "-c?match '\^?https://github"))
  # The runs spawn a process per address, so they run in the normal pass only (the mutation pass uses the static twin).
  if (-not $quiet) {
  $okAll = $true; foreach ($u in $okUrls) { if (-not (Test-InnoUrl $u)) { $okAll = $false } }
  Check 'ci-setup-inno.ps1 (run): accepts jrsoftware.org and the one official GitHub 7.1.1 release file' $okAll
  $badAll = $true; foreach ($u in $badUrls) { if (Test-InnoUrl $u) { $badAll = $false } }
  Check 'ci-setup-inno.ps1 (run): refuses any other GitHub repository, release, file, query, scheme or host' $badAll
  }

  # --- Release gate and versions --------------------------------------------------------------------------------
  $gate = $gateText | ConvertFrom-Json
  Check 'the release gate holds null or a 32-letter a-p Web Store id (nothing else)' (($null -eq $gate.extensionId) -or (($gate.extensionId -is [string]) -and ($gate.extensionId -cmatch '^[a-p]{32}$') -and ($gate.extensionId -ne 'abcdefghijklmnopabcdefghijklmnop')))
  $m = $manifestText | ConvertFrom-Json
  Check 'the extension manifest is Manifest V3 with a dotted numeric version and no key' (($m.manifest_version -eq 3) -and ($m.version -match '^\d+(\.\d+){0,3}$') -and ($null -eq $m.PSObject.Properties['key']))
  Check 'the packager switches only its own copy to the product profile and verifies it' (($pkg -match 'withProfile\(config, "product"\)') -and ($pkg -match 'profileOf\(after\) !== "product"') -and ($pkg -match 'mkdtempSync') -and ($pkg -match 'rmSync\(work'))

  # --- No secrets or signing material in the pipeline sources ------------------------------------------------
  $all = $wf + $build + $stage + $inno + $pkg + $gateText
  Check 'no private key, token or certificate material in the pipeline sources' ($all -notmatch '(?i)BEGIN [A-Z ]*PRIVATE KEY|ghp_[A-Za-z0-9]{20}|github_pat_|\.pfx|\.p12|AKIA[0-9A-Z]{16}')
  return ,$failed
}

if ($Negative) {
  $m = @(
    @{ n = 'workflow gets write permission'; f = '.github/workflows/windows-dist.yml'; a = "permissions:`n  contents: read"; b = "permissions:`n  contents: write" },
    @{ n = 'a job gets id-token'; f = '.github/workflows/windows-dist.yml'; a = "    timeout-minutes: 30`n"; b = "    timeout-minutes: 30`n    permissions:`n      id-token: write`n" },
    @{ n = 'push trigger added'; f = '.github/workflows/windows-dist.yml'; a = "on:`n  pull_request:"; b = "on:`n  push:`n    branches: [main]`n  pull_request:" },
    @{ n = 'a secret is read'; f = '.github/workflows/windows-dist.yml'; a = 'APP_VERSION: ${{ inputs.version }}'; b = 'APP_VERSION: ${{ secrets.VERSION }}' },
    @{ n = 'a release is created'; f = '.github/workflows/windows-dist.yml'; a = '      - name: Upload the release candidate'; b = "      - run: gh release create v0.8.0 dist/product/*`n      - name: Upload the release candidate" },
    @{ n = 'an action floats on a major tag'; f = '.github/workflows/windows-dist.yml'; a = "      - uses: actions/upload-artifact@v7.0.1`n        with:`n          name: chrome-extension-store-package"; b = "      - uses: actions/upload-artifact@v7`n        with:`n          name: chrome-extension-store-package" },
    @{ n = 'an action missing from the lock file'; f = '.github/workflows/actions.lock'; a = "        - 'actions/upload-artifact@v7.0.1'`ndependencies:"; b = "dependencies:" },
    @{ n = 'checkout keeps credentials'; f = '.github/workflows/windows-dist.yml'; a = "          persist-credentials: false`n      - name: Build start time"; b = "          persist-credentials: true`n      - name: Build start time" },
    @{ n = 'a cache is restored'; f = '.github/workflows/windows-dist.yml'; a = '      - run: npm ci'; b = "      - uses: actions/cache@v4.0.0`n        with:`n          path: target-dist`n          key: x`n      - run: npm ci" },
    @{ n = 'an input is spliced into a script'; f = '.github/workflows/windows-dist.yml'; a = '          if ($env:TAG) { $params.Tag = $env:TAG }'; b = '          if ($env:TAG) { $params.Tag = "${{ inputs.tag }}" }' },
    @{ n = 'version input gets a default'; f = '.github/workflows/windows-dist.yml'; a = "        description: ""Windows app version x.y.z (no default on purpose)""`n        required: true"; b = "        description: ""Windows app version x.y.z (no default on purpose)""`n        required: true`n        default: ""0.8.0""" },
    @{ n = 'build job runs on every pull request'; f = '.github/workflows/windows-dist.yml'; a = "    if: github.event_name == 'workflow_dispatch'`n"; b = '' },
    @{ n = 'the test build uploaded under the product name'; f = '.github/workflows/windows-dist.yml'; a = 'name: INSTALLER-TEST-NOT-FOR-DISTRIBUTION'; b = 'name: memocampus-windows-release-candidate' },
    @{ n = 'the test build uploaded for the product channel'; f = '.github/workflows/windows-dist.yml'; a = "if: `${{ inputs.channel == 'test' }}"; b = "if: `${{ inputs.channel == 'product' }}" },
    @{ n = 'an extension id passed to build.ps1 by the workflow'; f = '.github/workflows/windows-dist.yml'; a = "Channel = `$env:CHANNEL; Iscc"; b = "Channel = `$env:CHANNEL; ExtensionId = 'bcdefghijklmnopabcdefghijklmnopa'; Iscc" },
    @{ n = 'staging without the not-older-than guard'; f = '.github/workflows/windows-dist.yml'; a = ' -NotOlderThan ([datetime]::Parse($env:BUILD_START, [cultureinfo]::InvariantCulture, ''RoundtripKind''))'; b = '' },
    @{ n = 'tray tests run without the no-desktop switch'; f = '.github/workflows/windows-dist.yml'; a = 'cargo test --locked --features no-desktop'; b = 'cargo test --locked' },
    @{ n = 'a tray clippy step dropped'; f = '.github/workflows/windows-dist.yml'; a = "          cargo clippy --locked --all-targets -- -D warnings`n          cargo build --release --locked`n      - name: Stage"; b = "          cargo build --release --locked`n      - name: Stage" },
    @{ n = 'version check relaxed in build.ps1'; f = 'installer/build.ps1'; a = "'^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})$'"; b = "'^[0-9.]+$'" },
    @{ n = 'tag check removed in build.ps1'; f = 'installer/build.ps1'; a = 'if ($Tag -and $Tag -ne "v$Version")'; b = 'if ($false)' },
    @{ n = 'release gate bypassed in build.ps1'; f = 'installer/build.ps1'; a = "if (-not `$ExtensionId) { Fail 'the release gate is closed"; b = "if (`$false) { Fail 'the release gate is closed" },
    @{ n = 'synthetic id allowed for the product channel'; f = 'installer/build.ps1'; a = "if (`$ExtensionId -eq `$TestExtensionId) { Fail 'that is the synthetic"; b = "if (`$false) { Fail 'that is the synthetic" },
    @{ n = 'Inno Setup version check removed'; f = 'installer/build.ps1'; a = 'if ($innoVersion -ne $RequiredInno)'; b = 'if ($false)' },
    @{ n = 'product build compiled in test mode'; f = 'installer/build.ps1'; a = "if (`$Channel -eq 'test') { `$defs += '/DInstallerTestMode' } else { `$defs += '/DStoreListing' }"; b = "`$defs += '/DInstallerTestMode'; `$defs += '/DStoreListing'" },
    @{ n = 'both channels write the same folder'; f = 'installer/build.ps1'; a = '$ch = Join-Path $OutDir $Channel'; b = '$ch = Join-Path $OutDir ''out''' },
    @{ n = 'failed build leaves its output'; f = 'installer/build.ps1'; a = "  if (Test-Path -LiteralPath `$ch) { Remove-Item -LiteralPath `$ch -Recurse -Force }`n  throw"; b = "  throw" },
    @{ n = 'build.ps1 claims signed'; f = 'installer/build.ps1'; a = 'signed    = $false'; b = 'signed    = $true' },
    @{ n = 'stage guard removed'; f = 'installer/stage.ps1'; a = 'Stale build output'; b = 'Old build output' },
    @{ n = 'Inno download hash check removed'; f = 'installer/ci-setup-inno.ps1'; a = 'if ($got -ne $sha.ToUpperInvariant()) {'; b = 'if ($false) {' },
    @{ n = 'Inno download from another host'; f = 'installer/ci-setup-inno.ps1'; a = "'^https://(files\.)?jrsoftware\.org/'"; b = "'^https://'" },
    @{ n = 'Inno download from any GitHub repository'; f = 'installer/ci-setup-inno.ps1'; a = "'https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe') { return }"; b = "'https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe' -or `$u -cmatch '^https://github\.com/') { return }" },
    @{ n = 'made-up release gate id'; f = 'installer/release-gate.json'; a = '"extensionId": null'; b = '"extensionId": "abcdefghijklmnopabcdefghijklmnop"' },
    @{ n = 'manifest key added'; f = 'extension/manifest.json'; a = '"manifest_version": 3,'; b = '"manifest_version": 3, "key": "AAAA",' },
    @{ n = 'private key material in a script'; f = 'installer/build.ps1'; a = '# --- version:'; b = "# -----BEGIN PRIVATE KEY-----`n# --- version:" }
  )
  $scratch = Join-Path ([IO.Path]::GetTempPath()) ("memocampus-verify-dist-" + [guid]::NewGuid().ToString('N'))
  $missed = 0
  try {
    foreach ($x in $m) {
      $root = Join-Path $scratch ([guid]::NewGuid().ToString('N'))
      foreach ($rel in $files) {
        $dest = Join-Path $root $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo $rel) -Destination $dest
      }
      $p = Join-Path $root $x.f
      $t = (Get-Content -LiteralPath $p -Raw -Encoding UTF8).Replace("`r`n", "`n")
      if ($t.Split([string[]]@($x.a), [StringSplitOptions]::None).Count -ne 2) { "FAIL  mutation does not apply exactly once: $($x.n)"; $missed++; continue }
      [IO.File]::WriteAllText($p, $t.Replace($x.a, $x.b), (New-Object Text.UTF8Encoding($false)))
      $got = Invoke-DistChecks $root $true
      if ($got.Count -gt 0) { "PASS  caught: {0}  ({1})" -f $x.n, $got[0] } else { "FAIL  NOT CAUGHT: $($x.n)"; $missed++ }
    }
  } finally { Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue }
  if ($missed) { "$missed mutation(s) not caught"; exit 1 }
  $global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
  "all $($m.Count) mutations caught"
  return
}

$result = Invoke-DistChecks $repo $false
if ($result.Count) { "$($result.Count) check(s) failed"; exit 1 }
$global:LASTEXITCODE = 0  # success is established above; a native command's old exit code must not leak out
'all checks passed'
