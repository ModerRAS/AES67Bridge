package dev.aes67bridge.tv

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import androidx.tv.material3.MaterialTheme
import androidx.tv.material3.Surface
import androidx.lifecycle.lifecycleScope
import androidx.tv.material3.darkColorScheme
import dev.aes67bridge.tv.ui.ConsoleScreen
import kotlinx.coroutines.launch
import java.io.File

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Auto-update: at most once per 6 h; installs through the firmware root bridge when a
        // newer release is published (release/latest.json). Never blocks the UI.
        lifecycleScope.launch {
            UpdateManager.autoCheck(applicationContext)
            recoverRuntimeIfNeeded()
        }
        setContent {
            MaterialTheme(colorScheme = darkColorScheme(
                primary = Color(0xFF35D0A5),
                onPrimary = Color(0xFF04140E),
                background = Color(0xFF05070A),
                surface = Color(0xFF101720),
                onSurface = Color(0xFFE6EDF5),
            )) {
                ConsoleScreen()
            }
        }
    }

    /**
     * 0.3.0 recovery: the boot hook provisions /data/aes67bridge from this app's filesDir.
     * If the runtime is not marked running (e.g. the hook had no app data at boot, or the
     * box was rebooted while the app was being updated), proVision it now from the app.
     */
    private fun recoverRuntimeIfNeeded() {
        try {
            if (!File("/data/aes67bridge.persist/enabled").exists()) return
            val state = File("/data/aes67bridge/state.txt")
            val running = state.exists() && state.readText().trim() == "running"
            if (!running) AutoStartService.request(applicationContext, "ENABLE")
        } catch (_: Throwable) {
            // not fatal: the user can press "启动" in the UI
        }
    }
}
