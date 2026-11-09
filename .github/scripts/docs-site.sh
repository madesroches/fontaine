#!/usr/bin/env bash
# Checks the docs tree, builds the mdBook site into <out-dir> and writes the agent-facing
# Markdown copies plus llms.txt / llms-full.txt. Needs mdbook and lychee on PATH. Linux CI only.
set -euo pipefail

SITE_URL="https://madesroches.github.io/fontaine"

out="${1:?usage: docs-site.sh <out-dir>}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
docs="$repo/docs"
summary="$docs/SUMMARY.md"

fail=0
complain() {
  echo "docs-site: $*" >&2
  fail=1
}

# Pages in SUMMARY order, one path per line.
mapfile -t pages < <(sed -nE 's/^[[:space:]]*-?[[:space:]]*\[[^]]*\]\(([^)#]+\.md)\).*/\1/p' "$summary")
[ "${#pages[@]}" -gt 0 ] || complain "SUMMARY.md lists no pages"

for file in "$docs"/*.md; do
  name="$(basename "$file")"
  [ "$name" = SUMMARY.md ] && continue
  printf '%s\n' "${pages[@]}" | grep -qxF "$name" || complain "$name is not listed in SUMMARY.md"
done

for page in "${pages[@]}"; do
  [ "$page" = index.md ] && continue
  grep -qF "]($page)" "$docs/index.md" || complain "$page is not linked from index.md"
done

if grep -nF '](../' "$docs"/*.md >&2; then
  complain "links leaving docs/ must be absolute GitHub URLs"
fi

[ "$fail" -eq 0 ] || exit 1

rm -rf "$out"
mdbook build "$docs" -d "$out"
# src = "." publishes the book config alongside the pages.
rm -f "$out/book.toml" "$out/SUMMARY.md"
# ... and so does a stale docs/book from a local `mdbook serve docs`.
rm -rf "$out/book"

for page in "${pages[@]}"; do
  cp "$docs/$page" "$out/$page"
done

{
  echo "# Fontaine"
  echo
  echo "> A terminal AI coding agent for any model, with a guardian that judges risky tool calls, and an AFK mode that runs unattended without YOLO."
  echo
  echo "## Docs"
  echo
  sed -nE 's/^[[:space:]]*-?[[:space:]]*\[([^]]*)\]\(([^)#]+\.md)\).*/\1\t\2/p' "$summary" |
    while IFS=$'\t' read -r title page; do
      echo "- [$title]($SITE_URL/$page)"
    done
} >"$out/llms.txt"

: >"$out/llms-full.txt"
for page in "${pages[@]}"; do
  cat "$docs/$page" >>"$out/llms-full.txt"
  printf '\n\n' >>"$out/llms-full.txt"
done

entries="$(grep -c '^- \[' "$out/llms.txt")"
[ "$entries" -eq "${#pages[@]}" ] || complain "llms.txt has $entries entries for ${#pages[@]} pages"
for page in "${pages[@]}"; do
  [ -f "$out/$page" ] || complain "$page is missing from $out"
done
[ "$fail" -eq 0 ] || exit 1
# Offline, so CI never depends on external hosts. 404.html is excluded because mdBook renders
# it with <base href="/fontaine/">, whose root-relative links offline lychee cannot resolve.
lychee --offline --include-fragments --no-progress --exclude-path "$out/404.html" "$out/**/*.html"
echo "docs-site: built $out (${#pages[@]} pages)"
