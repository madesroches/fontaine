#!/usr/bin/env bash
# Usage: dev-installer.sh <installer> <tag> <version>
# Rewrites dist's shell or PowerShell installer (chosen by the file extension) in place so a dev
# build can use it: dist bakes the `v<cargo version>` release URL and version into the script, and
# neither exists for a dev tag.
# Every substitution is matched literally with its surrounding context and the count of matches is
# checked first, so a dist upgrade that changes the installer's text fails the dev run before
# anything is signed or uploaded instead of shipping a half-rewritten installer.
set -euo pipefail
# bash 5.2 would otherwise treat `&` in a replacement as the matched text.
shopt -u patsub_replacement 2>/dev/null || true

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <installer> <tag> <version>" >&2
  exit 2
fi
installer="$1"
tag="$2"
version="$3"

fail() {
  echo "dev-installer: $*" >&2
  exit 1
}

[ -f "$installer" ] || fail "'$installer' is not a file"
[[ "$tag" =~ ^dev-[0-9]+$ ]] || fail "tag '$tag' is not dev-<n>"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]+\+[0-9a-f]+$ ]] || fail "version '$version' is not <base>-dev.<n>+<sha>"

# The trailing sentinel keeps the final newline, which $(...) would strip.
content="$(cat "$installer"; printf x)"
content="${content%x}"

count() { # count <literal>
  local rest="${content//"$1"/}"
  echo $(((${#content} - ${#rest}) / ${#1}))
}

replace() { # replace <name> <expected count> <literal> <replacement>
  local n
  n="$(count "$3")"
  [ "$n" -eq "$2" ] || fail "$1: expected $2 occurrence(s) of '$3', found $n"
  content="${content//"$3"/"$4"}"
}

nl=$'\n'
case "$installer" in
  *.sh)
    base="$(printf '%s' "$content" | sed -n 's/^APP_VERSION="\([^"]*\)"$/\1/p')"
    line="APP_VERSION"
    ;;
  *.ps1)
    base="$(printf '%s' "$content" | sed -n "s/^\$app_version = '\([^']*\)'\$/\1/p")"
    line="app_version"
    ;;
  *) fail "'$installer' is neither a .sh nor a .ps1" ;;
esac
[ -n "$base" ] && [ "$(printf '%s\n' "$base" | wc -l)" -eq 1 ] || fail "$line line not found exactly once"
[[ "$base" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "unexpected $line '$base'"

replace "download URL" 4 "/releases/download/v$base" "/releases/download/$tag"
case "$installer" in
  *.sh) replace "APP_VERSION" 1 "${nl}APP_VERSION=\"$base\"${nl}" "${nl}APP_VERSION=\"$version\"${nl}" ;;
  *.ps1) replace "app_version" 1 "${nl}\$app_version = '$base'${nl}" "${nl}\$app_version = '$version'${nl}" ;;
esac
replace "receipt version" 1 ",\"version\":\"$base\"}${nl}" ",\"version\":\"$version\"}${nl}"
replace "usage text" 1 "${nl}The installer for fontaine $base${nl}" "${nl}The installer for fontaine $version${nl}"

printf '%s' "$content" > "$installer"
