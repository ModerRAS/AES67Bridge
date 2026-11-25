# dolby-pipeline（AES67Bridge Dolby 对象管线）集成契约

版本 0.1.0（2026-11-25）。目标读者：daemon-rs / app-tv / device 侧集成者。
本文件是 **桥（bridge）与 AES67 daemon 的接口契约**，也是"桥内 DSP 范围"的边界声明（自研 DSP = 无；渲染器自带 limiter 默认开，见 §5）。

---

## 0. 结论先行（集成者只需读这一段）

1. **输入**：daemon 持续读 `/data/haltee/live.bits`（TrueHD/AC3/E-AC3/DTS 原始字节流），把读到的字节**原样**交给 `dolby_pipeline::ObjectPipeline::push_bits(&[u8])`。不要剥 4B、不要找 IEC61937 前导、不要按 burst 切分（阶段 0 结论，见 `artifacts/aes67/roar-armv7-bench-2026-11-24.md` §4）。
2. **输出**：`push_bits` 返回 `Vec<dolby_pipeline::Block>`（每次 0..N 个）。每个 Block = **480 帧（48 kHz 下 10 ms）、planar f32、按 `Block::actual` 布局排列**。管线内部 bed 容器为 **16ch 9.1.6**（`BED_LAYOUT_916`：L R C LFE Ls Rs Lb Rb Lw Rw Tfl Tfr Tbl Tbr Tsl Tsr）；5.1/7.1/7.1.2/7.1.4 内容填入对应扬声器、其余静音，9.1.6 bed 的 Lw/Rw/Tsl/Tsr 不再被丢弃。daemon 只看到 `Block::actual`，看不到容器。固定实体：**解码在每个 major sync 读取 TrueHD `channel_meaning` 的 dialogue_norm，按 Dolby 参考 −31 dBFS 施加常数增益（code−31 dB；31 = 0 dB、27 = −4 dB；>31 的 6-bit 2ch 字段不猜）**，默认开、`--dialnorm 0` 关闭，见 §5。daemon 负责 L16/L24 打包、按 ≤8ch 拆流、RTP/SDP/status；**SDP 与 status 必须发布 `Block::actual` 的布局（标签/通道数/流数）**，不是 target。
3. **布局**：target = 启动时向后端声明的最大布局（daemon.conf `layout=`）；TrueHD 路径 actual = target（对象可渲染到任意目标扬声器，bed 容器 9.1.6 覆盖所有目标）；**PCM 路径**由 daemon 经 ROAR 渲染（channel-based element 真实源布局 + IAMF 下混，见 daemon 侧文档），SDP/status 报目标布局；未启用时仍为交集。source 多出的通道**丢弃 + warning，不做下混**；target 缺的通道**不补**。strict 是唯一实现；`layout_mode=upmix|downmix|auto` 目前**直接拒绝**（不是 warning 后静默启用）。
4. **桥内不做自研 DSP**：无 EQ/分频/延迟/房间校正/上混/下混/DRC。只做"对象→扬声器"的 ROAR 渲染、bed 16ch 9.1.6 容器→目标布局的 ROAR 映射、通道身份/布局交集。**例外（Dolby 标准做法）**：dialnorm 常数增益（渲染前施加、见 §5）与 ROAR 自带 look-ahead limiter（渲染后、−1 dBFS）。**唯一例外**：ROAR 渲染器自带的 **look-ahead peak limiter** 默认开启（−1 dBFS 上限、5 ms look-ahead、硬削波兜底），用于把渲染后超过满刻度的峰值限制住，避免 L24 转换削波；它是渲染器内部能力，不是桥自研 DSP，可通过 `RenderConfig::limiter=false` 关闭。预留 `BlockProcessor` 链（默认空、逐字节透明）仍作为将来 post-render per-channel 插入点。
5. **采样率**：管线固定 48 kHz（TrueHD 码流本身 48k；ROAR 侧 48k）。44.1k PCM 源由 daemon 在进桥前做 SRC（AES67 传输格式要求，不算音频调校 DSP）。
6. **失败降级**：解码错误/码流缺口 → 丢弃到下一个 major sync 继续；对象数 >128 → 该 AU 丢弃并计数；ROAR 渲染错误 → 返回 Err，daemon 重建管线（可选 bed-only 降级）；4.1.2 target → 启动期拒绝（ROAR validated patch 无 412 Layout）。
7. **实测**（Z9X armv7，release，单线程 解码+渲染+块组装）：`bits_20`（5.483 s Atmos）耗时 2.677 s ≈ **4.9 ms / 10 ms 块**；`bits_19` 2.023 s → 0.897 s。分线程（解码/渲染各一线程）后每核约 20–25%，10 ms 节拍余量 >2×。

---

## 1. 数据路径与线程模型（推荐）

```
live.bits FIFO (HAL tee, 2..8ch PCM 与压缩流并存；本管线只吃压缩流)
   │  read() 任意字节块（FIFO 字节流，无消息边界；可能丢块：32KB 非阻塞 FIFO 满即丢）
   ▼
ObjectPipeline::push_bits(bytes)                     [桥内]
   ├─ bits::EsNormalizer     分类 TrueHD/AC3/EAC3/DTS，字节原样透传
   ├─ decode::TrueHdDecoder  truehdd Extractor→Parser→Decoder
   │     · presentation 2 = 7.1 bed；presentation 3 = 对象通道 + OAMD
   │     · OAMD 每 ~38 AU 出现一次，属性向后续 AU 延续（已实现）
   │     · 40 samples/AU → decode::AuAssembler 累积成 480 帧
   ├─ oamd::from_oamd        对象属性（pos/gain/size/ramp/bed_assignment）
   └─ render::ObjectRenderer ROAR 对象渲染 → 目标布局 planar f32（ROAR speaker 顺序）
         └─ blocks::BlockPipeline: LayoutAdapter(strict) → Block(actual 布局) → BlockProcessor 链（默认空）
   ▼
Vec<Block>  → daemon：L16/L24 打包 → >8ch 标准多流拆分 → RTP/SDP/SAP/status
```

线程建议（性能已实测，见 §9）：
- **解码线程**：独占；`push_bits` + 组装 + ROAR 渲染可以都在此线程（当前实测 4.9 ms/块），也可把渲染拆到独立线程（两核各 ~25%）。**同一个 `ObjectRenderer` 绝不能被两个线程同时调用**（ROAR 不是 Sync）。
- **发送线程**：daemon 现有 pacer；Block 进 ring（建议 2–4 块 = 20–40 ms 抖动缓冲）。
- Block 的产生与消费必须保持顺序；`Block::seq` 单调递增，`Block::sample_pos = seq*480`（48 kHz 样本）。

---

## 2. 输入契约

### 2.1 live.bits（TrueHD / AC3 / E-AC3 / DTS）

- 路径：`/data/haltee/live.bits`（FIFO），元数据 `/data/haltee/live.bits.meta`（`fmt`/`codec_guess`/`first_bytes`/`bytes`/`dropped`）。daemon 用 meta 判断"当前是不是压缩流、是什么编码"，但**不要用它切分字节**。
- **不要做任何封装处理**：阶段 0 已钉死——TrueHD 的 4 字节是 access-unit 头（`check(4) | len_words(12) | input_timing(16)`，AU 字节数 = len_words×2），是码流语法的一部分；按"每 burst 剥 4B"会把 27,948 AU 的流变成 12 AU / 70 个 extract error（反证见阶段 0 报告 §4.1）。
- FIFO 读取起点可能在 AU 中间、也可能因为 FIFO 满而**丢块**（设备日志出现过 `bits_dropped=13,025,280` 的记录）。桥内处理：truehdd Extractor 自行 resync（按 major sync 0xF8726FBA/FBB，最坏 ~128–222 AU ≈ 107–185 ms 音频被跳过），解码器在首个 major sync 前丢弃 AU（presentation map 需要）。**daemon 不要尝试补偿缺口；把 DecodeStats 的计数报进 status 即可。**
- 48 kHz：TrueHD 48k 原生；其他压缩流（如 44.1k AC3）本管线不承诺解码路径（E-AC3 由 rustd/daemon 侧解，见 §2.2）。

### 2.2 E-AC-3 / DD+ / 其他已解 PCM（rustd 侧）

产品口径：**E-AC-3（含 JOC/Atmos）由 daemon/rustd 解**，桥只提供"对象/bed → ROAR"的输入契约。两条接法任选：

1. **bed-only PCM（最低要求）**：rustd 解出的 5.1/7.1 PCM 直接按 §4 的 Block 契约输出；布局交集仍由 daemon（或复用 `blocks::BlockPipeline`）处理。若需要过 ROAR（例如目标布局的高度声道），把 PCM 填成 `decode::DecodedBlock { bed, objects: vec![], metadata: None, object_mode: false, p2_channels: 6|8, .. }`，再调 `render::ObjectRenderer::render_block` + `blocks::BlockPipeline::build`。
2. **对象（JOC）路径**：rustd 解出 JOC 对象后，按 §3 的 **N03 契约**（32B 属性 + 单声道 PCM，480 帧）交给桥：`n03::N03Block::parse` → `ObjectRenderer::render_block_from_n03`（预留，当前可直接把 N03 记录转成 `DecodedBlock`：bed typ 2..4096 填 12ch bed，typ=1 填 objects，属性放 `ObjectMetadata`）。**JOC 样本目前缺失，未做端到端验证**（见 §11）。

### 2.3 N03 契约（桥内部解耦契约，也是将来扩展 ABI）

线格式（与 NSP Croksor 相同）：`count * (32B property + frames*4B f32 单声道 PCM)`，little-endian。

| 偏移 | 字段 | 说明 |
|---|---|---|
| 0 | ident u32 | 对象/通道稳定 id |
| 4 | frames u32 | 本块帧数（=480） |
| 8 | typ u32 | 1 = 动态对象；2,4,8,16,32,64,128,256,512,1024,2048,4096 = 12 个 bed 通道（ROAR Layout714 顺序 FL,FR,C,FLfe,SL,SR,BL,BR,TFL,TFR,TBL,TBR） |
| 12 | flags u32 | bit0=create/reset，bit2=position update（沿用 NSP 语义） |
| 16 | xyz f32×3 | ADM 对象坐标：x=右, y=上, z=后（渲染器映射到 ROAR Cartesian `[x,-z,y]` 并归一化到单位球） |
| 28 | gain f32 | 线性增益（1.0=unity） |

- 坐标转换：OAMD/DAMF 输出 `[right, front, up]` → N03/ADM 为 `[x, z, -y]`（`oamd::from_oamd` 已实现）。
- N03 不是对外稳定 ABI；若将来要与非 Rust 组件交换，再冻结为 C ABI。

---

## 3. 输出块契约（daemon 集成面）

`rust
pub struct Block {
    pub seq: u64,          // 单调递增
    pub sample_pos: u64,   // 块起始绝对样本位置（48 kHz），= seq * 480
    pub rate: u32,         // 48000
    pub frames: usize,     // 480
    pub target: LayoutSpec,// 启动声明的最大布局（daemon.conf layout=）
    pub source: LayoutSpec,// 内容布局（对象内容 = target；bed 内容 = 5.1/7.1）
    pub actual: LayoutSpec,// 实际发布布局（SDP/status 用这个！）
    pub object_count: usize,
    pub channels: Vec<Vec<f32>>, // planar，长度 = actual.len()，每通道 480 个 f32
}
`

- **SDP/status 规则**：
  - `sdp a=comment:channel-labels=` 与 `status.layout`/`stream.ch`/`flows` 一律由 `Block::actual` 派生；
  - 拆流沿用 daemon 现有规则：actual ≤8ch 单流；>8ch 标准多流每流 ≤8ch（例如 7.1.4 → 8+4、9.1.6 → 8+8）；不再按 target 的通道数报流。
  - 若 daemon.conf 的 `layout=` 与桥启动 target 不一致：**以桥的 target 为准**并在 status 记 WARN（桥启动时会校验 target 必须是 daemon 支持的 9 档之一）。
- 通道顺序 = `Block::actual.channels()` 的 canonical 顺序（= daemon 现有 layout 标签顺序）。不要二次重排。
- 采样格式：planar f32；daemon 负责转 L24（×2^23 后取整）或 L16。
- 时间戳：`sample_pos` 是音频时间轴；RTP 时间戳由 daemon 按 pacer/PTP 生成（桥不产出网络时间戳）。
- 背压：`push_bits` 同步返回 Block；桥没有内部无限缓冲。daemon 若处理不过来，应**停止/放慢读 FIFO**（HAL 侧 32KB FIFO 满即丢，属既有设计），并把 `DecodeStats` 计数写入 status。桥**不会**阻塞等待网络。

---

## 4. 布局契约（strict，唯一实现）

- target：启动时向后台声明的最大布局，9 档：`2.0/4.1.2/5.1/5.1.2/5.1.4/7.1/7.1.2/7.1.4/9.1.6`。
  - ROAR validated patch 没有 4.1.2 Layout → **`target=4.1.2` 在桥启动时返回 Unsupported**，daemon 应拒绝启动该 layout（不要静默换布局）。
- source：内容布局。规则：
  - 有对象（`object_mode && object_count>0`）→ source = target（对象可渲染到目标的所有扬声器）；
  - 否则 bed-only → source = 7.1（p2 8ch）或 5.1（6ch）。
- actual = target ∩ source（按通道身份，不是按位置索引）：
  - target 5.1.2 + source 5.1 → actual 5.1（Tfl/Tfr 不补）；
  - target 7.1.2 + source 7.1 → actual 7.1；
  - target 7.1.2 + source 5.1 → actual 5.1；
  - target 5.1 + source 7.1.4 → actual 5.1，source 的 Lb/Rb/Tfl/Tfr/Tbl/Tbr **丢弃 + warning**（**不做下混**）。
- 交集至少 3 个用例已固化在 `blocks.rs` 测试（`intersection_512_target_with_51_source_is_51` 等），另有 adapter 重排/丢弃/Identity 链透明性测试。
- **上混/下混（用户要求留接口，现在不实现）**：
  - `layout::UpmixAdapter` / `layout::DownmixAdapter` 是 trait 占位（只有签名与文档）；
  - 配置键 `layout_mode = strict|upmix|downmix|auto`，默认 strict；`LayoutMode::from_config` 对非 strict **返回 Unsupported 错误**（不静默启用）；
  - 将来加算法时**不得改变 strict 行为**（strict 的逐字节输出已被测试锁定）。
- 4.1.2 的将来路径（不在本版）：需要给 ROAR 打 4.1.2 Layout 增量（scratch worktree 那 +26 行，未验证）并重新走阶段 0 基准；当前明确不支持。

---

## 5. DSP 边界（桥内自研 DSP = 无；渲染器自带 limiter 默认开）

- **桥内不实现、不启用任何自研 DSP**：EQ、分频、延迟、房间校正、上混/下混、DRC/dialnorm 不做。这些属于"靠近音箱端/后级"的处理。
- **dialnorm（2026-11-25 决策：做）**：解码器在每个 major sync 读取 FBA `channel_meaning` 的 `twoch/sixch/eightch/sixteench_dialogue_norm`；按 Dolby 参考电平 **−31 dBFS** 换算常数增益 `code − 31 dB`（31 = 参考 = 0 dB，27 = −4 dB；code 0 = 未指定、>31 的 6-bit 2ch 字段不猜测，均按 0 dB），在**渲染前**施加到 bed 与对象 PCM（limiter 仍在最后）。`--dialnorm 0` / `daemon.conf dialnorm=0` 关闭。实测真实 Dolby Atmos 片源 code=31 → 0.00 dB（对参考母版是 no-op）。
- **例外（2026-11-25 决策）**：ROAR 渲染器自带的 **look-ahead peak limiter** 默认开启，上限 −1 dBFS、look-ahead 5 ms、attack 1 ms、release 200 ms、硬削波兜底（AOM 实现，实时零分配）。理由：ROAR 渲染输出的峰值实测可达 1.13–1.46（>0 dBFS），直接定标到 L24 会削波；limiter 把峰值限制在 −1 dBFS 以内。关闭方式：`RenderConfig::limiter = false`。启用后输出有 5 ms（240 样本）延迟，末块尾部由 daemon 丢帧即可（连续流不影响）。
- 唯一的**自研**插入点（预留给将来，**默认空链**）：

`rust
pub trait BlockProcessor {
    fn process(&mut self, block: &mut Block) -> Result<()>;
}
`"`
- 位置：**ROAR 渲染之后、LayoutAdapter 之后（拿到 actual 布局）、Block 提交 AES67 之前**；按输出通道处理。
- 默认 `BlockChain` 为空（`BlockChain::new()`），`"`IdentityProcessor` 仅用于证明透明性：测试 `empty_chain_and_identity_chain_are_byte_identical` 断言空链与 Identity 链（含双 Identity）输出逐字节相同。
- 约束（将来实现 DSP 时必须遵守）：不得改变 `actual`/`frames`/`seq`/`sample_pos`/通道数；必须是实时安全（无堆分配、无阻塞）；处理顺序 = 链顺序；失败返回 Err 由 daemon 决定降级（当前 daemon 应把 Err 视为该块静音/重建，不得 panic）。
- 集成方式二选一：**（推荐）Rust trait 直接接入 daemon 的构建**；或将来把处理器编译为 C ABI 插件。本版只提供 Rust trait + 文档，**不实现具体 DSP**。

---

## 6. 失败与降级矩阵

| 场景 | 桥行为 | daemon 动作 |
|---|---|---|
| live.bits 缺口/丢块/读取起点非 AU 边界 | truehdd resync 到下一个 major sync；丢弃不完整 AU；`DecodeStats.extract_errors/aus_skipped_pre_sync` 计数；不 panic | 继续读；把计数写 status；缺口期间 ring 补零（daemon 既有欠载逻辑） |
| TrueHD parse/decode 错误 | 丢该 AU，`reset_for_next_major_sync` 后继续；计数 | 同上；若连续错误多，重建 `ObjectPipeline` |
| 对象数 >128 | 该 AU 返回 Decode 错误（丢弃+resync）；`MAX_OBJECTS=128` 是 ROAR batch 硬上限 | 计数；必要时降级 bed-only |
| ROAR 渲染错误（OarError） | `push_bits` 返回 Err（该块及此后状态不可信） | 丢弃当前管线对象并重建；重建期间输出静音块；连续 N 次失败 → bed-only 降级（`TrueHdDecoder::bed_only()`）+ 直接按 bed 布局输出，不做对象渲染 |
| target 布局不支持（4.1.2/未知） | 启动期 Err(Unsupported) | 拒绝启动该 layout，GUI 报错；不要静默改布局 |
| 非 48 kHz 源 | 桥只跑 48k；TrueHD 码流恒 48k；PCM 源由 daemon 先 SRC | daemon 侧保持现有 44.1→48 SRC |
| FIFO 长时间无数据（暂停/切歌） | 无 Block 输出（内部 40→480 累积器可能残留 <480 样本，重建管线时丢弃） | 按 daemon 现有静音/停发逻辑；切流时重置 ObjectPipeline |
| 采样时钟漂移（无 PTP 硬件时间戳） | 与本桥无关（桥只出块） | daemon 现有 ring/prebuffer 策略 |

**status 建议字段**（daemon 现有 status.json 增补）：`actual_layout`、`object_count`、`decode_aus`、`decode_errors`、`extract_errors`、`rendered_blocks`、`dropped_samples`。

---

## 7. 启动/停止

- 启动：daemon 校验 `layout=` → 支持则 `ObjectPipeline::new(layout)`；4.1.2/未知 → 启动失败并明确报错。
- 运行：每读到一段 live.bits 字节就 `push_bits`；把返回的 Block 按序送 ring。
- 切流/暂停/恢复/换片：**丢弃旧 ObjectPipeline、新建一个**（解码器状态与 FIFO 字节流不连续，重建是最安全的；重建耗时 <10 ms）。SDP/status 跟随 actual 变化更新。
- 停止：drop pipeline（RAII 释放 ROAR）。

---

## 8. 模块/API 索引

| 模块 | 作用 | 主要 API |
|---|---|---|
| `bits.rs` | live.bits 分类 + 原样透传；TrueHD AU 头诊断 | `EsNormalizer::push`, `detect_codec`, `AuIter`, `TrueHdAuHeader::parse` |
| `decode.rs` | vendored truehdd 解码 → bed+objects；40→480 累积；eac3 帧扫描（daemon 侧 PCM 解） | `TrueHdDecoder::{new, bed_only, push_es, stats}`, `AuAssembler`, `DecodedBlock`, `DecodeStats`, `eac3_scan` |
| `oamd.rs` | OAMD → 对象属性（pos/gain/size/ramp/bed_assignment） | `from_oamd`, `ObjectMetadata`, `ObjectAttr`, `gain_db_to_linear` |
| `n03.rs` | 32B 属性 + 单声道 PCM 契约 | `N03Record`, `N03Block::{encode, parse}`, `BED_TYPES`, `BED_LAYOUT_714` |
| `render.rs` | ROAR 静态链接渲染（7.1.4/5.1.2/9.1.6/5.1/7.1/5.1.4/7.1.2/7.1.6/9.1.4/2.0） | `ObjectRenderer::{new, set_object_count, render_block, roar_layout}` |
| `blocks.rs` | 480 帧 Block + LayoutAdapter + BlockProcessor 链 + ObjectPipeline | `Block`, `BlockPipeline`, `BlockProcessor`, `IdentityProcessor`, `ObjectPipeline::{new, push_bits, render_decoded, processors_mut}` |
| `layout.rs` | 通道身份/布局/交集/adapter/mode | `LayoutSpec`, `ChannelId`, `intersect`, `IntersectionAdapter`, `LayoutMode` |

二进制（PC/离线用，产品 daemon 直接 link lib）：
- `dump-es`：live.bits → 原样 ES + AU 头/AU 数/计数器连续性诊断；
- `decode-probe`：ES 解码统计（可选 `--wav` 渲染成 7.1.4 f32 WAV）；
- `render-probe`：解码+渲染到指定 target，打印每块布局/对象数/通道能量与整段耗时。

---

## 9. 性能与资源（实测）

| 环境 | 场景 | 结果 |
|---|---|---|
| Z9X armv7（A55，release） | ROAR 12 objects→7.1.4（阶段 0 bench，含 12ch bed、每块位置更新） | p99 1.9–2.1 ms / 10 ms（19–21%），0/600 超 10ms |
| Z9X armv7（release，单线程 解码+渲染+块） | bits_20 5.483 s Atmos（548 块） | 2.677 s ≈ **4.9 ms/块** |
| Z9X armv7（release，单线程） | bits_19 2.023 s（202 块） | 0.897 s ≈ 4.4 ms/块 |
| Z9X armv7 | bits_20 → 9.1.6 | 2.946 s ≈ 5.4 ms/块 |
| x86_64 host | 同源码 | 快 ~3–5×（仅趋势参考） |
| 内存 | 单管线（ROAR + 解码器） | 粗略 <100 MB（未做峰值 RSS 精测；32-bit 设备需在整机 soak 时确认） |

线程建议见 §1；解码与渲染分线程后每核 20–25%，整机 10 ms 节拍余量 >2×。

---

## 10. 构建

```powershell
# host（x86_64，cargo test 全绿）
Set-Location projects\aes67-bridge\dolby-pipeline
cargo test

# armv7（NDK r27.2；不写入 .cargo/config，用环境变量）
$ndk='C:\Users\ModerRAS\AppData\Local\Android\Sdk\ndk\27.2.12479018\toolchains\llvm\prebuilt\windows-x86_64'
$env:CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER = "$ndk\bin\clang.exe"
$env:RUSTFLAGS = "-C link-arg=--target=armv7a-linux-androideabi30 -C link-arg=--sysroot=$ndk\sysroot"
cargo build --release --target armv7-linux-androideabi   # 产出 3 个 armv7 bin + rlib（可被 daemon 静态链接）
```

- 依赖全部 vendored 在 `vendor/`（truehd/eac3/oamd Apache-2.0；aom-roar = BSD-3-Clause-Clear + AOM Patent License 1.0，validated patch 为 NSP 增补）。来源与 license 记录在 `vendor/ORIGINS.json`（只记录，不做法务判断）。
- 产品基线 **只用** `roar-1.1-validated.patch`；scratch worktree 的额外 26 行（Layout412/NspBed 4.1.2 实验）未包含。

---

## 11. 已知缺口 / 不在本版

1. **4.1.2 target 不支持**（ROAR validated patch 无 412 Layout）；需要时按阶段 0 方式重新验证 + 重新打 patch。
2. **E-AC-3/JOC 对象路径未端到端验证**（缺 JOC 样本）；bed PCM 契约已可用（§2.2）；PCM 解码本身在 rustd 侧。
3. OAMD 的逐 AU 精细 ramp/位置插值：当前在 480 帧块粒度取最近一次 OAMD 属性；ramp_samples 已解析但未做块内样本级 ramp（WAV 验证与听感未发现需要；若需要可后续在 oamd/render 增加）。
4. DRC/dialnorm 不应用（产品口径：桥内无 DSP）；节目响度依赖内容/后级。
5. 对象数上限 128（ROAR batch 硬上限）；>128 的 OAMD 流按错误处理（计数+resync），不做截断渲染。
6. 峰值内存未精测；实时线程优先级/绑核需在整机 daemon 里落地（桥内不做线程策略）。
7. live.bits 首 4B 以外的 AU 头高位语义（check 位）未完全解码——不影响解码/渲染。
