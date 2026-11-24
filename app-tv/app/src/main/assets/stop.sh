#!/system/bin/sh
# Stop AES67 broadcast: disable FIFO export + kill the C daemon. Keeps overlay/HAL for fast restart.
set -u
ROOT=/data/aes67bridge
LOG=$ROOT/logs/daemon.log
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"
rm -f "$ROOT/live.enable"
# stop either engine (Rust daemon and legacy C sender)
for p in $(pidof aes67-daemon); do kill -9 "$p" 2>/dev/null; done
for p in $(pidof aes67_rtp_sender); do kill -9 "$p" 2>/dev/null; done
sleep 1
rm -f "$ROOT/status.json"
echo stopped > "$ROOT/state.txt"
chmod 0644 "$ROOT/state.txt"
echo "$(date) stopped" >> "$LOG"