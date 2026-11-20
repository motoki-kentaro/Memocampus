<#
.SYNOPSIS
  READ-ONLY snapshot of everything the installer lifecycle may or may not touch, and a comparison of
  two snapshots. It is the evidence tool for installer\E2E-ACCEPTANCE.md. It changes nothing.
.DESCRIPTION
  -Take <file>            writes a JSON snapshot of the current user's: Native Messaging registrations
                          (default values) for the three host names, the Run key (value names, and the
                          MemocampusTray data), StartupApproved\Run value names and bytes, the Inno
                          uninstall key of this product, the install folder (names, sizes, SHA-256), the
                          data folder %LOCALAPPDATA%\Memocampus (names, sizes, SHA-256 of every file),
                          the Start menu shortcut, and running memocampus-* processes.
  -Compare <before> <after>  prints every difference (what was added, removed or changed).
  Run it before and after each E2E step, inside the isolated Windows user or VM that the acceptance
  document requires. Reading the data folder opens files for reading only.
#>
[CmdletBinding(DefaultParameterSetName = 'Take')]
param(
  [Parameter(ParameterSetName = 'Take', Mandatory)][string]$Take,
  [Parameter(ParameterSetName = 'Compare', Mandatory)][string]$Compare,
  [Parameter(ParameterSetName = 'Compare', Mandatory, Position = 0)][string]$After
)
Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

function Read-Default([string]$path) {
  $k = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
  if (-not $k) { return $null }
  [string]$k.GetValue('', $null)
}

function Hash-Tree([string]$dir) {
  $out = [ordered]@{}
  if (-not (Test-Path -LiteralPath $dir)) { return $out }
  Get-ChildItem -LiteralPath $dir -Recurse -File -Force -ErrorAction SilentlyContinue | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($dir.Length).TrimStart('\')
    $h = try { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash } catch { 'UNREADABLE' }
    $out[$rel] = "$($_.Length) $h"
  }
  $out
}

function Value-Names([string]$path) {
  $k = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
  $o = [ordered]@{}
  if ($k) { foreach ($n in ($k.GetValueNames() | Sort-Object)) { $o[$n] = $null } }
  $o
}

if ($PSCmdlet.ParameterSetName -eq 'Take') {
  $chrome = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts'
  $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
  $sa = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
  $saData = [ordered]@{}
  $saKey = Get-Item -LiteralPath $sa -ErrorAction SilentlyContinue
  if ($saKey) { foreach ($n in ($saKey.GetValueNames() | Sort-Object)) { $saData[$n] = ([BitConverter]::ToString([byte[]]$saKey.GetValue($n))) } }
  $runData = [ordered]@{}
  $runKey = Get-Item -LiteralPath $run -ErrorAction SilentlyContinue
  if ($runKey) { foreach ($n in ($runKey.GetValueNames() | Sort-Object)) { $runData[$n] = if ($n -eq 'MemocampusTray') { [string]$runKey.GetValue($n) } else { '(value present)' } } }
  $installDir = Join-Path $env:LOCALAPPDATA 'Programs\Memocampus'
  $dataDir = Join-Path $env:LOCALAPPDATA 'Memocampus'
  $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Memocampus'
  $uninst = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{63D78D26-D4BF-49ED-B29D-FBFFF1EAF56A}_is1'
  $snap = [ordered]@{
    nativeMessaging = [ordered]@{
      product  = Read-Default "$chrome\io.github.memocampus.vault"
      dev      = Read-Default "$chrome\io.github.memocampus.vault.dev"
      legacyDev = Read-Default "$chrome\com.memocampus.vault"
    }
    runValues       = $runData
    startupApproved = $saData
    uninstallKeyPresent = [bool](Get-Item -LiteralPath $uninst -ErrorAction SilentlyContinue)
    installFolder   = Hash-Tree $installDir
    dataFolder      = Hash-Tree $dataDir
    startMenu       = Hash-Tree $startMenu
    processes       = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -like 'memocampus-*' } | Sort-Object Id | ForEach-Object { "$($_.ProcessName) pid=$($_.Id)" })
  }
  $snap | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Take -Encoding utf8
  "snapshot written: $Take"
  return
}

function Flatten($o, [string]$prefix, $into) {
  if ($null -eq $o) { $into[$prefix] = '(none)'; return }
  if ($o -is [System.Management.Automation.PSCustomObject]) {
    $props = @($o.PSObject.Properties)
    if ($props.Count -eq 0) { $into[$prefix] = '(empty)'; return }
    foreach ($p in $props) { Flatten $p.Value "$prefix/$($p.Name)" $into }
  } elseif ($o -is [array]) {
    $into[$prefix] = ($o -join '; ')
  } else { $into[$prefix] = [string]$o }
}
$a = @{}; $b = @{}
Flatten (Get-Content -LiteralPath $Compare -Raw | ConvertFrom-Json) '' $a
Flatten (Get-Content -LiteralPath $After -Raw | ConvertFrom-Json) '' $b
$keys = @($a.Keys + $b.Keys | Sort-Object -Unique)
$diff = 0
foreach ($k in $keys) {
  $x = if ($a.ContainsKey($k)) { $a[$k] } else { '(absent)' }
  $y = if ($b.ContainsKey($k)) { $b[$k] } else { '(absent)' }
  if ($x -ne $y) { $diff++; "CHANGED  $k`n    before: $x`n    after:  $y" }
}
if ($diff -eq 0) { 'no differences' } else { "$diff difference(s)" }
