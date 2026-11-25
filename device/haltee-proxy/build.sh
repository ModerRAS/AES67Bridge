#!/usr/bin/env bash
# Build the AES67Bridge HAL tee proxy (audio.primary.amlogic.so) with the Android NDK.
# Windows uses build.ps1; CI and Linux/macOS use this script.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/out/audio.primary.amlogic.so}"
mkdir -p "$(dirname "$out")"

pick_ndk() {
  for v in "${ANDROID_NDK_HOME:-}" "${ANDROID_NDK_ROOT:-}"; do
    if [ -n "$v" ] && [ -d "$v/toolchains/llvm" ]; then echo "$v"; return 0; fi
  done
  for sdk in "${ANDROID_SDK_ROOT:-}" "${ANDROID_HOME:-}"; do
    if [ -n "$sdk" ] && [ -d "$sdk/ndk" ]; then
      local newest
      newest="$(ls -1 "$sdk/ndk" | sort -V | tail -n1)"
      if [ -n "$newest" ] && [ -d "$sdk/ndk/$newest/toolchains/llvm" ]; then
        echo "$sdk/ndk/$newest"; return 0
      fi
    fi
  done
  return 1
}

NDK="$(pick_ndk)" || { echo "No Android NDK found; set ANDROID_NDK_HOME or ANDROID_NDK_ROOT." >&2; exit 1; }
case "$(uname -s)" in
  Linux*)  host=linux-x86_64 ;;
  Darwin*) host=darwin-x86_64 ;;
  *)       host=windows-x86_64 ;;
esac
clang="$NDK/toolchains/llvm/prebuilt/$host/bin/armv7a-linux-androideabi30-clang"
[ -x "$clang" ] || { echo "clang not found: $clang" >&2; exit 1; }

echo "ndk:   $NDK"
echo "src:   $here/haltee.c, $here/symmap.c"
echo "out:   $out"
"$clang" --target=armv7a-linux-androideabi30 -shared -fPIC -O2 -Wall -Wextra \
  -Wno-unused-parameter -Wno-sign-compare -o "$out" "$here/haltee.c" "$here/symmap.c" -lm

if command -v sha256sum >/dev/null 2>&1; then
  echo "built: $(wc -c < "$out") bytes  sha256=$(sha256sum "$out" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
  echo "built: $(wc -c < "$out") bytes  sha256=$(shasum -a 256 "$out" | cut -d' ' -f1)"
fi
