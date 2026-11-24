#!/system/bin/sh
# AES67 autostart bootstrap (root) — idempotent
# Stage: <app files dir> (passed via arg1) ; state: /data/aes67 ; HAL tee root: /data/haltee
set -u
STAGE="${1:-/data/data/dev.aes67.autostart/files}"
DATA=/data/aes67
ROOT=/data/haltee
OVL=$DATA/ovl
UPPER=$OVL/upper
WORK=$OVL/work
LOG=$DATA/bootstrap.log
mkdir -p $DATA $ROOT $ROOT/logs
exec >> $LOG 2>&1
echo "=== $(date) bootstrap start (stage=$STAGE) ==="

# 1) tmpfs upper (RAM only; /data ext4 cannot be overlay upper on this kernel)
mkdir -p $OVL
if ! grep -q " on $OVL type tmpfs" /proc/mounts; then
  mount -t tmpfs -o size=64m,mode=755 tmpfs $OVL || echo "tmpfs mount FAILED"
fi
mkdir -p $UPPER $WORK
chmod 755 $OVL $UPPER $WORK

# 2) stage proxy + original into upper
cp -f "$STAGE/audio.primary.amlogic.so" "$UPPER/audio.primary.amlogic.so"
cp -f "$STAGE/audio.primary.amlogic.so.orig" "$UPPER/audio.primary.amlogic.so.orig"
chmod 755 "$UPPER/audio.primary.amlogic.so" "$UPPER/audio.primary.amlogic.so.orig"
echo "stage md5:"; md5sum "$STAGE/audio.primary.amlogic.so" "$STAGE/audio.primary.amlogic.so.orig"

# 3) mount overlay over /vendor/lib/hw (only if not mounted)
if grep -q " /vendor/lib/hw overlay" /proc/mounts; then
  echo "overlay already mounted"
else
  mount -t overlay overlay -o lowerdir=/vendor/lib/hw,upperdir=$UPPER,workdir=$WORK /vendor/lib/hw
  echo "overlay mount rc=$?"
  # restart HAL so it loads the proxy
  setprop ctl.restart vendor.audio-hal
  sleep 3
fi
echo "merged:"; ls -l /vendor/lib/hw/audio.primary.amlogic.so* 2>&1
md5sum /vendor/lib/hw/audio.primary.amlogic.so 2>&1

# 4) root RTP daemon (resident)
if ! pidof haltee_rtp_sender >/dev/null 2>&1; then
  cp -f "$STAGE/haltee_rtp_sender" /data/local/tmp/aes67_rtp_sender 2>/dev/null
  chmod 755 /data/local/tmp/aes67_rtp_sender 2>/dev/null
  nohup /data/local/tmp/aes67_rtp_sender --fifo /data/haltee/live.pcm --meta /data/haltee/live.meta --status /data/haltee/status.json --seconds 0 >> $DATA/daemon.log 2>&1 &
  sleep 1
fi

# 5) AES67 mode (5ms HAL write block) + live FIFO export
echo 240 > $ROOT/latency.frames 2>/dev/null
echo 8192 > $ROOT/fifo.pipe_bytes 2>/dev/null
touch $ROOT/live.enable
chmod 666 $ROOT/live.enable $ROOT/latency.frames $ROOT/fifo.pipe_bytes 2>/dev/null

# 6) status for app
{
  echo "time=$(date)"
  echo "hal=$(getprop init.svc.vendor.audio-hal)"
  echo "daemon=$(pidof haltee_rtp_sender)"
  echo "overlay=$(grep -c ' /vendor/lib/hw overlay' /proc/mounts)"
  echo "live=$(ls $ROOT/live.enable 2>/dev/null)"
  echo "proxy_md5=$(md5sum /vendor/lib/hw/audio.primary.amlogic.so 2>/dev/null | cut -d' ' -f1)"
} > $DATA/status.txt 2>&1
chmod 644 $DATA/status.txt
echo "=== bootstrap done ==="