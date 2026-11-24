package dev.aes67bridge.tv

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.util.Log
import java.io.File
import java.io.FileOutputStream

class AutoStartService : Service() {
    private val tag = "AES67Bridge"
    private val channelId = "aes67bridge"

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val action = intent?.action ?: "ENABLE"
        startForeground(1, buildNotification("AES67Bridge: " + action))
        try {
            val dir = filesDir.apply { mkdirs() }.absolutePath
            diag("action=" + action + " dir=" + dir)
            when (action) {
                "DISABLE" -> { extractAll(dir); diag("trigger stop"); triggerRoot(dir + "/stop.sh") }
                "ROLLBACK" -> { extractAll(dir); diag("trigger rollback"); triggerRoot(dir + "/rollback.sh") }
                "LOWLAT" -> { extractAll(dir); diag("trigger lowlat"); triggerRoot(dir + "/mode-lowlat.sh") }
                "NORMAL" -> { extractAll(dir); diag("trigger normal"); triggerRoot(dir + "/mode-normal.sh") }
                "APPLY" -> { extractAll(dir); diag("trigger apply-config"); triggerRoot(dir + "/apply-config.sh") }
                else -> { extractAll(dir); diag("trigger start"); triggerRoot(dir + "/start.sh") }
            }
        } catch (t: Throwable) {
            Log.e(tag, "fail", t); diag("fail: " + t)
        }
        stopForeground(false)
        stopSelf()
        return START_NOT_STICKY
    }

    private fun diag(s: String) {
        Log.i(tag, s)
        try {
            File(filesDir, "diag.txt").appendText(System.currentTimeMillis().toString() + " " + s + "\n")
        } catch (t: Throwable) { }
    }

    private fun triggerRoot(scriptPath: String): Boolean {
        return try {
            ProcessBuilder("/system/bin/setprop", "zidoo.root.cmd2", scriptPath).start().waitFor()
            ProcessBuilder("/system/bin/setprop", "zidoo.root.cmd.start2", "1").start().waitFor()
            true
        } catch (t: Throwable) {
            Log.e(tag, "triggerRoot fail", t)
            false
        }
    }

    private fun extractAll(dir: String) {
        val names = listOf(
            "bootstrap.sh", "start.sh", "stop.sh", "restart.sh", "rollback.sh", "purge.sh", "mode-lowlat.sh", "mode-normal.sh", "apply-config.sh",
            "restart-daemon.sh", "purge.sh",
            "audio.primary.amlogic.so", "audio.primary.amlogic.so.orig", "haltee_rtp_sender"
        )
        for (n in names) {
            val f = File(dir, n)
            assets.open(n).use { input -> FileOutputStream(f).use { out -> input.copyTo(out) } }
            f.setReadable(true, false)
            f.setExecutable(true, false)
            diag("extracted " + n + " size=" + f.length())
        }
    }

    private fun buildNotification(text: String): Notification {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            nm.createNotificationChannel(NotificationChannel(channelId, "AES67Bridge", NotificationManager.IMPORTANCE_LOW))
        }
        val builder = if (android.os.Build.VERSION.SDK_INT >= 26)
            Notification.Builder(this, channelId) else Notification.Builder(this)
        return builder.setContentTitle("AES67Bridge").setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_speakerphone).setOngoing(true).build()
    }

    override fun onBind(intent: Intent?): IBinder? = null
}
