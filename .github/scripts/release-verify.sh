#!/usr/bin/env bash
# Usage: release-verify.sh <channel> <dir> <pubkey> <version> <commit>
# Fails on the first problem; success means every archive in <dir> is signed by <pubkey> with
# the exact trusted comment binding and matches its checksums. The unsigned shell installer is
# accepted on the release channel only.
set -euo pipefail

if [ "$#" -ne 5 ]; then
  echo "usage: $0 <channel> <dir> <pubkey> <version> <commit>" >&2
  exit 2
fi
channel="$1"
dir="$2"
pubkey="$3"
version="$4"
commit="$5"

fail() {
  echo "release-verify: $*" >&2
  exit 1
}

case "$channel" in
  release | dev) ;;
  *) fail "unknown channel '$channel'" ;;
esac
[ -n "$version" ] || fail "empty version"
[ -n "$commit" ] || fail "empty commit"
[ -d "$dir" ] || fail "'$dir' is not a directory"
[ -f "$pubkey" ] || fail "'$pubkey' is not a file"

archives=()
for path in "$dir"/* "$dir"/.[!.]*; do
  [ -e "$path" ] || [ -L "$path" ] || continue
  name="$(basename "$path")"
  [ -f "$path" ] && [ ! -L "$path" ] || fail "unrecognised asset '$name' (not a regular file)"
  case "$name" in
    fontaine-*.tar.xz | fontaine-*.zip) archives+=("$name") ;;
    fontaine-*.tar.xz.minisig | fontaine-*.zip.minisig) ;;
    fontaine-*.tar.xz.sha256 | fontaine-*.zip.sha256) ;;
    sha256.sum) ;;
    # Dev tags are `dev-<n>`, but dist bakes a `v<version>` download URL into the installer.
    fontaine-installer.sh) [ "$channel" = release ] || fail "unrecognised asset '$name'" ;;
    *) fail "unrecognised asset '$name'" ;;
  esac
done

[ "${#archives[@]}" -gt 0 ] || fail "no archives in '$dir'"
[ -f "$dir/sha256.sum" ] || fail "sha256.sum is missing"

for sig in "$dir"/*.minisig; do
  [ -e "$sig" ] || continue
  [ -f "${sig%.minisig}" ] || fail "$(basename "$sig") has no archive"
done

for name in "${archives[@]}"; do
  archive="$dir/$name"
  [ -f "$archive.minisig" ] || fail "$name has no .minisig"
  [ -f "$archive.sha256" ] || fail "$name has no .sha256"

  minisign -V -q -p "$pubkey" -m "$archive" -x "$archive.minisig" \
    || fail "$name: signature does not verify against $pubkey"

  # Read only after -V succeeded, which authenticates this line through the global signature.
  expected="trusted comment: fontaine-signature v1 channel=$channel file=$name version=$version commit=$commit"
  actual="$(sed -n '3p' "$archive.minisig")"
  [ "$actual" = "$expected" ] || fail "$name: trusted comment is '$actual', expected '$expected'"

  listed="$(awk '{ sub(/^\*/, "", $2); print $2 }' "$archive.sha256")"
  [ "$listed" = "$name" ] || fail "$name.sha256 names '$listed'"
  (cd "$dir" && sha256sum -c "$name.sha256" >/dev/null) || fail "$name: .sha256 mismatch"

  awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) found = 1 } END { exit !found }' "$dir/sha256.sum" \
    || fail "$name is not listed in sha256.sum"
done

(cd "$dir" && sha256sum -c sha256.sum >/dev/null) || fail "sha256.sum mismatch"

echo "release-verify: ${#archives[@]} archives verified for channel $channel"
