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
after the prompt on a new line, as in Claude Code; empty piped input leaves the prompt unchanged. If a pipe sends no data within 3 seconds, a warning goes to stderr and the run proceeds with the prompt argument alone; redirect `< /dev/null` to skip the wait.

| Invocation | Meaning |
|---|---|
| `fontaine` | Starts the terminal UI; see [The terminal UI](tui.md). |
| `fontaine -p "task"` | Runs the task headless and prints the result on stdout. |
| `--permission-mode <manual\|acceptEdits\|plan\|auto\|dontAsk>` | Session mode, overriding `[permissions] default_mode`. There is no bypass mode. A headless run starts in `manual` unless one of the two says otherwise; `auto` sends calls nothing pre-approved to the [guardian](guardian.md). |
| `--in-container` | States that the process runs inside a container; with `[guardian] profile = "minimal"` it lets the minimal guardian load when a runtime probe agrees. See [The guardian](guardian.md#the-minimal-profile). |
| `--model <account>/<model>` | Overrides `[model] model`. See [Configuration](configuration.md#accountsname). |
| `--output-format text\|json\|stream-json` | `text` prints the final answer, `json` one `result` object, `stream-json` one JSON object per line. |
| `--input-format text\|stream-json` | `stream-json` reads user messages as JSON lines on stdin. A prompt argument together with `stream-json` input is a usage error. |
| `--include-partial-messages` | With `stream-json` output, also emits `assistant_delta` events. |
| `--max-turns <n>` | Stops each turn after `n` model calls with `error_max_turns` (exit code 1). Without it a turn is capped at 1000 calls. |
| `--max-budget-usd <x>` | Stops the run once the session's spend, the main model plus the guardian and subagents, exceeds `x` with `error_max_budget_usd` (exit code 1). A model with no known price makes this a usage error (exit code 2). |
| `--json-schema <schema>` | An inline JSON Schema the final answer must follow. The parsed answer is returned in `result.structured_output`. A schema that is not a JSON object is a usage error. |
| `--allowed-tools <rule>` | Adds an allow rule in `Tool(specifier)` syntax for this session, such as `shell(npm test)`. Repeatable. Loaded as the CLI scope's `[permissions] allow`; an invalid rule is a usage error. |
| `--disallowed-tools <rule>` | Adds a deny rule for this session, such as `read(.env)`. Repeatable. See [Permissions](permissions.md#rules). |
| `--cd <dir>`, `-C <dir>` | Starts in this directory instead of the launch directory. It becomes the project directory: project settings, trust, workspace root and session identity come from it. Fontaine changes its own working directory to it, so relative `--settings`, `--add-dir` and `--debug-file` values and spawned processes resolve against it as if launched there. A directory that does not exist is a usage error (exit code 2). |
| `--add-dir <path>` | Adds a directory to the workspace roots. Repeatable. See [Tools](tools.md#workspace-roots). |
| `--session-id <uuid>` | Uses this UUID as the session id. A value that is not a UUID is a usage error (exit code 2), and so is an id whose session directory already exists. With `--continue` or `--resume` it is accepted only together with `--fork-session`, where it names the copy; otherwise it is a usage error. |
| `--continue`, `-c` | Reopens the most recently updated session that was started in the project directory (the launch directory, or `--cd`); starts a fresh session when there is none. |
| `--resume <id-or-name>`, `-r <id-or-name>` | Reopens the session with this id, or else the session started in the project directory (the launch directory, or `--cd`) that has this name (compared without regard to case). A value that matches no session is a usage error (exit code 2), and so is a name carried by several sessions: the error lists their ids. The value is required, so `fontaine -p --resume <id> "task"` reads `task` as the prompt. |
| `--name <name>`, `-n <name>` | Names the session being started, so `--resume` can find it by name. With `--fork-session` it names the copy; with `--continue` or `--resume` alone it renames the session reopened. A name is 1 to 80 characters with no control characters; anything else is a usage error. A fork the flag does not name is called the source's name followed by ` (branch)` when the source has one, so a name finds one session. |
| `--fork-session` | With `--continue` or `--resume`, starts a new session that is a copy of the one that would have been reopened. Alone it is a usage error. |
| `--system-prompt <text>` | Replaces the base system prompt. |
| `--append-system-prompt <text>` | Appends to the system prompt after every other layer. |
| `--settings <file>` | A settings file layered over the others (CLI scope). |
| `--debug-file <path>` | Writes diagnostics to a local, owner-only file. |

A headless run takes its credentials from `api_key_env` or from a `credentials.toml` already in the
data directory; it never prompts. Store them in the terminal UI with `/login`, or provision the
file in CI. See [Credentials](configuration.md#credentials).

The budget is checked between model calls, and before each goal evaluation and goal turn. A call that pushes the spend over the cap stops the
run before its tool calls execute; an answer that was already complete is kept. The spend includes
the guardian's calls, the goal evaluator's calls and the subagents' model calls.

## Scheduled prompts

A headless run exits after its answer, so a scheduled task could never fire: the scheduling tools
(`cron_create`, `cron_jobs`, `cron_delete`, `schedule_wakeup`) are not registered and `/loop` does
not exist. See [Scheduled prompts](tui.md#scheduled-prompts) for the terminal UI.

## Goals

`-p "/goal <condition>"` runs a [goal](tui.md#goals) to its end in one invocation. It is the only
slash command headless understands, and only as a message that starts with `/goal` followed by a
space or the end of the text. The first turn works on the condition; after each turn the evaluator
(the guardian's model) judges it, and *not met* runs another turn, until the condition is *met* or
*impossible*, or the goal pauses on `[goal] max_turns`, `[goal] max_cost_usd`, `--max-turns`,
`--max-budget-usd`, the no-progress cap, a failed turn or an evaluator that gave no verdict. The
spend of the evaluator counts toward `--max-budget-usd` and `total_cost_usd`.

`-p "/goal"` prints the goal's status and `-p "/goal clear"` removes it; neither calls a model, and
the `result` has `num_turns` 0. A goal restored by `--continue` or `--resume` is evaluated after the
message's turn ends, whatever the message was.

With `stream-json`, each evaluation prints a `goal` event, and the `result` gains a `goal` object
(see [Output](#output)). The exit code is 0 only when the goal ended *met*; a goal that ended
*impossible* or paused exits 1, with `subtype` unchanged. With `--output-format text`, such a run
prints `fontaine: goal impossible: <reason>` or `fontaine: goal paused: <why>` on stderr instead of
the last answer.

## Sessions

Each run is a session, named by a UUID: the one given with `--session-id`, or a fresh one. Its
files live in `<data dir>/sessions/<session id>/`, created readable by the current user only. The
conversation, the task list (`todos.json`) and the [goal](#goals) (`goal.json`) are kept there between turns, and the conversation is compacted by a summarizing call when it
outgrows the model's context window less a reserve, or earlier when
[`[context] autocompact_window`](configuration.md#context) is set (only for a model the catalog
lists, and, without that setting, only when its window is known; the summarizing call counts
toward the spend and the model-call count). A compaction that cannot finish leaves the
conversation as it was, prints a warning on stderr and does not stop the run.

A separate, append-only ledger beside it records what the user said, verbatim, and what the
harness did. Compaction never touches it. See [Architecture](architecture.md#the-session-ledger).

### Continuing a session

`--continue` and `--resume <id-or-name>` reopen a session: the conversation and the ledger come back, and
the new messages continue both. The run starts in the permission mode the settings and flags
resolve to, as a new session does, not in the mode the session ended in, and none of the earlier
session's "allow session" grants carry over. `--fork-session` copies both stores under a new id
(`--session-id` chooses it) and leaves the original untouched; the audit log of the copy starts
empty. `--continue` and a `--resume` by name look only at sessions whose primary workspace root is
the project directory; `--resume` by id takes a session of any directory.

## Subagents

When the main conversation has given its final answer but background
[subagents](agents.md#watching-and-stopping-subagents) are still running, the run waits for them.
Their reports reach the main conversation as notifications, each batch in a further turn, until
none is running; then the `result` is emitted. `--max-turns` and `--max-budget-usd` count those
turns and the subagents' spend, and a run stopped by either kills the running subagents and drops
their reports.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | The run completed. Denied tool calls do not change this. |
| 1 | A run error: a provider or model failure, a stop from `--max-turns` or `--max-budget-usd`, or a [goal](#goals) that ended impossible or paused. |
| 2 | A configuration or usage error. |

## Questions

`ask_user_question` stays in the tool list, but nobody can answer in a headless run: the call
returns "User input is unavailable in this session; proceed with your best judgment and state your
assumptions." and the run continues.

## Denials

Nothing prompts in a headless run. Every outcome that would ask a human, such as an `ask` rule, a
critical-path or protected-path call, or a call the mode does not pre-approve, becomes a denial
that is returned to the model as the tool result; the run continues and exits 0. External approval
delegates are not supported: there is no second approval path around the permission rules. See
[Permissions](permissions.md).

In `auto` the guardian answers instead, and a deny comes back to the model as
`Denied by guardian: [Rule] explanation`. An `escalate` verdict is a denial, and so is every
adjudication after three denials in a row or twenty in the session (the pause that a person would
resolve by approving a prompt). When the guardian model cannot be used at all, every adjudication
is a denial with the rule `GuardianUnavailable`; the `init` event reports `guardian_available:
false` and a line on stderr names the model and the reason, so the run does not look like a
guardian that judged everything unsafe.

Warnings about the configuration that do not stop the run, such as an additional directory that
was skipped, go to stderr as `fontaine: warning: ...` before the model is resolved, so a run that
then exits early (a usage error, for instance) still shows them. In `auto` and `plan` a guardian
running on the main model prints its notice as one line, headline and advice joined by a dash.

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
| `init` | Once, first | `model`, `guardian_model` and `guardian_profile` (the guardian and profile in force, which may differ from the configured ones: a default pick, the main model, or `full` after a failed containment check), `guardian_available`, `permission_mode`, `workspace_roots` (the session's [scratch directory](tools.md#the-scratch-directory) is the last entry when it could be created), `config_sources` (scope, path, whether loaded), `workspace_trust`, `suspended_allow_rules` (the broad allow rules `auto` sets aside; empty outside `auto`), `tools` |
| `assistant` | A complete assistant message | `text` |
| `assistant_delta` | With `--include-partial-messages`, for each text fragment | `text` |
| `tool_call` | The model called a tool | `tool_use_id`, `tool`, `args` |
| `gate_decision` | A tool call was adjudicated | `tool_use_id`, `agent_id` (`root` for the main conversation, `agent-N` for a subagent), `tier`, `decision`, `rule`; for guardian decisions also `explanation`, `confidence`, `guardian_model` and, when the guardian failed closed, `fail_closed_reason` (`error`, `timeout`, `unparseable`, `low_confidence` or `unavailable`); when a `[guardian.screen]` stage scored the call also `screen_risk`, `screen_allow_risk` and `screen_steering` (the allow reads, when they ran) and `screen_outcome` (`stop`, `allow`, `pass` or `unavailable`) |
| `tool_result` | A tool answered | `tool_use_id`, `tool`, `success`, `text` (what the model received) |
| `goal` | The evaluator judged a [goal](#goals) | `verdict` (`met`, `not_met`, `impossible`, or `failed` when no verdict arrived), `reason`, `evaluations` (verdicts so far) |
| `result` | At the end of every turn | see below |

`tier` is the stage of the permission pipeline that decided: `deny_rule`, `ask_rule`,
`critical_path`, `protected_path`, `hook`, `read_only_fast_path`, `tier05`, `allow_rule`, `mode`,
`screen`, `guardian`, `dont_ask`, `plan_block` or `human`. The audit log records the same values.

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
| `total_cost_usd` | Spend so far, main model plus guardian plus goal evaluator plus subagents, or null when none is priced |
| `usage` | `main`, `guardian` and `subagents`, each with `input_tokens`, `output_tokens` and `cost_usd`; `main` also has `cache_read_input_tokens` and `cache_creation_input_tokens`, the parts of its input read from and written to the provider's prompt cache (0 when the provider reports none); `guardian` totals every adjudication that reached the model, subagents' included (cached verdicts cost nothing); `subagents` is their model calls; `goal` is the goal evaluator's calls, including attempts that gave no verdict |
| `denials` | One entry per denied call: `tool`, `tier`, `rule`, `explanation` |
| `goal` | Only when a goal was set, continued, queried or cleared: `condition`, `status` (`active`, `met`, `impossible`, `paused` or `cleared`), `evaluations` and `reason` (the evaluator's last reason, or why it paused) |

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
