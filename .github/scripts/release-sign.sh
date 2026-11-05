#!/usr/bin/env bash
# Usage: release-sign.sh <channel> <dir> <pubkey> <version> <commit>
# Signs every fontaine-*.tar.xz and fontaine-*.zip in <dir> with the secret key in
# MINISIGN_SECRET_KEY (password in MINISIGN_PASSWORD), then runs release-verify.sh over the result.
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
  echo "release-sign: $*" >&2
  exit 1
}

case "$channel" in
  release | dev) ;;
  *) fail "unknown channel '$channel'" ;;
esac
[ -n "$version" ] || fail "empty version"
[ -n "$commit" ] || fail "empty commit"
[ -n "${MINISIGN_SECRET_KEY:-}" ] || fail "MINISIGN_SECRET_KEY is empty"
[ -n "${MINISIGN_PASSWORD:-}" ] || fail "MINISIGN_PASSWORD is empty"
[ -d "$dir" ] || fail "'$dir' is not a directory"

archives=()
for path in "$dir"/fontaine-*.tar.xz "$dir"/fontaine-*.zip; do
  [ -f "$path" ] && archives+=("$path")
done
[ "${#archives[@]}" -gt 0 ] || fail "no archives in '$dir'"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
keyfile="$(umask 077 && mktemp)"
trap 'rm -f "$keyfile"' EXIT
printf '%s\n' "$MINISIGN_SECRET_KEY" > "$keyfile"

for archive in "${archives[@]}"; do
  name="$(basename "$archive")"
  comment="fontaine-signature v1 channel=$channel file=$name version=$version commit=$commit"
  printf '%s\n' "$MINISIGN_PASSWORD" \
    | minisign -S -s "$keyfile" -m "$archive" -x "$archive.minisig" -t "$comment" >/dev/null \
    || fail "signing $name failed"
done

"$here/release-verify.sh" "$channel" "$dir" "$pubkey" "$version" "$commit"
