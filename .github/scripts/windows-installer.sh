#!/usr/bin/env bash
# Usage: windows-installer.sh <installer.ps1> <sha256 file>
# Patches dist's PowerShell installer in place so it checks the archive's sha256 before unpacking:
# dist's stock script has no checksum verification, unlike its shell installer. The digest comes
# from the .sha256 file of the archive that will be published, and the check is a function from
# windows-installer-sha256.ps1 (the same file the CI self-test runs). Both insertions use anchors
# matched literally with an exact count, so a dist upgrade that changes the text fails the run
# before anything is signed or uploaded instead of shipping an unchecked installer.
set -euo pipefail
# bash 5.2 would otherwise treat `&` in a replacement as the matched text.
shopt -u patsub_replacement 2>/dev/null || true

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <installer.ps1> <sha256 file>" >&2
  exit 2
fi
installer="$1"
sha_file="$2"
archive="fontaine-x86_64-pc-windows-msvc.zip"

fail() {
  echo "windows-installer: $*" >&2
  exit 1
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
snippet_file="$here/windows-installer-sha256.ps1"

[ -f "$installer" ] || fail "'$installer' is not a file"
[ -f "$sha_file" ] || fail "'$sha_file' is not a file"
[ -f "$snippet_file" ] || fail "'$snippet_file' is missing"

# dist writes `<hex> *<name>\n\n`; the trailing blank line is dropped before counting.
mapfile -t lines < <(grep -v '^[[:space:]]*$' "$sha_file" || true)
[ "${#lines[@]}" -eq 1 ] || fail "$sha_file must hold exactly one non-empty line, found ${#lines[@]}"
# The star marks binary mode.
[[ "${lines[0]}" =~ ^([0-9a-fA-F]{64})\ \*?(.+)$ ]] || fail "$sha_file is not '<sha256> *<name>'"
digest="${BASH_REMATCH[1]}"
listed="${BASH_REMATCH[2]}"
[ "$listed" = "$archive" ] || fail "$sha_file names '$listed', expected '$archive'"

# The trailing sentinel keeps the final newline, which $(...) would strip.
content="$(cat "$installer"; printf x)"
content="${content%x}"
snippet="$(cat "$snippet_file")"

nl=$'\n'
# The updater download uses a different -path, so this line is unique to the archive download.
call="  Invoke-DownloadFile -client \$wc -url \$url -path \$dir_path"
check="  Assert-ArchiveSha256 -Path \$dir_path -Expected '$digest'"
# Counted by lines: counting literal matches in the whole text undercounts adjacent duplicates.
n="$(grep -c -xF -- "$call" "$installer" || true)"
[ "$n" -eq 1 ] || fail "archive download: expected 1 line '$call', found $n"
# The definition goes before the final top-level Install-Binary call, which would otherwise run first.
n="$(grep -c '^function Download(' "$installer" || true)"
[ "$n" -eq 1 ] || fail "Download function: expected 1 line starting 'function Download(', found $n"

content="${content/"${nl}${call}${nl}"/"${nl}${call}${nl}${check}${nl}"}"
content="${content/"${nl}function Download("/"${nl}${snippet}${nl}${nl}function Download("}"

printf '%s' "$content" > "$installer"
