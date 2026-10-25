#!/usr/bin/env bash
# E1: dump the composed headless plugin tree (no network, no credentials).
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd); R=$HERE/runs/e1-dump
mkdir -p "$R/dsh-home"; cp "$HERE/scripts/model.patch.yml" "$R/"; chmod -R a+rwX "$R"
for f in ${@:-}; do :; done
docker run --rm --network none --user 1000:1000 -v "$R:/run/study" -e DSH_HOME=/run/study/dsh-home -e HOME=/home/node \
  -e DEEPSEEK_API_KEY=dsh-dummy-key -e TSX_TSCONFIG_PATH=/dsh/tsconfig.base.json -w /run/study \
  dsh-study:22ddee37 node --import /dsh/node_modules/tsx/dist/esm/index.mjs /dsh/apps/cli/src/bin.ts \
  --profile headless --patch /run/study/model.patch.yml --dump-config > "$R/config.yml" 2> "$R/err.txt"
