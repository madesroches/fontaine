#!/usr/bin/env bash
# usage: run_claude_baseline.sh NAME "task"   (manual only; never from tests or CI). Runs the host `claude -p` in a scratch dir with Bash allowed.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
NAME=$1; TASK=$2
RUN=$HERE/runs/$NAME; mkdir -p "$RUN/ws"; cd "$RUN/ws"
claude -p "$TASK" --allowedTools Bash --output-format stream-json --verbose > "$RUN/claude.jsonl" 2> "$RUN/claude.err" || true
