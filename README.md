# AES67Bridge

Zidoo Z9X 8K → AES67 音频桥（研究项目，源码 only）。

- `app-tv/`：Android TV 控制台（Kotlin + Compose for TV，`dev.aes67bridge.tv`）：GUI 启动/停止、AES67 参数、布局/延迟/时钟设置、状态与电平。
- `daemon-rs/`：root 侧 Rust 守护（HAL tee bits → TrueHD/EAC3 解码 → ROAR 渲染 → AES67 组播，L24/PT97，支持 8+4 双流）。
- `dolby-pipeline/`：TrueHD/EAC3 解析解码 + 对象渲染管线（vendored truehd/eac3/oamd/roar）。
- `device/haltee-proxy/`：vendor audio HAL 代理 `.so`（tmpfs overlay，不修改物理分区）。
- `device/boot-hook/`：开机自启钩子。本固件在 boot 时会把所有第三方 App 置 `stopped=true`（收不到 `BOOT_COMPLETED`），因此自启不使用 App 广播，而是把两条非持久属性写入 `/data/property/persistent_properties`，借固件自带的 root 命令桥（`zd_root_cmd.sh`）在每次开机以 root 执行 `start.sh`。安装：`device/boot-hook/install.ps1`。
- `device/daemon/`：早期 C 版 FIFO → SRC → L24 RTP 守护（legacy 引擎）。
- `app/`：早期 Java 控制台（`dev.aes67bridge.console`）。
- `ravenna-discovery/`：RAVENNA/mDNS 探索（暂停）。

构建见各目录 `build.ps1` / `cargo build`（依赖 Android SDK build-tools、NDK、rustup armv7 target；路径按本机调整）。

运行时所有文件在设备单一目录 `/data/aes67bridge/`（daemon.conf、live.bits/live.pcm FIFO、aes67.sdp、status.json 等）；`aes67boot.sh` 的日志在 `/data/aes67bridge/logs/boot.log`。
