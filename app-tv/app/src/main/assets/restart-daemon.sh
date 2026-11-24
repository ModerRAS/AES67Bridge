#!/system/bin/sh
# restart-daemon.sh — kill the resident C AES67 daemon and restart it against /data/aes67bridge.
set -u
ROOT=/data/aes67bridge
BIN=$ROOT/aes67_rtp_sender
CONF=$ROOT/daemon.conf
LOG=$ROOT/logs/daemon.log
FIFO=$ROOT/live.pcm
META=$ROOT/live.meta
STATUS=$ROOT/status.json
SDP=$ROOT/aes67-v4.sdp
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"

# kill only the C daemon (comm aes67_rtp_send*); Rust aes67-daemon/aes67-feed are not touched
for p in /proc/[0-9]*; do
  comm=$(cat "$p/comm" 2>/dev/null)
  case "$comm" in aes67_rtp_send*) kill -9 "${p#/proc/}" 2>/dev/null ;; esac
done
sleep 0.4

# the FIFO is created by the HAL proxy only while live.enable exists; wait briefly for it
i=0
while [ ! -p "$FIFO" ] && [ "$i" -lt 20 ]; do sleep 0.25; i=$((i+1)); done
if [ ! -x "$BIN" ]; then echo "ERROR daemon binary missing: $BIN"; exit 1; fi

if [ -x /system/bin/setsid ]; then
  setsid "$BIN" --fifo "$FIFO" --meta "$META" --seconds 0 --config "$CONF" --status "$STATUS" --sdp "$SDP" </dev/null >>"$LOG" 2>&1 &
else
  nohup "$BIN" --fifo "$FIFO" --meta "$META" --seconds 0 --config "$CONF" --status "$STATUS" --sdp "$SDP" </dev/null >>"$LOG" 2>&1 &
fi
STARTED=""
for i in 1 2 3 4 5 6 7 8 9 10; do
  sleep 0.2
  for p in /proc/[0-9]*; do
    comm=$(cat "$p/comm" 2>/dev/null)
    case "$comm" in aes67_rtp_send*) STARTED="${p#/proc/}" ;; esac
  done
  [ -n "$STARTED" ] && break
done
if [ -n "$STARTED" ]; then
  echo "started pid=$STARTED"
else
  echo "ERROR start failed (see $LOG)"
  exit 1
fi
