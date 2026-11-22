#!/usr/bin/env bash
# Usage: build-apk.sh <out.apk> [--debug-sign]
# Builds the arm64 APK without Gradle: cargo-ndk compiles the library, javac and d8 turn the Java
# activity into classes.dex, aapt2 packs the manifest, zipalign aligns it. The result is unsigned unless --debug-sign is given; a release APK is signed
# by the release or dev workflow's `sign` job, which alone holds the keystore.
# The version comes from rust/Cargo.toml; FONTAINE_APK_VERSION_CODE and FONTAINE_APK_VERSION_NAME
# override the code and name (see version-code.sh).
# Needs cargo-ndk, ANDROID_HOME (build-tools and platform 34), an NDK that cargo-ndk can find
# (ANDROID_NDK_HOME, else the newest under ANDROID_HOME/ndk; its llvm-nm checks the JNI exports)
# and a JDK 17 or newer (JAVA_HOME_17_X64 when set) for javac and for d8.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <out.apk> [--debug-sign]" >&2
  exit 2
fi
out="$(realpath -m "$1")"
mode="${2:-}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Before the build, so a bad override fails in seconds rather than after cargo ndk.
# shellcheck source=version-code.sh
. "$here/version-code.sh"
apk_version "$here/../Cargo.toml"
: "${ANDROID_HOME:?ANDROID_HOME is not set}"
tools="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)"
platform="$ANDROID_HOME/platforms/android-34/android.jar"
[ -f "$platform" ] || { echo "build-apk: $platform is missing" >&2; exit 1; }

# d8 in the build-tools is a wrapper that ends in `exec java` and ignores JAVA_HOME, so the JDK has
# to be first on PATH, not only in JAVA_HOME.
jdk_major() { "$1" -version 2>&1 | head -n 1 | grep -o '[0-9][0-9]*' | head -n 1; }
if [ -n "${JAVA_HOME_17_X64:-}" ]; then
  JAVA_HOME="$JAVA_HOME_17_X64"
else
  for tool in java javac; do
    command -v "$tool" >/dev/null || { echo "build-apk: $tool is not on PATH (need JDK 17 or newer)" >&2; exit 1; }
    major="$(jdk_major "$tool")"
    case "$major" in
      '' | *[!0-9]*) echo "build-apk: cannot read the $tool version" >&2; exit 1 ;;
    esac
    [ "$major" -ge 17 ] || { echo "build-apk: $tool is $major; JDK 17 or newer is required (set JAVA_HOME_17_X64)" >&2; exit 1; }
  done
  JAVA_HOME="$(dirname "$(dirname "$(realpath "$(command -v javac)")")")"
fi
export JAVA_HOME
export PATH="$JAVA_HOME/bin:$PATH"
[ -x "$JAVA_HOME/bin/javac" ] || { echo "build-apk: $JAVA_HOME/bin/javac is missing" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/stage/lib"

# dear-imgui-sys links c++_shared on Android; without the flag libc++_shared.so is not copied next
# to the library, so it is not packaged and the app fails to load.
(cd "$here/.." && cargo ndk -t arm64-v8a --platform 26 -o "$work/stage/lib" --link-libcxx-shared build -p fontaine-android --release)

lib="$work/stage/lib/arm64-v8a/libfontaine_android.so"
ndk="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n 1)}"
nm="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm"
[ -x "$nm" ] || { echo "build-apk: llvm-nm not found under '$ndk'" >&2; exit 1; }
# ART binds the activity's native methods by symbol name; a symbol the linker dropped would
# otherwise surface as an UnsatisfiedLinkError on the first launch.
exports="$("$nm" -D --defined-only "$lib")"
for symbol in nativeImeState nativeImeSubmit; do
  grep -q "Java_dev_fontaine_remote_FontaineActivity_$symbol" <<<"$exports" \
    || { echo "build-apk: $lib does not export Java_dev_fontaine_remote_FontaineActivity_$symbol" >&2; exit 1; }
done

mkdir -p "$work/classes" "$work/dex"
find "$here/java" -name '*.java' -print0 | xargs -0 javac --release 8 -Xlint:-options -cp "$platform" -d "$work/classes"
find "$work/classes" -name '*.class' -print0 | xargs -0 "$tools/d8" --lib "$platform" --min-api 26 --output "$work/dex"

"$tools/aapt2" link -o "$work/base.apk" --manifest "$here/AndroidManifest.xml" \
  -I "$platform" --min-sdk-version 26 --target-sdk-version 34 \
  --version-name "$name" --version-code "$code"
(cd "$work/stage" && zip -q -r "$work/base.apk" lib)
zip -q -j "$work/base.apk" "$work/dex/classes.dex"
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
