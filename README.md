# AES67Bridge

Zidoo Z9X 8K → AES67 音频桥（研究项目，源码 only）。

- `app-tv/`：Android TV 控制台（Kotlin + Compose for TV，`dev.aes67bridge.tv`）：GUI 启动/停止、AES67 参数、布局/延迟/时钟设置、状态与电平。
- `daemon-rs/`：root 侧 Rust 守护（HAL tee bits → TrueHD/EAC3 解码 → ROAR 渲染 → AES67 组播，L24/PT97，支持 8+4 双流）。运行行为：
  - `decode=auto`（默认）是真正的双源路由器：同时探测 `live.bits`（压缩码流，优先）和 `live.pcm`（已解码 PCM），电影走 TrueHD/Atmos 对象渲染，音乐 PCM 按配置布局渲染（2.0/5.1/7.1 等经 ROAR；2.0 也可渲染到 5.1.2 目标布局，未上混的声道保持静音）；两源都安静 20 s 后释放会话回到"等待片源"；源类型切换时重建会话（约 1 s，含 PTP 单播回退时更长）。
  - 输出电平旋钮：`gain_db`（统一增益，作用在 limiter 前，负值留余量）与 `limiter_db`（ROAR 峰值限幅上限，默认 −1 dBFS）；两者都可在 GUI 设置里改，status 里能看到 `bits.out_peak`/`bits.limited`/`bits.dialnorm_db`。
  - `pt`（RTP 动态 Payload Type，默认随编码：L24=97 / L16=96）可在 GUI「输出」页设置；`sap=1`（默认开）向 224.2.127.254:9875 周期公告 SDP（AES67 发现），GUI 设置会写全配置，不会再丢键。
- `dolby-pipeline/`：TrueHD/EAC3 解析解码 + 对象渲染管线（vendored truehd/eac3/oamd/roar）。
- `device/haltee-proxy/`：vendor audio HAL 代理 `.so`（tmpfs overlay，不修改物理分区）。
- `device/boot-hook/`：开机自启钩子。本固件在 boot 时会把所有第三方 App 置 `stopped=true`（收不到 `BOOT_COMPLETED`），因此自启不使用 App 广播，而是把两条非持久属性写入 `/data/property/persistent_properties`，借固件自带的 root 命令桥（`zd_root_cmd.sh`）在每次开机以 root 执行 `start.sh`。安装：`device/boot-hook/install.ps1`。
- `device/daemon/`：早期 C 版 FIFO → SRC → L24 RTP 守护（legacy 引擎）。
- `app/`：早期 Java 控制台（`dev.aes67bridge.console`）。
- `ravenna-discovery/`：RAVENNA/mDNS 探索（暂停）。

构建见 [BUILD.md](BUILD.md)：daemon（cargo armv7）、HAL 代理（`device/haltee-proxy/build.ps1`，自动找 NDK）、TV App（Gradle，assets 里三个二进制由仓库源码构建或从设备拉取，不入库）。

运行时所有文件在设备单一目录 `/data/aes67bridge/`（daemon.conf、live.bits/live.pcm FIFO、aes67.sdp、status.json 等）；`aes67boot.sh` 的日志在 `/data/aes67bridge/logs/boot.log`。

## License

本项目采用 **GNU General Public License v3.0**（GPL-3.0）开源，全文见 [LICENSE](LICENSE)。

Vendored 第三方组件保留其原始许可证：

- `dolby-pipeline/vendor/truehd`、`dolby-pipeline/vendor/eac3`、`dolby-pipeline/vendor/oamd` — Apache-2.0
- `dolby-pipeline/vendor/roar` — BSD-3-Clause-Clear + Alliance for Open Media Patent License 1.0
- `daemon-rs/vendor/nsp_aes67` — Apache-2.0（来自 NativeSpatialPipe 的 AES67 模块，上游提交与本地补丁见 `PATCHES.md`，许可证全文见该目录 `LICENSE`/`NOTICE`）

以上均与 GPL-3.0 兼容。

仓库不包含 Zidoo 固件自带的专有二进制（Amlogic HAL `audio.primary.amlogic.so` 原厂库等）；构建 App 时从自己的设备 `adb pull`，见 [BUILD.md](BUILD.md)。
