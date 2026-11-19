#!/usr/bin/env bash
# Usage: build-apk.sh <out.apk> [--debug-sign]
# Builds the arm64 APK without Gradle: cargo-ndk compiles the library, aapt2 packs the manifest,
# zipalign aligns it. The result is unsigned unless --debug-sign is given; a release APK is signed
# by the release workflow's `sign` job, which alone holds the keystore.
# Needs cargo-ndk, ANDROID_HOME (build-tools and platform 34) and an NDK that cargo-ndk can find.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <out.apk> [--debug-sign]" >&2
  exit 2
fi
out="$(realpath -m "$1")"
mode="${2:-}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${ANDROID_HOME:?ANDROID_HOME is not set}"
tools="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)"
platform="$ANDROID_HOME/platforms/android-34/android.jar"
[ -f "$platform" ] || { echo "build-apk: $platform is missing" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/stage/lib"

# dear-imgui-sys links c++_shared on Android; without the flag libc++_shared.so is not copied next
# to the library, so it is not packaged and the app fails to load.
(cd "$here/.." && cargo ndk -t arm64-v8a -o "$work/stage/lib" --link-libcxx-shared build -p fontaine-android --release)

# The version comes from the workspace so it cannot drift from the release tag, which the release
# workflow checks against it; the code is major*1000000 + minor*1000 + patch so it only grows.
version="$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)".*/\1/p' "$here/../Cargo.toml" | head -n 1)"
[ -n "$version" ] || { echo "build-apk: no version in rust/Cargo.toml" >&2; exit 1; }
IFS=. read -r major minor patch <<<"$version"
code=$((major * 1000000 + minor * 1000 + patch))

"$tools/aapt2" link -o "$work/base.apk" --manifest "$here/AndroidManifest.xml" \
  -I "$platform" --min-sdk-version 26 --target-sdk-version 34 \
  --version-name "$version" --version-code "$code"
(cd "$work/stage" && zip -q -r "$work/base.apk" lib)
"$tools/zipalign" -p -f 4 "$work/base.apk" "$work/aligned.apk"

if [ "$mode" = "--debug-sign" ]; then
  keytool -genkeypair -keystore "$work/debug.jks" -storepass android -keypass android \
    -alias debug -keyalg RSA -keysize 2048 -validity 30 -dname "CN=fontaine debug" >/dev/null 2>&1
  "$tools/apksigner" sign --ks "$work/debug.jks" --ks-pass pass:android --key-pass pass:android \
    --out "$out" "$work/aligned.apk"
else
  mv "$work/aligned.apk" "$out"
fi
echo "build-apk: wrote $out"
