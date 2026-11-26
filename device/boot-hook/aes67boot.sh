#!/system/bin/sh
# aes67boot.sh - FBE-safe boot hook in /data/aes67bridge.persist/boot.
# Runs as root at every boot via the vendor root bridge:
#   property zidoo.root.cmd2 = /data/aes67bridge/boot/trigger.sh (unchanged from 0.2.x)
#   -> trigger.sh: sh /data/aes67bridge.persist/boot/aes67boot.sh
# Recreates the legacy ext4 trigger before the tmpfs hides it, switches
# /data/aes67bridge to tmpfs (when enabled), waits for user unlock, then runs the
# app-dir start.sh. If app data is missing it logs and exits; opening the app later
# re-provisions through start.sh.
set -u
PERSIST=/data/aes67bridge.persist
ROOT=/data/aes67bridge
APP=/data/user/0/dev.aes67bridge.tv/files
LOG=$PERSIST/logs/boot.log
umask 027
mkdir -p "$PERSIST/logs" "$PERSIST/boot"
chown root:root "$PERSIST" "$PERSIST/logs" "$PERSIST/boot" 2>/dev/null
chmod 0755 "$PERSIST" "$PERSIST/boot"
chmod 0700 "$PERSIST/logs"
exec >> "$LOG" 2>&1
echo "=== $(date) boot hook start uid=$(id -u) ==="

# 1) recreate the legacy ext4 trigger BEFORE the tmpfs hides /data/aes67bridge
mkdir -p "$ROOT/boot" 2>/dev/null
printf '%s\n' "sh $PERSIST/boot/aes67boot.sh" > "$ROOT/boot/trigger.sh"
chmod 0755 "$ROOT/boot/trigger.sh"

# 2) runtime tmpfs (do not mount it if the user has not migrated yet)
if [ -f "$PERSIST/enabled" ]; then
    if ! grep -q " $ROOT tmpfs " /proc/mounts; then
        mkdir -p "$ROOT"
        mount -t tmpfs -o size=64m,mode=0775 tmpfs "$ROOT"
        echo "tmpfs mounted on $ROOT rc=$?"
    fi
    chown root:audio "$ROOT" 2>/dev/null || chown root:1005 "$ROOT"
    chmod 0775 "$ROOT"
else
    echo "runtime not enabled (no $PERSIST/enabled); keeping legacy ext4 layout"
fi
mkdir -p "$ROOT/logs" "$ROOT/stage"
chown root:root "$ROOT/logs" 2>/dev/null
chmod 0700 "$ROOT/logs"

# 3) wait for FBE unlock + app assets, then provision/start
i=0
while [ ! -x "$APP/start.sh" ] && [ "$i" -lt 180 ]; do
    sleep 1
    i=$((i+1))
done
if [ -x "$APP/start.sh" ]; then
    echo "app ready after $i s; running start.sh"
    sh "$APP/start.sh"
    echo "start.sh rc=$?"
    echo "ok $(date)" > "$PERSIST/boot-state.txt"
else
    echo "ERROR: $APP/start.sh missing after $i s (app data cleared or FBE locked); daemon not started"
    echo "missing-app-data $(date)" > "$PERSIST/boot-state.txt"
fi

# 4) keep the property trigger alive if the property store lost it
PP=/data/property/persistent_properties
if ! grep -qa 'zidoo.root.cmd2' "$PP" 2>/dev/null; then
    cat "$PERSIST/boot/pp_append.bin" >> "$PP"
    chown root:root "$PP"
    chmod 0600 "$PP"
    echo "property trigger re-appended"
fi
echo "=== boot hook done $(date) daemon=$(pidof aes67-daemon) ==="
