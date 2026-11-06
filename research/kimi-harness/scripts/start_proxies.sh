#!/usr/bin/env bash
# Starts the two recording proxies (Kimi API key instance, madesroches OAuth instance) in the background.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$HERE/runs"
nohup python3 -I "$HERE/scripts/record_proxy.py" --runs "$HERE/runs" --upstream https://api.kimi.ai/coding/v1 \
  --account kimi --auth apikey --tag kimi --harness-port 18080 --fontaine-port 18081 > "$HERE/runs/proxy-kimi.log" 2>&1 &
nohup python3 -I "$HERE/scripts/record_proxy.py" --runs "$HERE/runs" --upstream https://opencode.ai/inference/go \
  --account madesroches --auth oauth --tag go --harness-port 18082 --fontaine-port 18083 > "$HERE/runs/proxy-go.log" 2>&1 &
sleep 1; cat "$HERE/runs/proxy-kimi.log" "$HERE/runs/proxy-go.log"
