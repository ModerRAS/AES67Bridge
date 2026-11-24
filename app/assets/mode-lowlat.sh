#!/system/bin/sh
# AES67 low-latency mode: 5ms HAL write block + 8KB pipe (takes effect on next stream open)
echo 240 > /data/haltee/latency.frames
chmod 666 /data/haltee/latency.frames 2>/dev/null
echo 8192 > /data/haltee/fifo.pipe_bytes
chmod 666 /data/haltee/fifo.pipe_bytes 2>/dev/null
echo lowlat > /data/haltee/mode.txt
