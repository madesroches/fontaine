#!/usr/bin/env bash
# Scores the hosted System One screen models on the employee-levels question, two runs each.
# Manual and billed: not part of CI. Usage (from rust/): ../evals/screen_models.sh KEY_FILE [MAX_COST_USD]
set -euo pipefail

key_file=${1:?usage: screen_models.sh KEY_FILE [MAX_COST_USD]}
cap=${2:-1}
out=../evals/scorecards

models=(
  typesafe/jev-1.14
  baseten/joey-1
  respan/span-02
  perplexity/pplx-decider-flash-v1.2
  cloudflare/clef-flash
  liquid/d1.1
)

for model in "${models[@]}"; do
  slug=${model#*/}
  # The committed Jev scorecard stays as the earlier measurement; this session's control is separate.
  [[ $model == typesafe/* ]] && slug=$slug-control
  for suffix in "" "-r2"; do
    cargo run -q -p fontaine-evals -- --dataset ../evals/dataset \
      --guardian "screen:$model" --screen-url https://openrouter.ai/api \
      --screen-framing employee-levels --screen-api-key-file "$key_file" \
      --screen-max-cost-usd "$cap" \
      --out "$out/screen-$slug-employee-levels$suffix.json" || echo "run failed: $model$suffix" >&2
  done
done
