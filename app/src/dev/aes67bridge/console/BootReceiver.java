package dev.aes67bridge.console;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

public class BootReceiver extends BroadcastReceiver {
    @Override public void onReceive(Context c, Intent i) {
        Log.i("AES67Bridge", "boot receiver: " + (i == null ? "" : i.getAction()));
        Intent svc = new Intent(c, AutoStartService.class);
        svc.setAction("ENABLE");
        c.startForegroundService(svc);
    }
}
