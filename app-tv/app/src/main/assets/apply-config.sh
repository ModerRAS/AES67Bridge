#!/system/bin/sh
# Apply UI config (single source of truth = /data/aes67bridge/daemon.conf), then restart the daemon.
# Mode is DERIVED from hal_buf: 20 -> normal (20ms), 5 -> lowlat (5ms).
set -u
APP=/data/aes67bridge/stage
ROOT=/data/aes67bridge
CONF=$ROOT/daemon.conf
LOG=$ROOT/logs/daemon.log
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"
# The app private dir is the only config source the GUI writes; sync it to the stage
# first, otherwise the stage still holds the previous start.sh copy (keys like sap vanish).
APPDIR=/data/user/0/dev.aes67bridge.tv/files
if [ -f "$APPDIR/daemon.conf" ]; then cp -f "$APPDIR/daemon.conf" "$APP/daemon.conf" 2>/dev/null; fi
if [ -f "$APP/daemon.conf" ]; then cp -f "$APP/daemon.conf" "$CONF"; fi
chown root:root "$CONF" 2>/dev/null
chmod 0644 "$CONF" 2>/dev/null

group=239.69.67.1; port=5004; ptime_us=1000; maxch=8; layout=7.1; codec=pcm
split=1; sap=1; gain_db=0; hal_buf=20; fifo_kb=32; ptp=0; asrc=0
pcm_export=1; bits_export=1
if [ -f "$CONF" ]; then sed 's/\r$//' "$CONF" > "$ROOT/.conf.lf"; . "$ROOT/.conf.lf"; fi

if [ "$hal_buf" = "5" ]; then
  echo 240 > "$ROOT/latency.frames"
  echo 8192 > "$ROOT/fifo.pipe_bytes"
  echo lowlat > "$ROOT/mode.txt"
else
  rm -f "$ROOT/latency.frames"
  # FIFO depth from the UI (8/16/32 KB).  The HAL tee applies it whenever it
  # (re)opens the PCM FIFO, so a stream switch picks a new value up without a
  # HAL restart.  Clamp to the tee's accepted range.
  pipe_bytes=$((fifo_kb * 1024))
  [ "$pipe_bytes" -ge 4096 ] && [ "$pipe_bytes" -le 32768 ] || pipe_bytes=32768
  echo "$pipe_bytes" > "$ROOT/fifo.pipe_bytes"
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

if [ -f "$APP/restart.sh" ]; then
  sh "$APP/restart.sh" >> "$LOG" 2>&1
else
  echo "ERROR restart.sh missing" >> "$LOG"
fi
echo running > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt" 2>/dev/null
chmod 0644 "$ROOT/state.txt"
echo "$(date) applied group=$group port=$port ptime_us=$ptime_us layout=$layout hal_buf=$hal_buf mode=$(cat $ROOT/mode.txt 2>/dev/null) fifo_kb=$fifo_kb" >> "$LOG"
