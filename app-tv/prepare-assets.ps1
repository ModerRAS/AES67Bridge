<#
.SYNOPSIS
  Prepare the runtime blobs the TV app bundles as assets.

.DESCRIPTION
  app-tv\app\src\main\assets needs:
    audio.primary.amlogic.so       HAL tee proxy -> built from device\haltee-proxy
    audio.primary.amlogic.so.orig  stock HAL backup -> pulled from your own device (optional)

  The proxy is built from this repo, so CI can produce it. The stock HAL belongs to
  the TV firmware and is never redistributed: release APKs omit it and bootstrap.sh
  provisions it from the device on first run. When building locally with -Device the
  stock HAL is pulled once (kept out of git by \*.orig).

.PARAMETER Device
  adb serial (ip:port) to pull the stock HAL from. Optional.

.PARAMETER Force
  Re-pull / re-copy even when the asset is already present.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File prepare-assets.ps1 -Device 192.168.63.158:5555
  powershell -ExecutionPolicy Bypass -File prepare-assets.ps1   # CI: proxy only
#> 
param(
  [string]$Device = '',
  [switch]$Force
)
$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
$assets = Join-Path $here 'app\src\main\assets'
$repo = Split-Path -Parent $here
New-Item -ItemType Directory -Force -Path $assets | Out-Null

function Report([string]$name, [string]$path) {
  if (Test-Path $path) {
    $md5 = (Get-FileHash $path -Algorithm MD5).Hash.ToLower()
    Write-Output ("{0,-32} {1,9} bytes  md5={2}" -f $name, (Get-Item $path).Length, $md5)
  } else {
    Write-Output ("{0,-32} MISSING (ok for CI builds)" -f $name)
  }
}

# 1) HAL tee proxy: always (re)built from repo sources
$proxyScript = Join-Path $repo 'device\haltee-proxy\build.ps1'
if (-not (Test-Path $proxyScript)) { throw "missing $proxyScript" }
& powershell -NoProfile -ExecutionPolicy Bypass -File $proxyScript | Write-Output
$proxyOut = Join-Path $repo 'device\haltee-proxy\out\audio.primary.amlogic.so'
if (-not (Test-Path $proxyOut)) { throw "proxy build produced no output: $proxyOut" }
Copy-Item -Force $proxyOut (Join-Path $assets 'audio.primary.amlogic.so')

# 2) stock HAL backup (optional; local dev only)
$origPath = Join-Path $assets 'audio.primary.amlogic.so.orig'
if ($Force -or -not (Test-Path $origPath)) {
  if ($Device) {
    & adb -s $Device pull /vendor/lib/hw/audio.primary.amlogic.so $origPath | Write-Output
  } elseif (-not (Test-Path $origPath)) {
    Write-Output 'NOTE: no -Device and no stock HAL backup; the APK will provision it on the device at first run.'
  }
}

Write-Output ''
Report 'audio.primary.amlogic.so' (Join-Path $assets 'audio.primary.amlogic.so')
Report 'audio.primary.amlogic.so.orig' $origPath
