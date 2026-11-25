#!/system/bin/sh
# Start AES67 broadcast (idempotent): ensure runtime dir + overlay/HAL tee, apply config, mark running.
set -u
# App 解压源 → 运行期统一 stage（root 同步；运行期文件都在 /data/aes67bridge）
APP=/data/user/0/dev.aes67bridge.tv/files
STAGE=/data/aes67bridge/stage
mkdir -p "$STAGE"
cp -f "$APP"/* "$STAGE"/ 2>/dev/null
chown root:root "$STAGE"/* 2>/dev/null
chmod 0755 "$STAGE"/*.sh "$STAGE"/haltee_rtp_sender 2>/dev/null
chmod 0644 "$STAGE"/*.so "$STAGE"/*.orig 2>/dev/null
ROOT=/data/aes67bridge
LOG=$ROOT/logs/daemon.log
umask 027
mkdir -p "$ROOT/logs"
chown root:root "$ROOT/logs"
chmod 0700 "$ROOT/logs"
echo "$(date) start requested" >> "$LOG"

# 1) ensure runtime dir, overlay + HAL tee (bootstrap is idempotent, starts the daemon)
sh "$STAGE/bootstrap.sh" "$STAGE"

# 2) apply config (derives mode from hal_buf, restarts daemon)
sh "$STAGE/apply-config.sh"

# 3) enable FIFO export and mark running
touch "$ROOT/live.enable"
chmod 0644 "$ROOT/live.enable"
echo running > "$ROOT/state.txt"
chmod 0644 "$ROOT/state.txt"
echo "$(date) start done pid=$(pidof aes67_rtp_sender)" >> "$LOG"