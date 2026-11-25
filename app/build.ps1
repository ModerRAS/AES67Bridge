<#
.SYNOPSIS
  Build the legacy Java console APK (dev.aes67bridge.console) from this repo.

.DESCRIPTION
  Self-contained: sources come from <repo>\app\src, the manifest from
  <repo>\app\AndroidManifest.xml and the shell assets from <repo>\app\assets.
  Nothing is taken from a scratch checkout.

  The runtime assets the app needs at boot (HAL tee proxy, stock HAL copy,
  legacy C sender) are optional parameters because two of them are not in the
  repository:
    -HalProxy     audio.primary.amlogic.so from device\haltee-proxy\out
                  (build it with device\haltee-proxy\build.ps1)
    -HalOrig      stock audio.primary.amlogic.so pulled from your own device
                  (adb pull /vendor/lib/hw/audio.primary.amlogic.so)
    -LegacySender haltee_rtp_sender built from device\daemon\aes67_rtp_sender.c

.PARAMETER Keystore
  Signing keystore. Default: %LOCALAPPDATA%\AES67Bridge\debug.keystore, created
  on first use (store/key password "android", alias "aes67").

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File build.ps1
  powershell -ExecutionPolicy Bypass -File build.ps1 -HalProxy ..\device\haltee-proxy\out\audio.primary.amlogic.so -HalOrig .\stock-hal.so
#>
param(
  [string]$Keystore = '',
  [string]$HalProxy = '',
  [string]$HalOrig = '',
  [string]$LegacySender = '',
  [string]$Out = ''
)
# Native tools (javac/aapt2/d8/apksigner) write progress to stderr; PowerShell
# turns that into terminating errors under 'Stop', so stay on 'Continue' and
# check $LASTEXITCODE explicitly.
$ErrorActionPreference = 'Continue'
$proj = $PSScriptRoot
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Resolve-Sdk {
  $roots = @()
  if ($env:ANDROID_SDK_ROOT) { $roots += $env:ANDROID_SDK_ROOT }
  if ($env:ANDROID_HOME) { $roots += $env:ANDROID_HOME }
  $roots += (Join-Path $env:LOCALAPPDATA 'Android\Sdk')
  foreach ($r in $roots) { if (Test-Path (Join-Path $r 'build-tools')) { return $r } }
  throw 'Android SDK not found (set ANDROID_SDK_ROOT).'
}
function Resolve-Latest([string]$dir) {
  $best = Get-ChildItem $dir -Directory | Sort-Object Name -Descending | Select-Object -First 1
  if (-not $best) { throw "no subdirectory under $dir" }
  return $best.FullName
}

$sdk = Resolve-Sdk
$bt = Resolve-Latest (Join-Path $sdk 'build-tools')
$platform = Get-ChildItem (Join-Path $sdk 'platforms') -Directory | Sort-Object Name -Descending |
  Where-Object { Test-Path (Join-Path $_.FullName 'android.jar') } | Select-Object -First 1
if (-not $platform) { throw "no platforms/android-*/android.jar under $sdk" }
$androidJar = Join-Path $platform.FullName 'android.jar'

if (-not $Out) { $Out = Join-Path $proj 'out' }
$classes = Join-Path $Out 'classes'
$assets = Join-Path $Out 'assets'
Remove-Item -Recurse -Force $Out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $classes, $assets | Out-Null
$env:TEMP = Join-Path $Out 'tmp'
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
$env:TMP = $env:TEMP

# 1) assets: repo shell scripts + optional runtime blobs
Copy-Item -Force (Join-Path $proj 'assets\*') $assets
$optional = @(
  @{ Param = 'HalProxy'; Name = 'audio.primary.amlogic.so'; Value = $HalProxy },
  @{ Param = 'HalOrig'; Name = 'audio.primary.amlogic.so.orig'; Value = $HalOrig },
  @{ Param = 'LegacySender'; Name = 'haltee_rtp_sender'; Value = $LegacySender }
)
foreach ($item in $optional) {
  if ($item.Value) {
    if (-not (Test-Path $item.Value)) { throw "-$($item.Param) not found: $($item.Value)" }
    Copy-Item -Force $item.Value (Join-Path $assets $item.Name)
  } else {
    Write-Output "skip asset $($item.Name) (-$($item.Param) not given)"
  }
}

# 2) javac -> classes
$srcs = Get-ChildItem (Join-Path $proj 'src') -Recurse -Filter *.java | ForEach-Object { $_.FullName }
if (-not $srcs) { throw 'no java sources under src\' }
& javac -source 8 -target 8 -Xlint:-options -bootclasspath $androidJar -d $classes @srcs 2>&1 |
  Out-File -FilePath (Join-Path $Out 'javac.log') -Encoding utf8
if ($LASTEXITCODE -ne 0) {
  Get-Content (Join-Path $Out 'javac.log') -Encoding utf8 | Select-Object -First 20
  throw 'javac failed'
}

# 3) aapt2 link -> base.apk (manifest + assets)
& (Join-Path $bt 'aapt2.exe') link -o (Join-Path $Out 'base.apk') --manifest (Join-Path $proj 'AndroidManifest.xml') -A $assets -I $androidJar --min-sdk-version 30 --target-sdk-version 30 2>&1 | Out-Null
if (-not (Test-Path (Join-Path $Out 'base.apk'))) { throw 'aapt2 link failed' }

# 4) d8 -> classes.dex, then add it to the apk
$classFiles = Get-ChildItem $classes -Recurse -Filter *.class | ForEach-Object { $_.FullName }
& (Join-Path $bt 'd8.bat') --lib $androidJar --min-api 30 --output $Out @classFiles 2>&1 | Out-Null
if (-not (Test-Path (Join-Path $Out 'classes.dex'))) { throw 'd8 failed (no classes.dex)' }
$zip = [System.IO.Compression.ZipFile]::Open((Join-Path $Out 'base.apk'), 'Update')
try {
  $old = $zip.Entries | Where-Object { $_.FullName -eq 'classes.dex' } | Select-Object -First 1
  if ($old) { $old.Delete() }
  [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $Out 'classes.dex'), 'classes.dex', [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
} finally { $zip.Dispose() }

# 5) zipalign + sign
& (Join-Path $bt 'zipalign.exe') -f 4 (Join-Path $Out 'base.apk') (Join-Path $Out 'aligned.apk')
if (-not $Keystore) {
  $Keystore = Join-Path $env:LOCALAPPDATA 'AES67Bridge\debug.keystore'
  if (-not (Test-Path $Keystore)) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Keystore) | Out-Null
    Write-Output "creating debug keystore: $Keystore"
    & keytool -genkeypair -keystore $Keystore -storetype JKS -alias aes67 -keyalg RSA -keysize 2048 -validity 10000 -storepass android -keypass android -dname 'CN=AES67Bridge, O=AES67Bridge' 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'keytool failed' }
  }
}
$apk = Join-Path $Out 'aes67-console.apk'
& (Join-Path $bt 'apksigner.bat') sign --ks $Keystore --ks-pass pass:android --key-pass pass:android --out $apk (Join-Path $Out 'aligned.apk') 2>&1 | Out-Null
if (-not (Test-Path $apk)) { throw 'apksigner failed' }
Write-Output ("apk: {0} bytes  md5={1}" -f (Get-Item $apk).Length, (Get-FileHash $apk -Algorithm MD5).Hash.ToLower())
Write-Output "install: adb install -r $apk"
