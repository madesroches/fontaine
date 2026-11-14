#!/usr/bin/env bash
# Stands in for the `claude` executable and records the stream-json traffic both ways, so a
# consumer program run against it leaves a transcript of what the SDK and Claude Code said.
# Manual use only, through capture.sh.
: "${CAPTURE_DIR:?}"
: "${REAL_CLAUDE:?}"
exec "$REAL_CLAUDE" "$@" \
  < <(tee -a "$CAPTURE_DIR/stdin.jsonl") \
  > >(tee -a "$CAPTURE_DIR/stdout.jsonl")
