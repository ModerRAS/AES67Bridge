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
| legacy C sender (optional) | `device/daemon/aes67_rtp_sender.c` | `device/daemon/out/aes67_rtp_sender` |

## 0. Prerequisites

- Rust (stable) with the `armv7-linux-androideabi` target:
  `rustup target add armv7-linux-androideabi`
- Android NDK >= 27 (clang + sysroot). The daemon linker is configured in
  `daemon-rs/.cargo/config.toml`; edit that file if your NDK lives elsewhere.
- Android SDK platform 34 + build-tools 34, JDK 17 (TV app).
- `adb` and a rooted Zidoo Z9X 8K for deployment.

## 1. Rust daemon (armv7)

```powershell
cd daemon-rs
cargo test                                   # host unit + integration tests
cargo build --release --target armv7-linux-androideabi --bins
```

The x86_64 host binaries (`aes67-feed`, `aes67-recv`, `aes67-compliance`,
`aes67-dump-src`) come from the same crate; `aes67-compliance` prints the
AES67 conformance table and is the quickest smoke test.

## 2. HAL tee proxy (armv7 C)

```powershell
cd device\haltee-proxy
powershell -ExecutionPolicy Bypass -File build.ps1
```

The script resolves the NDK itself (`-Ndk` or `ANDROID_NDK_HOME` or the newest
`ndk/` under the SDK) and prints the md5 of the result. For the 2026-11-25
deployed proxy the rebuild is byte-identical:
`69e25abad4af7cbebdaafbe3ff37526e` (75,324 B).

## 3. Android TV app

The app bundles three runtime blobs as assets. None of them may be committed:

| Asset | How to produce it |
| --- | --- |
| `audio.primary.amlogic.so` | build with `device/haltee-proxy/build.ps1` |
| `haltee_rtp_sender` (legacy C sender, optional) | build from `device/daemon/aes67_rtp_sender.c` with the NDK |
| `audio.primary.amlogic.so.orig` (stock HAL, rollback copy) | `adb pull /vendor/lib/hw/audio.primary.amlogic.so` from *your own* device |

```powershell
adb -s <ip>:5555 pull /vendor/lib/hw/audio.primary.amlogic.so app-tv\app\src\main\assets\audio.primary.amlogic.so.orig
Copy-Item device\haltee-proxy\out\audio.primary.amlogic.so app-tv\app\src\main\assets\
cd app-tv
.\gradlew.bat assembleDebug
adb -s <ip>:5555 install -r app\build\outputs\apk\debug\app-debug.apk
```

`.gitignore` excludes `*.so`, `*.orig`, `haltee_rtp_sender` and `build/`, so a
fresh clone has to perform the three steps above before it can build the APK.

## 4. Deploy

See the runtime layout in `README.md` and the handoff reports under
`C:\WorkSpace\Audio\HANDOFF`. Short version: push the binaries into
`/data/aes67bridge/stage/` (or let the app's `start.sh` copy them from its
private files dir), then run `sh /data/aes67bridge/stage/start.sh`.
`start.sh` -> `bootstrap.sh` re-creates the tmpfs overlay, copies the proxy into
`/vendor/lib/hw` through it, and (re)starts the daemon.

## 5. Reproducibility notes (measured 2026-11-25)

- HAL proxy: rebuilding `haltee.c` + `symmap.c` with this script is
  **byte-identical** to the library deployed on the device —
  `69e25abad4af7cbebdaafbe3ff37526e`, 75,324 B.
- Rust daemon: the same tree, same flags, same checkout path builds to the same
  md5 (verified twice), so the build is deterministic *for a fixed path*.
  It is **not** path-independent: the compiler embeds absolute source paths, so
  moving the checkout changes the output (a `--remap-path-prefix` experiment
  from `C:\WorkSpace\Audio\aes67-bridge` back to
  `C:\WorkSpace\Claw\projects\aes67-bridge` moved the binary from 3,729,848 to
  3,730,348 bytes).
- Consequence for auditing: compare a deployment against a rebuild **from the
  same directory layout**, or compare behaviour, not md5. As of 2026-11-25 the
  deployed daemon is a HEAD build (`92b1cbb148a431c9101cbdaebffee095`,
  3,729,884 B) produced by this repo; the previous deployment
  (`466783c280941c7b25e14414fcfa4fe4`, 3,729,644 B) could not be reproduced from
  HEAD, which is why it was replaced.
- Legacy C sender (`device/daemon/aes67_rtp_sender.c`): a rebuild gives
  211,228–211,268 B depending on flags, while the binary deployed on the device
  is 215,560 B (`80882a75cd2c70eb057bd91337baac6e`). The two are not reconcilable
  from the current source; the legacy engine (`engine=legacy`) is the only
  consumer and the shipping engine is `engine=rust`, so this is a documented
  gap rather than a regression.
