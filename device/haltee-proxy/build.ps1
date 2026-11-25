<#
.SYNOPSIS
  Build the AES67Bridge HAL tee proxy (audio.primary.amlogic.so) from this repo.

.DESCRIPTION
  Compiles device/haltee-proxy/haltee.c + symmap.c for Android armv7 with the
  NDK clang toolchain. The output is the .so that bootstrap.sh copies into the
  tmpfs overlay mounted over /vendor/lib/hw.

  Self-contained: sources are taken from this directory, never from a scratch
  checkout elsewhere. The only external dependency is an Android NDK (>= 27).

.PARAMETER Ndk
  Path to the NDK root (the directory that contains toolchains/llvm).
  Default: $env:ANDROID_NDK_HOME, else the newest NDK under the SDK's ndk/.

.PARAMETER Out
  Output file. Default: <repo>\device\haltee-proxy\out\audio.primary.amlogic.so

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File build.ps1
  powershell -ExecutionPolicy Bypass -File build.ps1 -Ndk C:\Android\Sdk\ndk\27.2.12479018
#>
param(
  [string]$Ndk = '',
  [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot

function Resolve-Ndk([string]$explicit) {
  if ($explicit) {
    if (Test-Path (Join-Path $explicit 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe')) { return $explicit }
    throw "NDK not usable (no toolchains/llvm/.../clang.exe): $explicit"
  }
  if ($env:ANDROID_NDK_HOME -and (Test-Path (Join-Path $env:ANDROID_NDK_HOME 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe'))) {
    return $env:ANDROID_NDK_HOME
  }
  $sdkRoots = @()
  if ($env:ANDROID_SDK_ROOT) { $sdkRoots += $env:ANDROID_SDK_ROOT }
  if ($env:ANDROID_HOME) { $sdkRoots += $env:ANDROID_HOME }
  $sdkRoots += (Join-Path $env:LOCALAPPDATA 'Android\Sdk')
  foreach ($sdk in $sdkRoots) {
    $ndkDir = Join-Path $sdk 'ndk'
    if (Test-Path $ndkDir) {
      $candidate = Get-ChildItem $ndkDir -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName 'toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe') } |
        Sort-Object Name -Descending | Select-Object -First 1
      if ($candidate) { return $candidate.FullName }
    }
  }
  throw 'No Android NDK found. Pass -Ndk <ndk root> or set ANDROID_NDK_HOME.'
}

$ndkRoot = Resolve-Ndk $Ndk
$bin = Join-Path $ndkRoot 'toolchains\llvm\prebuilt\windows-x86_64\bin'
$clang = Join-Path $bin 'clang.exe'
$readelf = Join-Path $bin 'llvm-readelf.exe'
if (-not $Out) { $Out = Join-Path $here 'out\audio.primary.amlogic.so' }
$outDir = Split-Path -Parent $Out
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$src = @((Join-Path $here 'haltee.c'), (Join-Path $here 'symmap.c'))
foreach ($f in $src) { if (-not (Test-Path $f)) { throw "missing source: $f" } }

# Keep temp files inside the repo: TEMP on a RAM disk / restricted temp dir
# breaks clang's temp-object handling on this host.
$tmp = Join-Path $outDir '.tmp'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TEMP = $tmp
$env:TMP = $tmp

Write-Output "ndk:   $ndkRoot"
Write-Output "src:   $($src -join ', ')"
Write-Output "out:   $Out"

& $clang --target=armv7a-linux-androideabi30 -shared -fPIC -O2 -Wall -Wextra -Wno-unused-parameter -Wno-sign-compare -o $Out @src -lm
if ($LASTEXITCODE -ne 0) { throw "clang failed (exit $LASTEXITCODE)" }

$item = Get-Item $Out
$md5 = (Get-FileHash $Out -Algorithm MD5).Hash.ToLower()
Write-Output ("built: {0} bytes  md5={1}" -f $item.Length, $md5)
if (Test-Path $readelf) {
  & $readelf -h $Out | Select-String -Pattern 'Class|Machine|Type' | ForEach-Object { Write-Output $_.Line.Trim() }
  & $readelf -d $Out | Select-String -Pattern 'NEEDED|SONAME' | ForEach-Object { Write-Output $_.Line.Trim() }
}
Write-Output 'Deploy: adb push <out> /data/aes67bridge/stage/audio.primary.amlogic.so, then run stage/start.sh (bootstrap re-copies it into the overlay).'
