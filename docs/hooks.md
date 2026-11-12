# User hooks

A hook is a handler you configure to look at a tool call, a prompt or a moment in the life of a
session. Hooks are how you add your own checks, rewrite arguments, keep a record and feed the
agent context without changing Fontaine. They follow Claude Code's hooks: the event arrives as
JSON, the answer is JSON, and exit code 2 blocks. A hook can add context or block; it can never
widen a permission, and a repository can only tighten.

A handler is a shell [`command`](#command-handlers), an [`http`](#http-handlers) endpoint or a
[`prompt`](#prompt-handlers) put to the guardian's model. Not every event takes every handler.

| Event | When | What it can do | Handlers |
|---|---|---|---|
| `pre_tool_use` | At the start of the [pipeline](permissions.md#the-pipeline), before the floor. | Deny, ask or allow the call, rewrite its arguments, add a note for the guardian. | command, http, prompt |
| `post_tool_use` | After the tool ran and succeeded, in the result pipeline. | Add a note to the result the agent reads. | command, http, prompt |
| `post_tool_use_failure` | After the tool failed. | Add a note to the error the agent reads. | command, http, prompt |
| `permission_denied` | When the pipeline denied the call. | Say the agent may retry a call the guardian denied. | command, http, prompt |
| `task_created` | Before `board_update` creates a task. | Block the create. | command, http, prompt |
| `task_completed` | Before `board_update` completes a task. | Block, keeping the task open. | command, http, prompt |
| `teammate_idle` | When an agent's turn ends while it holds a task in progress on its parent's board. | Block, keeping the agent working. | command, http, prompt |
| `session_start` | When a session starts, after `/clear` and after a compaction. | Add context for the next prompt. | command |
| `session_end` | When the session ends and before `/clear`. | Nothing: it is told. | command, http |
| `user_prompt_submit` | When you send a prompt. | Block the prompt, or add context to it. | command, http, prompt |
| `stop` | When the main agent's turn ends with an answer. | Keep the agent working. | command, http, prompt |
| `stop_failure` | When the main agent's turn ends in an error. | Nothing: it is told. | command, http |
| `subagent_start` | Before a subagent first runs. | Add context ahead of its task. | command, http |
| `subagent_stop` | When a subagent finishes a run. | Keep the subagent working. | command, http, prompt |
| `pre_compact` | When a compaction is about to ask the model for a summary. | Block it, or add focus to the summary. | command, http |
| `post_compact` | After a compaction saved its summary. | Nothing: it is told. | command, http |
| `notification` | When an approval or a question waits for you in the terminal UI. | Nothing: it is told. | command, http |

In the terminal UI, `/hooks` lists the configured hooks by event with their type and scope, marks
the ones that do not run and why, and names the events that have none.

## Configuring hooks

```toml
[[hooks.pre_tool_use]]
matcher = "shell(git *)"
command = "/home/me/.config/fontaine/hooks/git-guard.sh"
timeout_secs = 10

[[hooks.post_tool_use]]
matcher = "edit"
command = "/home/me/.config/fontaine/hooks/format.sh"
```

| Key | Meaning | Default |
|---|---|---|
| `type` | `command`, `http` or `prompt`. | `command` |
| `matcher` | What the hook runs for; see [Matchers](#matchers). | none: every call or every time |
| `command` | `command` hooks: the command line, run by the shell: `sh -c` (through Git Bash on Windows, found the same way as for the `shell` tool, including `FONTAINE_GIT_BASH_PATH`); the command reaches Git Bash as written, including every `\\`. On Windows without Git Bash it runs in PowerShell instead, and a `pre_tool_use` hook then fails closed: one that exits with anything other than 0 or 2, cannot start or times out denies the call with a reason saying so. Required for `command`. | none |
| `url` | `http` hooks: the endpoint the event is POSTed to. Required for `http`. | none |
| `headers` | `http` hooks: a table of extra request headers. `$VAR` and `${VAR}` in a value expand only for variables named in `allowed_env_vars`; any other name becomes empty. | none |
| `allowed_env_vars` | `http` hooks: the environment variables `headers` may read. It may not name a variable the `shell` tool's scrub removes (an API-key variable of an account, a telemetry key, ...): that is a startup error. | none |
| `prompt` | `prompt` hooks: the question put to the model. `$ARGUMENTS` stands for the event; without it the event is appended. Required for `prompt`. | none |
| `timeout_secs` | At least 1. A command hook that takes longer is cut off: its process tree is stopped, not just the shell (on Windows, a descendant started before the command joins its Job Object can escape). | `30`; `session_end` has `1.5` seconds |

A hook has exactly the fields of its type, and the event must take its type: anything else is a
load error naming the key.

A hook without a `matcher` runs for every call and counts as broad, which matters for `allow` in
`auto` (below). A tool matcher is matched the way a deny or ask rule is, failing toward matching,
so a hook meant to tighten runs for every call it could concern; the commands run in the primary
workspace root.

### Matchers

| Events | The matcher selects on |
|---|---|
| `pre_tool_use`, `post_tool_use`, `post_tool_use_failure`, `permission_denied` | The tool, in the `Tool(specifier)` rule form. |
| `session_start` | `startup`, `resume`, `fork`, `clear` or `compact`. |
| `session_end` | `clear`, `resume`, `prompt_input_exit` or `other`. |
| `stop_failure` | The error kind: `rate_limit`, `overloaded`, `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error` or `unknown`. |
| `subagent_start`, `subagent_stop` | The agent type name. |
| `pre_compact`, `post_compact` | `manual` (`/compact`) or `auto` (the threshold). |
| `notification` | `permission_prompt` (an approval) or `elicitation_dialog` (a question). |
| `user_prompt_submit`, `stop`, `task_created`, `task_completed`, `teammate_idle` | Nothing: a `matcher` is a load error. |

Outside the tool events a matcher is a `|`-separated list of exact names; an omitted matcher, `""`
and `*` match everything. A name outside the event's set is a load error.

Hooks come from any scope. Hooks of different scopes are all kept; the same hook defined twice is
kept once, under the scope with more trust. No setting in a scope turns off the hooks of another.

## What the hook receives

One JSON object on standard input:

| Field | Value |
|---|---|
| `session_id` | The session id. |
| `cwd` | The primary workspace root. |
| `hook_event_name` | The event's name as Claude Code writes it: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`, `SessionStart`, `Stop`, ... |
| `tool_name` | Fontaine's tool name (`shell`, `read`, `mcp__github__create_issue`, ...). |
| `tool_input` | The call's arguments. |
| `tool_use_id` | The model's id for the call, when there is one. |
| `agent_id` | `root`, or the id of the subagent that made the call. |
| `permission_mode` | The mode the call is judged under, by its Fontaine name (`manual`, `acceptEdits`, `plan`, `dontAsk`, `auto`). |
| `workspace_roots` | Every workspace root, including the session's [scratch directory](tools.md#the-scratch-directory), which comes after the startup roots and before any `/add-dir` root. |
| `tool_response` | `post_tool_use` only: `{"is_error": false, "output": "..."}`. |
| `error` | `post_tool_use_failure` only: the error text the agent reads. |
| `reason`, `tier` | `permission_denied` only: why, and which part of the pipeline denied it. |

The task-board events send only `session_id`, `cwd`, `hook_event_name` and `agent_id` (for `task_completed`, the agent completing the task, which is not always its owner), plus their own
field (see [below](#task-boards-task_created-task_completed-and-teammate_idle)): they carry no
tool. The other lifecycle events send `session_id`, `cwd`, `hook_event_name`, `agent_id`,
`permission_mode` and `workspace_roots`, plus their own fields (see [Lifecycle events](#lifecycle-events)).
An `http` hook gets the same object as the request body, and a `prompt` hook as data inside an
untrusted block.

There is no `transcript_path`: Fontaine keeps no transcript file. The environment variable
`FONTAINE_PROJECT_DIR` holds the primary root.

The hook runs with the same scrubbed environment as the `shell` tool: the variables named by
`api_key_env` of any account, `[telemetry] api_key_env`, `[guardian] scanner_hf_token_env`, the
`MICROMEGAS_*` family and the OTLP header variables are not passed. The entries of
[`[env]`](configuration.md#env) are set on it, after that scrub and before `FONTAINE_PROJECT_DIR`.

## What the hook answers

Every handler answers in the same shape. A `command` hook exits 0 and writes nothing (no opinion),
one JSON object or, for the events that take it, plain text; an `http` hook answers 2xx with an
empty body or one JSON object; a `prompt` hook is [asked for a verdict](#prompt-handlers). The
object's `hookSpecificOutput` holds the answer; `hookEventName`, when present, must name the event.

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "no pushes on Fridays",
    "updatedInput": { "command": "git push --dry-run" },
    "guardian_context": "the user runs this hook to keep Friday releases quiet"
  }
}
```

| Event | Fields |
|---|---|
| `pre_tool_use` | `permissionDecision` (`allow`, `deny` or `ask`), `permissionDecisionReason`, `updatedInput` (an object that replaces the call's arguments), `guardian_context` (a note for the guardian). |
| `post_tool_use`, `post_tool_use_failure` | `additionalContext`, a string added after the tool's result or error. |
| `permission_denied` | `retry`, a boolean. A `command` or `http` hook only; a `prompt` hook's answer is discarded. |
| the other events | See [Lifecycle events](#lifecycle-events). |

Any handler can also block with `{"decision": "block", "reason": "..."}`. Exit code 2 blocks too,
with the hook's standard error as the reason. A `pre_tool_use` block is a deny; for `post_tool_use`
the reason is added to the result, since the call already ran. Any other exit code, a non-2xx
status, a connection error or an unreadable answer is a non-blocking error: the hook decides
nothing and the call goes on through the normal pipeline.

A `systemMessage` string in the answer is shown to you as a warning, on every event except
`notification`, `session_end`, `stop_failure`, `pre_compact` and `post_compact`. The fields
`continue`, `stopReason`, `suppressOutput` and `terminalSequence` are ignored: **a hook never ends
the turn**.

Text a hook adds for the model (`additionalContext`, plain standard output, a block reason fed
back to the agent) is cut at 10,000 characters with a marker, where Claude Code saves the rest to
a file.

## How hooks compose and where they sit

- **`pre_tool_use` runs first.** If several hooks answer, **deny beats ask beats allow**. The last
  `updatedInput` in configured order wins, and the rewritten call is what every later step judges:
  the floor, the pre-classifier, the rules, the guardian and the audit log, which records the
  digest of the original arguments too.
- **`deny`** denies the call. **`ask`** forces the human prompt like an `ask` rule (a denial in
  `dontAsk` and headless runs).
- **`allow` is an allow rule, no more.** It acts after the floor and never past it: a deny rule, an
  ask rule, a critical path or a protected path still stands. It also only counts where an allow
  rule's matcher would match, so a `shell(git *)` hook does not allow `ls && git push`. In `auto`
  a hook with a narrow matcher skips the guardian, and a hook with a broad one (no matcher, or one
  that is a [suspended allow rule](permissions.md#allow-rules)) is suspended and the guardian
  decides; Claude Code honors a broad hook, so this is stricter. An `allow` stands only for the arguments the hook saw: if a
  later hook rewrites the call with `updatedInput`, the allow is dropped and the call takes the
  normal path (a hook that rewrites and allows in one reply allows its own rewrite).
- **`guardian_context`** is shown to the guardian inside an untrusted block, as a note and never as
  an instruction.
- **`post_tool_use`** runs last in the result pipeline, after anything that screens tool results,
  and its `additionalContext` lands after the tool's output. It fires only for a call that
  succeeded; a failed call fires `post_tool_use_failure`, which adds to the error the agent reads.
  Background job output reaches them like any result.
- **`permission_denied`** runs for every denial the pipeline makes, whatever the mode. A hook's
  `retry: true` makes the denied call run **once** if the agent sends exactly the same call again,
  as `/denied` followed by `r` does, and the agent is told so. It can only do that for a call the
  guardian denied: the floor and the mode have no retry to give, so for them the hook only gets to
  hear about it.

Every subagent tool call runs the same hooks.

## Task boards: `task_created`, `task_completed` and `teammate_idle`

These three events are quality gates for [task boards](agents.md#task-boards) and exist only while
`[agents] teams` is on (the default). They are configured like the others, without a `matcher`, which is
rejected at load because the events have no tool name:

```toml
[[hooks.task_completed]]
command = "/home/me/.config/fontaine/hooks/tests-green.sh"

[[hooks.teammate_idle]]
command = "/home/me/.config/fontaine/hooks/anything-left.sh"
timeout_secs = 20
```

| Event | Fires | Extra input | A block |
|---|---|---|---|
| `task_created` | Before `board_update create` takes the board lock. | `task` (the task object without `id`, which the create assigns), and Claude Code's `task_subject`, `task_description` and `teammate_name`. | The task is not created and the reason is the error the agent reads. |
| `task_completed` | Before `board_update complete` takes effect on any board, with no board lock held. | `task`: the task object (`id`, `title`, `description`, `status`, `owner`, `blocked_by`, `created_by`). | The task stays in progress and the reason is the error the owner reads. |
| `teammate_idle` | After an agent's turn ends while it owns an in-progress task on its parent's board, before it is marked idle and before its report is delivered. It never fires for an agent holding no such task. | `teammate_name` (the agent's name). | The agent is resumed with the reason as feedback instead of going idle. It is a gate on unfinished work and never hands out new work. |

The answer is Claude Code's: exit code 2 (or `decision: "block"`, or a prompt hook's `ok: false`) blocks and the reason is what the agent reads. A hook has no
`allow` answer; nothing it says can loosen anything, it can only add work, and a JSON answer of
`allow` is ignored. Several hooks run in configured order and the first to block decides.

- **A failing gate does not block.** A hook that cannot start, exits with another code, or runs
  past its timeout warns you once and blocks nothing: these are quality gates, not security gates,
  and one that failed closed forever would wedge the agents.
- **A gate that always blocks is bounded.** A `teammate_idle` block resumes an
  agent at most three times in a row without the board that holds its task changing (a create,
  claim, complete or release on it resets the count), then it goes idle with a warning.
- **Where they may come from.** Your user config, `--settings` and plugins can define both. A
  repository's `task_completed` hook loads like its other hooks (after workspace trust, and it
  counts in the trust prompt) and can only keep a task open. A repository's `teammate_idle` hook is
  ignored with a warning, because forcing more model calls is spend, not tightening. A plugin's
  `hooks.json` names the events `TaskCreated`, `TaskCompleted` and `TeammateIdle`; a matcher on
  them is ignored.

## Handler types

### Command handlers

The default. The command runs with the event on standard input and answers with exit code and
standard output, as above.

### HTTP handlers

```toml
[[hooks.session_end]]
type = "http"
url = "https://hooks.example.com/fontaine"
headers = { Authorization = "Bearer $HOOK_TOKEN" }
allowed_env_vars = ["HOOK_TOKEN"]
```

The event is POSTed as the JSON body with `Content-Type: application/json` and your headers, on a
client that follows `[network]`. Only the variables named in `allowed_env_vars` expand in a header
value; any other `$NAME` becomes the empty string, so a hook cannot read the rest of your
environment. A 2xx answer with an empty body has no opinion, and one with a JSON object is the
answer. Any other status, a connection error, a timeout or a body that is not a JSON object is a
failure that decides nothing. An `http` hook sends session data off your machine, so a repository
cannot define one (see [below](#hooks-a-repository-defines)).

### Prompt handlers

```toml
[[hooks.stop]]
type = "prompt"
prompt = "Did the agent run the tests after its last edit? $ARGUMENTS"
```

A `prompt` hook is one question to the guardian's model, so it forces model spend and a repository
cannot define one. `$ARGUMENTS` in the prompt is replaced by the event, as data in an untrusted
block that the model is told never to obey; without it the event is appended. The model answers
`{ "ok": bool, "reason": string, "impossible": bool }`. `ok: false` blocks, with the reason, through
the event's usual block path; `ok: true` has no opinion and never allows anything. For `stop` and
`subagent_stop`, `impossible: true` lets the agent stop: what the prompt asks cannot be met by more
work. The usage counts with the guardian's. A model that is unavailable, slow or unreadable is a
failure that decides nothing. There is no `model` key: it runs on the guardian's model.

## Lifecycle events

Context a hook adds is delivered as a harness note, `[Hook context from <event>, not the user's
words: ...]`, in the next model request. It is never recorded as something you said, so a hook
can never become the intent the guardian reads.

| Event | Fires | Extra input | A hook can |
|---|---|---|---|
| `session_start` | After the session starts, after `/clear` (`clear`) and after a compaction that saved (`compact`). The source is `startup`, `resume`, `fork`, `clear` or `compact`. | `source`, `model` | Write `additionalContext` or plain standard output: it goes ahead of the next prompt. After an automatic compaction it is added to the very request that triggered it. |
| `session_end` | When the terminal UI ends (`prompt_input_exit`, or `resume` when it switches session), before `/clear` (`clear`), and when a headless run ends (`other`). | `reason` | Nothing. It has 1.5 seconds. On a quit it starts after the session's work has been cancelled and shares the two-second exit budget with the rest of the cleanup, so it can be cut off earlier than its own timeout. |
| `user_prompt_submit` | When you send a prompt: typed, a skill line, `/goal <condition>`, `/init`, the first `/loop` iteration, or a headless `-p` prompt. | `prompt` | Block (the prompt is not sent and nothing is recorded; the reason is shown to you), or add context with `additionalContext` or plain standard output. |
| `stop` | When the main agent's turn ends with an answer, before a [`/goal`](tui.md) evaluation. | `stop_hook_active`, `last_assistant_message` | Block, or add `additionalContext`: the agent works on, with the reason as a harness note. |
| `stop_failure` | When the main agent's turn ends in an error. | `error` (the kind), `error_details`, `last_assistant_message` (always empty: Fontaine has no answer text for a failed turn) | Nothing. |
| `subagent_start` | Before a subagent's first run, with [agents](agents.md#subagent-hooks) on or off. | `agent_type` | Write `additionalContext`: it goes ahead of the subagent's task. |
| `subagent_stop` | When a subagent finishes a run, after its mail is read and before `teammate_idle`. | `stop_hook_active`, `agent_type`, `last_assistant_message` | Block or add `additionalContext`: the subagent is resumed with the reason. |
| `pre_compact` | When a compaction has something to summarize, just before the model is asked. Nothing fires when the conversation is under the threshold. | `trigger`, `custom_instructions` (the `/compact` focus, or null) | Block (the compaction is skipped, with a warning), or write plain standard output, which is added to the summary request as `Additional focus: ...`. |
| `post_compact` | After the summary was saved. | `trigger`, `compact_summary` | Nothing. |
| `notification` | When an approval or a question starts waiting for you in the terminal UI, whatever `[tui] notify` is set to. It runs in the background. | `message`, `title`, `notification_type` | Nothing. |

A `user_prompt_submit` hook does not fire for turns Fontaine starts itself (a scheduled task, a
subagent's report, a goal or stop continuation), for text you steer a running turn with, for text
typed to a subagent, or for the free-text "Other" answer to a question.

In a headless run, a blocked `user_prompt_submit` ends the run before any model call with exit code
0 and a `success` result: the block message followed by `Original prompt: <the prompt>`.

### Loop bounds

A `stop` or `subagent_stop` hook that keeps blocking is let go after **8 continuations in a row**,
with a warning, and the turn or the agent ends. An agent that made tool calls since the previous
stop is working, not looping, and starts the count again; so does a prompt you send. The input
field `stop_hook_active` is `true` on every continuation after the first.

On a turn's end the `stop` hooks run first; only when none blocks does a goal evaluation run.

## Failure never loosens

A hook that cannot start, exits with a code other than 0 or 2, runs past its timeout, or writes
anything but the JSON above decides nothing. It cannot make a call run that would not have. The
failure of a `pre_tool_use`, `post_tool_use`, `post_tool_use_failure` or `permission_denied` hook is only logged to the
debug file; the failure of any other event's hook warns you. Hooks are your own processes, so the pipeline does
not gate them, and they are not tool calls.

## Hooks a repository defines

Hooks in `.fontaine/settings.toml` or `.fontaine/settings.local.toml` are executables the
repository chose:

- In the terminal UI they start only after you trust the workspace; see
  [Workspace trust](tui.md#workspace-trust). A headless run loads them without asking, as Claude
  Code's `-p` does.
- **They can only tighten.** A repository hook's `allow` and its `retry` are ignored; its `deny`,
  `ask`, `updatedInput`, `guardian_context` and `additionalContext` work as for any hook. An
  untracked `settings.local.toml` of a trusted root is your own file, and its hooks' `allow` counts.
- **`http` and `prompt` hooks and the events that force model calls are ignored.** A repository's
  `http` or `prompt` hook, and its `stop`, `subagent_stop` and `teammate_idle` hooks, are ignored
  with a warning at startup: they send session data off your machine or spend model calls, and a
  repository may only tighten. Claude Code loads them.

| Where the hook is defined | `command` | `http`, `prompt` | `stop`, `subagent_stop`, `teammate_idle` |
|---|---|---|---|
| Your user config, `--settings` | runs | runs | runs |
| An untracked `settings.local.toml` of a trusted root | runs | runs | ignored |
| `.fontaine/settings.toml` | after workspace trust | ignored | ignored |
| A plugin | runs | runs | runs |

## Hooks a plugin defines

A [plugin's](plugins.md) `hooks/hooks.json` loads as hooks and starts without the
workspace-trust gate: install approved them, `http` and `prompt` hooks included. They can only
tighten, like a repository's hooks: their `allow` and `retry` never count, while `deny`, `ask`,
`updatedInput`, `guardian_context` and `additionalContext` work as usual. Claude Code's matchers
over tool names map to Fontaine rules where they can; an un-mappable matcher skips its hook entry
with a warning. [Plugins](plugins.md#component-mapping) lists what loads and what is skipped.

A plugin's hooks are a layer of their own: `/plugin install`, `/plugin update`, `/plugin
enable` and `/reload-plugins` in a running session load, replace or remove them for every event,
without a restart, while your own and your project's hooks stay as they were at startup. The
floor order is unchanged, and a plugin hook's `allow` and `retry` still never count. A plugin
loaded this way does not run its `session_start` hooks, since that event already happened in this
session; they run at the next session. See [Plugins](plugins.md#in-the-terminal-ui).

## What is not built

- **Events:** `PermissionRequest` (an `allow` there would answer the human prompt, a second way to
  loosen next to `pre_tool_use`), `Setup`, `UserPromptExpansion`, `PostToolBatch`,
  `MessageDisplay`, `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `DirectoryAdded`,
  `FileChanged`, `WorktreeCreate`, `WorktreeRemove`, `PreModelSwitch`, `PostModelSwitch`,
  `Elicitation` and `ElicitationResult`. A plugin that names one loads with a warning.
- **Handler types:** `agent` and `mcp_tool`. An `mcp_tool` hook would call a tool outside the
  per-call gate. A plugin hook of those types is skipped with a warning.
- **Handler fields:** `args`, `if` and `shell: "powershell"` skip a plugin's hook with a warning;
  `async`, `asyncRewake`, `onFailure`, `statusMessage` and `once` are ignored.

## Spans

Each run of a hook is a `fontaine.hook` span with the `hook_event`, the `hook_type` and whether it
succeeded. The tool events' spans sit under the `fontaine.permission` span of the call; the
lifecycle events have no such parent. See [Telemetry](telemetry.md).
