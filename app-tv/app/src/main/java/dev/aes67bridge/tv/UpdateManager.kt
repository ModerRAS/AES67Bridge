package dev.aes67bridge.tv

import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/**
 * Fixed-path release feed (no GitHub API rate limit):
 *   https://raw.githubusercontent.com/ModerRAS/AES67Bridge/main/release/latest.json
 * Written by .github/workflows/release.yml on every tag and committed back to main.
 *
 * On this rooted Zidoo the firmware exposes a root command bridge (zd_root_cmd.sh via
 * the zidoo.root.cmd2 / zidoo.root.cmd.start2 properties), which AutoStartService already
 * uses for the HAL overlay. The updater reuses it to install APK/daemon silently; when the
 * bridge is unavailable it falls back to the standard package-installer intent.
 */
object UpdateManager {
    private const val TAG = "AES67Bridge"
    const val LATEST_JSON_URL =
        "https://raw.githubusercontent.com/ModerRAS/AES67Bridge/main/release/latest.json"
    const val LATEST_JSON_MIRROR =
        "https://cdn.jsdelivr.net/gh/ModerRAS/AES67Bridge@main/release/latest.json"

    private const val PREFS = "updater"
    private const val KEY_LAST_CHECK = "last_check_ms"
    private const val KEY_AUTO_CHECK = "auto_check"
    private const val KEY_AUTO_INSTALL = "auto_install"
    private const val CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000L

    data class Release(
        val tag: String,
        val versionCode: Int,
        val versionName: String,
        val notes: String,
        val apkUrl: String?,
        val apkSha256: String?,
        val apkSize: Long,
        val daemonUrl: String?,
        val daemonSha256: String?,
        val daemonVersion: String?
    ) {
        val daemonAvailable: Boolean get() = !daemonUrl.isNullOrEmpty()
    }

    /** Latest release found by the last check (Compose state). */
    var available by mutableStateOf<Release?>(null)
        private set
    /** Human-readable updater state for the settings/console UI (Compose state). */
    var status by mutableStateOf("")
        private set
    var busy by mutableStateOf(false)
        private set

    fun updateUrl(): String = LATEST_JSON_URL

    /** Check for updates on app start (default on; never installs by itself). */
    fun autoCheckEnabled(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_AUTO_CHECK, true)

    fun setAutoCheckEnabled(context: Context, enabled: Boolean) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putBoolean(KEY_AUTO_CHECK, enabled).apply()
        status = if (enabled) "启动时检查：开" else "启动时检查：关"
    }

    /** Silent install without asking (default OFF: updates are a button press). */
    fun autoInstallEnabled(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_AUTO_INSTALL, false)

    fun setAutoInstallEnabled(context: Context, enabled: Boolean) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putBoolean(KEY_AUTO_INSTALL, enabled).apply()
        status = if (enabled) "自动安装：开（下载后静默安装）" else "自动安装：关（需手动点更新）"
    }

    fun currentVersionName(): String = BuildConfig.VERSION_NAME
    fun currentVersionCode(): Int = BuildConfig.VERSION_CODE

    fun isNewer(release: Release): Boolean = release.versionCode > BuildConfig.VERSION_CODE

    /**
     * Called once per app start. Only checks by default (a newer release shows up as the
     * "更新 vX" button on the console); silent install happens only when the user
     * explicitly enabled auto-install in settings.
     */
    suspend fun autoCheck(context: Context) {
        if (!autoCheckEnabled(context)) return
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val now = System.currentTimeMillis()
        if (now - prefs.getLong(KEY_LAST_CHECK, 0L) < CHECK_INTERVAL_MS) {
            available?.let { if (isNewer(it)) status = "发现新版本 v" + it.versionName + "，可在设置里更新" }
            return
        }
        prefs.edit().putLong(KEY_LAST_CHECK, now).apply()
        check(context)
        val release = available ?: return
        if (!isNewer(release)) return
        if (!autoInstallEnabled(context)) {
            status = "发现新版本 v" + release.versionName + "，点主界面「更新」按钮安装"
            return
        }
        status = "发现新版本 v" + release.versionName + "，自动安装中…"
        try {
            applyUpdate(context, release)
        } catch (t: Throwable) {
            status = "自动更新失败：" + (t.message ?: t.toString())
            Log.w(TAG, "auto update failed", t)
        }
    }

    /** Fetch latest.json and compare with the installed version. */
    suspend fun check(context: Context): Release? = withContext(Dispatchers.IO) {
        busy = true
        status = "正在检查更新…"
        try {
            val body = fetchText(LATEST_JSON_URL) ?: fetchText(LATEST_JSON_MIRROR)
            if (body == null) {
                status = "检查失败：无法获取版本信息"
                return@withContext null
            }
            val release = parseRelease(body)
            available = release
            status = if (isNewer(release)) {
                "发现新版本 v" + release.versionName + "（当前 v" + BuildConfig.VERSION_NAME + "）"
            } else {
                "已是最新版本 v" + BuildConfig.VERSION_NAME
            }
            release
        } catch (t: Throwable) {
            status = "检查失败：" + (t.message ?: t.toString())
            Log.w(TAG, "check failed", t)
            null
        } finally {
            busy = false
        }
    }

    /** Download both assets, install the daemon, then the APK (APK install restarts the app). */
    suspend fun applyUpdate(context: Context, release: Release) = withContext(Dispatchers.IO) {
        busy = true
        try {
            val dir = File(context.cacheDir, "updates").apply { mkdirs() }
            if (release.daemonAvailable) {
                status = "下载守护进程 v" + (release.daemonVersion ?: release.versionName) + "…"
                // Download into the persistent app dir: on 0.3.0+ this copy is the seed both
                // for the runtime tmpfs and for the boot hook after a reboot.
                val daemon = download(release.daemonUrl!!, File(context.filesDir, "aes67-daemon.download"), release.daemonSha256, 0)
                status = "安装守护进程…"
                val ok = installDaemon(context, daemon)
                status = if (ok) "守护进程已更新" else "守护进程更新失败（继续更新 App）"
            }
            if (release.apkUrl.isNullOrEmpty()) {
                status = "版本信息里没有 APK"
                return@withContext
            }
            status = "下载 APK v" + release.versionName + "…"
            val apk = download(release.apkUrl, File(dir, "update.apk"), release.apkSha256, release.apkSize)
            status = "校验通过，安装 APK…"
            if (!installApkRoot(context, apk)) {
                status = "root 安装失败，打开系统安装器"
                installApkIntent(context, apk)
            }
        } finally {
            busy = false
        }
    }

    private fun parseRelease(body: String): Release {
        val root = JSONObject(body)
        val apk = root.optJSONObject("apk")
        val daemon = root.optJSONObject("daemon")
        return Release(
            tag = root.optString("tag"),
            versionCode = root.optInt("versionCode"),
            versionName = root.optString("versionName"),
            notes = root.optString("notes"),
            apkUrl = apk?.optString("url")?.takeIf { it.isNotEmpty() },
            apkSha256 = apk?.optString("sha256")?.takeIf { it.isNotEmpty() },
            apkSize = apk?.optLong("size", 0L) ?: 0L,
            daemonUrl = daemon?.optString("url")?.takeIf { it.isNotEmpty() },
            daemonSha256 = daemon?.optString("sha256")?.takeIf { it.isNotEmpty() },
            daemonVersion = daemon?.optString("version")?.takeIf { it.isNotEmpty() }
        )
    }

    private fun fetchText(url: String): String? {
        var conn: HttpURLConnection? = null
        return try {
            conn = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = 10_000
                readTimeout = 15_000
                requestMethod = "GET"
                setRequestProperty("User-Agent", "AES67Bridge-TV/" + BuildConfig.VERSION_NAME)
                setRequestProperty("Cache-Control", "no-cache")
                instanceFollowRedirects = true
            }
            if (conn.responseCode != 200) return null
            conn.inputStream.bufferedReader().use { it.readText() }
        } catch (t: Throwable) {
            Log.w(TAG, "fetch " + url + " failed", t)
            null
        } finally {
            conn?.disconnect()
        }
    }

    private fun download(url: String, dest: File, sha256: String?, expectedSize: Long): File {
        var conn: HttpURLConnection? = null
        try {
            conn = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = 15_000
                readTimeout = 60_000
                requestMethod = "GET"
                setRequestProperty("User-Agent", "AES67Bridge-TV/" + BuildConfig.VERSION_NAME)
                instanceFollowRedirects = true
            }
            if (conn.responseCode != 200) throw RuntimeException("HTTP " + conn.responseCode + " for " + url)
            val digest = MessageDigest.getInstance("SHA-256")
            var total = 0L
            conn.inputStream.use { input ->
                FileOutputStream(dest).use { out ->
                    val buf = ByteArray(64 * 1024)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        out.write(buf, 0, n)
                        digest.update(buf, 0, n)
                        total += n
                        if (expectedSize > 0) {
                            status = "下载中 " + (total * 100 / expectedSize) + "%"
                        }
                    }
                }
            }
            dest.setReadable(true, false)
            if (expectedSize > 0 && total != expectedSize) {
                dest.delete()
                throw RuntimeException("size mismatch: got " + total + " expected " + expectedSize)
            }
            if (!sha256.isNullOrEmpty()) {
                val actual = digest.digest().joinToString("") { "%02x".format(it) }
                if (!actual.equals(sha256, ignoreCase = true)) {
                    dest.delete()
                    throw RuntimeException("sha256 mismatch: got " + actual)
                }
            }
            return dest
        } finally {
            conn?.disconnect()
        }
    }

    /** Copy the daemon binary into /data/aes67bridge and restart it through the root bridge. */
    private fun installDaemon(context: Context, daemon: File): Boolean {
        val log = File(context.filesDir, "update-daemon.log")
        log.writeText("")
        log.setReadable(true, false)
        val script = File(context.filesDir, "update-daemon.sh")
        val appDir = context.filesDir.absolutePath
        script.writeText(
            "#!/system/bin/sh\n" +
                "set -u\n" +
                "APP='" + appDir + "'\n" +
                "echo START >> '" + log.absolutePath + "'\n" +
                // 1) persistent copy (seed for the boot hook / tmpfs runtime)
                "cp -f '" + daemon.absolutePath + "' \"\$APP/.aes67-daemon.new\" || { echo CPFAIL >> '" + log.absolutePath + "'; exit 1; }\n" +
                "OWNER=\$(stat -c '%u:%g' \"\$APP\" 2>/dev/null); [ -n \"\$OWNER\" ] || OWNER=10082:10082\n" +
                "chown \"\$OWNER\" \"\$APP/.aes67-daemon.new\"\n" +
                "chmod 0755 \"\$APP/.aes67-daemon.new\"\n" +
                "mv -f \"\$APP/.aes67-daemon.new\" \"\$APP/aes67-daemon\"\n" +
                // 2) runtime copy (temp + rename: never rewrite a mapped binary in place)
                "cp -f \"\$APP/aes67-daemon\" /data/aes67bridge/.aes67-daemon.new || { echo CPFAIL >> '" + log.absolutePath + "'; exit 1; }\n" +
                "chown root:root /data/aes67bridge/.aes67-daemon.new\n" +
                "chmod 0750 /data/aes67bridge/.aes67-daemon.new\n" +
                "mv -f /data/aes67bridge/.aes67-daemon.new /data/aes67bridge/aes67-daemon\n" +
                "if [ -f /data/aes67bridge/stage/restart.sh ]; then sh /data/aes67bridge/stage/restart.sh >> /data/aes67bridge/logs/daemon.log 2>&1; fi\n" +
                "echo OK >> '" + log.absolutePath + "'\n"
        )
        script.setReadable(true, false)
        script.setExecutable(true, false)
        if (!triggerRoot(script.absolutePath)) return false
        val ok = waitFor(log, "OK", 30_000)
        if (ok) {
            // keep the verified daemon in filesDir as the persistent seed; drop the download
            daemon.delete()
        }
        return ok
    }

    /** Install the downloaded APK with pm install through the root bridge. */
    private fun installApkRoot(context: Context, apk: File): Boolean {
        val log = File(context.filesDir, "update-apk.log")
        log.writeText("")
        log.setReadable(true, false)
        log.setWritable(true, false)
        val script = File(context.filesDir, "update-apk.sh")
        script.writeText(
            "#!/system/bin/sh\n" +
                "echo START >> '" + log.absolutePath + "'\n" +
                "pm install -r -d '" + apk.absolutePath + "' >> '" + log.absolutePath + "' 2>&1\n" +
                "echo DONE >> '" + log.absolutePath + "'\n"
        )
        script.setReadable(true, false)
        script.setExecutable(true, false)
        if (!triggerRoot(script.absolutePath)) return false
        // The install kills this process, so only a best-effort wait; the marker is for diagnostics.
        return waitFor(log, "DONE", 90_000)
    }

    private fun waitFor(file: File, marker: String, timeoutMs: Long): Boolean {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            Thread.sleep(500)
            val text = try { file.readText() } catch (t: Throwable) { "" }
            if (text.contains(marker)) return true
        }
        return false
    }

    private fun triggerRoot(scriptPath: String): Boolean = try {
        ProcessBuilder("/system/bin/setprop", "zidoo.root.cmd2", scriptPath).start().waitFor()
        ProcessBuilder("/system/bin/setprop", "zidoo.root.cmd.start2", "1").start().waitFor()
        true
    } catch (t: Throwable) {
        Log.w(TAG, "root bridge unavailable", t)
        false
    }

    /** Fallback when the root bridge does not exist: hand the APK to the system installer. */
    private fun installApkIntent(context: Context, apk: File) {
        try {
            val uri = androidx.core.content.FileProvider.getUriForFile(
                context, context.packageName + ".files", apk
            )
            val intent = Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            context.startActivity(intent)
        } catch (t: Throwable) {
            Log.e(TAG, "install intent failed", t)
            status = "安装失败：" + (t.message ?: t.toString())
        }
    }
}
