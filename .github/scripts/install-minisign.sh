#!/usr/bin/env bash
# Installs the pinned upstream minisign CLI (x86_64 linux) after checking the tarball's sha256.
# Usage: install-minisign.sh [dest-dir]; adds dest-dir to GITHUB_PATH when run in Actions.
set -euo pipefail

version="0.12"
sha256="9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73"
url="https://github.com/jedisct1/minisign/releases/download/${version}/minisign-${version}-linux.tar.gz"
dest="${1:-${RUNNER_TEMP:-/tmp}/minisign-bin}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl --proto '=https' --tlsv1.2 --retry 3 --retry-all-errors -fsSL -o "$work/minisign.tar.gz" "$url"
echo "${sha256}  $work/minisign.tar.gz" | sha256sum -c - >/dev/null \
  || { echo "::error::minisign tarball sha256 mismatch" >&2; exit 1; }

mkdir -p "$dest"
tar -xzf "$work/minisign.tar.gz" -C "$work" minisign-linux/x86_64/minisign
install -m 0755 "$work/minisign-linux/x86_64/minisign" "$dest/minisign"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$dest" >> "$GITHUB_PATH"
fi
"$dest/minisign" -v
