#!/system/bin/sh
# Start AES67 broadcast (idempotent): ensure runtime dir + overlay/HAL tee, apply config, mark running.
set -u
STAGE=/data/user/0/dev.aes67bridge.tv/files
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
