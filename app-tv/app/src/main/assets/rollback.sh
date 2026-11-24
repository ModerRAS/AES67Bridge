#!/system/bin/sh
# AES67 autostart rollback (root) — stop everything, restore stock audio.
# Keeps /data/aes67bridge itself (state.txt=rolledback); removes the daemon copy (handled by rollback).
set -u
ROOT=/data/aes67bridge
OVL=$ROOT/ovl
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"
echo "=== $(date) rollback ==="

# 1) stop the C daemon (only aes67_rtp_send*; Rust aes67-daemon/aes67-feed are left alone)
for p in $(pidof aes67_rtp_sender); do kill -9 "$p" 2>/dev/null; done
rm -f "$ROOT/aes67_rtp_sender"
rm -f /data/local/tmp/haltee/restart-daemon.sh

# 2) stop live/mode flags
rm -f "$ROOT/live.enable" "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes"

# 3) stop HAL before unmount (mmap holds files)
setprop ctl.stop vendor.audio-hal
sleep 2
if grep -q " /vendor/lib/hw overlay" /proc/mounts; then
  umount /vendor/lib/hw 2>/dev/null || umount -l /vendor/lib/hw 2>/dev/null
fi
if grep -q " $OVL tmpfs " /proc/mounts; then
  umount "$OVL" 2>/dev/null || umount -l "$OVL" 2>/dev/null
fi
setprop ctl.start vendor.audio-hal
sleep 2
setprop ctl.restart audioserver
sleep 2

# 4) clear status, mark rolled back (no transitional status files)
rm -f "$ROOT/status.json"
echo rolledback > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt" 2>/dev/null
chmod 0644 "$ROOT/state.txt"
{
  echo "time=$(date)"
  echo "hal=$(getprop init.svc.vendor.audio-hal)"
  echo "daemon="
  echo "overlay=$(grep -c ' /vendor/lib/hw overlay' /proc/mounts)"
  echo "live="
} >> "$ROOT/logs/rollback.log" 2>&1
echo "=== rollback done ==="
