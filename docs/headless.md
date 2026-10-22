# Headless runs

`fontaine -p "task"` runs one task without a terminal and exits. It is the surface the CI
integration tests, the eval runner and any external consumer drive, so its output is a versioned
contract: [a checked-in JSON Schema](schemas/headless-v1.schema.json) describes every object it
prints.

```
fontaine -p "explain what main.rs does"
fontaine -p --output-format json "list the FIXME comments" | jq .result
echo "summarize the build" | fontaine -p
```

## Command line

`-p` / `--print` is a flag and the prompt is a positional argument, as in Claude Code. Without a
prompt argument the prompt is read from stdin; if stdin is a terminal the run fails at once with a usage error (exit 2). With one, text piped on stdin is appended
after the prompt on a new line, as in Claude Code; empty piped input leaves the prompt unchanged. If a pipe sends no data within 3 seconds, a warning goes to stderr and the run proceeds with the prompt argument alone; redirect `< /dev/null` to skip the wait. A prompt that is also a subcommand name, such as
`auth`, is taken as the subcommand; write `fontaine -p -- auth` to pass it as the prompt.

| Invocation | Meaning |
|---|---|
| `fontaine` | Starts the terminal UI; see [The terminal UI](tui.md). |
| `fontaine -p "task"` | Runs the task headless and prints the result on stdout. |
| `--permission-mode <manual\|acceptEdits\|plan\|auto\|dontAsk>` | Session mode, overriding `[permissions] default_mode`. There is no bypass mode. `auto` is a usage error (exit code 2), as is `default_mode = "auto"`, until the guardian ships. |
| `--model <account>/<model>` | Overrides `[model] model`. See [Configuration](configuration.md#accountsname). |
| `--output-format text\|json\|stream-json` | `text` prints the final answer, `json` one `result` object, `stream-json` one JSON object per line. |
| `--input-format text\|stream-json` | `stream-json` reads user messages as JSON lines on stdin. A prompt argument together with `stream-json` input is a usage error. |
| `--include-partial-messages` | With `stream-json` output, also emits `assistant_delta` events. |
| `--max-turns <n>` | Stops each turn after `n` model calls with `error_max_turns` (exit code 1). Without it a turn is capped at 1000 calls. |
| `--max-budget-usd <x>` | Stops the run once the main model's spend exceeds `x` with `error_max_budget_usd` (exit code 1). A model with no known price makes this a usage error (exit code 2). Only the main model's spend counts today. |
| `--json-schema <schema>` | An inline JSON Schema the final answer must follow. The parsed answer is returned in `result.structured_output`. A schema that is not a JSON object is a usage error. |
| `--allowed-tools <rule>` | Adds an allow rule in `Tool(specifier)` syntax for this session, such as `shell(npm test)`. Repeatable. Loaded as the CLI scope's `[permissions] allow`; an invalid rule is a usage error. |
| `--disallowed-tools <rule>` | Adds a deny rule for this session, such as `read(.env)`. Repeatable. See [Permissions](permissions.md#rules). |
| `--add-dir <path>` | Adds a directory to the workspace roots. Repeatable. See [Tools](tools.md#workspace-roots). |
| `--session-id <uuid>` | Uses this UUID as the session id. A value that is not a UUID is a usage error (exit code 2), and so is an id whose session directory already exists. |
| `--system-prompt <text>` | Replaces the base system prompt. |
| `--append-system-prompt <text>` | Appends to the system prompt after every other layer. |
| `--settings <file>` | A settings file layered over the others (CLI scope). |
| `--debug-file <path>` | Writes diagnostics to a local, owner-only file. |
| `fontaine auth login <account>` | Stores an account's credential. See [Credentials](configuration.md#credentials). |
| `fontaine auth login --telemetry` | Stores the telemetry ingestion key. |

The budget is checked between model calls. A call that pushes the spend over the cap stops the
run before its tool calls execute; an answer that was already complete is kept.

## Sessions

Each run is a session, named by a UUID: the one given with `--session-id`, or a fresh one. Its
files live in `<data dir>/sessions/<session id>/`, created readable by the current user only. The
conversation is kept there between turns, and it is compacted by a summarizing call when it
outgrows the model's context window (only when the window is known; the summarizing call counts
toward the spend and the model-call count). A compaction that cannot finish leaves the
conversation as it was, prints a warning on stderr and does not stop the run.

A separate, append-only ledger beside it records what the user said, verbatim, and what the
harness did. Compaction never touches it. See [Architecture](architecture.md#the-session-ledger).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | The run completed. Denied tool calls do not change this. |
| 1 | A run error: a provider or model failure, or a stop from `--max-turns` or `--max-budget-usd`. |
| 2 | A configuration or usage error. |

## Denials

Nothing prompts in a headless run. Every outcome that would ask a human, such as an `ask` rule, a
critical-path or protected-path call, or a call the mode does not pre-approve, becomes a denial
that is returned to the model as the tool result; the run continues and exits 0. External approval
delegates are not supported: there is no second approval path around the permission rules. See
[Permissions](permissions.md).

Every adjudicated call emits a `gate_decision` event, and the `result` event's `denials` lists each
denied call as `{tool, tier, rule, explanation}`, where `rule` names the rule and the scope it came
from when a rule decided and `explanation` is what the model was told.

## Output

Every JSON object carries `schema_version` (currently `1`) and `session_id`. Additive fields keep
the version; a rename or a removal bumps it. Consumers must ignore fields they do not know.

With `--output-format json`, the output is a single `result` object. With `stream-json`, each of
these is one line, in this order:

| `type` | When | Main fields |
|---|---|---|
| `init` | Once, first | `model`, `guardian_model` (null when unset), `guardian_available`, `guardian_profile`, `permission_mode`, `workspace_roots`, `config_sources` (scope, path, whether loaded), `workspace_trust`, `suspended_allow_rules`, `tools` |
| `assistant` | A complete assistant message | `text` |
| `assistant_delta` | With `--include-partial-messages`, for each text fragment | `text` |
| `tool_call` | The model called a tool | `tool_use_id`, `tool`, `args` |
| `gate_decision` | A tool call was adjudicated | `tool_use_id`, `tier`, `decision`, `rule`; for guardian decisions also `explanation`, `confidence`, `guardian_model` |
| `tool_result` | A tool answered | `tool_use_id`, `tool`, `success`, `text` (what the model received) |
| `result` | At the end of every turn | see below |

`tier` is the stage of the permission pipeline that decided: `deny_rule`, `ask_rule`,
`critical_path`, `protected_path`, `hook`, `read_only_fast_path`, `tier05`, `allow_rule`, `mode`,
`guardian`, `dont_ask`, `plan_block` or `human`. The audit log records the same values.

The `result` object:

| Field | Meaning |
|---|---|
| `subtype` | `success`, `error_max_turns`, `error_max_budget_usd` or `error_during_execution` |
| `is_error` | `false` only for `success` |
| `result` | The final answer text; empty for an error |
| `error` | A message for an error, otherwise null |
| `structured_output` | With `--json-schema`, the answer parsed as JSON |
| `num_turns` | Model calls made so far in the session |
| `duration_ms` | Wall time of the turn |
| `total_cost_usd` | Spend so far, or null when no price is known |
| `usage` | `main`, `guardian` and `subagents`, each with `input_tokens`, `output_tokens` and `cost_usd` |
| `denials` | One entry per denied call: `tool`, `tier`, `rule`, `explanation` |

Usage and cost accumulate over the session, so each turn's `result` reports the totals so far.

A `stream-json` run prints the `result` of every turn. A `json` or `text` run prints only the last
one. With `text`, an error goes to stderr and nothing goes to stdout.

## Multi-turn input

With `--input-format stream-json`, each line on stdin is one user message, in Claude Code's shape:

```json
{"type":"user","message":{"role":"user","content":"first question"}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"a follow-up"}]}}
```

Every message is one turn of the same conversation. The run ends at the end of stdin, or at the
first turn that fails. A line that is not a user message is a usage error (exit code 2). The input
line is part of the same schema.

```
printf '%s\n' '{"type":"user","message":{"role":"user","content":"hi"}}' \
  | fontaine -p --input-format stream-json --output-format stream-json
```

## Using it as a driver

The eval runner and the integration tests run `fontaine -p` against a scripted model endpoint, so
the output contract is what they assert on. To script a run, point an `openai-compatible` account
at a local server and pass `--output-format stream-json`; read the `init` line to confirm which
configuration files and workspace roots the run used, and the `result` line for the outcome.
