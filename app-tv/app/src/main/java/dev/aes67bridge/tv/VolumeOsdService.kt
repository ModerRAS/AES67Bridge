package dev.aes67bridge.tv

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.RectF
import android.graphics.drawable.GradientDrawable
import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.content.ContextCompat

/**
 * On-screen AES67 volume bar.
 *
 * The daemon owns the level (remote keys -> last-stage trim). This foreground
 * service follows @aes67bridge.ctl and draws a short-lived overlay over whatever
 * is on screen. It never changes the Android system volume: the Zidoo runs in
 * volume-passthrough mode, so the AES67 trim is the only level control.
 *
 * Styles (SharedPreferences "aes67bridge" / "osd_style"):
 *   bar     - system-like horizontal bar with the dB value (default)
 *   numeric - only "-12.0 dB", bottom-right
 */
class VolumeOsdService : Service() {
    companion object {
        private const val TAG = "AES67VolumeOsd"
        private const val SOCKET = "aes67bridge.ctl"
        private const val CHANNEL = "aes67-osd"
        private const val PREFS = "aes67bridge"
        private const val PREF_STYLE = "osd_style"
        private const val HIDE_MS = 1500L
        private const val NOTIFICATION_ID = 2

        fun request(context: Context) {
            try {
                ContextCompat.startForegroundService(
                    context,
                    Intent(context, VolumeOsdService::class.java),
                )
            } catch (t: Throwable) {
                Log.w(TAG, "startForegroundService failed", t)
            }
        }
    }

    private val handler = Handler(Looper.getMainLooper())
    @Volatile private var running = false
    private var reader: Thread? = null
    private var windowManager: WindowManager? = null
    private var overlay: View? = null
    private var bar: VolumeBar? = null
    private var label: TextView? = null
    private var currentStyle: String = "bar"
    private var hideTask: Runnable? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
        startForeground(NOTIFICATION_ID, buildNotification())
        windowManager = getSystemService(WINDOW_SERVICE) as WindowManager
        running = true
        reader = Thread({ readLoop() }, "aes67-osd-ctl").also {
            it.isDaemon = true
            it.start()
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onDestroy() {
        running = false
        reader?.interrupt()
        destroyOverlay()
        super.onDestroy()
    }

    private fun createChannel() {
        val manager = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "AES67 volume", NotificationManager.IMPORTANCE_LOW),
        )
    }

    private fun buildNotification(): Notification =
        Notification.Builder(this, CHANNEL)
            .setContentTitle("AES67Bridge")
            .setContentText("Volume display")
            .setSmallIcon(android.R.drawable.stat_sys_speakerphone)
            .setOngoing(true)
            .build()

    /** Follow the daemon control socket; reconnect forever while the service lives. */
    private fun readLoop() {
        while (running) {
            var socket: LocalSocket? = null
            try {
                socket = LocalSocket()
                socket.connect(
                    LocalSocketAddress(SOCKET, LocalSocketAddress.Namespace.ABSTRACT),
                )
                var first = true
                socket.inputStream.bufferedReader().use { input ->
                    while (running) {
                        val line = input.readLine() ?: break
                        val db = Regex("\"trim_db\"\\s*:\\s*(-?[0-9.]+)")
                            .find(line)?.groupValues?.get(1)?.toFloatOrNull()
                        val muted = Regex("\"muted\"\\s*:\\s*(true|false)")
                            .find(line)?.groupValues?.get(1) == "true"
                        if (db == null) continue
                        if (first) {
                            // The daemon pushes the current state on connect; do not
                            // pop the OSD just because the service (re)connected.
                            first = false
                            continue
                        }
                        handler.post { show(db, muted) }
                    }
                }
            } catch (_: Throwable) {
                // daemon not up yet, or the socket was replaced
            } finally {
                try {
                    socket?.close()
                } catch (_: Throwable) {
                }
            }
            if (!running) break
            try {
                Thread.sleep(1000)
            } catch (_: InterruptedException) {
                break
            }
        }
    }

    private fun show(db: Float, muted: Boolean) {
        if (!Settings.canDrawOverlays(this)) {
            Log.w(TAG, "overlay permission missing; volume bar disabled")
            return
        }
        val style = getSharedPreferences(PREFS, MODE_PRIVATE)
            .getString(PREF_STYLE, "bar")
            ?.takeIf { it == "bar" || it == "numeric" } ?: "bar"
        if (overlay == null || style != currentStyle) {
            destroyOverlay()
            buildOverlay(style)
        }
        val text = if (muted) "静音" else String.format(java.util.Locale.US, "%.1f dB", db)
        label?.text = if (style == "bar") "AES67   $text" else text
        bar?.setLevel(db, muted)
        val view = overlay ?: return
        if (view.parent == null) {
            try {
                windowManager?.addView(view, layoutParams(style))
            } catch (t: Throwable) {
                Log.w(TAG, "addView failed", t)
                return
            }
        }
        view.alpha = 1f
        hideTask?.let { handler.removeCallbacks(it) }
        val task = Runnable { removeOverlay() }
        hideTask = task
        handler.postDelayed(task, HIDE_MS)
    }

    private fun buildOverlay(style: String) {
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(14), dp(20), dp(14))
            background = rounded(0xCC0B1118.toInt(), 18)
        }
        label = TextView(this).apply {
            setTextColor(Color.WHITE)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, if (style == "bar") 20f else 30f)
            gravity = Gravity.CENTER
        }
        if (style == "bar") {
            container.addView(
                label,
                LinearLayout.LayoutParams(dp(300), LinearLayout.LayoutParams.WRAP_CONTENT),
            )
            bar = VolumeBar(this).apply {
                setLevel(0f, false)
            }
            container.addView(
                bar,
                LinearLayout.LayoutParams(
                    dp(300),
                    dp(10),
                ).apply { topMargin = dp(10) },
            )
        } else {
            container.addView(
                label,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                ),
            )
            bar = null
        }
        overlay = container
        currentStyle = style
    }

    private fun layoutParams(style: String): WindowManager.LayoutParams {
        val params = WindowManager.LayoutParams(
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
            PixelFormat.TRANSLUCENT,
        )
        if (style == "bar") {
            params.gravity = Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL
            params.y = dp(90)
        } else {
            params.gravity = Gravity.BOTTOM or Gravity.END
            params.x = dp(48)
            params.y = dp(72)
        }
        return params
    }

    private fun removeOverlay() {
        val view = overlay ?: return
        try {
            if (view.parent != null) windowManager?.removeView(view)
        } catch (_: Throwable) {
        }
    }

    private fun destroyOverlay() {
        hideTask?.let { handler.removeCallbacks(it) }
        hideTask = null
        removeOverlay()
        overlay = null
        bar = null
        label = null
    }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    private fun rounded(color: Int, radiusDp: Int): GradientDrawable =
        GradientDrawable().apply {
            setColor(color)
            cornerRadius = dp(radiusDp).toFloat()
            setStroke(dp(1), 0x33FFFFFF)
        }

    /** Horizontal level bar; the fill maps 0 .. -60 dB to 100 % .. 0 %. */
    private class VolumeBar(context: Context) : View(context) {
        private val trackPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = 0x33FFFFFF
        }
        private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = 0xFF35D0A5.toInt()
        }
        private var fraction = 0f

        fun setLevel(db: Float, muted: Boolean) {
            fraction = if (muted) 0f else ((db + 60f) / 60f).coerceIn(0f, 1f)
            invalidate()
        }

        override fun onDraw(canvas: Canvas) {
            val radius = height / 2f
            canvas.drawRoundRect(
                RectF(0f, 0f, width.toFloat(), height.toFloat()),
                radius,
                radius,
                trackPaint,
            )
            if (fraction > 0f) {
                canvas.drawRoundRect(
                    RectF(0f, 0f, width * fraction, height.toFloat()),
                    radius,
                    radius,
                    fillPaint,
                )
            }
        }
    }
}
