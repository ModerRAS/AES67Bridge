package dev.aes67bridge.tv.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.FlowRowScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.aes67bridge.tv.AutoStartService
import dev.aes67bridge.tv.UpdateManager
import java.io.File
import kotlinx.coroutines.launch

private val PBg = Color(0xFF0B1219)
private val PAccent = Color(0xFF35D0A5)
private val PDim = Color(0xFF7E92A6)
private val PText = Color(0xFFEAF2FA)
private val PLine = Color(0xFF2A3947)

/** Tail of the root-side tool result log (install hook / enable ADB etc.). */
private fun readToolResult(ctx: android.content.Context): String = try {
    val f = File(ctx.filesDir, "tool-result.txt")
    if (!f.exists()) "" else f.readLines().takeLast(14).joinToString("\n")
} catch (t: Throwable) {
    ""
}

/** 布局是唯一真相：声道数与通道标签都由它派生，不允许布局和声道各选各的。 */
private val LayoutCh = linkedMapOf(
    "2.0" to 2, "4.1.2" to 7, "5.1" to 6, "5.1.2" to 8, "7.1" to 8,
    "7.1.2" to 10, "5.1.4" to 10, "7.1.4" to 12, "9.1.6" to 16
)
private val LayoutLabels = mapOf(
    "2.0" to "L R",
    "4.1.2" to "L R LFE Ls Rs Tfl Tfr",
    "5.1" to "L R C LFE Ls Rs",
    "5.1.2" to "L R C LFE Ls Rs Tfl Tfr",
    "7.1" to "L R C LFE Ls Rs Lb Rb",
    "7.1.2" to "L R C LFE Ls Rs Lb Rb Tfl Tfr",
    "5.1.4" to "L R C LFE Ls Rs Tfl Tfr Tbl Tbr",
    "7.1.4" to "L R C LFE Ls Rs Lb Rb Tfl Tfr Tbl Tbr",
    "9.1.6" to "L R C LFE Ls Rs Lb Rb Lw Rw Tfl Tfr Tbl Tbr Tsl Tsr"
)

internal val ConfDefault = mapOf(
    "proto" to "aes67", "group" to "239.69.67.1", "port" to "5004",
    "ptime_us" to "1000", "codec" to "pcm", "layout" to "7.1",
    "split" to "1", "sap" to "1", "gain_db" to "0", "limiter_db" to "-1",
    "hal_buf" to "20", "fifo_kb" to "32", "auto_idle_ms" to "0", "ptp" to "0",
    "ptp_gm" to "", "ptp_domain" to "0", "ptp_profile" to "aes67", "asrc" to "0", "pt" to "",
    "http_port" to "8080", "resampler" to "rubato",
    "pcm_export" to "1", "bits_export" to "1", "boot" to "1",
    "engine" to "rust", "decode" to "auto"
)

/** 完整配置：默认值打底 + 当前已加载/修改的值（保证 daemon.conf 每次写全，不再丢 sap 这类键）。 */
internal fun completeConf(vals: Map<String, String>): Map<String, String> =
    ConfDefault + vals

private fun validMcast(addr: String): Boolean {
    val p = addr.split(".")
    if (p.size != 4) return false
    val o = p.mapNotNull { it.toIntOrNull() }
    if (o.size != 4 || o.any { it < 0 || it > 255 }) return false
    return o[0] in 224..239
}

private fun validIp(addr: String): Boolean {
    val p = addr.split(".")
    if (p.size != 4) return false
    val o = p.mapNotNull { it.toIntOrNull() }
    return o.size == 4 && o.all { it in 0..255 } && addr != "0.0.0.0"
}

private fun validPort(s: String): Boolean {
    val v = s.toIntOrNull() ?: return false
    return v in 1..65535
}

@Composable
private fun OptChip(
    text: String,
    selected: Boolean = false,
    accent: Boolean = false,
    muted: Boolean = false,
    big: Boolean = false,
    onClick: () -> Unit
) {
    var focused by remember { mutableStateOf(false) }
    val bg = when {
        muted -> Color(0xFF121A22)
        accent -> PAccent
        selected -> Color(0xFF1F3A34)
        else -> Color(0xFF18232E)
    }
    val fg = when {
        muted -> Color(0xFF51616E)
        accent -> Color(0xFF04140E)
        selected -> PAccent
        else -> Color(0xFFCBD8E4)
    }
    Box(
        Modifier
            .onFocusChanged { focused = it.isFocused }
            .focusable()
            .clip(RoundedCornerShape(50))
            .background(bg)
            .border(
                if (focused) 2.dp else 1.dp,
                if (focused && !muted) PAccent else PLine,
                RoundedCornerShape(50)
            )
            .clickable(enabled = !muted) { onClick() }
            .padding(horizontal = if (big) 20.dp else 13.dp, vertical = if (big) 10.dp else 6.dp)
    ) {
        Text(text, color = fg, fontSize = if (big) 17.sp else 13.sp, maxLines = 1)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RowLabel(label: String, content: @Composable FlowRowScope.() -> Unit) {
    Row(verticalAlignment = Alignment.Top, modifier = Modifier.padding(vertical = 4.dp)) {
        Text(
            label, color = PDim, fontSize = 13.sp,
            modifier = Modifier.width(84.dp).padding(top = 6.dp)
        )
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
            modifier = Modifier.weight(1f),
            content = content
        )
    }
}

@Composable
private fun Hint(text: String) {
    Text(
        text, color = Color(0xFF8FA3B6), fontSize = 11.sp, lineHeight = 15.sp,
        modifier = Modifier.padding(start = 84.dp, bottom = 6.dp)
    )
}

/** 数字键盘：单独成一个页面，保证焦点不会跑到下面那层设置里。 */
@Composable
private fun KeypadContent(
    title: String,
    initial: String,
    allowDot: Boolean,
    onCancel: () -> Unit,
    onOk: (String) -> Unit
) {
    var buf by remember(initial) { mutableStateOf(initial) }
    val keys = listOf(
        listOf("1", "2", "3"),
        listOf("4", "5", "6"),
        listOf("7", "8", "9"),
        listOf(".", "0", "<")
    )
    // 数字键盘/外接键盘直接输入：数字、小数点、退格、回车（确定）、ESC/返回（取消）。
    // 遥控器 DPAD 仍可逐个移动焦点按 ENTER 点屏上的键。
    Column(
        Modifier
            .fillMaxWidth()
            .onPreviewKeyEvent { ev ->
                if (ev.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                val kc = ev.nativeKeyEvent.keyCode
                when {
                    kc >= android.view.KeyEvent.KEYCODE_0 && kc <= android.view.KeyEvent.KEYCODE_9 -> {
                        if (buf.length < 24) buf += (kc - android.view.KeyEvent.KEYCODE_0).toString()
                        true
                    }
                    kc >= android.view.KeyEvent.KEYCODE_NUMPAD_0 && kc <= android.view.KeyEvent.KEYCODE_NUMPAD_9 -> {
                        if (buf.length < 24) buf += (kc - android.view.KeyEvent.KEYCODE_NUMPAD_0).toString()
                        true
                    }
                    kc == android.view.KeyEvent.KEYCODE_PERIOD || kc == android.view.KeyEvent.KEYCODE_NUMPAD_DOT -> {
                        if (allowDot && buf.length < 24) buf += "."
                        true
                    }
                    kc == android.view.KeyEvent.KEYCODE_DEL || kc == android.view.KeyEvent.KEYCODE_FORWARD_DEL -> {
                        if (buf.isNotEmpty()) buf = buf.dropLast(1)
                        true
                    }
                    kc == android.view.KeyEvent.KEYCODE_ENTER || kc == android.view.KeyEvent.KEYCODE_NUMPAD_ENTER -> {
                        onOk(buf.trim())
                        true
                    }
                    kc == android.view.KeyEvent.KEYCODE_ESCAPE -> {
                        onCancel()
                        true
                    }
                    else -> false
                }
            }
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, color = PText, fontSize = 22.sp)
            Spacer(Modifier.width(16.dp))
            Text(if (buf.isEmpty()) "__" else buf, color = PAccent, fontSize = 24.sp)
        }
        Text("支持数字键盘直接输入：数字 · 小数点 · 退格 · 回车确定 · ESC 取消", color = PDim, fontSize = 11.sp)
        Spacer(Modifier.height(12.dp))
        keys.forEach { row ->
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(bottom = 8.dp)) {
                row.forEach { k ->
                    val dotOk = allowDot || k != "."
                    OptChip(k, muted = !dotOk, big = true) {
                        when (k) {
                            "<" -> buf = buf.dropLast(1)
                            else -> if (buf.length < 24) buf += k
                        }
                    }
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OptChip("清空", big = true) { buf = "" }
            OptChip("取消", big = true) { onCancel() }
            OptChip("确定", accent = true, big = true) { onOk(buf.trim()) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SettingsPanel(onClose: () -> Unit, onApply: (Map<String, String>) -> Unit) {
    val ctx = LocalContext.current
    val cats = listOf("输出", "信号", "延迟/时钟", "系统", "装机")
    var cat by remember { mutableStateOf(0) }

    val vals = remember {
        mutableStateMapOf<String, String>().apply {
            try {
                val f = File(ctx.filesDir, "daemon.conf")
                if (f.exists()) {
                    f.readText().lineSequence().forEach { ln ->
                        val i = ln.indexOf('=')
                        if (i > 0) put(ln.substring(0, i).trim(), ln.substring(i + 1).trim())
                    }
                }
            } catch (t: Throwable) {
            }
            val lay = this["layout"]?.takeIf { LayoutCh.containsKey(it) }
                ?: LayoutCh.entries.firstOrNull { it.value == (this["maxch"]?.toIntOrNull() ?: -1) }?.key
                ?: "7.1"
            put("layout", lay)
            put("maxch", (LayoutCh[lay] ?: 8).toString())
        }
    }
    fun get(k: String): String = vals[k] ?: ConfDefault[k] ?: ""
    fun set(k: String, v: String) {
        vals[k] = v
    }
    // 应用时永远提交完整配置（默认值兜底），避免 daemon.conf 缺键导致守护回落默认值。
    fun applyMap(extra: Map<String, String> = emptyMap()): Map<String, String> =
        completeConf(vals) + extra

    // 0 主设置；1 组播编辑；2 PTP GM 编辑；3..6 数字键盘（组播地址/端口/GM 地址/domain）
    // 读一次当前 HAL 实际流（abstract status socket，旧引擎回退 JSON 文件），用来说明"布局 ≠ 源声道"这类矛盾。
    val srcSt = remember { readStatus() }
    val srcCh = remember { srcSt.ch.toIntOrNull() }
    val srcRate = remember { srcSt.rate.takeIf { it.isNotEmpty() } }

    val scope = rememberCoroutineScope()
    var autoCheckOn by remember { mutableStateOf(UpdateManager.autoCheckEnabled(ctx)) }
    var autoInstallOn by remember { mutableStateOf(UpdateManager.autoInstallEnabled(ctx)) }
    var page by remember { mutableStateOf(0) }
    var gAddr by remember { mutableStateOf("") }
    var gPort by remember { mutableStateOf("") }
    var gmAddr by remember { mutableStateOf("") }
    var gmDomain by remember { mutableStateOf("") }
    var adbConfirm by remember { mutableStateOf(false) }
    var toolResult by remember { mutableStateOf(readToolResult(ctx)) }
    var osdStyle by remember {
        mutableStateOf(
            ctx.getSharedPreferences("aes67bridge", android.content.Context.MODE_PRIVATE)
                .getString("osd_style", "bar") ?: "bar"
        )
    }

    Dialog(
        onDismissRequest = {
            // 返回键逐级退：数字键盘 → 上一页 → 设置主页 → 关闭。
            when {
                page in 3..4 -> page = 1
                page in 5..6 -> page = 2
                page == 1 || page == 2 -> page = 0
                else -> onClose()
            }
        },
        properties = DialogProperties(usePlatformDefaultWidth = false)
    ) {
        Box(
            Modifier
                .then(if (page == 0) Modifier.fillMaxWidth(0.92f) else Modifier.width(620.dp))
                .clip(RoundedCornerShape(22.dp))
                .background(PBg)
                .border(1.dp, PLine, RoundedCornerShape(22.dp))
                .padding(horizontal = 24.dp, vertical = 18.dp)
        ) {
            // 一次只组合一个页面：旧页面直接离开组合树，焦点不可能留在下面那层。
            when (page) {
                0 -> Column {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("设置", color = PText, fontSize = 28.sp)
                        Spacer(Modifier.width(16.dp))
                        Text("改完点「应用设置」，重启守护生效", color = PDim, fontSize = 12.sp)
                        Spacer(Modifier.weight(1f))
                        OptChip("关闭") { onClose() }
                    }
                    Spacer(Modifier.height(12.dp))
                    Row {
                        Column(Modifier.width(150.dp)) {
                            cats.forEachIndexed { i, name ->
                                Box(Modifier.padding(vertical = 3.dp)) {
                                    OptChip((if (cat == i) "● " else "○ ") + name, selected = cat == i) { cat = i }
                                }
                            }
                        }
                        Spacer(Modifier.width(18.dp))
                        Column(
                            Modifier.weight(1f).height(430.dp)
                                .verticalScroll(rememberScrollState())
                        ) {
                            if (cat == 0) {
                                RowLabel("协议") {
                                    OptChip("AES67", selected = get("proto") == "aes67") { set("proto", "aes67") }
                                    OptChip("RAVENNA · 暂缓", muted = true) { }
                                }
                                RowLabel("组播") {
                                    OptChip(
                                        "标准 239.69.67.1:5004",
                                        selected = get("group") == "239.69.67.1" && get("port") == "5004"
                                    ) { set("group", "239.69.67.1"); set("port", "5004") }
                                    OptChip(
                                        "备选 239.69.68.1:5004",
                                        selected = get("group") == "239.69.68.1" && get("port") == "5004"
                                    ) { set("group", "239.69.68.1"); set("port", "5004") }
                                    val isPreset1 = get("group") == "239.69.67.1" && get("port") == "5004"
                                    val isPreset2 = get("group") == "239.69.68.1" && get("port") == "5004"
                                    val isCustom = !isPreset1 && !isPreset2
                                    OptChip(
                                        if (isCustom) "自定义 " + get("group") + ":" + get("port") else "自定义…",
                                        selected = isCustom
                                    ) { gAddr = get("group"); gPort = get("port"); page = 1 }
                                }
                                Hint("当前 " + get("group") + ":" + get("port") + " · 端口默认 5004（AES67 RTP 标准）；上面两个只是快捷填充，自定义时「自定义」会亮起")
                                RowLabel("ptime") {
                                    listOf(125, 250, 500, 1000, 2000, 4000).forEach { us ->
                                        OptChip(
                                            when (us) {
                                                125 -> "125us·实验"
                                                2000 -> "2ms·AES67"
                                                4000 -> "4ms·省CPU"
                                                else -> us.toString() + "us"
                                            },
                                            selected = get("ptime_us") == us.toString()
                                        ) {
                                            set("ptime_us", us.toString())
                                        }
                                    }
                                }
                                Hint("1000us = ST 2110-30 Level A（正式路径）。125us（Level B）在本机实测有丢包且需自旋占一个核，仅作实验；250/500us 为 AES67 附加档。2/4ms 仅 AES67（ST 2110-30 不允许）：包率降到 1/2、1/4，发送 CPU 明显下降，但延迟 +1/+3ms 且接收端必须支持对应 ptime；4ms 多声道会自动拆成更多 RTP 流。")
                                RowLabel("编码") {
                                    OptChip("L24", selected = get("codec") == "pcm") { set("codec", "pcm") }
                                    OptChip("L16", selected = get("codec") == "l16") { set("codec", "l16") }
                                }
                                RowLabel("Payload Type") {
                                    OptChip("自动", selected = get("pt").isEmpty() || get("pt") == "0") { set("pt", "") }
                                    OptChip("97 · L24", selected = get("pt") == "97") {
                                        set("pt", "97"); set("codec", "pcm")
                                    }
                                    OptChip("96 · L16", selected = get("pt") == "96") {
                                        set("pt", "96"); set("codec", "l16")
                                    }
                                }
                                Hint("RTP 动态 PT（SDP 会广播）：AES67 惯例 97=L24、96=L16；这里选 96/97 会同时切换编码，接收端用「按 PT 推采样宽度」的方式解析时才不会把 24bit 当 16bit 读成白噪音")
                                RowLabel("采样率") {
                                    OptChip("48kHz · AES67（固定）", selected = true) { }
                                }
                                Hint("AES67 固定 48kHz 输出；44.1k 源由守护重采样（SRC），主界面「信号源」显示的是原始采样率，不是输出采样率")
                                RowLabel("布局") {
                                    LayoutCh.keys.forEach { l ->
                                        val ch = LayoutCh[l] ?: 2
                                        // >8ch 布局只作为 TrueHD/Atmos 对象渲染的目标（PCM 路径 HAL 上限 8ch）；
                                        // 4.1.2 暂不支持（ROAR validated patch 无 Layout412）。
                                        val supported = l != "4.1.2"
                                        OptChip(
                                            if (ch > 8) l + "·对象" else l,
                                            selected = get("layout") == l,
                                            muted = !supported
                                        ) {
                                            if (supported) {
                                                set("layout", l)
                                                set("maxch", ch.toString())
                                            }
                                        }
                                    }
                                }
                                Hint("声道 " + get("maxch") + " ch 由布局决定 · " + (LayoutLabels[get("layout")] ?: ""))
                                Hint("PCM 路径最多 8ch，且 HAL 通道语义只有标准 7.1（没有高度位）：选 5.1.2 时由 ROAR 把 7.1/5.1 渲染到目标布局（Lb/Rb 折进 Ls/Rs），但高度声道只能来自 TrueHD/Atmos 码流路径（对象解码后渲染，不受 HAL PCM 通道限制）。4.1.2 不被 audio policy 接受；9.1.6 仅作为 TrueHD/Atmos 对象渲染目标")
                                if (srcCh != null && srcCh != (LayoutCh[get("layout")] ?: 0)) {
                                    Text(
                                        "⚠ 当前 HAL 源只有 " + srcCh + "ch" + (if (srcRate != null) " / " + srcRate + "Hz" else "") +
                                            "，与布局 " + get("layout") + "（" + (LayoutCh[get("layout")] ?: 0) + "ch）不一致：" +
                                            "布局只描述通道顺序，不会补通道；请改布局或检查播放器输出声道",
                                        color = Color(0xFFF2C14E), fontSize = 11.sp, lineHeight = 15.sp,
                                        modifier = Modifier.padding(start = 84.dp, bottom = 6.dp)
                                    )
                                }
                                RowLabel("分包") {
                                    OptChip("标准多流（>8ch 拆流）", selected = get("split") == "1") { set("split", "1") }
                                    OptChip("单流扩展（RTP 分片）", selected = get("split") == "0") { set("split", "0") }
                                }
                                Hint(
                                    "标准：AES67 一条流最多 8ch（48k/1ms），>8ch 自动拆成多条 RTP 流，每条流独立 group/port/SSRC，SDP 多 m= 段；" +
                                        "单流扩展：所有声道塞一条流，超 MTU 用 RTP 分片（同 ts、seq 连续、末包 marker=1），非标准，仅兼容测试用"
                                )
                                RowLabel("SAP") {
                                    OptChip("开", selected = get("sap") == "1") { set("sap", "1") }
                                    OptChip("关", selected = get("sap") == "0") { set("sap", "0") }
                                }
                                Hint("开：向 224.2.127.254:9875 周期公告本流 SDP，控制器可自动发现（守护空闲时也保持一条静音 RTP 流，随时能被发现）；关：静默发送。也可以用 HTTP 拉 SDP：http://芝杜IP:" + (get("http_port").ifEmpty { "8080" }) + "/aes67.sdp（http_port=0 关闭）")
                            }
                            if (cat == 1) {
                                RowLabel("PCM 导出") {
                                    OptChip("开", selected = get("pcm_export") == "1") { set("pcm_export", "1") }
                                    OptChip("关", selected = get("pcm_export") == "0") { set("pcm_export", "0") }
                                }
                                RowLabel("码流导出") {
                                    OptChip("开", selected = get("bits_export") == "1") { set("bits_export", "1") }
                                    OptChip("关", selected = get("bits_export") == "0") { set("bits_export", "0") }
                                }
                                Hint("导出是播放器把 PCM / 压缩码流交给本机的开关：PCM 用于 AES67，码流用于 TrueHD/AC3 自解码")
                                RowLabel("源码解码") {
                                    OptChip("自动", selected = get("decode") == "auto") { set("decode", "auto") }
                                    OptChip("PCM", selected = get("decode") == "pcm") { set("decode", "pcm") }
                                    OptChip("AC3/EAC3", selected = get("decode") == "eac3") { set("decode", "eac3") }
                                    OptChip("TrueHD/Atmos", selected = get("decode") == "truehd") { set("decode", "truehd") }
                                }
                                Hint("自动：按 live.bits.meta 格式判断（TrueHD 走对象渲染，AC3/EAC3 走解码，PCM 直通）；强制模式用于排查")
                                RowLabel("输入增益") {
                                    listOf(-24, -12, -6, 0, 6).forEach { g ->
                                        OptChip(
                                            if (g > 0) "+" + g else g.toString(),
                                            selected = get("gain_db") == g.toString()
                                        ) { set("gain_db", g.toString()) }
                                    }
                                }
                                Hint("在渲染/限幅之前统一加减：负值留余量，正值由 limiter 兜底（+ 过多会听到限幅泵动）")
                                RowLabel("音量条") {
                                    OptChip("横条", selected = osdStyle == "bar") {
                                        osdStyle = "bar"
                                        ctx.getSharedPreferences("aes67bridge", android.content.Context.MODE_PRIVATE)
                                            .edit().putString("osd_style", "bar").apply()
                                    }
                                    OptChip("数字", selected = osdStyle == "numeric") {
                                        osdStyle = "numeric"
                                        ctx.getSharedPreferences("aes67bridge", android.content.Context.MODE_PRIVATE)
                                            .edit().putString("osd_style", "numeric").apply()
                                    }
                                }
                                Hint("遥控器音量键在屏幕上的显示样式：横条（类系统）或纯数字 −dB；只显示 AES67 电平，不改变系统音量")
                                RowLabel("限幅上限") {
                                    listOf(-1, -3, -6, -12).forEach { db ->
                                        OptChip(
                                            db.toString() + " dBFS",
                                            selected = get("limiter_db") == db.toString()
                                        ) { set("limiter_db", db.toString()) }
                                    }
                                }
                                Hint("ROAR 峰值限幅上限：越小余量越大、下游表越不容易顶满；默认 -1 dBFS（AES67 母带惯例）")
                            }
                            if (cat == 2) {
                                RowLabel("HAL 缓冲") {
                                    OptChip("20ms 标准", selected = get("hal_buf") == "20") { set("hal_buf", "20") }
                                    OptChip("5ms 低延迟", selected = get("hal_buf") == "5") { set("hal_buf", "5") }
                                }
                                Hint("HAL 写缓冲：20ms 稳、5ms 延迟低但容易欠载；与下面 FIFO 是两回事")
                                RowLabel("FIFO") {
                                    listOf(8, 16, 32).forEach { k ->
                                        OptChip(k.toString() + "KB", selected = get("fifo_kb") == k.toString()) {
                                            set("fifo_kb", k.toString())
                                        }
                                    }
                                }
                                Hint("HAL 与守护之间的共享 FIFO 深度：越大越抗抖动，延迟越高")
                                RowLabel("时钟源") {
                                    OptChip("关(local)", selected = get("ptp") == "0") { set("ptp", "0") }
                                    OptChip(
                                        "自动(组播)",
                                        selected = get("ptp") == "1" && get("ptp_gm").isEmpty()
                                    ) {
                                        set("ptp", "1"); set("ptp_gm", ""); set("ptp_domain", "0"); set("ptp_profile", "aes67")
                                    }
                                    OptChip(
                                        if (get("ptp") == "1" && get("ptp_gm").isNotEmpty())
                                            "GM " + get("ptp_gm") + " · d" + get("ptp_domain")
                                        else "手动 GM…",
                                        selected = get("ptp") == "1" && get("ptp_gm").isNotEmpty()
                                    ) { gmAddr = get("ptp_gm"); gmDomain = get("ptp_domain"); page = 2 }
                                }
                                RowLabel("PTP Profile") {
                                    OptChip(
                                        "AES67 · d0",
                                        selected = get("ptp_profile") != "st2059-2"
                                    ) {
                                        set("ptp", "1"); set("ptp_profile", "aes67"); set("ptp_domain", "0")
                                    }
                                    OptChip(
                                        "ST 2059-2 · d127",
                                        selected = get("ptp_profile") == "st2059-2"
                                    ) {
                                        set("ptp", "1"); set("ptp_profile", "st2059-2"); set("ptp_domain", "127")
                                    }
                                }
                                Hint("对接 SMPTE ST 2110 网络用 ST 2059-2（domain 127，Sync 125ms/Announce 1s 参数）；纯 AES67 域用 AES67 media profile（domain 0）。手动 GM 时可单独改 domain。")
                                Hint("自动：监听 PTP 组播通告并锁定（实测有效，推荐）；手动：填 GM 的 IP + domain（若 GM 不支持 unicast 协商会自动回退组播）；关：用本机时钟")
                                RowLabel("ASRC") {
                                    OptChip("关", selected = get("asrc") == "0") { set("asrc", "0") }
                                    OptChip("开(rubato)", selected = get("asrc") == "1") { set("asrc", "1") }
                                }
                                Hint("ASRC 做采样率自适应补偿，PTP 锁定后用；未锁定时保持关闭")
                            }
                            if (cat == 3) {
                                RowLabel("开机自启") {
                                    OptChip("开", selected = get("boot") == "1") { set("boot", "1") }
                                    OptChip("关", selected = get("boot") == "0") { set("boot", "0") }
                                }
                                Hint("开：开机由 BootReceiver 自动拉起守护并开始广播")
                                RowLabel("守护") {
                                    OptChip("重启守护") { onApply(applyMap(mapOf("restart" to "1"))) }
                                }
                                RowLabel("危险") {
                                    OptChip("回滚原厂 HAL") { onApply(mapOf("rollback" to "1")) }
                                }
                            }
                            if (cat == 3) {
                                Hint("") // separator
                                RowLabel("启动检查") {
                                    OptChip("开", selected = autoCheckOn) {
                                        autoCheckOn = true
                                        UpdateManager.setAutoCheckEnabled(ctx, true)
                                    }
                                    OptChip("关", selected = !autoCheckOn) {
                                        autoCheckOn = false
                                        UpdateManager.setAutoCheckEnabled(ctx, false)
                                    }
                                }
                                Hint("开：每次启动最多 6 小时查一次版本，只提示「有新版本」，不会自己安装")
                                RowLabel("自动安装") {
                                    OptChip("关（推荐）", selected = !autoInstallOn) {
                                        autoInstallOn = false
                                        UpdateManager.setAutoInstallEnabled(ctx, false)
                                    }
                                    OptChip("开", selected = autoInstallOn) {
                                        autoInstallOn = true
                                        UpdateManager.setAutoInstallEnabled(ctx, true)
                                    }
                                }
                                Hint("默认关：发现新版只在主界面出现「更新 vX」按钮，点了才下载安装；开：静默自动装（APK + 守护，会短暂中断）")
                                RowLabel("版本") {
                                    Text(
                                        "App v" + UpdateManager.currentVersionName() +
                                            "  ·  守护 " + (srcSt.version.ifEmpty { "?" }),
                                        color = PText, fontSize = 13.sp,
                                        modifier = Modifier.padding(vertical = 6.dp)
                                    )
                                }
                                RowLabel("更新") {
                                    OptChip(if (UpdateManager.busy) "检查中…" else "检查更新", muted = UpdateManager.busy) {
                                        scope.launch { UpdateManager.check(ctx) }
                                    }
                                    val rel = UpdateManager.available
                                    if (rel != null && UpdateManager.isNewer(rel)) {
                                        OptChip("安装 v" + rel.versionName, accent = true) {
                                            scope.launch { UpdateManager.applyUpdate(ctx, rel) }
                                        }
                                    }
                                }
                                if (UpdateManager.status.isNotEmpty()) {
                                    Hint(UpdateManager.status)
                                }
                            }
                            if (cat == 4) {
                                Hint("装机工具：把 /data/aes67bridge 切到内存盘（重启后生效），或者打开无线 ADB。打开 ADB 后本机可在局域网内被 root 控制，请只在可信网络使用。")
                                RowLabel("运行时") {
                                    OptChip("安装/修复开机自启", accent = true) {
                                        AutoStartService.request(ctx, "INSTALL_BOOTHOOK")
                                        toolResult = readToolResult(ctx)
                                    }
                                    OptChip("查看结果") { toolResult = readToolResult(ctx) }
                                }
                                Hint("写入 /data/aes67bridge.persist/boot 启动钩子并把运行时切到 tmpfs；App 私有目录仍是唯一持久层，重启后自动恢复")
                                RowLabel("无线 ADB") {
                                    OptChip("开启（会 root）") { adbConfirm = true }
                                    OptChip("关闭") {
                                        AutoStartService.request(ctx, "ADB_DISABLE")
                                        toolResult = readToolResult(ctx)
                                    }
                                }
                                Hint("开启会重启 adbd（约 2 秒）；之后电脑可 adb connect <盒子IP>:5555，adb root 即 uid=0。不用时请关闭。")
                                RowLabel("公钥") {
                                    Text(
                                        if (File(ctx.filesDir, "adb_pubkey.txt").exists())
                                            "已提供 adb_pubkey.txt"
                                        else
                                            "未提供（可把电脑 adbkey.pub 的一行放进 App 私有目录 adb_pubkey.txt）",
                                        color = PDim, fontSize = 12.sp,
                                        modifier = Modifier.padding(vertical = 6.dp)
                                    )
                                }
                                if (toolResult.isNotEmpty()) {
                                    Text(
                                        toolResult, color = Color(0xFF8FA3B6), fontSize = 11.sp,
                                        lineHeight = 15.sp, modifier = Modifier.padding(top = 8.dp)
                                    )
                                }
                            }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        OptChip("应用设置", accent = true) { onApply(applyMap()) }
                        OptChip("应用并重启守护") { onApply(applyMap(mapOf("restart" to "1"))) }
                    }
                    Text(
                        "AES67Bridge " + UpdateManager.currentVersionName() + " · status socket 实时",
                        color = PDim, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp)
                    )
                }

                // ---- 自定义组播（手写地址/端口）----
                1 -> Column(Modifier.fillMaxWidth()) {
                    Text("自定义组播", color = PText, fontSize = 22.sp)
                    Spacer(Modifier.height(12.dp))
                    RowLabel("地址") {
                        OptChip(gAddr.ifEmpty { "点此输入" }, selected = validMcast(gAddr)) {
                            page = 3
                        }
                    }
                    RowLabel("端口") {
                        OptChip(gPort.ifEmpty { "点此输入" }, selected = validPort(gPort)) {
                            page = 4
                        }
                    }
                    Hint("地址范围 224.0.0.0–239.255.255.255，端口 1–65535")
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                        OptChip("返回") { page = 0 }
                        OptChip("确定", accent = validMcast(gAddr) && validPort(gPort)) {
                            if (validMcast(gAddr) && validPort(gPort)) {
                                set("group", gAddr)
                                set("port", gPort)
                                page = 0
                            }
                        }
                    }
                }

                // ---- PTP 手动 GM ----
                2 -> Column(Modifier.fillMaxWidth()) {
                    Text("PTP 主时钟（GM）", color = PText, fontSize = 22.sp)
                    Spacer(Modifier.height(12.dp))
                    RowLabel("GM 地址") {
                        OptChip(gmAddr.ifEmpty { "点此输入" }, selected = validIp(gmAddr)) { page = 5 }
                    }
                    RowLabel("domain") {
                        OptChip(gmDomain.ifEmpty { "0" }, selected = (gmDomain.toIntOrNull() ?: 0) in 0..127) {
                            page = 6
                        }
                    }
                    Hint("填 GM 的 IP（例子：192.168.63.215）；domain 0–127。不再写死某台 GM")
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                        OptChip("返回") { page = 0 }
                        OptChip("确定", accent = validIp(gmAddr) && (gmDomain.toIntOrNull() ?: -1) in 0..127) {
                            if (validIp(gmAddr) && (gmDomain.toIntOrNull() ?: -1) in 0..127) {
                                set("ptp", "1")
                                set("ptp_gm", gmAddr)
                                set("ptp_domain", gmDomain)
                                page = 0
                            }
                        }
                    }
                }

                3 -> KeypadContent("组播地址", gAddr, true, { page = 1 }, { gAddr = it; page = 1 })
                4 -> KeypadContent("端口", gPort, false, { page = 1 }, { gPort = it; page = 1 })
                5 -> KeypadContent("GM 地址", gmAddr, true, { page = 2 }, { gmAddr = it; page = 2 })
                else -> KeypadContent("domain", gmDomain, false, { page = 2 }, { gmDomain = it; page = 2 })
            }
        }
    }

    if (adbConfirm) {
        Dialog(onDismissRequest = { adbConfirm = false }) {
            Column(
                Modifier
                    .width(560.dp)
                    .clip(RoundedCornerShape(18.dp))
                    .background(PBg)
                    .border(1.dp, PLine, RoundedCornerShape(18.dp))
                    .padding(22.dp)
            ) {
                Text("开启无线 ADB", color = PText, fontSize = 20.sp)
                Text(
                    "这会打开厂商 ADB 门闩（persist.zidoo.test.dc）并重启 adbd：本机将可在局域网内被 root 控制，等同把 root 暴露给同网段设备。只在可信网络使用，不用时请关闭。",
                    color = PDim, fontSize = 12.sp, lineHeight = 16.sp,
                    modifier = Modifier.padding(top = 10.dp)
                )
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(top = 16.dp)
                ) {
                    OptChip("取消") { adbConfirm = false }
                    OptChip("确认开启", accent = true) {
                        adbConfirm = false
                        AutoStartService.request(ctx, "ADB_ENABLE")
                        toolResult = readToolResult(ctx)
                    }
                }
            }
        }
    }
}