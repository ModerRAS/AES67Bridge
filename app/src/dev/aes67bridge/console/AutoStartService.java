package dev.aes67bridge.console;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.IOException;

public class AutoStartService extends Service {
    static final String TAG = "AES67Bridge";
    static final String CH = "aes67bridge";

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? "ENABLE" : (intent.getAction() == null ? "ENABLE" : intent.getAction());
        startForeground(1, buildNotification("AES67Bridge: " + action));
        try {
            File fdir = getFilesDir();
            fdir.mkdirs();
            String dir = fdir.getAbsolutePath();
            diag("action=" + action + " dir=" + dir);
            if ("LOWLAT".equals(action) || "NORMAL".equals(action)) {
                extractAll(dir);
                String sp = dir + ("LOWLAT".equals(action) ? "/mode-lowlat.sh" : "/mode-normal.sh");
                diag("trigger " + sp);
                triggerRoot(sp);
                stopForeground(false); stopSelf();
                return START_NOT_STICKY;
            }
            if ("DISABLE".equals(action)) {
                diag("trigger rollback");
                triggerRoot(dir + "/rollback.sh");
            } else {
                diag("extract assets");
                extractAll(dir);
                diag("trigger bootstrap");
                triggerRoot(dir + "/bootstrap.sh");
            }
        } catch (Throwable t) {
            Log.e(TAG, "fail", t);
            diag("fail: " + t);
        }
        stopForeground(false);
        stopSelf();
        return START_NOT_STICKY;
    }

    static void diag(String s) {
        Log.i(TAG, s);
        try {
            java.io.FileWriter fw = new java.io.FileWriter("/data/data/dev.aes67bridge.console/diag.txt", true);
            fw.write(new java.text.SimpleDateFormat("HH:mm:ss").format(new java.util.Date()) + " " + s + "\n");
            fw.close();
        } catch (Throwable t) { }
    }

    static boolean triggerRoot(String scriptPath) {
        try {
            Process p1 = Runtime.getRuntime().exec(new String[]{"/system/bin/setprop", "zidoo.root.cmd2", scriptPath});
            p1.waitFor();
            Process p2 = Runtime.getRuntime().exec(new String[]{"/system/bin/setprop", "zidoo.root.cmd.start2", "1"});
            p2.waitFor();
            return true;
        } catch (Throwable t) {
            Log.e(TAG, "triggerRoot fail", t);
            return false;
        }
    }

    private void extractAll(String dir) throws IOException {
        String[] names = {"bootstrap.sh", "rollback.sh", "mode-lowlat.sh", "mode-normal.sh", "audio.primary.amlogic.so", "audio.primary.amlogic.so.orig", "haltee_rtp_sender"};
        for (String n : names) {
            File f = new File(dir, n);
            InputStream in = getAssets().open(n);
            FileOutputStream out = new FileOutputStream(f);
            byte[] buf = new byte[65536];
            int r;
            while ((r = in.read(buf)) > 0) out.write(buf, 0, r);
            in.close(); out.close();
            f.setReadable(true, false);
            f.setExecutable(true, false);
            diag("extracted " + n + " size=" + f.length());
        }
    }

    static String readStatus(Context c) {
        StringBuilder sb = new StringBuilder();
        File f = new File("/data/aes67/status.txt");
        if (!f.exists()) {
            sb.append("(还没有状态文件 — 尚未启用过，或 root 后门尚未执行)\n");
        } else {
            try {
                InputStream in = new java.io.FileInputStream(f);
                byte[] buf = new byte[8192];
                int n = in.read(buf);
                in.close();
                if (n > 0) sb.append(new String(buf, 0, n));
            } catch (Throwable t) {
                sb.append("读取失败: ").append(t).append("\n");
            }
        }
        return sb.toString();
    }

    private Notification buildNotification(String text) {
        NotificationManager nm = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            NotificationChannel ch = new NotificationChannel(CH, "AES67Bridge", NotificationManager.IMPORTANCE_LOW);
            nm.createNotificationChannel(ch);
        }
        Notification.Builder b = (android.os.Build.VERSION.SDK_INT >= 26)
                ? new Notification.Builder(this, CH) : new Notification.Builder(this);
        b.setContentTitle("AES67Bridge").setContentText(text).setSmallIcon(android.R.drawable.stat_sys_speakerphone).setOngoing(true);
        return b.build();
    }

    @Override public IBinder onBind(Intent i) { return null; }
}
