<#
.SYNOPSIS
  Build the legacy C sender (aes67_rtp_sender) for Android armv7 from this repo.

.DESCRIPTION
  The legacy engine (engine=legacy in daemon.conf) uses this binary instead of
  the Rust daemon. The Rust daemon does not need it. Output goes to
  device\daemon\out\haltee_rtp_sender so app-tv\prepare-assets.ps1 can pick it up.
#>
param(
  [string]$Ndk = '',
  [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot

function Resolve-Ndk([string]$explicit) {
  $probe = 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe'
  if ($explicit) {
    if (Test-Path (Join-Path $explicit $probe)) { return $explicit }
    throw "NDK not usable: $explicit"
  }
  if ($env:ANDROID_NDK_HOME -and (Test-Path (Join-Path $env:ANDROID_NDK_HOME $probe))) { return $env:ANDROID_NDK_HOME }
  $roots = @()
  if ($env:ANDROID_SDK_ROOT) { $roots += $env:ANDROID_SDK_ROOT }
  if ($env:ANDROID_HOME) { $roots += $env:ANDROID_HOME }
  $roots += (Join-Path $env:LOCALAPPDATA 'Android\Sdk')
  foreach ($r in $roots) {
    $ndkDir = Join-Path $r 'ndk'
    if (Test-Path $ndkDir) {
      $c = Get-ChildItem $ndkDir -Directory | Where-Object { Test-Path (Join-Path $_.FullName $probe) } |
        Sort-Object Name -Descending | Select-Object -First 1
      if ($c) { return $c.FullName }
    }
  }
  throw 'No Android NDK found. Pass -Ndk <ndk root> or set ANDROID_NDK_HOME.'
}

$ndkRoot = Resolve-Ndk $Ndk
$clang = Join-Path $ndkRoot 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe'
if (-not $Out) { $Out = Join-Path $here 'out\haltee_rtp_sender' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
$env:TEMP = Join-Path (Split-Path -Parent $Out) '.tmp'
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
$env:TMP = $env:TEMP

$src = Join-Path $here 'aes67_rtp_sender.c'
if (-not (Test-Path $src)) { throw "missing source: $src" }
& $clang --target=armv7a-linux-androideabi30 -O2 -Wall -Wextra -o $Out $src -lm -llog -landroid
if ($LASTEXITCODE -ne 0) { throw "clang failed (exit $LASTEXITCODE)" }
Write-Output ("built: {0} bytes  md5={1}" -f (Get-Item $Out).Length, (Get-FileHash $Out -Algorithm MD5).Hash.ToLower())
