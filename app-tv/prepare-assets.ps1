<#
.SYNOPSIS
  Prepare the three runtime blobs the TV app bundles as assets.

.DESCRIPTION
  app-tv\app\src\main\assets needs:
    audio.primary.amlogic.so       HAL tee proxy  -> built from device\haltee-proxy
    audio.primary.amlogic.so.orig  stock HAL      -> pulled from your own device
    haltee_rtp_sender              legacy C sender (optional; only the legacy engine uses it)

  None of them are committed (\.gitignore drops *.so / *.orig / the sender).
  Run this before the first Gradle build.

.PARAMETER Device
  adb serial (ip:port) to pull the stock HAL from. Required when the .orig asset
  is missing; ignored when it already exists unless -Force is given.

.PARAMETER Force
  Re-pull / re-copy even when the asset is already present.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File prepare-assets.ps1 -Device 192.168.63.158:5555
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
    Write-Output ("{0,-32} MISSING" -f $name)
  }
}

# 1) HAL tee proxy: always (re)built from repo sources
$proxyScript = Join-Path $repo 'device\haltee-proxy\build.ps1'
if (-not (Test-Path $proxyScript)) { throw "missing $proxyScript" }
& powershell -NoProfile -ExecutionPolicy Bypass -File $proxyScript | Write-Output
$proxyOut = Join-Path $repo 'device\haltee-proxy\out\audio.primary.amlogic.so'
if (-not (Test-Path $proxyOut)) { throw "proxy build produced no output: $proxyOut" }
Copy-Item -Force $proxyOut (Join-Path $assets 'audio.primary.amlogic.so')

# 2) stock HAL (rollback copy): pull from the device, never from the repo
$orig = Join-Path $assets 'audio.primary.amlogic.so.orig'
if ($Force -or -not (Test-Path $orig)) {
  if (-not $Device) {
    Write-Output 'WARN stock HAL asset missing and -Device not given; bootstrap.sh rollback will not have the original.'
  } else {
    & adb -s $Device pull /vendor/lib/hw/audio.primary.amlogic.so $orig
    if ($LASTEXITCODE -ne 0) { Write-Output 'WARN adb pull of the stock HAL failed' }
  }
}

# 3) legacy C sender (optional)
$senderSrc = Join-Path $repo 'device\daemon\out\haltee_rtp_sender'
$senderDst = Join-Path $assets 'haltee_rtp_sender'
if (Test-Path $senderSrc) {
  Copy-Item -Force $senderSrc $senderDst
} elseif (-not (Test-Path $senderDst)) {
  Write-Output 'WARN legacy sender not built (device\daemon\out\haltee_rtp_sender); only needed for engine=legacy.'
}

Write-Output '--- assets ---'
Report 'audio.primary.amlogic.so' (Join-Path $assets 'audio.primary.amlogic.so')
Report 'audio.primary.amlogic.so.orig' $orig
Report 'haltee_rtp_sender' $senderDst
