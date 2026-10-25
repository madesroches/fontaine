#!/usr/bin/env bash
# E3: one trigger per iteration mechanism (see the doc's experiment log).
set -u
cd "$(dirname "$0")/.."
export FIXTURE=repo-failing PATCHES="e3.patch.yml"
STALL=1 scripts/run_dsh.sh e3-stall "Fetch http://127.0.0.1:8099/ with the web_fetch tool and tell me exactly what the tool returned." 
scripts/run_dsh.sh e3-promote "Run ./slow-test.sh with the bash tool using timeoutMs 10000, then tell me exactly what the tool result said and what you did next."
scripts/run_dsh.sh e3-repeat "Run ./check.sh. It must pass before you stop. Do not edit it. Keep rerunning it with exactly the same command until it passes; if it never does after many tries, tell me."
scripts/run_dsh.sh e3-goal "Use create_goal to make a goal: 'make every test in this repo pass (npm test) by editing src/ only, then verify'. Then work on it."
# second-chance triggers, run after the first four did not fire repeat-tool-reminder / goal rounds
scripts/run_dsh.sh e3-repeat2 "Call the bash tool with the exact command './check.sh' and description 'run check', one tool call per assistant turn (never loop in the shell, never batch), 9 times in a row, changing nothing between calls. Then report what you saw after each call, quoting any extra message the system shows you."
scripts/run_dsh.sh e3-goal2 "Use create_goal to create a goal with objective 'make every test pass with npm test; fix src/slug.mjs and package.json'. Do NOT start the work. After create_goal returns, just reply 'goal created' and stop."
