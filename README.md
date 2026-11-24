# AES67Bridge

Zidoo Z9X 8K → AES67 音频桥（研究项目）。

- `app/`：Android 控制台（dev.aes67bridge.console）
- `device/haltee-proxy/`：vendor audio HAL 代理 .so（内存 overlay，不修改分区）
- `device/daemon/`：root 侧 FIFO → SRC → L24 RTP 守护

构建见各目录 `build.ps1`（依赖 Android SDK build-tools / NDK，路径按本机调整）。
