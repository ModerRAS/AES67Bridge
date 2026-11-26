#!/system/bin/sh
# install-boot-hook.sh - in-app "install/repair boot hook" (root bridge).
set -u
APP=/data/user/0/dev.aes67bridge.tv/files
ROOT=/data/aes67bridge
PERSIST=/data/aes67bridge.persist
LOG=$APP/tool-result.txt
umask 027
mkdir -p "$PERSIST/boot" "$PERSIST/logs"
chown root:root "$PERSIST" "$PERSIST/boot" "$PERSIST/logs" 2>/dev/null
chmod 0755 "$PERSIST" "$PERSIST/boot"
chmod 0700 "$PERSIST/logs"
exec >> "$LOG" 2>&1
chmod 0644 "$LOG" 2>/dev/null
echo "=== $(date) install-boot-hook ==="
APP_UID=$(stat -c '%u' "$APP" 2>/dev/null)
APP_GID=$(stat -c '%g' "$APP" 2>/dev/null)
[ -n "$APP_UID" ] || APP_UID=10082
[ -n "$APP_GID" ] || APP_GID=10082

# 1) migrate a running 0.2.x runtime into the persistent app dir (idempotent)
if [ -x "$ROOT/aes67-daemon" ] && [ ! -x "$APP/aes67-daemon" ]; then
    cp -f "$ROOT/aes67-daemon" "$APP/aes67-daemon" \
        && chown "$APP_UID:$APP_GID" "$APP/aes67-daemon" \
        && chmod 0755 "$APP/aes67-daemon" \
        && echo "migrated daemon binary -> app dir"
fi
if [ -d "$ROOT/tools" ]; then
    mkdir -p "$APP/tools"
    cp -rf "$ROOT/tools/." "$APP/tools/" 2>/dev/null
    chown -R "$APP_UID:$APP_GID" "$APP/tools" 2>/dev/null
    echo "migrated tools -> app dir"
fi
if [ -d "$ROOT/media" ] && [ ! -d "$APP/media" ]; then
    cp -r "$ROOT/media" "$APP/media" 2>/dev/null
    chown -R "$APP_UID:$APP_GID" "$APP/media" 2>/dev/null
    echo "migrated media -> app dir"
fi

# 2) install the hook scripts
for f in aes67boot.sh trigger.sh pp_append.bin; do
    if [ -f "$APP/$f" ]; then cp -f "$APP/$f" "$PERSIST/boot/$f"; fi
done
chmod 0755 "$PERSIST/boot/aes67boot.sh" "$PERSIST/boot/trigger.sh" 2>/dev/null
chmod 0644 "$PERSIST/boot/pp_append.bin" 2>/dev/null

# 3) legacy ext4 trigger (the property points here; the tmpfs must not hide it yet)
if ! grep -q " $ROOT tmpfs " /proc/mounts; then
    mkdir -p "$ROOT/boot"
    printf '%s\n' "sh $PERSIST/boot/aes67boot.sh" > "$ROOT/boot/trigger.sh"
    chmod 0755 "$ROOT/boot/trigger.sh"
    cp -f "$PERSIST/boot/aes67boot.sh" "$ROOT/boot/aes67boot.sh" 2>/dev/null
    chmod 0755 "$ROOT/boot/aes67boot.sh" 2>/dev/null
    echo "legacy ext4 trigger installed"
else
    echo "tmpfs runtime already mounted; ext4 trigger recreated by the hook at boot"
fi

# 4) keep the property trigger alive (append the fragment only if it was lost)
PP=/data/property/persistent_properties
cp -f "$PP" "$PERSIST/pp.backup" 2>/dev/null
if ! grep -qa 'zidoo.root.cmd2' "$PP" 2>/dev/null; then
    cat "$PERSIST/boot/pp_append.bin" >> "$PP" \
        && chown root:root "$PP" \
        && chmod 0600 "$PP" \
        && echo "property trigger re-appended"
fi

# 5) enable the tmpfs runtime (activation: next reboot or the next start.sh)
touch "$PERSIST/enabled"
chmod 0644 "$PERSIST/enabled"
echo "INSTALL_BOOTHOOK OK: hook=$PERSIST/boot/aes67boot.sh; runtime tmpfs enabled on next boot"
