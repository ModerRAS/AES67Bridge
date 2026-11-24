#!/system/bin/sh
# AES67 autostart bootstrap (root) — idempotent.
# Single runtime dir: /data/aes67bridge (root:audio 0775).
# Stage: <app files dir> (passed via arg1). HAL tee overlay lives in $ROOT/ovl (tmpfs, RAM only).
set -u
STAGE="${1:-/data/user/0/dev.aes67bridge.tv/files}"
ROOT=/data/aes67bridge
OVL=$ROOT/ovl
UPPER=$OVL/upper
WORK=$OVL/work
LOGDIR=$ROOT/logs
LOG=$LOGDIR/bootstrap.log
umask 027

mkdir -p "$ROOT" "$LOGDIR"
chown root:audio "$ROOT" 2>/dev/null || chown root:1005 "$ROOT"
chmod 0775 "$ROOT"
chown root:root "$LOGDIR"
chmod 0700 "$LOGDIR"
exec >> "$LOG" 2>&1
echo "=== $(date) bootstrap start (stage=$STAGE) ==="

# 0) config + state files (0644 root): daemon.conf is seeded from the stage only when absent
if [ ! -f "$ROOT/daemon.conf" ] && [ -f "$STAGE/daemon.conf" ]; then cp -f "$STAGE/daemon.conf" "$ROOT/daemon.conf"; fi
if [ ! -f "$ROOT/daemon.conf" ]; then
  printf 'group=239.69.67.1\nport=5004\nptime_us=1000\nmaxch=8\nlayout=7.1\nhal_buf=20\npcm_export=1\nbits_export=1\n' > "$ROOT/daemon.conf"
fi
[ -f "$ROOT/state.txt" ] || echo stopped > "$ROOT/state.txt"
[ -f "$ROOT/mode.txt" ] || echo normal > "$ROOT/mode.txt"
chown root:root "$ROOT/daemon.conf" "$ROOT/state.txt" "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/daemon.conf" "$ROOT/state.txt" "$ROOT/mode.txt"

# 1) tmpfs upper for the /vendor/lib/hw overlay (RAM only; /data ext4 cannot be overlay upper here)
mkdir -p "$OVL"
if ! grep -q " $OVL tmpfs " /proc/mounts; then
  mount -t tmpfs -o size=64m,mode=755 tmpfs "$OVL" || echo "tmpfs mount FAILED"
fi
mkdir -p "$UPPER" "$WORK"
chmod 755 "$OVL" "$UPPER" "$WORK"

# 2) stage proxy + stock original into the overlay upper (rename keeps a running HAL mapping intact)
cp -f "$STAGE/audio.primary.amlogic.so" "$UPPER/.audio.primary.amlogic.so.new"
cp -f "$STAGE/audio.primary.amlogic.so.orig" "$UPPER/.audio.primary.amlogic.so.orig.new"
chmod 755 "$UPPER/.audio.primary.amlogic.so.new" "$UPPER/.audio.primary.amlogic.so.orig.new"
mv -f "$UPPER/.audio.primary.amlogic.so.new" "$UPPER/audio.primary.amlogic.so"
mv -f "$UPPER/.audio.primary.amlogic.so.orig.new" "$UPPER/audio.primary.amlogic.so.orig"
echo "stage md5:"; md5sum "$STAGE/audio.primary.amlogic.so" "$STAGE/audio.primary.amlogic.so.orig"

# 3) mount overlay over /vendor/lib/hw; restart HAL only when newly mounted
if grep -q " /vendor/lib/hw overlay" /proc/mounts; then
  echo "overlay already mounted"
else
  mount -t overlay overlay -o lowerdir=/vendor/lib/hw,upperdir=$UPPER,workdir=$WORK /vendor/lib/hw
  echo "overlay mount rc=$?"
  setprop ctl.restart vendor.audio-hal
  sleep 3
fi
echo "merged:"; ls -l /vendor/lib/hw/audio.primary.amlogic.so* 2>&1
md5sum /vendor/lib/hw/audio.primary.amlogic.so 2>&1

# 4) resident AES67 daemon binary inside the runtime dir (0750 root)
if [ -f "$STAGE/haltee_rtp_sender" ]; then
  cp -f "$STAGE/haltee_rtp_sender" "$ROOT/.aes67_rtp_sender.new"
  chown root:root "$ROOT/.aes67_rtp_sender.new"
  chmod 0750 "$ROOT/.aes67_rtp_sender.new"
  mv -f "$ROOT/.aes67_rtp_sender.new" "$ROOT/aes67_rtp_sender"
fi

# 5) daemon launcher (root helper stays in /data/local/tmp/haltee)
mkdir -p /data/local/tmp/haltee
if [ -f "$STAGE/restart-daemon.sh" ]; then
  cp -f "$STAGE/restart-daemon.sh" /data/local/tmp/haltee/restart-daemon.sh
  chmod 755 /data/local/tmp/haltee/restart-daemon.sh
fi

# 6) AES67 mode from daemon.conf (single source of truth; default = standard 20ms)
hal_buf=20; pcm_export=1; bits_export=1
if [ -f "$ROOT/daemon.conf" ]; then sed 's/\r$//' "$ROOT/daemon.conf" > "$ROOT/.conf.lf"; . "$ROOT/.conf.lf"; fi
if [ "$hal_buf" = "5" ]; then
  echo 240 > "$ROOT/latency.frames"
  echo 8192 > "$ROOT/fifo.pipe_bytes"
  echo lowlat > "$ROOT/mode.txt"
else
  rm -f "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes" 2>/dev/null
  echo normal > "$ROOT/mode.txt"
fi
if [ "$pcm_export" = "1" ] || [ "$bits_export" = "1" ]; then
  touch "$ROOT/live.enable"
else
  rm -f "$ROOT/live.enable"
fi
chown root:root "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/mode.txt" "$ROOT/live.enable" 2>/dev/null
chmod 0644 "$ROOT/latency.frames" "$ROOT/fifo.pipe_bytes" 2>/dev/null

# 7) wait for the HAL proxy to create the FIFOs (gated by live.enable), then (re)start the daemon
i=0
while [ ! -p "$ROOT/live.pcm" ] && [ "$i" -lt 20 ]; do sleep 0.25; i=$((i+1)); done
if [ "${engine:-legacy}" = "rust" ] && [ -f "$STAGE/restart.sh" ] && [ -x "$ROOT/aes67-daemon" ]; then
  echo "bootstrap: rust engine -> restart.sh"
  sh "$STAGE/restart.sh" >> "$LOG" 2>&1
elif [ -x /data/local/tmp/haltee/restart-daemon.sh ]; then
  sh /data/local/tmp/haltee/restart-daemon.sh >> "$LOG" 2>&1
else
  echo "ERROR restart-daemon.sh missing" >> "$LOG"
fi
echo running > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt"
chmod 0644 "$ROOT/state.txt"
echo "fifos:"; ls -l "$ROOT/live.pcm" "$ROOT/live.bits" 2>&1
echo "=== bootstrap done ==="