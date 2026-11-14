#!/usr/bin/env bash
# Manual, never in CI: runs the consumer programs in rust/fontaine/tests/sdk_compat/ts against
# the real `claude` and writes scrubbed transcripts to rust/fontaine/tests/fixtures/claude_sdk/.
# Uses a small model and tiny prompts, and needs a signed-in `claude`.
#
#   evals/claude_sdk/capture.sh [scenario ...]    # default: every scenario below
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
ts="$root/rust/fontaine/tests/sdk_compat/ts"
out="$root/rust/fontaine/tests/fixtures/claude_sdk"
real="${REAL_CLAUDE:-$(command -v claude)}"
export REAL_CLAUDE="$real"
export SCENARIO_MODEL="${SCENARIO_MODEL:-haiku}"

[ -d "$ts/node_modules" ] || (cd "$ts" && npm ci)
mkdir -p "$out"

run() { # name, env assignments...
  local name="$1"; shift
  local work cap
  work="$(mktemp -d)"; cap="$(mktemp -d)"
  echo "== $name" >&2
  (
    cd "$work"
    env "$@" CAPTURE_DIR="$cap" FONTAINE_BIN="$here/tee_claude.sh" \
      timeout 150 node "$ts/${name%%@*}.mjs" > "$cap/report.json" 2> "$cap/stderr.txt" || true
  )
  for f in stdin stdout; do
    [ -f "$cap/$f.jsonl" ] || : > "$cap/$f.jsonl"
    SCRUB_CWD="$work" python3 "$here/scrub.py" < "$cap/$f.jsonl" > "$out/${name//@/_}.$f.jsonl"
  done
  SCRUB_CWD="$work" python3 "$here/scrub.py" < "$cap/report.json" > "$out/${name//@/_}.report.json" || true
  rm -rf "$work" "$cap"
}

all=(
  "query_stream"
  "interrupt@tool"
  "mid_turn@tool"
  "can_use_tool@allow"
  "can_use_tool@deny"
  "can_use_tool@update"
  "ask_question"
  "sdk_mcp"
  "hook@deny"
  "controls"
  "subagent"
)
want=("${@:-${all[@]}}")

for s in "${want[@]}"; do
  case "$s" in
    query_stream) run "$s" SCENARIO_PROMPT="say hi in two words" ;;
    interrupt@tool) run "$s" SCENARIO_PROMPT="run this exact shell command: python3 -c \"import time; time.sleep(25)\"" SCENARIO_AFTER="say done" ;;
    mid_turn@tool) run "$s" SCENARIO_FIRST="run this exact shell command: python3 -c \"import time; time.sleep(8); print(1)\"" SCENARIO_SECOND="after that, also say the word second" ;;
    can_use_tool@allow) run "$s" SCENARIO_MODE=allow SCENARIO_PROMPT="run the shell command: echo hi" ;;
    can_use_tool@deny) run "$s" SCENARIO_MODE=deny SCENARIO_PROMPT="run the shell command: echo hi" ;;
    can_use_tool@update) run "$s" SCENARIO_MODE=update SCENARIO_PROMPT="run the shell command: echo hi" \
      SCENARIO_UPDATED_PERMISSIONS='[{"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"allow","destination":"session"}]' ;;
    ask_question) run "$s" SCENARIO_PROMPT="use the AskUserQuestion tool to ask which colour I prefer, red or blue, then say it" ;;
    sdk_mcp) run "$s" SCENARIO_PROMPT="use the add tool from the host server on 2 and 3" ;;
    hook@deny) run "$s" SCENARIO_HOOK=deny SCENARIO_PROMPT="run the shell command: echo hi" ;;
    controls) run "$s" SCENARIO_NEW_MODEL=sonnet ;;
    subagent) run "$s" SCENARIO_PROMPT="use the helper agent to run: echo hi" ;;
    *) echo "unknown scenario $s" >&2; exit 2 ;;
  esac
done
