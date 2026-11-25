package dev.aes67bridge.tv.ui

import java.io.File

data class Status(
    val hasData: Boolean = false,
    val version: String = "",
    val codec: String = "",
    val enc: String = "",
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
    val streamId: Long = 0,
    /** true = status.stream.id>0 且在 live.bits.meta 里找到同一 stream_id（真实码流已开流）。 */
    val hasSource: Boolean = false,
    val srcFmt: String = "",
    val srcFmtName: String = "",
    val srcGuess: String = "",
    val srcChmask: String = "",
    val srcRate: String = "",
    val ageMs: Long = -1,
    val levels: List<Float> = emptyList(),
    /** daemon 真实源状态：最近 1 s 内收到过源字节。 */
    val srcActive: Boolean = false,
    val srcAgeMs: Long = 0,
    val srcCodec: String = "",
    val pt: String = "",
    val outPeak: Float = 0f,
    val limited: Long = 0,
    val dialnormDb: Float = 0f
)

/** 运行时目录已统一到 /data/aes67bridge；旧 /data/haltee 仅作迁移期只读回退。 */
/** Rust daemon 的状态通道：abstract UNIX socket（不再轮询 JSON 文件）。 */
private const val STATUS_SOCKET = "aes67bridge.status"
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


private const val BITS_META_PATH = "/data/aes67bridge/live.bits.meta"

/** live.bits.meta 的 key=value 快照：这是 HAL tee 真实开流时才写的原始码流信息。 */
private fun readMetaMap(): Map<String, String> {
    val body = readFile(BITS_META_PATH) ?: return emptyMap()
    return body.lineSequence().mapNotNull { ln ->
        val i = ln.indexOf('=');
        if (i > 0) ln.substring(0, i).trim() to ln.substring(i + 1).trim() else null
    }.toMap()
}

/** 读 abstract UNIX socket；连接成功即视为新鲜（daemon 每次 publish 写一帧后关闭）。 */
private fun readStatusSocket(): String? {
    var socket: android.net.LocalSocket? = null
    return try {
        socket = android.net.LocalSocket()
        socket.connect(
            android.net.LocalSocketAddress(STATUS_SOCKET, android.net.LocalSocketAddress.Namespace.ABSTRACT)
        )
        socket.inputStream.use { it.readBytes().toString(Charsets.UTF_8) }
    } catch (t: Throwable) {
        null
    } finally {
        try { socket?.close() } catch (t: Throwable) { }
    }
}

fun readStatus(): Status {
    val socketBody = readStatusSocket()
    val f = firstExisting(STATUS_PATH, STATUS_PATH_LEGACY)
    val js = socketBody ?: (f?.let { readFile(it.path) } ?: return Status())
    val age = if (socketBody != null) 0L else System.currentTimeMillis() - (f?.lastModified() ?: 0L)
    val levels = try {
        val m = Regex("\"levels\"\\s*:\\s*\\[([^\\]]*)\\]").find(js)
        m?.groupValues?.get(1)?.split(",")?.mapNotNull { it.trim().toFloatOrNull() } ?: emptyList()
    } catch (t: Throwable) { emptyList() }
    val streamId = grab(Regex("\"stream\"\\s*:\\s*\\{\"id\"\\s*:\\s*(-?\\d+)"), js).toLongOrNull() ?: 0L
    val srcBlob = grab(Regex("\"src\"\\s*:\\s*\\{([^}]*)\\}"), js)
    val srcActive = grab(Regex("\"active\"\\s*:\\s*([01])"), srcBlob) == "1"
    val srcAgeMs = grab(Regex("\"age_ms\"\\s*:\\s*(\\d+)"), srcBlob).toLongOrNull() ?: 0L
    val srcCodec = grab(Regex("\"codec\"\\s*:\\s*\"([a-z0-9_]+)\""), srcBlob)
    val srcRate = grab(Regex("\"rate\"\\s*:\\s*(\\d+)"), srcBlob)
    val meta = if (streamId > 0) readMetaMap() else emptyMap()
    val metaId = meta["stream_id"]?.toLongOrNull() ?: -1L
    // HAL 可能在 daemon 绑流后再次开流（meta stream_id 更大）；meta 不比当前会话旧即视为同一真实码流。
    val hasSource = streamId > 0 && metaId > 0 && metaId >= streamId
    return Status(
        hasData = age in 0..5000,
        version = grab(Regex("\"version\"\\s*:\\s*\"([0-9.]+)\""), js),
        codec = grab(Regex("\"codec\"\\s*:\\s*\"([a-zA-Z0-9_]+)\""), js),
        enc = grab(Regex("\"encoding\"\\s*:\\s*\"([A-Za-z0-9]+)\""), js),
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
        streamId = streamId,
        hasSource = hasSource,
        srcFmt = meta["fmt"] ?: "",
        srcFmtName = meta["fmt_name"] ?: "",
        srcGuess = meta["codec_guess"] ?: "",
        srcChmask = meta["chmask"] ?: "",
        srcRate = if (srcRate.isNotEmpty()) srcRate else (meta["rate"] ?: ""),
        ageMs = age,
        levels = levels,
        srcActive = srcActive,
        srcAgeMs = srcAgeMs,
        srcCodec = srcCodec,
        pt = grab(Regex("\"pt\"\\s*:\\s*(\\d+)"), js),
        outPeak = grab(Regex("\"out_peak\"\\s*:\\s*(-?[0-9.]+)"), js).toFloatOrNull() ?: 0f,
        limited = grab(Regex("\"limited\"\\s*:\\s*(\\d+)"), js).toLongOrNull() ?: 0L,
        dialnormDb = grab(Regex("\"dialnorm_db\"\\s*:\\s*(-?[0-9.]+)"), js).toFloatOrNull() ?: 0f
    )
}
/** 守护脚本写的生命周期状态：running / stopped / rolledback；没有文件时返回空。 */
fun readState(): String {
    val f = firstExisting(STATE_PATH, STATE_PATH_LEGACY) ?: return ""
    return try { f.readText().trim() } catch (t: Throwable) { "" }
}