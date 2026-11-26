package dev.aes67bridge.tv

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        Log.i("AES67Bridge", "boot: " + (intent?.action ?: ""))
        context.startForegroundService(Intent(context, AutoStartService::class.java).setAction("ENABLE"))
        VolumeOsdService.request(context)
    }
}
