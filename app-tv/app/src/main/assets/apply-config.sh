#!/system/bin/sh
# Apply UI config (single source of truth = /data/aes67bridge/daemon.conf), then restart daemon.
# Mode is DERIVED from hal_buf: 20 -> normal (20ms), 5 -> lowlat (5ms).
set -u
APP=/data/data/dev.aes67bridge.tv/files
ROOT=/data/aes67bridge
CONF=$ROOT/daemon.conf
LOG=$ROOT/logs/daemon.log
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"
if [ -f "$APP/daemon.conf" ]; then cp -f "$APP/daemon.conf" "$CONF"; fi
chown root:root "$CONF" 2>/dev/null
chmod 0644 "$CONF" 2>/dev/null

group=239.69.67.1; port=5004; ptime_us=1000; maxch=8; layout=7.1; codec=pcm
split=1; sap=1; gain_db=0; hal_buf=20; fifo_kb=32; ptp=0; asrc=0
pcm_export=1; bits_export=1
[ -f "$CONF" ] && . "$CONF"

if [ "$hal_buf" = "5" ]; then
  echo 240 > "$ROOT/latency.frames"
  echo 8192 > "$ROOT/fifo.pipe_bytes"
  echo lowlat > "$ROOT/mode.txt"
else
  rm -f "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes"
  echo normal > "$ROOT/mode.txt"
fi
chown root:root "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/mode.txt"
for f in "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes"; do
  [ -f "$f" ] && { chown root:root "$f" 2>/dev/null; chmod 0644 "$f"; }
done

if [ "$pcm_export" = "1" ] || [ "$bits_export" = "1" ]; then
  touch "$ROOT/live.enable"
else
  rm -f "$ROOT/live.enable"
fi
chmod 0644 "$ROOT/live.enable" 2>/dev/null

# daemon binary must exist (rollback removes it); reinstall from the app stage
if [ ! -x "$ROOT/aes67_rtp_sender" ] && [ -f "$APP/haltee_rtp_sender" ]; then
  cp -f "$APP/haltee_rtp_sender" "$ROOT/aes67_rtp_sender"
  chown root:root "$ROOT/aes67_rtp_sender"
  chmod 0750 "$ROOT/aes67_rtp_sender"
fi

if [ -f "$APP/restart.sh" ]; then
  sh "$APP/restart.sh" >> "$LOG" 2>&1
else
  echo "ERROR restart.sh missing" >> "$LOG"
fi
echo running > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt" 2>/dev/null
chmod 0644 "$ROOT/state.txt"
echo "$(date) applied group=$group port=$port ptime_us=$ptime_us layout=$layout hal_buf=$hal_buf mode=$(cat $ROOT/mode.txt 2>/dev/null) fifo_kb=$fifo_kb" >> "$LOG"
