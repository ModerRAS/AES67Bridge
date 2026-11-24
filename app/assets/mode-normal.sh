#!/system/bin/sh
# AES67 standard mode: 20ms HAL write block, default pipe
rm -f /data/haltee/latency.frames /data/haltee/fifo.pipe_bytes
echo normal > /data/haltee/mode.txt
