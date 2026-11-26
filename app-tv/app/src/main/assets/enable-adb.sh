#!/system/bin/sh
# enable-adb.sh - open wireless ADB for this Zidoo (root bridge).
# Root cause of "adb offline": vendor gate persist.zidoo.test.dc / dc1 default to 0,
# so adbd listens on 5555 but never answers the handshake.
# Optional: /data/user/0/dev.aes67bridge.tv/files/adb_pubkey.txt with the host
# public key (one line from adbkey.pub) is installed to /data/misc/adb/adb_keys.
# WARNING: wireless ADB means root over the LAN; keep the box on a trusted network.
set -u
APP=/data/user/0/dev.aes67bridge.tv/files
LOG=$APP/tool-result.txt
umask 027
exec >> "$LOG" 2>&1
chmod 0644 "$LOG" 2>/dev/null
echo "=== $(date) enable-adb ==="
if [ -f "$APP/adb_pubkey.txt" ]; then
    key=$(head -n 1 "$APP/adb_pubkey.txt" | tr -d '\r')
    case "$key" in
        ssh-rsa*|ssh-ed25519*|ecdsa-sha2-*)
            mkdir -p /data/misc/adb
            printf '%s\n' "$key" > /data/misc/adb/adb_keys
            chown system:shell /data/misc/adb /data/misc/adb/adb_keys
            chmod 0770 /data/misc/adb
            chmod 0640 /data/misc/adb/adb_keys
            echo "host adb public key installed" ;;
        *)
            echo "adb_pubkey.txt ignored: not an OpenSSH public key line" ;;
    esac
fi
setprop persist.zidoo.test.dc 1
setprop persist.zidoo.test.dc1 1
setprop persist.adb.tcp.port 5555
setprop ctl.restart adbd
sleep 2
IP=$(ip route get 1.1.1.1 2>/dev/null | head -n 1 | sed 's/.* src \([0-9.]*\).*/\1/')
if [ -z "$IP" ]; then IP=box-ip; fi
echo "dc=$(getprop persist.zidoo.test.dc) dc1=$(getprop persist.zidoo.test.dc1) port=$(getprop persist.adb.tcp.port) adbd=$(getprop init.svc.adbd) adbroot=$(getprop service.adb.root)"
echo "ADB enabled: adb connect $IP:5555 ; then adb root"
echo "WARNING: wireless ADB means root on the LAN - keep this box on a trusted network."
echo "ENABLE_ADB done"
