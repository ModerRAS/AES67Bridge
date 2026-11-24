param()
$ErrorActionPreference = 'Continue'
$proj = 'C:\WorkSpace\Claw\scratch\zidoo-autostart-2026-11-24'
$sdk = 'C:\Users\ModerRAS\AppData\Local\Android\Sdk'
$bt = Join-Path $sdk 'build-tools\34.0.0'
$androidJar = Join-Path $sdk 'platforms\android-34\android.jar'
$out = Join-Path $proj 'out'
Remove-Item (Join-Path $out '*') -Force -Recurse -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $out | Out-Null
Remove-Item (Join-Path $proj 'classes\*') -Force -Recurse -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $proj 'classes') | Out-Null

# assets: proxy v5.1 + original + daemon v4.3
Copy-Item -Force 'C:\WorkSpace\Claw\artifacts\aes67\haltee-v5\audio.primary.amlogic.so' (Join-Path $proj 'assets\audio.primary.amlogic.so')
Copy-Item -Force 'R:\zidoo-reverse\x\vendor\lib\hw\audio.primary.amlogic.so' (Join-Path $proj 'assets\audio.primary.amlogic.so.orig')
Copy-Item -Force 'C:\WorkSpace\Claw\scratch\aes67-sender-2026-11-24\device\aes67_rtp_sender_v43' (Join-Path $proj 'assets\haltee_rtp_sender')
Write-Output ('assets proxy=' + (Get-Item (Join-Path $proj 'assets\audio.primary.amlogic.so')).Length + ' daemon=' + (Get-Item (Join-Path $proj 'assets\haltee_rtp_sender')).Length)

$srcs = Get-ChildItem (Join-Path $proj 'src') -Recurse -Filter *.java | ForEach-Object { $_.FullName }
& javac -source 8 -target 8 -bootclasspath $androidJar -d (Join-Path $proj 'classes') $srcs > (Join-Path $proj 'javac.log') 2>&1
if ($LASTEXITCODE -ne 0) { 'JAVAC FAILED:'; Get-Content (Join-Path $proj 'javac.log') | Select-Object -First 15; exit 1 }

& (Join-Path $bt 'aapt2.exe') link -o (Join-Path $out 'base.apk') --manifest (Join-Path $proj 'AndroidManifest.xml') -A (Join-Path $proj 'assets') -I $androidJar --min-sdk-version 30 --target-sdk-version 30 2>&1 | Out-Null
if (!(Test-Path (Join-Path $out 'base.apk'))) { 'AAPT2 LINK FAILED'; exit 1 }

$classes = Get-ChildItem (Join-Path $proj 'classes') -Recurse -Filter *.class | ForEach-Object { $_.FullName }
& (Join-Path $bt 'd8.bat') --lib $androidJar --min-api 30 --output $out $classes > (Join-Path $proj 'd8.log') 2>&1
if (!(Test-Path (Join-Path $out 'classes.dex'))) { 'D8 FAILED:'; Get-Content (Join-Path $proj 'd8.log') | Select-Object -First 15; exit 1 }

Push-Location $out
& 'C:\Users\ModerRAS\scoop\shims\7z.exe' a -tzip base.apk classes.dex | Out-Null
Pop-Location
& (Join-Path $bt 'zipalign.exe') -f 4 (Join-Path $out 'base.apk') (Join-Path $out 'aligned.apk')
& (Join-Path $bt 'apksigner.bat') sign --ks 'C:\WorkSpace\Claw\scratch\zidoo-root-2026-11-23\hello-app\out\debug.keystore' --ks-pass pass:android --key-pass pass:android --out (Join-Path $out 'aes67-auto.apk') (Join-Path $out 'aligned.apk') 2>&1 | Out-Null
if (Test-Path (Join-Path $out 'aes67-auto.apk')) { 'APK OK size=' + (Get-Item (Join-Path $out 'aes67-auto.apk')).Length } else { 'SIGN FAILED'; exit 1 }
