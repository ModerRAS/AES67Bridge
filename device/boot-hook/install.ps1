# install.ps1 - deploy the AES67Bridge boot hook to a rooted Zidoo device.
# Requires: adb connected as root (adb root), app-tv installed and started at least once.
$ErrorActionPreference = 'Stop'
$fragB64 = 'ChoKFXppZG9vLnJvb3QuY21kLnN0YXJ0MhIBMQo0Cg96aWRvby5yb290LmNtZDISIS9kYXRhL2FlczY3YnJpZGdlL2Jvb3QvdHJpZ2dlci5zaA=='
$repo = Split-Path -Parent $PSScriptRoot
$tmp  = Join-Path $env:TEMP 'aes67boot'
New-Item -ItemType Directory -Force $tmp | Out-Null
$aes67boot = Join-Path $PSScriptRoot 'aes67boot.sh'
$trigger   = Join-Path $PSScriptRoot 'trigger.sh'
$frag      = Join-Path $tmp 'pp_append.bin'
[IO.File]::WriteAllBytes($frag, [Convert]::FromBase64String($fragB64))

adb root | Out-Null
adb shell 'mkdir -p /data/aes67bridge/boot /data/aes67bridge/logs'
adb push $aes67boot /data/aes67bridge/boot/aes67boot.sh | Out-Null
adb push $trigger   /data/aes67bridge/boot/trigger.sh  | Out-Null
adb push $frag      /data/aes67bridge/boot/pp_append.bin | Out-Null
adb shell 'chown root:root /data/aes67bridge/boot/*; chmod 0755 /data/aes67bridge/boot/aes67boot.sh /data/aes67bridge/boot/trigger.sh; chmod 0644 /data/aes67bridge/boot/pp_append.bin'

# Seed persistent_properties (idempotent); back up first.
adb shell 'cp /data/property/persistent_properties /data/local/tmp/persistent_properties.bak 2>/dev/null'
adb shell 'PP=/data/property/persistent_properties; if ! grep -qa zidoo.root.cmd2 $PP 2>/dev/null; then cat /data/aes67bridge/boot/pp_append.bin >> $PP; chown root:root $PP; chmod 0600 $PP; fi'

Write-Host 'Boot hook installed. Reboot and verify /data/aes67bridge/logs/boot.log + overlay/daemon.'
