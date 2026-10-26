#!/usr/bin/env bash
# usage: run_fontaine.sh NAME SETTINGS(proxy.toml) "prompt" [extra fontaine args...]
# Runs the host fontaine binary (never sees a container) with the go account's base_url pointed at the recording proxy.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; SETTINGS=$2; PROMPT=$3; shift 3
RUN=$HERE/runs/$NAME; rm -rf "$RUN"; mkdir -p "$RUN"
# Workspace outside the repo: Fontaine loads every ancestor AGENTS.md, so a cwd under the repo would add the repo's own to the system prompt.
WS=$(mktemp -d); trap 'rm -rf "$RUN/ws"; cp -a "$WS" "$RUN/ws"; rm -rf "$WS"' EXIT
[ -n "${FIXTURE:-}" ] && cp -a "$HERE/fixtures/$FIXTURE/." "$WS/"
echo "$NAME" > "$HERE/runs/CURRENT"
FONTAINE=${FONTAINE:-$HERE/../../rust/target/release/fontaine}
cd "$WS"
set +e
"$FONTAINE" -p "$PROMPT" --settings "$HERE/scripts/$SETTINGS" --model "${MODEL:-go/deepseek-v4.1-flash}" "$@" < /dev/null > "$RUN/fontaine.out" 2> "$RUN/fontaine.err"
echo "exit=$?" | tee "$RUN/exit.txt"
