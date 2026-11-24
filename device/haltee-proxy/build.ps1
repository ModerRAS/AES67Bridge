$ErrorActionPreference='Stop'
$ndk='C:\Users\ModerRAS\AppData\Local\Android\Sdk\ndk\27.2.12479018\toolchains\llvm\prebuilt\windows-x86_64\bin'
$src='C:\WorkSpace\Claw\scratch\haltee-2026-11-24\src'
$out='C:\WorkSpace\Claw\scratch\haltee-2026-11-24\out'
New-Item -ItemType Directory -Force -Path $out,'C:\WorkSpace\Claw\scratch\haltee-2026-11-24\buildtmp' | Out-Null
$env:TEMP='C:\WorkSpace\Claw\scratch\haltee-2026-11-24\buildtmp'
$env:TMP=$env:TEMP
$clang="$ndk\clang.exe"
Write-Output "clang: $clang"
& $clang --target=armv7a-linux-androideabi30 -shared -fPIC -O2 -Wall -Wextra -Wno-unused-parameter -Wno-sign-compare -o "$out\audio.primary.amlogic.so" "$src\haltee.c" "$src\symmap.c" -lm
Write-Output "exit=$LASTEXITCODE"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Get-Item "$out\audio.primary.amlogic.so" | Select-Object FullName,Length | Format-Table -AutoSize | Out-String -Width 200
& "$ndk\llvm-readelf.exe" -h "$out\audio.primary.amlogic.so" | Select-String -Pattern 'Class|Machine|Type' | ForEach-Object { $_.Line }
& "$ndk\llvm-readelf.exe" --dyn-syms -W "$out\audio.primary.amlogic.so" | Select-String -Pattern 'HMI|haltee_dev_open|haltee_write|haltee_open_out' | ForEach-Object { $_.Line }
& "$ndk\llvm-readelf.exe" -d "$out\audio.primary.amlogic.so" | Select-String -Pattern 'NEEDED|SONAME' | ForEach-Object { $_.Line }