package dev.aes67bridge.tv.ui

import java.io.File

data class Status(
    val hasData: Boolean = false,
    val codec: String = "",
    val rate: String = "",
    val ch: String = "",
    val rsIn: String = "",
    val rsOut: String = "",
    val packets: String = "",
    val err: String = "",
    val dest: String = "",
    val mode: String = "",
    val ptime: String = "",
    val split: String = "",
    val layout: String = "",
    val ageMs: Long = -1,
    val levels: List<Float> = emptyList()
)

/** 运行时目录已统一到 /data/aes67bridge；旧 /data/haltee 仅作迁移期只读回退。 */
private const val STATUS_PATH = "/data/aes67bridge/status.json"
private const val STATUS_PATH_LEGACY = "/data/haltee/status.json"
private const val STATE_PATH = "/data/aes67bridge/state.txt"
private const val STATE_PATH_LEGACY = "/data/haltee/state.txt"

private fun readFile(path: String): String? {
    return try {
        val f = File(path)
        if (!f.exists()) null else f.readText()
    } catch (t: Throwable) { null }
}

private fun firstExisting(vararg paths: String): File? =
    paths.map { File(it) }.firstOrNull { it.exists() }

private fun grab(re: Regex, s: String?): String {
    if (s == null) return ""
    return try { re.find(s)?.groupValues?.get(1) ?: "" } catch (t: Throwable) { "" }
}

fun readStatus(): Status {
    val f = firstExisting(STATUS_PATH, STATUS_PATH_LEGACY) ?: return Status()
    val js = readFile(f.path) ?: return Status()
    val age = System.currentTimeMillis() - f.lastModified()
    val levels = try {
        val m = Regex("\"levels\"\\s*:\\s*\\[([^\\]]*)\\]").find(js)
        m?.groupValues?.get(1)?.split(",")?.mapNotNull { it.trim().toFloatOrNull() } ?: emptyList()
    } catch (t: Throwable) { emptyList() }
    return Status(
        hasData = age in 0..5000,
        codec = grab(Regex("\"codec\"\\s*:\\s*\"([a-zA-Z0-9_]+)\""), js),
        rate = grab(Regex("\"rate\"\\s*:\\s*(\\d+)"), js),
        ch = grab(Regex("\"ch\"\\s*:\\s*(\\d+)"), js),
        rsIn = grab(Regex("\"rs_in\"\\s*:\\s*(\\d+)"), js),
        rsOut = grab(Regex("\"rs_out\"\\s*:\\s*(\\d+)"), js),
        packets = grab(Regex("\"packets\"\\s*:\\s*(\\d+)"), js),
        err = grab(Regex("\"send_err\"\\s*:\\s*(\\d+)"), js),
        dest = grab(Regex("\"dest\"\\s*:\\s*\"([0-9.:]+)\""), js),
        mode = grab(Regex("\"mode\"\\s*:\\s*\"([a-z]+)\""), js),
        ptime = grab(Regex("\"ptime_us\"\\s*:\\s*(\\d+)"), js),
        split = grab(Regex("\"split\"\\s*:\\s*(\\d+)"), js),
        layout = grab(Regex("\"layout\"\\s*:\\s*\"([0-9.]+)\""), js),
        ageMs = age,
        levels = levels
    )
}
/** 守护脚本写的生命周期状态：running / stopped / rolledback；没有文件时返回空。 */
fun readState(): String {
    val f = firstExisting(STATE_PATH, STATE_PATH_LEGACY) ?: return ""
    return try { f.readText().trim() } catch (t: Throwable) { "" }
}
