#!/system/bin/sh
# AES67 standard mode: 20ms HAL write block, default pipe
set -u
ROOT=/data/aes67bridge
umask 027
rm -f "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes"
echo normal > "$ROOT/mode.txt"
chown root:root "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/mode.txt"
