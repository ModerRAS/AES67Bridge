package dev.aes67bridge.tv.ui

import android.content.Intent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.foundation.shape.CircleShape
import androidx.tv.material3.Button
import androidx.tv.material3.Text as TvText
import dev.aes67bridge.tv.AutoStartService
import kotlinx.coroutines.delay
import java.io.File

private val Bg0 = Color(0xFF06090D)
private val Bg1 = Color(0xFF0C141C)
private val Card = Color(0xFF111A24)
private val Accent = Color(0xFF35D0A5)
private val Danger = Color(0xFFE05C5C)
private val Dim = Color(0xFF7E92A6)
private val Warn = Color(0xFFF2C14E)

@Composable
fun ConsoleScreen() {
    var st by remember { mutableStateOf(Status()) }
    var stateText by remember { mutableStateOf("") }
    var conf by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    var showSettings by remember { mutableStateOf(false) }
    var pending by remember { mutableStateOf("") }
    val ctx = LocalContext.current
    LaunchedEffect(Unit) {
        while (true) {
            st = readStatus()
            stateText = readState()
            conf = readConf(ctx)
            delay(700)
        }
    }
    val running = stateText == "running" && st.hasData
    // 动作闭环：点了之后先显示"启动中/停止中"，等 state.txt 真正翻转再清掉；超时也清掉
    LaunchedEffect(stateText, st.hasData) {
        if (pending == "start" && stateText == "running" && st.hasData) pending = ""
        if (pending == "stop" && stateText == "stopped") pending = ""
        if (pending == "rollback" && stateText == "rolledback") pending = ""
    }
    LaunchedEffect(pending) {
        if (pending.isNotEmpty()) {
            delay(9000)
            pending = ""
        }
    }
    fun send(action: String) {
        try {
            ctx.startForegroundService(Intent(ctx, AutoStartService::class.java).setAction(action))
        } catch (t: Throwable) { }
    }
    fun writeConf(m: Map<String, String>) {
        try {
            val keys = listOf("group", "port", "ptime_us", "maxch", "codec", "layout", "split", "sap", "gain_db", "hal_buf", "fifo_kb", "ptp", "ptp_gm", "ptp_domain", "asrc", "pcm_export", "bits_export", "boot", "engine", "decode")
            val sb = StringBuilder()
            for (k in keys) { val v = m[k]; if (v != null) sb.append(k).append('=').append(v).append('\n') }
            File(ctx.filesDir, "daemon.conf").writeText(sb.toString())
        } catch (t: Throwable) { }
    }

    Column(
        Modifier
            .fillMaxSize()
            .background(Brush.verticalGradient(listOf(Bg0, Bg1, Bg0)))
            .padding(start = 52.dp, end = 52.dp, top = 34.dp, bottom = 24.dp)
    ) {
        Header(st, running, stateText, pending)
        Spacer(Modifier.height(22.dp))
        val layout = conf["layout"] ?: st.layout.ifEmpty { "7.1" }
        val outCh = layoutChannels(layout)
        val srcCh = st.ch.toIntOrNull()
        Row(horizontalArrangement = Arrangement.spacedBy(18.dp)) {
            InfoCard("信号源（HAL 抓到的原始流）", fmtValue(st), "源 " + fmtRate(st.rate) + "Hz · SRC " + nz(st.rsIn) + " → " + nz(st.rsOut), Modifier.weight(1f))
            InfoCard("AES67 输出", "48k / " + outCh + "ch", "layout " + layout + " · " + (conf["group"] ?: "-") + ":" + (conf["port"] ?: "-"), Modifier.weight(1f))
            InfoCard(
                "延迟模式",
                if (st.mode.isEmpty()) "-" else st.mode,
                "ptime " + nz(st.ptime) + "µs · " + when (st.split) {
                    "1" -> "标准多流"
                    "0" -> "单流扩展"
                    else -> "-"
                },
                Modifier.weight(1f)
            )
            InfoCard("RTP 输出", nz(st.packets) + " 包", "err " + nz(st.err) + " · " + nz(st.dest), Modifier.weight(1f))
        }
        if (outCh > 8 && (st.codec.isEmpty() || st.codec == "pcm")) {
            Text(
                "⚠ 布局 " + layout + " 需要 " + outCh + "ch，但实测 HAL PCM 上限只有 8ch：PCM 路径会丢声道或拒播；" +
                    "该布局只能作为 TrueHD/Atmos 对象渲染（ROAR）的目标，PCM 请选 2.0/5.1/7.1",
                color = Warn, fontSize = 12.sp, modifier = Modifier.padding(top = 10.dp)
            )
        }
        if (srcCh != null && srcCh != outCh && outCh <= 8) {
            Text(
                "⚠ 源 " + st.ch + "ch ≠ 布局 " + layout + "（" + outCh + "ch）：布局只描述通道顺序，不会补通道。" +
                    "请把布局改成与源一致，或在播放器里确认输出声道",
                color = Warn, fontSize = 12.sp, modifier = Modifier.padding(top = 10.dp)
            )
        }
        if (st.rate.isNotEmpty() && st.rate != "48000") {
            Text(
                "ℹ 界面显示源 " + fmtRate(st.rate) + "Hz：AES67 输出固定 48kHz，44.1k 源由守护 SRC（当前 C v5.3 尚未 SRC，Rust 守护已测通）",
                color = Dim, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp)
            )
        }
        Spacer(Modifier.height(18.dp))
        MeterPanel(st, Modifier.fillMaxWidth().height(320.dp))
        Spacer(Modifier.height(18.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.CenterVertically) {
            // 启动/停止都保持可点：脚本是幂等的，避免按钮被禁用后焦点丢失"点了没反应"
            TvButton(
                if (pending == "start") "启动中…" else "启动",
                enabled = pending.isEmpty()
            ) { pending = "start"; send("ENABLE") }
            TvButton(
                if (pending == "stop") "停止中…" else "停止",
                enabled = pending.isEmpty()
            ) { pending = "stop"; send("DISABLE") }
            TvButton(if (showSettings) "收起设置" else "设置") { showSettings = !showSettings }
            TvButton("刷新") {
                st = readStatus(); stateText = readState(); conf = readConf(ctx)
            }
            TvButton("回滚原厂 HAL") { pending = "rollback"; send("ROLLBACK") }
        }
        if (showSettings) {
            SettingsPanel(
                onClose = { showSettings = false },
                onApply = { m ->
                    writeConf(m)
                    if (m["rollback"] == "1") send("ROLLBACK") else send("APPLY")
                    showSettings = false
                }
            )
        }
        val confHalBuf = conf["hal_buf"] ?: "20"
        val expectMode = if (confHalBuf == "5") "lowlat" else "normal"
        if (st.hasData && st.mode.isNotEmpty() && st.mode != expectMode) {
            Text(
                "⚠ 延迟模式配置为「" + (if (confHalBuf == "5") "5ms 低延迟" else "20ms 标准") +
                    "」，但实际运行为 " + st.mode + "：点设置→应用设置，脚本会按 hal_buf 重写 mode.txt 并重启守护",
                color = Warn, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp)
            )
        }
        if (stateText == "running" && !st.hasData) {
            Text(
                "⚠ 状态文件是 running，但 status.json 缺失或已过期（" +
                    (if (st.ageMs >= 0) "上次更新 " + (st.ageMs / 1000) + "s 前" else "文件不存在") +
                    "）：守护可能没起来，按「启动」或看 /data/aes67bridge/logs/daemon.log",
                color = Warn, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp)
            )
        }
        Spacer(Modifier.height(10.dp))
        Text(
            "state.txt=" + (if (stateText.isEmpty()) "-" else stateText) +
                " · status.json " + (if (st.ageMs >= 0) (st.ageMs / 1000.0).toString().take(5) + "s 前" else "-") +
                " · 输出 " + nz(st.dest),
            color = Dim, fontSize = 12.sp
        )
    }
}

private fun nz(s: String) = if (s.isEmpty()) "-" else s

private fun fmtRate(r: String): String = when (r) {
    "44100" -> "44.1k"
    "48000" -> "48k"
    "96000" -> "96k"
    "88200" -> "88.2k"
    "176400" -> "176.4k"
    "192000" -> "192k"
    else -> if (r.isEmpty()) "-" else r
}

private fun fmtValue(st: Status): String {
    if (!st.hasData) return "待机"
    val codec = if (st.codec.isEmpty()) "-" else st.codec.uppercase()
    return codec + " " + fmtRate(st.rate) + " / " + nz(st.ch) + "ch"
}

private fun readConf(ctx: android.content.Context): Map<String, String> {
    return try {
        val f = File(ctx.filesDir, "daemon.conf")
        if (!f.exists()) emptyMap()
        else f.readText().lineSequence()
            .mapNotNull { ln -> val i = ln.indexOf('='); if (i > 0) ln.substring(0, i).trim() to ln.substring(i + 1).trim() else null }
            .toMap()
    } catch (t: Throwable) {
        emptyMap()
    }
}

private fun layoutChannels(layout: String): Int = when (layout) {
    "2.0" -> 2
    "4.1.2" -> 7
    "5.1" -> 6
    "5.1.2" -> 8
    "7.1" -> 8
    "7.1.2" -> 10
    "5.1.4" -> 10
    "7.1.4" -> 12
    "9.1.6" -> 16
    else -> 2
}

@Composable
private fun Header(st: Status, running: Boolean, stateText: String, pending: String) {
    val (pillText, pillColor) = when {
        pending == "start" -> "◌ 启动中…" to Color(0xFF2C3E50)
        pending == "stop" -> "◌ 停止中…" to Color(0xFF2C3E50)
        pending == "rollback" -> "◌ 回滚中…" to Color(0xFF2C3E50)
        running -> "● 运行中" to Accent
        stateText == "stopped" -> "○ 已停止" to Color(0xFF4A5A6A)
        stateText == "rolledback" -> "○ 已回滚" to Color(0xFF4A5A6A)
        stateText == "running" -> "◌ 启动中…" to Color(0xFF2C3E50)
        else -> "○ 未知" to Color(0xFF4A5A6A)
    }
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.weight(1f)) {
            Text("AES67Bridge", color = Color(0xFFF2F7FC), fontSize = 40.sp)
            Text("Z9X 8K · HAL tee → AES67 · L24/PT97 · 48kHz", color = Dim, fontSize = 14.sp)
        }
        Pill(pillText, pillColor)
        Spacer(Modifier.width(14.dp))
        Pill(if (st.mode.isEmpty()) "-" else st.mode, Color(0xFF2C3E50))
    }
}

@Composable
private fun Pill(text: String, color: Color) {
    Box(
        Modifier
            .clip(RoundedCornerShape(50))
            .background(color)
            .padding(horizontal = 22.dp, vertical = 8.dp)
    ) {
        Text(text, color = Color.White, fontSize = 15.sp)
    }
}

@Composable
private fun InfoCard(title: String, value: String, sub: String, modifier: Modifier = Modifier) {
    Column(
        modifier
            .clip(RoundedCornerShape(18.dp))
            .background(Card)
            .padding(horizontal = 24.dp, vertical = 18.dp)
    ) {
        Text(title, color = Dim, fontSize = 13.sp)
        Spacer(Modifier.height(6.dp))
        Text(value, color = Color(0xFFEAF2FA), fontSize = 26.sp)
        Spacer(Modifier.height(4.dp))
        Text(sub, color = Dim, fontSize = 12.sp)
    }
}

@Composable
private fun MeterPanel(st: Status, modifier: Modifier = Modifier) {
    val levels = st.levels
    val names = channelLabels(st.layout, st.ch)
    Box(
        modifier
            .clip(RoundedCornerShape(20.dp))
            .background(Card)
    ) {
        Canvas(Modifier.fillMaxSize().padding(24.dp)) {
            val n = if (levels.isEmpty()) 2 else levels.size.coerceAtMost(names.size)
            val left = 56.dp.toPx()
            val top = 18.dp.toPx()
            val right = size.width
            val bottom = size.height - 30.dp.toPx()
            val gap = 14.dp.toPx()
            val bw = (right - left - gap * (n - 1)) / n
            val tickPaint = android.graphics.Paint().apply { color = 0xFF51667A.toInt(); textSize = 20f }
            for (db in 0 downTo -60 step 20) {
                val y = bottom - (bottom - top) * ((db + 60f) / 60f)
                drawLine(Color(0xFF26323F), Offset(left - 10.dp.toPx(), y), Offset(right, y), 1f)
                drawContext.canvas.nativeCanvas.drawText(db.toString(), 4.dp.toPx(), y + 7f, tickPaint)
            }
            val barPaint = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG)
            val labelPaint = android.graphics.Paint().apply { color = 0xFFC8D6E2.toInt(); textSize = 22f }
            for (i in 0 until n) {
                val x = left + i * (bw + gap)
                val db = levels.getOrElse(i) { -120f }
                val norm = ((db + 60f) / 60f).coerceIn(0f, 1f)
                val bh = (bottom - top) * norm
                barPaint.shader = android.graphics.LinearGradient(0f, bottom, 0f, top,
                    intArrayOf(0xFF2ECC9F.toInt(), 0xFFF2C14E.toInt(), 0xFFE05C5C.toInt()),
                    floatArrayOf(0f, 0.72f, 1f), android.graphics.Shader.TileMode.CLAMP)
                drawContext.canvas.nativeCanvas.drawRoundRect(
                    android.graphics.RectF(x, bottom - bh, x + bw, bottom), 8f, 8f, barPaint
                )
                val nm = names.getOrElse(i) { (i + 1).toString() }
                val tw = labelPaint.measureText(nm).toInt()
                drawContext.canvas.nativeCanvas.drawText(nm, x + bw / 2 - tw / 2, size.height - 6.dp.toPx(), labelPaint)
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Chip(text: String, selected: Boolean = false, accent: Boolean = false, onClick: () -> Unit) {
    var focused by remember { mutableStateOf(false) }
    val bg = when {
        accent -> Accent
        selected -> Color(0xFF1F3A34)
        else -> Color(0xFF18232E)
    }
    val fg = when {
        accent -> Color(0xFF04140E)
        selected -> Accent
        else -> Color(0xFFCBD8E4)
    }
    Box(
        Modifier
            .onFocusChanged { focused = it.isFocused }
            .focusable()
            .clip(RoundedCornerShape(50))
            .background(bg)
            .border(if (focused) 2.dp else 1.dp, if (focused) Accent else Color(0xFF2A3947), RoundedCornerShape(50))
            .clickable { onClick() }
            .padding(horizontal = 14.dp, vertical = 7.dp)
    ) {
        Text(text, color = fg, fontSize = 13.sp, maxLines = 1)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SettingsDialog(
    onClose: () -> Unit,
    onApply: (Map<String, String>) -> Unit
) {
    val cats = listOf("输出", "信号", "延迟/时钟", "系统")
    var cat by remember { mutableStateOf(0) }
    val vals = remember { mutableStateMapOf<String, String>() }
    fun get(k: String, d: String) = vals[k] ?: d
    Dialog(onDismissRequest = onClose) {
        Box(
            Modifier
                .fillMaxWidth(0.92f)
                .clip(RoundedCornerShape(22.dp))
                .background(Color(0xFF0B1219))
                .padding(horizontal = 24.dp, vertical = 18.dp)
        ) {
            Column {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("设置", color = Color(0xFFF2F7FC), fontSize = 28.sp)
                    Spacer(Modifier.width(16.dp))
                    Text("改完点「应用设置」，重启守护生效", color = Dim, fontSize = 12.sp)
                    Spacer(Modifier.weight(1f))
                    Chip("关闭") { onClose() }
                }
                Spacer(Modifier.height(12.dp))
                Row {
                    Column(Modifier.width(150.dp)) {
                        cats.forEachIndexed { i, name ->
                            Box(Modifier.padding(vertical = 3.dp)) {
                                Chip((if (cat == i) "● " else "○ ") + name, selected = cat == i) { cat = i }
                            }
                        }
                    }
                    Spacer(Modifier.width(18.dp))
                    Column(
                        Modifier
                            .weight(1f)
                            .height(400.dp)
                            .verticalScroll(rememberScrollState())
                    ) {
                        when (cat) {
                            0 -> {
                                SetRow("协议") {
                                    Chip("AES67", get("proto", "aes67") == "aes67") { vals["proto"] = "aes67" }
                                    Chip("RAVENNA(原型)", get("proto", "aes67") == "ravenna") { vals["proto"] = "ravenna" }
                                }
                                SetRow("组播") {
                                    Chip("A 239.69.67.1:5067", get("group", "239.69.67.1") == "239.69.67.1") { vals["group"] = "239.69.67.1"; vals["port"] = "5067" }
                                    Chip("B 239.69.68.1:5068", get("group", "239.69.67.1") == "239.69.68.1") { vals["group"] = "239.69.68.1"; vals["port"] = "5068" }
                                }
                                SetRow("ptime") {
                                    listOf(125, 250, 500, 1000).forEach { us ->
                                        Chip(us.toString() + "us", get("ptime_us", "1000") == us.toString()) { vals["ptime_us"] = us.toString() }
                                    }
                                }
                                SetRow("编码") {
                                    Chip("L24", get("codec", "pcm") == "pcm") { vals["codec"] = "pcm" }
                                    Chip("L16(待)", get("codec", "pcm") == "l16") { vals["codec"] = "l16" }
                                }
                                SetRow("声道") {
                                    listOf(2, 6, 8, 10, 12, 16).forEach { n ->
                                        Chip(n.toString(), get("maxch", "8") == n.toString()) { vals["maxch"] = n.toString() }
                                    }
                                }
                                SetRow("布局") {
                                    listOf("2.0", "5.1", "7.1", "5.1.4", "7.1.4", "9.1.6").forEach { l ->
                                        Chip(l, get("layout", "5.1") == l) { vals["layout"] = l }
                                    }
                                }
                                SetRow("分包") {
                                    Chip("自动", get("split", "1") == "1") { vals["split"] = "1" }
                                    Chip("no-split", get("split", "0") == "0") { vals["split"] = "0" }
                                }
                                SetRow("SAP") {
                                    Chip("开", get("sap", "1") == "1") { vals["sap"] = "1" }
                                    Chip("关", get("sap", "0") == "0") { vals["sap"] = "0" }
                                }
                            }
                            1 -> {
                                SetRow("PCM 导出") {
                                    Chip("开", get("pcm_export", "1") == "1") { vals["pcm_export"] = "1" }
                                    Chip("关", get("pcm_export", "0") == "0") { vals["pcm_export"] = "0" }
                                }
                                SetRow("码流导出") {
                                    Chip("开", get("bits_export", "1") == "1") { vals["bits_export"] = "1" }
                                    Chip("关", get("bits_export", "0") == "0") { vals["bits_export"] = "0" }
                                }
                                SetRow("输入增益") {
                                    listOf(-12, -6, 0, 6, 12).forEach { g ->
                                        Chip(if (g > 0) "+" + g else g.toString(), get("gain_db", "0") == g.toString()) { vals["gain_db"] = g.toString() }
                                    }
                                }
                            }
                            2 -> {
                                SetRow("HAL 缓冲") {
                                    Chip("20ms 标准", get("hal_buf", "20") == "20") { vals["hal_buf"] = "20" }
                                    Chip("5ms 低延迟", get("hal_buf", "5") == "5") { vals["hal_buf"] = "5" }
                                }
                                SetRow("FIFO") {
                                    listOf(8, 16, 32).forEach { k ->
                                        Chip(k.toString() + "KB", get("fifo_kb", "32") == k.toString()) { vals["fifo_kb"] = k.toString() }
                                    }
                                }
                                SetRow("PTP") {
                                    Chip("关(local)", get("ptp", "0") == "0") { vals["ptp"] = "0" }
                                    Chip("泰山派 GM", get("ptp", "1") == "1") { vals["ptp"] = "1" }
                                }
                                SetRow("ASRC") {
                                    Chip("关", get("asrc", "0") == "0") { vals["asrc"] = "0" }
                                    Chip("开(rubato)", get("asrc", "1") == "1") { vals["asrc"] = "1" }
                                }
                            }
                            else -> {
                                SetRow("开机自启") {
                                    Chip("开", get("boot", "1") == "1") { vals["boot"] = "1" }
                                    Chip("关", get("boot", "0") == "0") { vals["boot"] = "0" }
                                }
                                SetRow("守护") {
                                    Chip("重启守护") { onApply(vals.toMap() + ("restart" to "1")) }
                                }
                                SetRow("危险") {
                                    Chip("回滚原厂 HAL") { onApply(mapOf("rollback" to "1")) }
                                }
                                Text("AES67Bridge 0.1.0 · status.json 实时", color = Dim, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp))
                            }
                        }
                    }
                }
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Chip("应用设置", accent = true) { onApply(vals.toMap()) }
                    Chip("应用并重启守护") { onApply(vals.toMap() + ("restart" to "1")) }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SetRow(label: String, content: @Composable FlowRowScope.() -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(vertical = 5.dp)) {
        Text(label, color = Dim, fontSize = 13.sp, modifier = Modifier.width(88.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.weight(1f), content = content)
    }
}

@Composable
private fun TvButton(text: String, enabled: Boolean = true, onClick: () -> Unit) {
    Button(onClick = onClick, enabled = enabled) {
        TvText(text)
    }
}

private fun channelLabels(layout: String, ch: String): List<String> = when (layout) {
    "2.0" -> listOf("L", "R")
    "4.1.2" -> listOf("L", "R", "LFE", "Ls", "Rs", "Tfl", "Tfr")
    "5.1" -> listOf("L", "R", "C", "LFE", "Ls", "Rs")
    "5.1.2" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr")
    "7.1" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb")
    "7.1.2" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr")
    "5.1.4" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr")
    "7.1.4" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr")
    "9.1.6" -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr", "Tbl", "Tbr", "Tsl", "Tsr")
    else -> when (ch.toIntOrNull()) {
        2 -> listOf("L", "R")
        6 -> listOf("L", "R", "C", "LFE", "Ls", "Rs")
        7 -> listOf("L", "R", "LFE", "Ls", "Rs", "Tfl", "Tfr")
        8 -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb")
        10 -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr")
        12 -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr")
        16 -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "Lw", "Rw", "Tfl", "Tfr", "Tbl", "Tbr", "Tsl", "Tsr")
        else -> listOf("L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb")
    }
}

