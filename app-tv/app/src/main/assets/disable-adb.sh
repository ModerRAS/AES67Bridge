#!/system/bin/sh
# disable-adb.sh - close the vendor ADB gate again (root bridge).
set -u
APP=/data/user/0/dev.aes67bridge.tv/files
LOG=$APP/tool-result.txt
umask 027
exec >> "$LOG" 2>&1
chmod 0644 "$LOG" 2>/dev/null
echo "=== $(date) disable-adb ==="
setprop persist.zidoo.test.dc 0
setprop persist.zidoo.test.dc1 0
setprop persist.adb.tcp.port ""
rm -f /data/misc/adb/adb_keys
setprop ctl.restart adbd
sleep 2
echo "dc=$(getprop persist.zidoo.test.dc) dc1=$(getprop persist.zidoo.test.dc1) adbd=$(getprop init.svc.adbd)"
echo "ADB disabled (adbd restarted; 5555 no longer answers)"
echo "DISABLE_ADB done"
