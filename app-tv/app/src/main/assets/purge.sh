#!/system/bin/sh
# purge.sh (root) - clear runtime state under /data/aes67bridge.
# Keeps the daemon binary, HAL proxy assets and daemon.conf.
set -u
ROOT=/data/aes67bridge
umask 027

# 1) stop the daemon so the FIFOs can be removed
for p in $(pidof aes67-daemon); do kill -9 "$p" 2>/dev/null; done
sleep 1
rm -f "$ROOT/live.enable"

# 2) runtime data files
rm -f "$ROOT/live.pcm" "$ROOT/live.bits" "$ROOT/live.meta" "$ROOT/live.bits.meta"
rm -f "$ROOT/status.json" "$ROOT/state.txt" "$ROOT/mode.txt"
rm -f "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes"
rm -f "$ROOT"/*.sdp
rm -rf "$ROOT/logs"/*

# 3) leftovers from pre-0.2.0 installs (legacy C sender)
rm -f "$ROOT/aes67_rtp_sender" "$ROOT/haltee_rtp_sender"
rm -f /data/local/tmp/aes67_rtp_sender
rm -f /data/aes67bridge/legacy/restart-daemon.sh
rmdir /data/aes67bridge/legacy 2>/dev/null

echo "purged; remaining entries (binaries/config kept):"
ls -l "$ROOT" 2>&1
