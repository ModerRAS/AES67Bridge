#!/system/bin/sh
# AES67 autostart rollback (root) — stop everything, restore stock audio
DATA=/data/aes67
ROOT=/data/haltee
echo "=== $(date) rollback ==="
# stop daemon
for p in $(pidof haltee_rtp_sender); do kill -9 $p; done
rm -f /data/local/tmp/aes67_rtp_sender
# stop live/mode flags
rm -f $ROOT/live.enable $ROOT/latency.frames $ROOT/fifo.pipe_bytes
# stop HAL before unmount (mmap holds files)
setprop ctl.stop vendor.audio-hal
sleep 2
if grep -q " /vendor/lib/hw " /proc/mounts; then
  umount /vendor/lib/hw 2>/dev/null || umount -l /vendor/lib/hw 2>/dev/null
fi
if grep -q " on $DATA/ovl " /proc/mounts; then
  umount $DATA/ovl 2>/dev/null || umount -l $DATA/ovl 2>/dev/null
fi
setprop ctl.start vendor.audio-hal
sleep 2
setprop ctl.restart audioserver
sleep 2
{
  echo "time=$(date)"
  echo "hal=$(getprop init.svc.vendor.audio-hal)"
  echo "daemon="
  echo "overlay=$(grep -c ' /vendor/lib/hw overlay' /proc/mounts)"
  echo "live="
} > $DATA/status.txt
chmod 644 $DATA/status.txt
echo "=== rollback done ==="
