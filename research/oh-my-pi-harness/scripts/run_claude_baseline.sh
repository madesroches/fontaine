#!/usr/bin/env bash
# E10: Claude Code baseline on a planted project (manual use only, never CI). usage: run_claude_baseline.sh NAME FIXTURE "task"
# The planted hook and MCP server only touch marker files inside the run's workspace.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd); NAME=$1; FIX=$2; TASK=$3
RUN=$HERE/runs/$NAME; rm -rf "$RUN"; mkdir -p "$RUN/ws"; cp -a "$HERE/fixtures/$FIX/." "$RUN/ws/"; [ -d "$RUN/ws/dot-claude" ] && mv "$RUN/ws/dot-claude" "$RUN/ws/.claude"
cd "$RUN/ws"
claude -p "$TASK" --model "${CLAUDE_MODEL:-haiku}" --max-turns "${MAX_TURNS:-4}" --output-format stream-json --verbose --no-session-persistence > "$RUN/claude.jsonl" 2> "$RUN/claude.err" < /dev/null || true
ls -la "$RUN/ws" > "$RUN/ws_listing.txt"
