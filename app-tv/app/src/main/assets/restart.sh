#!/system/bin/sh
# restart.sh — restart the configured AES67 engine (legacy C or new Rust daemon).
set -u
STAGE=/data/user/0/dev.aes67bridge.tv/files
ROOT=/data/aes67bridge
CONF=$ROOT/daemon.conf
LOG=$ROOT/logs/daemon.log
umask 027
mkdir -p "$ROOT/logs"; chown root:root "$ROOT/logs"; chmod 0700 "$ROOT/logs"
group=239.69.67.1; port=5004; ptime_us=1000; layout=7.1; decode=auto; engine=legacy
ptp=0; ptp_gm=; ptp_domain=0
if [ -f "$CONF" ]; then sed 's/\r$//' "$CONF" > "$ROOT/.conf.lf"; chmod 0600 "$ROOT/.conf.lf"; . "$ROOT/.conf.lf"; fi

# stop either engine
for p in /proc/[0-9]*; do
  comm=$(cat "$p/comm" 2>/dev/null)
  case "$comm" in aes67_rtp_send*|aes67-daemon) kill -9 "${p#/proc/}" 2>/dev/null ;; esac
done
sleep 0.4

# stale SDP from the previous engine/format would mislead receivers before the first block
rm -f "$ROOT"/aes67.sdp "$ROOT"/aes67-v4.sdp 2>/dev/null

if [ "$engine" = "rust" ]; then
  BIN=$ROOT/aes67-daemon
  [ -x "$BIN" ] || BIN=/data/aes67bridge/legacy/aes67-daemon
  if [ ! -x "$BIN" ]; then echo "$(date) ERROR rust daemon missing" >> "$LOG"; exit 1; fi
  PTPARG=""
  [ "$ptp" = "1" ] && PTPARG="--ptp --ptp-domain ${ptp_domain:-0}"
  GM=""
  [ -n "${ptp_gm:-}" ] && GM="--ptp-gm $ptp_gm"
  DUMPARGS=""
  [ "${dump:-0}" = "1" ] && DUMPARGS="--dump-bits $ROOT/dump.bits --dump-pcm $ROOT/dump.pcm"
  setsid "$BIN" --codec "$decode" --target-layout "$layout" \
    --bits "$ROOT/live.bits" --bits-meta "$ROOT/live.bits.meta" \
    --fifo "$ROOT/live.pcm" --meta "$ROOT/live.meta" \
    --sdp "$ROOT/aes67.sdp" --status "$ROOT/status.json" \
    --lock "$ROOT/daemon.lock" --config "$CONF" \
    --group "$group" --port "$port" --ptime-us "$ptime_us" \
    --local-ip auto --sched-fifo 80 $PTPARG $GM $DUMPARGS --seconds 0 \
    </dev/null >>"$LOG" 2>&1 &
  sleep 1
else
  if [ -x /data/aes67bridge/legacy/restart-daemon.sh ]; then
    sh /data/aes67bridge/legacy/restart-daemon.sh >> "$LOG" 2>&1
  else
    echo "$(date) ERROR legacy restart-daemon.sh missing" >> "$LOG"; exit 1
  fi
fi
echo "$(date) engine=$engine decode=$decode layout=$layout group=$group:$port ptp=$ptp gm=${ptp_gm:-multicast}" >> "$LOG"