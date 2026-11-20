#!/usr/bin/env bash
# Sourced by build-apk.sh and the CI checks; defines apk_version <Cargo.toml> and nothing else.
# Sets `name` and `code` for the APK. The version comes from the workspace so it cannot drift from
# the release tag, which the release workflow checks against it; the code is
# major*1000000 + minor*1000 + patch so it only grows. A prerelease or build suffix stays in the
# name and is ignored for the code. FONTAINE_APK_VERSION_CODE and FONTAINE_APK_VERSION_NAME
# override them: the dev channel builds many times between version bumps, and Android only
# updates in place when the code grows.

apk_version() {
  local cargo_toml="$1" version
  name="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$cargo_toml" | head -n 1)"
  version="$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)[^"]*".*/\1/p' "$cargo_toml" | head -n 1)"
  [ -n "$version" ] || { echo "version-code: no version in $cargo_toml" >&2; return 1; }
  local major minor patch
  IFS=. read -r major minor patch <<<"$version"
  code=$((major * 1000000 + minor * 1000 + patch))

  if [ -n "${FONTAINE_APK_VERSION_CODE:-}" ]; then
    # Android stores the code as a signed 32-bit integer and rejects 2100000000 and above.
    case "$FONTAINE_APK_VERSION_CODE" in
      '' | *[!0-9]* | 0*)
        echo "version-code: FONTAINE_APK_VERSION_CODE must be a positive integer" >&2
        return 1 ;;
    esac
    if [ "${#FONTAINE_APK_VERSION_CODE}" -gt 10 ] || [ "$FONTAINE_APK_VERSION_CODE" -ge 2100000000 ]; then
      echo "version-code: FONTAINE_APK_VERSION_CODE must be below 2100000000" >&2
      return 1
    fi
    code="$FONTAINE_APK_VERSION_CODE"
  fi
  if [ -n "${FONTAINE_APK_VERSION_NAME:-}" ]; then
    name="$FONTAINE_APK_VERSION_NAME"
  fi
}
