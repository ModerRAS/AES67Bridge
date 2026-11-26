#!/system/bin/sh
# start.sh - provision the AES67Bridge runtime and start it (root, idempotent).
#
# 0.3.0 layout:
#   app private dir  /data/user/0/dev.aes67bridge.tv/files  = persistent layer
#                      (assets/scripts/tee, daemon.conf, aes67-daemon, tools, media)
#   /data/aes67bridge.persist/boot                          = tiny FBE-safe boot hook
#   /data/aes67bridge                                       = runtime, tmpfs when enabled
#
# /data/aes67bridge.persist/enabled enables the tmpfs runtime. Without the marker
# the legacy ext4 layout is used, so a 0.2.x install keeps working until migrated.
set -u
APP=/data/user/0/dev.aes67bridge.tv/files
ROOT=/data/aes67bridge
PERSIST=/data/aes67bridge.persist
STAGE=$ROOT/stage
umask 027

fresh=0
if [ -f "$PERSIST/enabled" ]; then
    if ! grep -q " $ROOT tmpfs " /proc/mounts; then
        mkdir -p "$ROOT"
        mount -t tmpfs -o size=64m,mode=0775 tmpfs "$ROOT" || { echo "ERROR: tmpfs mount failed on $ROOT"; exit 1; }
        fresh=1
    fi
    chown root:audio "$ROOT" 2>/dev/null || chown root:1005 "$ROOT"
    chmod 0775 "$ROOT"
fi
mkdir -p "$ROOT/logs" "$ROOT/stage"
chown root:root "$ROOT/logs" 2>/dev/null
chmod 0700 "$ROOT/logs"
LOG=$ROOT/logs/daemon.log
exec >> "$LOG" 2>&1
echo "=== $(date) start.sh fresh=$fresh ==="

# 1) persistent daemon binary -> runtime (atomic replace, never rewrite a running inode)
if [ -x "$APP/aes67-daemon" ]; then
    a=$(md5sum "$APP/aes67-daemon" | cut -d' ' -f1)
    b=$(md5sum "$ROOT/aes67-daemon" 2>/dev/null | cut -d' ' -f1)
    if [ "$a" != "$b" ]; then
        cp -f "$APP/aes67-daemon" "$ROOT/.aes67-daemon.new" \
            && chown root:root "$ROOT/.aes67-daemon.new" \
            && chmod 0750 "$ROOT/.aes67-daemon.new" \
            && mv -f "$ROOT/.aes67-daemon.new" "$ROOT/aes67-daemon" \
            && echo "seeded daemon binary $a"
    fi
fi

# 2) GUI config is the single source of truth (written to the app dir)
if [ -f "$APP/daemon.conf" ]; then
    cp -f "$APP/daemon.conf" "$ROOT/daemon.conf"
    chown root:root "$ROOT/daemon.conf" 2>/dev/null
    chmod 0644 "$ROOT/daemon.conf" 2>/dev/null
fi

# 3) stage = app dir scripts + HAL tee .so files
mkdir -p "$STAGE"
for f in "$APP"/*.sh "$APP"/audio.primary.amlogic.so "$APP"/audio.primary.amlogic.so.orig; do
    [ -e "$f" ] || continue
    cp -f "$f" "$STAGE"/
done
chown root:root "$STAGE"/* 2>/dev/null
chmod 0755 "$STAGE"/*.sh "$STAGE"/*.so "$STAGE"/*.orig 2>/dev/null

# 4) runtime flag before the HAL restarts so the tee opens the FIFOs on the new tmpfs
touch "$ROOT/live.enable"
chmod 0644 "$ROOT/live.enable"
if [ "$fresh" = "1" ] && grep -q " /vendor/lib/hw overlay" /proc/mounts; then
    # Switching to tmpfs while the HAL already has the tee loaded: restart it so the
    # tee opens the new FIFOs. At boot the overlay is not mounted yet and bootstrap
    # mounts it + restarts the HAL anyway, so no double restart there.
    echo "fresh tmpfs runtime with HAL already loaded: restarting vendor audio HAL"
    setprop ctl.restart vendor.audio-hal
    sleep 3
fi

# 5) HAL tee overlay + publish + daemon, then apply the config once more
sh "$STAGE/bootstrap.sh" "$STAGE"
sh "$STAGE/apply-config.sh"

# Volume OSD: allow the overlay and start the foreground service (idempotent).
appops set dev.aes67bridge.tv SYSTEM_ALERT_WINDOW allow 2>/dev/null
am start-foreground-service -n dev.aes67bridge.tv/.VolumeOsdService 2>/dev/null

echo running > "$ROOT/state.txt"
chown root:root "$ROOT/state.txt" 2>/dev/null
chmod 0644 "$ROOT/state.txt"
echo "=== start.sh done $(date) daemon=$(pidof aes67-daemon) ==="
