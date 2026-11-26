# AES67Bridge

Zidoo Z9X 8K → AES67 音频桥（研究项目，源码 only）。

- 采样率转换：默认 `resampler=rubato`（sinc，44.1k→48k live 路径 2026-11-26 实测干净：重采样器输出与线上 RTP 载荷音调 SNR ≈200 dB）；`resampler=c-poly`（C daemon 的 160 相 × 287 抽头 Q30 多相 FIR）保留做 A/B。
- `app-tv/`：Android TV 控制台（Kotlin + Compose for TV，`dev.aes67bridge.tv`）：GUI 启动/停止、AES67 参数、布局/延迟/时钟设置、状态与电平。
- `daemon-rs/`：root 侧 Rust 守护（HAL tee bits → TrueHD/EAC3 解码 → ROAR 渲染 → AES67 组播，L24/PT97，支持 8+4 双流）。运行行为：
  - `decode=auto`（默认）是真正的双源路由器：同时探测 `live.bits`（压缩码流，优先）和 `live.pcm`（已解码 PCM），电影走 TrueHD/Atmos 对象渲染，音乐 PCM 按配置布局渲染（2.0/5.1/7.1 等经 ROAR；2.0 也可渲染到 5.1.2 目标布局，未上混的声道保持静音）；两源都安静 20 s 后释放会话回到"等待片源"；源类型切换时重建会话（约 1 s，含 PTP 单播回退时更长）。
  - 输出电平旋钮：`gain_db`（统一增益，作用在 limiter 前，负值留余量）与 `limiter_db`（ROAR 峰值限幅上限，默认 −1 dBFS）；两者都可在 GUI 设置里改，status 里能看到 `bits.out_peak`/`bits.limited`/`bits.dialnorm_db`。
  - `pt`（RTP 动态 Payload Type，默认随编码：L24=97 / L16=96）可在 GUI「输出」页设置；`sap=1`（默认开）向 224.2.127.254:9875 周期公告 SDP（AES67 发现），GUI 设置会写全配置，不会再丢键。
  - **0.3.0**：clean-runtime 重构——运行时目录 `/data/aes67bridge` 改挂 tmpfs（重启即清、开机从 App 私有目录重铺），持久层收进 App 私有目录 `/data/user/0/dev.aes67bridge.tv/files`，boot hook 移到 FBE 安全的 `/data/aes67bridge.persist/boot`。设置页新增「装机」：一键安装/修复开机自启、一键开启/关闭无线 ADB；补齐启动自愈（运行时未 running 时打开 App 自动重铺）。
  - **0.2.9**：修复热更新时直接覆盖 HAL 正在 mmap 的 `.so` 会让 HAL 进程 SIGBUS/SIGSEGV 崩溃的问题——改为临时文件 + rename 原子替换；内容不变时完全不碰文件，只有内容变化才重启 HAL。
- **0.2.8**：应用内热更新修复——HAL 代理升级时改为通过 overlay 的合并路径发布，内容变化才重启 HAL（此前直接写 upper 在被挂载的 overlay 下不会刷新合并视图，升级后要重启设备才生效）。
- **0.2.7**：修复多声道目标布局下偶发的爆音/白噪声（HAL tee FIFO 整帧原子写 + 换流边界排空/重开 + daemon 侧排空/重同步）；live.pcm 新增系统混音回退，第三方应用（ExoPlayer/Kodi/浏览器等）的 PCM 声音也能进 AES67（DIRECT 优先，可关）。
- **SMPTE ST 2110-30 对齐（0.2.6）**：PTP profile 可选 `aes67`（media profile，domain 0）或 `st2059-2`（ST 2059-2，默认 domain 127，announce 1 s / sync 125 ms 参数，支持 one-step 与 two-step Sync）；SDP 的 `clock-domain`/`ts-refclk:ptp=IEEE1588-2008:<GMID>:<domain>`/`mediaclk:direct=0` 自动跟随；多声道 `channel-order` 按 SMPTE2110 约定生成：2.0→(ST)、4.1.2→(ST,U05)、5.1→(51)、5.1.2→(51,U02)、7.1→(71)、7.1.2→(71,U02)、5.1.4→(51,U04)、7.1.4→(71,U04)、9.1.6→(71,U08)。**正式支持的水平是 1 ms（Level A）**；125 µs（Level B）实测在本机需自旋占一个核且仍有约 0.3% 丢包，仅作实验开关，不用于生产（设置页标注「实验」）。
- `dolby-pipeline/`：TrueHD/EAC3 解析解码 + 对象渲染管线（vendored truehd/eac3/oamd/roar）。
- `device/haltee-proxy/`：vendor audio HAL 代理 `.so`（tmpfs overlay，不修改物理分区）。**FIFO 帧对齐契约**：代理只按整帧写 `live.pcm`（每块 ≤PIPE_BUF 的原子写），FIFO 满时只丢整帧；换播放流时先排空 FIFO 里的旧字节、关闭再重开 writer（daemon 看到 EOF 丢弃半帧），并且先更新 `live.meta` 再写新流第一个字节。daemon 侧在会话/格式重建时主动排空 FIFO、20 ms 轮询 meta、数据断档 100 ms 后丢弃半帧。两侧合起来保证"帧长切换/丢帧"后字节流不会错位——错位会让 24-bit PCM 被当成移位后的字节解码，听感就是持续爆音/白噪声。另外 `live.pcm` 默认优先导出 DIRECT 流（官方播放器/位精确），DIRECT 静默 ≥200 ms 时回退到 PRIMARY 系统混音输出，因此第三方应用（ExoPlayer/Kodi/浏览器等）的 PCM 声音也会进 AES67——代价是混音流包含系统提示音、最多立体声、并且跟随 Android 系统音量（不再是位精确）；创建空文件 `/data/aes67bridge/live.mixer.disable` 可关闭回退、恢复只抓 DIRECT。
- `device/boot-hook/`：开机自启钩子（0.3.0 起安装在 `/data/aes67bridge.persist/boot`）。本固件 boot 时把所有第三方 App 置 `stopped=true`（收不到 `BOOT_COMPLETED`），因此自启走厂商 root 属性桥：`persistent_properties` 中预置 `zidoo.root.cmd2=/data/aes67bridge/boot/trigger.sh`，开机由 `zd_root_cmd.sh` 以 root 执行 trigger → `aes67boot.sh`（重建 ext4 trigger、挂 tmpfs、等 FBE 解锁、执行 App 私有目录的 `start.sh`）。App 设置页「装机」可在不需要 ADB 的情况下安装/修复；`device/boot-hook/install.ps1` 保留给开发/批量部署。
- `ravenna-discovery/`：RAVENNA/mDNS 探索（暂停）。

构建见 [BUILD.md](BUILD.md)：daemon（cargo armv7）、HAL 代理（`device/haltee-proxy/build.ps1`，自动找 NDK）、TV App（Gradle；代理与 daemon 由仓库源码构建，原厂 HAL 备份由设备自行 provision，不入库）。CI/CD 见 `.github/workflows/`：push 构建测试，tag `v*` 自动发布 Release（APK + daemon + SHA256），TV App 内置自动更新（读 `release/latest.json`）。

运行时分三层（0.3.0+）：

- **持久层**：App 私有目录 `/data/user/0/dev.aes67bridge.tv/files` —— 脚本/tee 资产、`daemon.conf`（GUI 唯一真源）、`aes67-daemon`、`tools/`、`media/`；
- **FBE 安全的小持久目录**：`/data/aes67bridge.persist/boot/` —— 开机钩子（trigger / aes67boot.sh / 属性片段）与启动日志，解锁前也能执行；
- **运行层**：`/data/aes67bridge`（启用后是 tmpfs，`size=64m`）—— `live.pcm`/`live.bits`、meta、SDP、`status.json`、`logs/`、`stage/`、`ovl/`、daemon 运行副本；重启即清空，开机由 App 私有目录重新铺。未启用时保持 0.2.x 的 ext4 布局，可平滑升级/回退。

## 给其他 Z9X 8K 用户安装（0.3.0+）

> **安全声明（重要）**：本项目的安装与自启依赖芝杜原厂固件自带的 root 属性后门（`zidoo_daemon` 每秒轮询 `zidoo.root.cmd*` 并以 root 执行命令）；App 内「开启无线 ADB」还会打开厂商 ADB 门闩（`persist.zidoo.test.dc/dc1`）并重启 adbd。也就是说：**点这个按钮等于把设备变成可被局域网内 root 控制的机器**；再叠加本机默认开放的 `:18888`（无鉴权装 APK）、`:9529`、`:139/445`、`:5555`，只应使用在你自己的受控设备与可信网络中。不用时请关闭 ADB，或在路由器上隔离这些端口。本项目不刷机、不改 `/system`、不碰 OTA 验签。

1. **装 APK**（任选）：芝杜 Web Install（`http://<盒子IP>:18888/` 上传 APK）／U 盘 + 文件管理器／`adb install -r AES67Bridge-x.y.z.apk`。
2. **装开机自启**（不需要 ADB）：打开 App → 设置 → **装机** → 「安装/修复开机自启」。它会把启动钩子写进 `/data/aes67bridge.persist/boot`，并把运行时目录切到内存盘（重启后生效）；App 私有目录是唯一持久层。
3. **（可选，仅开发者）开启无线 ADB**：同一页面点「开启（会 root）」并确认。之后在电脑上 `adb connect <盒子IP>:5555`、`adb root`，`adb shell id` 应为 `uid=0`。若固件要求主机授权，可把电脑 `%USERPROFILE%\.android\adbkey.pub` 的一行内容放进 App 私有目录的 `adb_pubkey.txt` 后再点一次；不用时在同一页面关闭。
4. **重启盒子**验证：`/data/aes67bridge` 已挂 tmpfs、daemon 已启动、AES67 有流；「装机」页可查看最近一次操作结果（`filesDir/tool-result.txt`）。

**ADB 门闩（offline 根因）**：原厂 adbd 被 patch，`persist.zidoo.test.dc`/`dc1` 默认 0 时 5555 只 LISTEN、不回握手（表现为 adb offline）。App 走原厂 root 桥把它置 1。另有官方隐藏开关：系统设置某条目 5 秒内连按 MENU（键码 82）6 次（反编译 `com.android.quick.settings`）；本项目不依赖该隐藏开关，仅作备用说明。

**升级 / 回滚**：App 内更新保留 filesDir 持久层；回滚原厂 HAL 用设置页「回滚原厂 HAL」或 `rollback.sh`。0.2.x 升 0.3.0 时「安装自启」会自动把旧运行时的 daemon/tools/media 迁移进 App 私有目录；旧 ext4 目录保留（卸掉 tmpfs 即可见），不影响回退。

## 发布与自动更新

- **CI**（`.github/workflows/ci.yml`）：push/PR 时跑 Rust 测试 + armv7 daemon 构建 + Android APK 构建。
- **CD**（`.github/workflows/release.yml`）：推 `v*` tag 时自动构建 daemon/代理/APK，生成 SHA256 与 `release/latest.json`，发布 GitHub Release 并把 `latest.json` 提交回 `main`。
- **更新（手动按钮）**：主界面有「检查更新 / 更新 vX」按钮，默认**不会**遇到新版就自动安装；设置页可开「启动检查」（默认开，只提示）和「自动安装」（默认关）。检查 `https://raw.githubusercontent.com/ModerRAS/AES67Bridge/main/release/latest.json`（固定路径 JSON，不走 GitHub API 限额），下载后校验 SHA256；芝杜有 root，APK 与 daemon 二进制都通过固件的 root 命令桥静默安装并重启，无需人工点装。

## License

本项目采用 **GNU General Public License v3.0**（GPL-3.0）开源，全文见 [LICENSE](LICENSE)。

Vendored 第三方组件保留其原始许可证：

- `dolby-pipeline/vendor/truehd`、`dolby-pipeline/vendor/eac3`、`dolby-pipeline/vendor/oamd` — Apache-2.0
- `dolby-pipeline/vendor/roar` — BSD-3-Clause-Clear + Alliance for Open Media Patent License 1.0
- `daemon-rs/vendor/nsp_aes67` — Apache-2.0（来自 NativeSpatialPipe 的 AES67 模块，上游提交与本地补丁见 `PATCHES.md`，许可证全文见该目录 `LICENSE`/`NOTICE`）

以上均与 GPL-3.0 兼容。

仓库不包含 Zidoo 固件自带的专有二进制（Amlogic HAL `audio.primary.amlogic.so` 原厂库等）；构建 App 时从自己的设备 `adb pull`，见 [BUILD.md](BUILD.md)。

## 发现与接入（AES67 / RAVENNA 控制器）

- **SAP**：守护运行期间持续向 `224.2.127.254:9875` 公告 SDP（RFC 2974：IPv4 announce/delete、TTL 255、删除包只带 `o=` 行、每个 RTP flow 一个会话）。空闲时也会保持一条**静音 RTP 流**，所以控制器（如 aes67-linux-daemon 的 SAP 浏览）随时能发现并配置本源，不必等音乐/电影开播。
- **HTTP SDP 拉取**：`http://<设备IP>:8080/aes67.sdp`（`daemon.conf http_port=8080`，`0` 关闭）。不支持 SAP 的控制器可以直接拉这份 SDP 配置 Sink。
- 已知限制：输出超过 8ch 时守护按 flow 拆流，每个 flow 目前是独立的 SAP 会话；严格 AES67 多流应在一个 SDP 里用多个 `m=` 段描述（后续合并）。
