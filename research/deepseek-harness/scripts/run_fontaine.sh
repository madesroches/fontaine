#!/usr/bin/env bash
# usage: run_fontaine.sh NAME SETTINGS(proxy.toml|proxy_messages.toml) "prompt" [extra fontaine args...]
# Runs the host fontaine binary (never sees a container) with the go account's base_url pointed at the recording proxy.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; SETTINGS=$2; PROMPT=$3; shift 3
RUN=$HERE/runs/$NAME; mkdir -p "$RUN/ws"
[ -n "${FIXTURE:-}" ] && cp -a "$HERE/fixtures/$FIXTURE/." "$RUN/ws/"
echo "$NAME" > "$HERE/runs/CURRENT"
FONTAINE=${FONTAINE:-$HERE/../../rust/target/release/fontaine}
cd "$RUN/ws"
set +e
"$FONTAINE" -p "$PROMPT" --settings "$HERE/scripts/$SETTINGS" --model "${MODEL:-go/deepseek-v4.1-flash}" "$@" > "$RUN/fontaine.out" 2> "$RUN/fontaine.err"
echo "exit=$?" | tee "$RUN/exit.txt"
