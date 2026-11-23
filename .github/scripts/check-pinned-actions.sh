#!/usr/bin/env bash
# Fails when a `uses:` in the workflows is not a local or docker reference and lacks a 40-hex SHA.
set -euo pipefail

dir="${1:-$(dirname "$0")/../workflows}"
status=0
shopt -s nullglob
for file in "$dir"/*.yml "$dir"/*.yaml; do
  n=0
  while IFS= read -r line || [[ -n $line ]]; do
    n=$((n + 1))
    trimmed="${line#"${line%%[![:space:]]*}"}"
    trimmed="${trimmed#-}"
    trimmed="${trimmed#"${trimmed%%[![:space:]]*}"}"
    case "$trimmed" in
      uses:*) ;;
      *) continue ;;
    esac
    ref="${trimmed#uses:}"
    ref="${ref#"${ref%%[![:space:]]*}"}"
    ref="${ref%%[[:space:]]*}"
    ref="${ref#[\"\']}"
    ref="${ref%[\"\']}"
    case "$ref" in
      ./* | docker://*) continue ;;
    esac
    sha="${ref##*@}"
    if [[ "$ref" != *@* || ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
      echo "$file:$n: action not pinned to a full commit SHA: $ref" >&2
      status=1
    fi
  done < "$file"
done
exit "$status"
