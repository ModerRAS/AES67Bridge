<#
.SYNOPSIS
  Install the AES67Bridge boot hook (developer path, needs adb root).

.DESCRIPTION
  0.3.0 layout:
    * /data/aes67bridge.persist/boot/  tiny FBE-safe boot hook (survives reboot)
    * /data/aes67bridge/boot/          legacy ext4 trigger the vendor property points at
                                       (only reachable while the runtime tmpfs is unmounted)
    * /data/aes67bridge                runtime, mounted as tmpfs when enabled
  The app can also install this from its own settings page ("装机" tab), which does not
  need adb. This script is for development / batch deployment.

  Requires: adb as root (adb root) and the app installed at least once (its assets are
  the source for the daemon/scripts on first run).
#>
$ErrorActionPreference = 'Stop'
$fragB64 = 'ChoKFXppZG9vLnJvb3QuY21kLnN0YXJ0MhIBMQo0Cg96aWRvby5yb290LmNtZDISIS9kYXRhL2FlczY3YnJpZGdlL2Jvb3QvdHJpZ2dlci5zaA=='
$tmp  = Join-Path $env:TEMP 'aes67boot'
New-Item -ItemType Directory -Force $tmp | Out-Null
$aes67boot = Join-Path $PSScriptRoot 'aes67boot.sh'
$trigger   = Join-Path $PSScriptRoot 'trigger.sh'
$frag      = Join-Path $tmp 'pp_append.bin'
[IO.File]::WriteAllBytes($frag, [Convert]::FromBase64String($fragB64))

adb root | Out-Null
adb shell 'mkdir -p /data/aes67bridge.persist/boot /data/aes67bridge.persist/logs'
adb push $aes67boot /data/aes67bridge.persist/boot/aes67boot.sh | Out-Null
adb push $trigger   /data/aes67bridge.persist/boot/trigger.sh  | Out-Null
adb push $frag      /data/aes67bridge.persist/boot/pp_append.bin | Out-Null
adb shell 'chown -R root:root /data/aes67bridge.persist; chmod 0755 /data/aes67bridge.persist /data/aes67bridge.persist/boot; chmod 0700 /data/aes67bridge.persist/logs; chmod 0755 /data/aes67bridge.persist/boot/aes67boot.sh /data/aes67bridge.persist/boot/trigger.sh; chmod 0644 /data/aes67bridge.persist/boot/pp_append.bin'

# Legacy ext4 trigger (the vendor property still points here; the tmpfs hides it at runtime,
# the boot hook recreates it before mounting).
adb shell 'if ! grep -q " /data/aes67bridge tmpfs " /proc/mounts; then mkdir -p /data/aes67bridge/boot; printf "%s
" "sh /data/aes67bridge.persist/boot/aes67boot.sh" > /data/aes67bridge/boot/trigger.sh; chmod 0755 /data/aes67bridge/boot/trigger.sh; cp -f /data/aes67bridge.persist/boot/aes67boot.sh /data/aes67bridge/boot/aes67boot.sh; chmod 0755 /data/aes67bridge/boot/aes67boot.sh; fi'

# Seed the property trigger (idempotent; keeps a backup).
adb shell 'cp /data/property/persistent_properties /data/local/tmp/persistent_properties.bak 2>/dev/null'
adb shell 'PP=/data/property/persistent_properties; if ! grep -qa zidoo.root.cmd2 $PP 2>/dev/null; then cat /data/aes67bridge.persist/boot/pp_append.bin >> $PP; chown root:root $PP; chmod 0600 $PP; fi'

# Enable the tmpfs runtime (takes effect on the next reboot / start.sh run).
adb shell 'touch /data/aes67bridge.persist/enabled; chmod 0644 /data/aes67bridge.persist/enabled'

Write-Host 'Boot hook installed. Reboot, then verify /data/aes67bridge.persist/logs/boot.log and that /data/aes67bridge is tmpfs.'
