#!/usr/bin/env bash
# E9: Claude Code baseline on the same fixture. Manual use only (never CI). usage: run_claude_baseline.sh NAME "task"
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd); NAME=$1; TASK=$2
RUN=$HERE/runs/$NAME; mkdir -p "$RUN/ws"; cp -a "$HERE/fixtures/repo-failing/." "$RUN/ws/"
cd "$RUN/ws"
claude -p "$TASK" --model "${CLAUDE_MODEL:-haiku}" --allowedTools "Bash Read Edit Write" --max-turns "${MAX_TURNS:-14}" --output-format stream-json --verbose --no-session-persistence > "$RUN/claude.jsonl" 2> "$RUN/claude.err" < /dev/null || true
