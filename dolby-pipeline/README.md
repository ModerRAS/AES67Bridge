# dolby-pipeline

AES67Bridge 的 Dolby TrueHD/Atmos 对象管线（Rust lib + bins）。

**先读 [INTERFACE.md](INTERFACE.md)** —— 与 daemon-rs 的集成契约、输入/输出块、背压、失败降级、布局契约、DSP 边界。

管线：`live.bits → TrueHD ES（原样透传）→ truehdd 解码（p2 bed / p3 对象+OAMD）→ OAMD 属性
→ N03 契约 → ROAR 1.1（validated patch）对象渲染 → 布局交集（strict）→ 480 帧 planar f32 Block`。

- 桥内 **不做 DSP**（无 EQ/分频/延迟/房间校正/上混/下混/DRC/limiter）；只在 ROAR 渲染后、块提交前预留
  `BlockProcessor` 空链插入点（默认零开销、逐字节透明）。
- 布局：实际布局 = target ∩ source 通道标签；源多出的丢弃 + warning，目标缺的不补，不做下混；
  SDP/status 必须报实际布局。
- 第三方源码在 `vendor/`（truehd/eac3/oamd Apache-2.0；aom-roar BSD-3-Clause-Clear + AOM Patent
  License 1.0），来源见 `vendor/ORIGINS.json`。

## 构建 / 测试

```powershell
cargo test          # 39 unit + 5 真实码流集成测试（无 artifacts 时自动 skip）
cargo build --release
```

armv7（NDK r27.2）交叉编译见 INTERFACE.md §10；`cargo test` 覆盖：

- live.bits 边界/透传（含 bits_19 AU 头与计数器连续性）；
- TrueHD p2 8ch bed 与 ffmpeg **逐采样位精确**（bits_19 97,120×8 与完整 ref 576,680×8）；
- 40 样本 AU → 480 帧块对齐、对象数上限 128；
- OAMD 属性（pos/gain/size/ramp/bed_assignment）；
- N03 编解码往返；
- ROAR 渲染通道能量/左右定位/目标布局 512/714/916；
- 布局交集 3 个规格用例 + adapter 重排/丢弃；
- 空 BlockProcessor 链与 Identity 链输出逐字节相同。

## 离线工具

```powershell
cargo run --bin dump-es      -- <live.bits> [out.es]      # 原样透传 + AU 头/AU 数诊断
cargo run --bin decode-probe -- <file> [--wav out.wav]    # 解码统计（可渲染 7.1.4 WAV）
cargo run --bin render-probe -- <file> [7.1.4|5.1.2|9.1.6] # 端到端渲染/能量/耗时
```

产品仓库只引用本 crate（lib），不要复制 bench/scratch 代码。
