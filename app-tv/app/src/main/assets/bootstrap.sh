#!/system/bin/sh
# AES67Bridge autostart bootstrap (root) - idempotent.
# Single runtime dir: /data/aes67bridge. Stage = app private files dir (arg1).
# HAL tee overlay lives in $ROOT/ovl (tmpfs, RAM only).
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
  printf 'group=239.69.67.1\nport=5004\nptime_us=1000\nmaxch=2\nlayout=2.0\nhal_buf=20\npcm_export=1\nbits_export=1\n' > "$ROOT/daemon.conf"
fi
[ -f "$ROOT/state.txt" ] || echo stopped > "$ROOT/state.txt"
[ -f "$ROOT/mode.txt" ] || echo normal > "$ROOT/mode.txt"
chown root:root "$ROOT/daemon.conf" "$ROOT/state.txt" "$ROOT/mode.txt" 2>/dev/null
chmod 0644 "$ROOT/daemon.conf" "$ROOT/state.txt" "$ROOT/mode.txt"

# 1) tmpfs upper for the /vendor/lib/hw overlay. In the 0.3.0 layout the whole
#    runtime dir already lives on tmpfs, so $OVL is tmpfs-backed and only the
#    legacy ext4 layout needs its own tmpfs mount here.
mkdir -p "$OVL"
if grep -q " $ROOT tmpfs " /proc/mounts; then
  echo "runtime is tmpfs; $OVL needs no extra tmpfs"
elif ! grep -q " $OVL tmpfs " /proc/mounts; then
  mount -t tmpfs -o size=64m,mode=755 tmpfs "$OVL" || echo "tmpfs mount FAILED"
fi
mkdir -p "$UPPER" "$WORK"
chmod 755 "$OVL" "$UPPER" "$WORK"

# 2) stock HAL backup is provisioned from this device, never redistributed in the APK.
#    Only possible while /vendor/lib/hw is still the stock directory (not overlaid yet).
if [ ! -f "$STAGE/audio.primary.amlogic.so.orig" ] && ! grep -q " /vendor/lib/hw overlay " /proc/mounts; then
  cp -f /vendor/lib/hw/audio.primary.amlogic.so "$STAGE/audio.primary.amlogic.so.orig" 2>/dev/null \
    && chmod 0644 "$STAGE/audio.primary.amlogic.so.orig" \
    && echo "provisioned stock HAL backup from device"
fi
if [ ! -f "$STAGE/audio.primary.amlogic.so" ]; then
  echo "ERROR HAL tee proxy missing from stage ($STAGE/audio.primary.amlogic.so)"
else
  cp -f "$STAGE/audio.primary.amlogic.so" "$UPPER/.audio.primary.amlogic.so.new"
  chmod 755 "$UPPER/.audio.primary.amlogic.so.new"
  mv -f "$UPPER/.audio.primary.amlogic.so.new" "$UPPER/audio.primary.amlogic.so"
  if [ -f "$STAGE/audio.primary.amlogic.so.orig" ]; then
    cp -f "$STAGE/audio.primary.amlogic.so.orig" "$UPPER/.audio.primary.amlogic.so.orig.new"
    chmod 755 "$UPPER/.audio.primary.amlogic.so.orig.new"
    mv -f "$UPPER/.audio.primary.amlogic.so.orig.new" "$UPPER/audio.primary.amlogic.so.orig"
  fi
  echo "stage md5:"; md5sum "$STAGE/audio.primary.amlogic.so" "$STAGE/audio.primary.amlogic.so.orig" 2>&1
fi

# 3) mount overlay over /vendor/lib/hw, publish a CHANGED stage tee through the
#    MERGED path, and restart the HAL only when its content changed.
#    Two traps here, both learned the hard way:
#    a) writing $UPPER/audio.primary.amlogic.so directly does NOT refresh the
#       merged dentry while the overlay is mounted (the merged dir keeps serving
#       the old inode until the overlay is remounted, i.e. until a reboot);
#    b) copying over /vendor/lib/hw/audio.primary.amlogic.so in place truncates
#       and rewrites the very file a running HAL has mmap()ed: its code pages
#       change under it and the HAL dies with SIGBUS/SIGSEGV.  Replace by
#       rename() instead (new inode, old one stays alive until the restart) and
#       do nothing at all when the content is already identical.
mounted=0
if grep -q " /vendor/lib/hw overlay" /proc/mounts; then
  echo "overlay already mounted"
else
  mount -t overlay overlay -o lowerdir=/vendor/lib/hw,upperdir=$UPPER,workdir=$WORK /vendor/lib/hw
  echo "overlay mount rc=$?"
  mounted=1
fi
publish_merged() {
  name="$1"
  src="$STAGE/$name"
  if [ ! -f "$src" ]; then
    return 0
  fi
  smd5=$(md5sum "$src" | cut -d' ' -f1)
  dmd5=$(md5sum "/vendor/lib/hw/$name" 2>/dev/null | cut -d' ' -f1)
  if [ "$smd5" = "$dmd5" ]; then
    echo "$name unchanged ($smd5)"
    return 0
  fi
  cp -f "$src" "/vendor/lib/hw/.$name.new" || return 1
  chmod 0755 "/vendor/lib/hw/.$name.new"
  mv -f "/vendor/lib/hw/.$name.new" "/vendor/lib/hw/$name" || return 1
  echo "$name updated: $dmd5 -> $smd5"
  return 2
}
changed=0
publish_merged audio.primary.amlogic.so
if [ "$?" = "2" ]; then changed=1; fi
publish_merged audio.primary.amlogic.so.orig
if [ "$?" = "2" ]; then changed=1; fi
echo "merged:"; ls -l /vendor/lib/hw/audio.primary.amlogic.so* 2>&1
md5sum /vendor/lib/hw/audio.primary.amlogic.so 2>&1
if [ "$mounted" = "1" ] || [ "$changed" = "1" ]; then
  echo "HAL restart required (fresh mount or tee changed)"
  setprop ctl.restart vendor.audio-hal
  sleep 3
fi

# 4) AES67 mode from daemon.conf (single source of truth; default = standard 20ms)
hal_buf=20; pcm_export=1; bits_export=1
if [ -f "$ROOT/daemon.conf" ]; then sed 's/\r$//' "$ROOT/daemon.conf" > "$ROOT/.conf.lf"; . "$ROOT/.conf.lf"; fi
if [ "$hal_buf" = "5" ]; then
  echo 240 > "$ROOT/latency.frames"
  echo 8192 > "$ROOT/fifo.pipe_bytes"
  echo lowlat > "$ROOT/mode.txt"
else
  rm -f "$ROOT/latency.frames" 2>/dev/null
  pipe_bytes=$((fifo_kb * 1024))
  [ "$pipe_bytes" -ge 4096 ] && [ "$pipe_bytes" -le 32768 ] || pipe_bytes=32768
  echo "$pipe_bytes" > "$ROOT/fifo.pipe_bytes"
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

# 5) wait for the HAL proxy to create the FIFOs (gated by live.enable), then (re)start the daemon
i=0
while [ ! -p "$ROOT/live.pcm" ] && [ "$i" -lt 20 ]; do sleep 0.25; i=$((i+1)); done
chmod 0644 "$ROOT/live.bits.meta" "$ROOT/live.meta" 2>/dev/null
if [ -x "$ROOT/aes67-daemon" ] && [ -f "$STAGE/restart.sh" ]; then
  echo "bootstrap: rust engine -> restart.sh"
  sh "$STAGE/restart.sh" >> "$LOG" 2>&1
else
  echo "ERROR rust daemon or restart.sh missing (daemon=$ROOT/aes67-daemon stage=$STAGE/restart.sh)" >> "$LOG"
fi
echo running > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt"
chmod 0644 "$ROOT/state.txt"
echo "fifos:"; ls -l "$ROOT/live.pcm" "$ROOT/live.bits" 2>&1
echo "=== bootstrap done ==="
