#!/system/bin/sh
# AES67Bridge boot hook - runs as root at every boot via the Zidoo root-cmd bridge.
#
# Mechanism (no /system or /vendor modification):
#   * /data/property/persistent_properties is pre-seeded with two non-persist keys:
#       zidoo.root.cmd.start2=1
#       zidoo.root.cmd2=/data/aes67bridge/boot/trigger.sh
#     init loads these at boot (it accepts non-persist keys from that file).
#   * /system/bin/zd_root_cmd.sh (started as root by /system/bin/zidoo_daemon) sees
#     start2=1, deletes trigger.sh, and executes its content as root.
#   * trigger.sh contains: sh /data/aes67bridge/boot/aes67boot.sh
#
# Why not BOOT_COMPLETED: this firmware marks every third-party app stopped=true at
# boot, so BroadcastReceiver apps never receive BOOT_COMPLETED. See README.
set -u
ROOT=/data/aes67bridge
STAGE=/data/user/0/dev.aes67bridge.tv/files
LOG=$ROOT/logs/boot.log
mkdir -p $ROOT/logs $ROOT/boot
chown root:root $ROOT/logs $ROOT/boot
chmod 0700 $ROOT/logs
chmod 0755 $ROOT/boot
exec >> $LOG 2>&1
echo "=== boot hook start $(date) uid=$(id -u) ==="
i=0
# /data/user/0 is FBE-encrypted: wait until the user is unlocked and app assets exist.
while [ ! -f $STAGE/start.sh ] && [ $i -lt 180 ]; do sleep 1; i=$((i+1)); done
if [ -f $STAGE/start.sh ]; then
  echo "stage ready after $i s"
  sh $STAGE/start.sh
  echo "start.sh rc=$?"
else
  echo "ERROR: stage/start.sh not found after wait"
fi
# recreate the one-shot trigger (zd_root_cmd.sh deletes it before executing)
printf '%s\n' 'sh /data/aes67bridge/boot/aes67boot.sh' > /data/aes67bridge/boot/trigger.sh
chmod 0755 /data/aes67bridge/boot/trigger.sh
# persistent trigger keys may be dropped by property-store rewrites; re-append once.
PP=/data/property/persistent_properties
if ! grep -qa 'zidoo.root.cmd2' $PP 2>/dev/null; then
  cat $ROOT/boot/pp_append.bin >> $PP
  chown root:root $PP
  chmod 0600 $PP
  echo "property trigger re-appended"
fi
echo "=== boot hook done $(date) daemon=$(pidof aes67-daemon) ==="
