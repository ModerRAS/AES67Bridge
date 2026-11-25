# BUILD — AES67Bridge

Everything that ships is built from this repository. The only things that are
*not* in the repository are (a) the Android SDK/NDK and a Rust toolchain and
(b) the stock Amlogic HAL blob, which belongs to the TV firmware and is pulled
from the device instead of being redistributed.

| Artifact | Source in repo | Output |
| --- | --- | --- |
| `aes67-daemon` (armv7) | `daemon-rs/` (+ tracked `daemon-rs/vendor/nsp_aes67`, `dolby-pipeline/`) | `daemon-rs/target/armv7-linux-androideabi/release/` |
| `audio.primary.amlogic.so` (HAL tee proxy) | `device/haltee-proxy/{haltee.c,symmap.c}` | `device/haltee-proxy/out/audio.primary.amlogic.so` |
| `dev.aes67bridge.tv` (Android TV app) | `app-tv/` | `app-tv/app/build/outputs/apk/` |

The legacy C FIFO→SRC→RTP sender (`device/daemon/`, `engine=legacy`) and the old
Java console app (`app/`) were removed in 0.2.0; the Rust daemon is the only engine.

## 0. Prerequisites

- Rust (stable) with the `armv7-linux-androideabi` target:
  `rustup target add armv7-linux-androideabi`
- Android NDK >= 27 (clang + sysroot). The daemon linker is configured in
  `daemon-rs/.cargo/config.toml`; edit that file if your NDK lives elsewhere.
  CI overrides it with `CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER` + `RUSTFLAGS`.
- Android SDK platform 34 + build-tools 34, JDK 17, Gradle 8.11.1 (no wrapper is
  committed; use the system `gradle` or `gradle/actions/setup-gradle` in CI).
- `adb` and a rooted Zidoo Z9X 8K for deployment.

## 1. Rust daemon (armv7)

```powershell
cd daemon-rs
cargo test                                   # host unit + integration tests
cargo build --release --target armv7-linux-androideabi --bins
```

The x86_64 host binaries (`aes67-feed`, `aes67-recv`, `aes67-compliance`,
`aes67-dump-src`) come from the same crate. `aes67-daemon --help` lists the CLI;
`daemon.conf` keys are documented in `daemon-rs/src/config.rs`.

## 2. HAL tee proxy (armv7 C)

Windows:

```powershell
cd device\haltee-proxy
powershell -ExecutionPolicy Bypass -File build.ps1
```

Linux/macOS/CI: `device/haltee-proxy/build.sh` (honours `ANDROID_NDK_HOME` /
`ANDROID_NDK_ROOT` / `ANDROID_HOME`). For the 2026-11-25 deployed proxy the
Windows rebuild is byte-identical: `69e25abad4af7cbebdaafbe3ff37526e` (75,324 B).

## 3. Android TV app

The app bundles runtime blobs as assets. Neither the proxy nor the stock HAL is
committed:

| Asset | How to produce it |
| --- | --- |
| `audio.primary.amlogic.so` | build with `device/haltee-proxy/build.ps1` (`prepare-assets.ps1`) |
| `audio.primary.amlogic.so.orig` (stock HAL backup) | optional: `adb pull /vendor/lib/hw/audio.primary.amlogic.so` from *your own* device. Release APKs omit it; `bootstrap.sh` provisions it from the device on first run. |

```powershell
cd app-tv
powershell -ExecutionPolicy Bypass -File prepare-assets.ps1 -Device <ip>:5555
gradle assembleDebug
adb -s <ip>:5555 install -r app\build\outputs\apk\debug\app-debug.apk
```

## 4. Deploy

Short version: install the APK, then start the bridge from the TV UI (or let the
boot hook run `start.sh`). `start.sh` copies the app-private assets into
`/data/aes67bridge/stage/`, `bootstrap.sh` builds the tmpfs overlay, copies the
proxy over `/vendor/lib/hw`, provisions the stock HAL backup when needed, and
(re)starts the Rust daemon. Runtime files live in `/data/aes67bridge/`.

## 5. CI / CD / auto-update

- `.github/workflows/ci.yml`: Rust tests, armv7 daemon + proxy builds, APK build.
- `.github/workflows/release.yml`: on tag `v*` builds the same artifacts,
  writes `release/latest.json` (versionCode/versionName/url/sha256 for APK and
  daemon), publishes a GitHub Release and commits `latest.json` back to `main`.
- The TV app checks `https://raw.githubusercontent.com/ModerRAS/AES67Bridge/main/release/latest.json`
  (fixed path, no GitHub API rate limit), verifies SHA256 and installs APK + daemon
  through the firmware's root command bridge (`zidoo.root.cmd2` + `zd_root_cmd.sh`).

## 6. Reproducibility notes (measured 2026-11-25)

- HAL proxy: rebuilding `haltee.c` + `symmap.c` with `build.ps1` is
  **byte-identical** to the library deployed on the device —
  `69e25abad4af7cbebdaafbe3ff37526e`, 75,324 B.
- Rust daemon: the same tree, same flags, same checkout path builds to the same
  md5 (`44C83EA9727080697E97CFC76481762C` for 0.1.0/70abbf6), so the build is
  deterministic *for a fixed path*. It is **not** path-independent: the compiler
  embeds absolute source paths, so moving the checkout changes the output.
  CI builds therefore do not reproduce desktop md5s; compare behaviour or compare
  rebuilds from the same directory layout.
