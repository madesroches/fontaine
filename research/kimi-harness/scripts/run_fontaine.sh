#!/usr/bin/env bash
# usage: run_fontaine.sh NAME SETTINGS(proxy.toml|proxy_messages.toml) "prompt" [extra fontaine args...]
# Runs the host fontaine binary (never a container) with the kimi / madesroches base_urls pointed at the recording proxies.
# env: FIXTURE=<dir under fixtures/>  MODEL=<provider/model> (default kimi/kimi-for-coding)
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; SETTINGS=$2; PROMPT=$3; shift 3
RUN=$HERE/runs/$NAME; mkdir -p "$RUN/ws"
[ -n "${FIXTURE:-}" ] && cp -a "$HERE/fixtures/$FIXTURE/." "$RUN/ws/"
echo "$NAME" > "$HERE/runs/CURRENT"
FONTAINE=${FONTAINE:-$HERE/../../rust/target/release/fontaine}
cd "$RUN/ws"
set +e
"$FONTAINE" -p "$PROMPT" --permission-mode auto --settings "$HERE/scripts/$SETTINGS" --model "${MODEL:-kimi/kimi-for-coding}" "$@" > "$RUN/fontaine.out" 2> "$RUN/fontaine.err"
echo "exit=$?" | tee "$RUN/exit.txt"
