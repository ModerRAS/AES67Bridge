#!/system/bin/sh
# AES67 low-latency mode: 5ms HAL write block + 8KB pipe (takes effect on next stream open)
set -u
ROOT=/data/aes67bridge
umask 027
echo 240 > "$ROOT/latency.frames"
echo 8192 > "$ROOT/fifo.pipe_bytes"
echo lowlat > "$ROOT/mode.txt"
chown root:root "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes" "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes" "$ROOT/mode.txt"
