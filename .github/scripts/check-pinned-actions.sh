#!/usr/bin/env bash
# Fails when a `uses:` in the workflows is not a local or docker reference and lacks a 40-hex SHA.
set -euo pipefail

dir="${1:-$(dirname "$0")/../workflows}"
status=0
for file in "$dir"/*.yml; do
  n=0
  while IFS= read -r line; do
    n=$((n + 1))
    trimmed="${line#"${line%%[![:space:]]*}"}"
    trimmed="${trimmed#- }"
    case "$trimmed" in
      uses:*) ;;
      *) continue ;;
    esac
    ref="${trimmed#uses:}"
    ref="${ref#"${ref%%[![:space:]]*}"}"
    ref="${ref%% *}"
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
