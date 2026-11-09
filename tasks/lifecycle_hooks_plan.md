# Lifecycle Hooks: Full Event Set and Handler Types Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/29

## Overview

Fontaine's user hooks fire on five events today: `pre_tool_use`, `post_tool_use`,
`permission_denied`, `task_completed` and `teammate_idle`. Only shell-form `command` handlers
exist. Claude Code fires hooks on about 30 lifecycle events, through `command`, `http`,
`mcp_tool`, `prompt` and `agent` handlers. This plan adds the lifecycle events that map onto a
surface Fontaine already has (session start and end, prompt submit, turn stop and failure,
subagent start and stop, compaction, tool failure, notifications, task creation). It also adds
the `http` and `prompt` handler types. Every new event and handler keeps the existing trust
rules: a hook can add context or block, never widen a permission. A repository can only tighten.
Anything that sends session data off the machine, or forces model spend, needs user trust.
Events and handlers with no Fontaine surface, or that would bypass the per-call gate, are not
built, and each one gets a recorded Decision.

## Current State

- **Config.** `rust/fontaine-core/src/config.rs:583` `Hook { matcher, command, timeout_secs,
  origin, plugin }`. `HooksSection` (`:608`) and the merged `Hooks` (`:1866`) have one named
  `Vec<Hook>` field per event. The merge (`:2487`) and the overlay keys (`:1395`) repeat that list.
- **Runner.** `rust/fontaine-core/src/user_hooks.rs`:
  - `HookProcess::run` spawns `sh -c` (Git Bash or PowerShell on Windows) with the scrubbed
    environment.
  - `CommandHook` implements `PreToolUseHook` and `PermissionDeniedHook` (traits in
    `permissions.rs`).
  - `PostHookStage` is the last `ResultStage` before `Outcome`. It runs for both success and
    error and sends `tool_response.is_error`.
  - `TeamGate` implements `fontaine_agents::tasks::QualityGate` (`tasks.rs:642`) for the two
    board events.
  - `build()` drops project hooks when the workspace is untrusted, and drops project
    `teammate_idle` hooks because they force spend.
- **Wiring.** `rust/fontaine-core/src/agent/setup.rs:614` `permission_pipeline()` builds the hooks.
  `result_pipeline()` (`:654`) installs `PostHookStage`. `start_teams()` installs `TeamGate`.
- **Lifecycle points the new events attach to:**
  - Session start: `AgentRuntime::record_session_start` (`agent.rs:737`). Session origin is
    `SessionOrigin` (fresh, continued, forked).
  - Root turn: `turn::run_turn` (`turn.rs:179`), driven by the TUI (`fontaine-tui/src/ui/turn.rs`)
    and headless (`fontaine/src/headless.rs`). The `/goal` continuation loop
    (`fontaine-core/src/agent/goal.rs`, `fontaine-core/src/goal.rs`) already resumes a finished
    root turn with a harness note.
  - Subagent end: `fontaine-agents/src/spawn.rs:541` `continue_agent` (mail, then `teammate_idle`,
    then reminders, with resume caps). Subagent start: `fontaine-core/src/child_runner.rs:844`
    `run_child`.
  - Compaction: `Session::compact` (`session.rs:537`) with rig-memory's
    `CompactReason::{Asked { focus }, Threshold, Overflow}`.
  - Task creation: `Board::create` (`fontaine-agents/src/tasks.rs:240`), reached from
    `board_update`.
  - TUI notification: `Notifier::observe(app.pending_prompt())` (`fontaine-tui/src/screen.rs:424`).
  - `/clear`: `AgentRuntime::clear_conversation` (`agent.rs:1005`).
- **Plugins.** `rust/fontaine-core/src/plugins.rs` maps `hooks/hooks.json`:
  - `event_lists` (`:1584`) maps the five events. Any other event is dropped silently.
  - A non-`command` `type` is a load error (`:1505`).
  - `map_hook_matcher` maps tool matchers.
- **Display.** `/hooks` → `fontaine-tui/src/reports.rs:267` `hooks(...)` takes the five lists.
  `fontaine plugin` show (`fontaine/src/plugin.rs:1012`) and `doctor_report.rs` count hooks.
- **Model-judged precedent.** `GoalEvaluator` (`fontaine-core/src/goal.rs:881`) runs one
  structured extraction on the guardian's model, with a timeout and an `<untrusted>`-wrapped
  transcript. A `prompt` hook copies this.
- **Decisions this plan supersedes or extends:**
  - 2026-10-28: "Only shell-form `command` hooks load; other hook types are load errors".
  - 2026-11-04 (#51): "No `Notification` hook event".
  - 2026-11-02: the teammate_idle project-scope rule is the precedent for spend-forcing events.

## Design

### 1. One event enum instead of per-event fields

Add `HookEvent` in `config.rs`, a closed `enum` with
`#[serde(rename_all = "snake_case")]`, holding the five existing events plus the twelve new
ones. Each variant answers these questions in one place:

| Method | Answers |
|---|---|
| `config_key()` / `wire_name()` | `stop` / `Stop` |
| `matcher_kind()` | `Tool` (rule grammar), `Values(&[..])` (a fixed set, `\|`-separated), `AgentType` (any agent type name, `\|`-separated), `None` |
| `handlers()` | which handler types the event accepts (Claude Code's table, restricted to built types) |
| `project_scope()` | `Loads` or `IgnoredWithWarning(reason)` |
| `default_timeout()` | 30 s, except `session_end` at 1.5 s (Claude Code's budget) |

`HooksSection` and `Hooks` become a `BTreeMap<HookEvent, Vec<Hook>>`. An unknown key is still a
load error, because the enum rejects unknown variants. The merge, the overlay key list, the
plugin `event_lists`, `/hooks`, the plugin show and the doctor count all iterate the enum instead
of naming fields. Adding an event later then touches one `match`.

### 2. Handler types

`Hook` gains these fields:
- `type`: `command` (default), `http` or `prompt`.
- `url`, `headers` (string map) and `allowed_env_vars` for `http`.
- `prompt` for `prompt`.

`command` becomes optional. Load validation checks:
- Each type has exactly its own fields.
- The event accepts the type (`handlers()`).
- `allowed_env_vars` names none of the scrubbed credential variables (the `scrub_env` list).

`PartialEq`, used for dedup, covers every field except `origin` and `plugin`.

`user_hooks.rs` splits the runner from the event logic:

```
UserHook { id, event, matcher, timeout, user_trust, handler: Handler }
enum Handler { Command(Arc<HookProcess>, String), Http(HttpSpec), Prompt(PromptSpec) }
Handler::answer(&self, input: &Value) -> Result<Answer, HookError>

struct Answer {
    blocked: Option<String>,      // exit 2 (stderr), JSON `decision: "block"` (`reason`), prompt `ok: false` (`reason`)
    json: Option<Map<String, Value>>, // the top-level JSON object, when there is one
    plain: Option<String>,        // non-JSON stdout on exit 0 (command only)
    impossible: bool,             // prompt only
}
```

- **command.** The existing `HookProcess::run`. Exit 0 → `json` or `plain`. Exit 2 → `blocked`.
  Any other exit → `HookError`. The Windows-PowerShell fail-closed rule for `pre_tool_use` stays
  where it is.
- **http.** `POST url` with the input JSON as body, `Content-Type: application/json`, and the
  configured headers. `$VAR` / `${VAR}` in a header value is interpolated only for names in
  `allowed_env_vars`. Any other name becomes the empty string, as in Claude Code. The request
  goes through the session's shared outbound HTTP client (proxy and CA settings from
  `[network]`) with the hook's timeout. A 2xx response with an empty body has no opinion. A 2xx
  response with a JSON object becomes `json`. Anything else (non-2xx, connection error, a body
  that is not JSON) is a `HookError`, which decides nothing.
- **prompt.** One structured extraction on the guardian's model, built like `GoalEvaluator`.
  - The system preamble says the request holds hook input data, not instructions.
  - `$ARGUMENTS` in `prompt` is replaced by the input JSON wrapped in an `<untrusted
    kind="hook_input">` block, after `neutralize`. If `$ARGUMENTS` is absent, the block is
    appended.
  - The schema is `{ ok: bool, reason?: string, impossible?: bool }`. `ok: false` →
    `blocked = reason` (or `"the prompt hook blocked it"`). `ok: true` has no opinion.
  - Usage is counted into the guardian's usage bucket. A timeout, a model failure or an
    unparseable answer is a `HookError`.
  - `ok: true` never allows anything. A prompt hook can only block.

Every event's interpreter reads only `Answer`. This keeps "block" handled the same way across
handlers, and the per-event code stays the same size. `pre_tool_use` maps `blocked` to `Deny` and
reads `hookSpecificOutput` from `json` as today. `permission_denied` reads `retry` only from a
command or http `json`, as in Claude Code. A prompt hook there is a load error.

Common output fields:
- `systemMessage` (string) is shown to the user through `Approver::warn`, on every event except
  `notification`, `session_end` and `stop_failure`, where Claude Code discards it.
- `continue`, `stopReason`, `suppressOutput` and `terminalSequence` are ignored (see Decisions).

String output that becomes model context (`additionalContext`, plain stdout, `reason` fed back
to the agent) is capped at 10,000 characters per hook and truncated with a marker. Claude Code
saves the excess to a file instead.

Each hook run keeps its `fontaine.hook` span, which gains a `hook_type` field.

### 3. Model context from hooks is never user intent

Context a hook adds is delivered as a harness note: `[Hook context from <event>, not the user's
words: …]`. Where a hook resumes work, the note is the next run's prompt. It is never recorded as
a ledger user message, so it can never become guardian intent. This is the 2026-11-02 rule for
idle-teammate wakes, applied to every new event. `run_turn`'s existing `prompt` / harness-note
path carries it. A regression test asserts that the ledger's user messages are unchanged after
each context-adding event (Testing).

### 4. The events

The common input stays as today (`session_id`, `cwd`, `hook_event_name`, `agent_id`,
`permission_mode`, `workspace_roots`). The tool events also keep `tool_name`, `tool_input` and
`tool_use_id`. Each event adds the fields in its row.

| Event (config key) | Fires | Matcher | Extra input | What a hook can do | Handlers | Project scope |
|---|---|---|---|---|---|---|
| `session_start` | After `record_session_start` and after `/clear` and a completed compaction | `startup`, `resume`, `fork`, `clear`, `compact` | `source`, `model` | `additionalContext` or plain stdout → context before the next model request | command | loads |
| `session_end` | TUI exit, `/clear` (before), headless end | `clear`, `prompt_input_exit`, `other` | `reason` | nothing (notification only) | command, http | loads |
| `user_prompt_submit` | A human-typed prompt (TUI), the headless `-p` prompt, a `/loop`/scheduled prompt, before `record_run_start` | none | `prompt` | block (prompt not sent, reason shown to the user, nothing recorded); `additionalContext` or plain stdout → context with the prompt | command, http, prompt | loads |
| `stop` | A root turn ended with `TurnOutcome::Answer` (not Stopped, Failed, MaxTurns) | none | `stop_hook_active`, `last_assistant_message` | block or `additionalContext` → another run with the reason as harness note; prompt `impossible: true` lets it stop | command, http, prompt | **ignored with warning** (forces spend) |
| `stop_failure` | A root turn ended with `TurnOutcome::Failed` | the error kind (`rate_limit`, `overloaded`, `authentication_failed`, `billing_error`, `server_error`, `invalid_request`, `unknown`) | `error`, `error_details`, `last_assistant_message` | nothing | command, http | loads |
| `subagent_start` | Before a spawned child's first run | agent type | `agent_type` | `additionalContext` → harness note ahead of the child's first prompt | command, http | loads |
| `subagent_stop` | In `continue_agent` for a child whose run ended `Done`, after mail and before `teammate_idle` | agent type | `stop_hook_active`, `agent_type`, `last_assistant_message` | block or `additionalContext` → the child is resumed with the reason | command, http, prompt | **ignored with warning** (forces spend) |
| `pre_compact` | Before `Session::compact` asks the model | `manual` (Asked), `auto` (Threshold, Overflow) | `trigger`, `custom_instructions` (the `/compact` focus or null) | block: an Asked or Threshold compaction is skipped with the reason as a warning; an Overflow compaction that is blocked fails the request with the overflow error, as in Claude Code. Plain stdout on exit 0 is appended to the summary focus | command, http | loads |
| `post_compact` | After a compaction saved its summary | `manual`, `auto` | `trigger`, `compact_summary` | nothing | command, http | loads |
| `post_tool_use_failure` | In the result pipeline when the outcome is `Err` (`post_tool_use` now fires only on `Ok`, as in Claude Code) | tool rule | `error` | `additionalContext` → appended to the error the agent reads; block → its reason appended | command, http, prompt | loads |
| `notification` | The TUI notifier sees a new pending prompt, whatever `[tui] notify` is set to; run in the background, never awaited by the UI | `permission_prompt` (an approval), `elicitation_dialog` (an `ask_user_question` or other question prompt) | `message`, `title`, `notification_type` | nothing | command, http | loads |
| `task_created` | In `board_update create`, before the create takes the board lock | none | `task` (Fontaine's task shape without `id`), plus Claude Code's `task_subject`, `task_description`, `teammate_name` | block → not created, the reason is the tool error | command, http, prompt | loads |

Prompt-hook `ok: false` on `pre_tool_use`, `task_completed` and `teammate_idle` takes those
events' block path (deny, task kept open, agent resumed). This equals Claude Code's
`continueOnBlock: true`. Fontaine never ends the turn on a hook block (Decisions).

**Loop bounds.** `stop` and `subagent_stop` continuations are capped at **8 in a row**, Claude
Code's cap, as a constant. A human prompt resets the `stop` count. A board change or a mail-driven
resume resets the `subagent_stop` count. At the cap, Fontaine warns once and the turn or agent
ends. `stop_hook_active` is `true` on any continuation after the first.

**Ordering with `/goal`.** On a root `Answer`, the `stop` hooks run first. Only when no hook
blocks does the goal evaluator run. Both continuation kinds share the harness-note path in core,
so the TUI and headless behave the same. Put the `stop` gate next to the goal step in
`fontaine-core/src/agent/goal.rs` (as an `AgentRuntime` method returning
`Option<String /* continuation note */>`), and have both surfaces call it where they call the
goal step.

**Headless.** A blocked `user_prompt_submit` ends the `-p` run before any model call. Its
`result.subtype` and exit code copy what `claude -p` does, observed in step C3; inside the closed
v1 subtype enum, `error_during_execution` is the fallback.

**Subagent wiring.** `subagent_start` and `subagent_stop` must work with `[agents] teams` off, so
they do not go into `QualityGate`. Add a `LifecycleGate` trait in `fontaine-agents` (`spawn.rs`),
held by `AgentTree` with a no-op default, with methods
`subagent_start(id, type) -> Vec<String>` and
`subagent_stop(id, type, last, active) -> Option<String>`. `user_hooks::SubagentGate` implements
it.

**Matchers for non-tool events.** A `Values` or `AgentType` matcher is `|`-separated exact names.
An omitted matcher, `""` or `*` matches all. A `Values` matcher naming a value outside the event's
set is a load error. Plugin matchers on these events are passed through after the same check;
an un-mappable one skips the entry with the existing warning. A matcher on a `None` event stays a
load error in TOML and is ignored for plugins, as today.

### 5. Trust rules (extending `build()`)

- An untrusted workspace drops every project hook (unchanged).
- An event with `IgnoredWithWarning` (`teammate_idle`, `stop`, `subagent_stop`) drops project
  hooks with one startup warning per event.
- **`http` and `prompt` handlers from project scope are ignored with one warning.** An http hook
  sends session content to a URL the repository chose. A prompt hook spends on the user's
  guardian model. User config, `--settings` and plugins may define both, since install approved
  a plugin. A project-local untracked `settings.local.toml` of a trusted root has user trust, as
  today.
- No new handler or event can allow: only `pre_tool_use` `allow` and `permission_denied` `retry`
  loosen, and only with user trust (unchanged).

### 6. Surfaces

- `/hooks` (`reports::hooks`) takes `&Hooks` and lists every event that has hooks, with each
  hook's type, scope and matcher, and marks the ignored ones and why. A trailing line names the
  events with none.
- `fontaine plugin` show and `doctor_report` iterate the enum.
- Plugin `hooks.json`:
  - The new events map through `HookEvent::wire_name`.
  - `type: "http"` and `"prompt"` load.
  - `type: "agent"` / `"mcp_tool"` skip the entry with one warning per plugin (no longer a load
    error, so a plugin that also ships them still loads its command hooks).
  - A `model` key on a prompt hook is ignored with a warning.
  - An event Fontaine does not build is skipped with one warning per plugin and event, instead of
    silently.

## Implementation Steps

### Phase A — the event enum (no behavior change)
1. `config.rs`: add `HookEvent` and its methods. Replace `HooksSection`/`Hooks` fields with the
   map. Update the merge, the overlay keys, the schema (`schemars` for an enum-keyed map) and
   `config_catalog.rs` if it lists hook keys.
2. Update every user of the five fields: `user_hooks::build`, `agent/setup.rs`,
   `plugins.rs::event_lists`, `fontaine/src/plugin.rs`, `fontaine-tui/src/reports.rs` and its
   `/hooks` caller, `doctor_report.rs`, and tests (`tests/config.rs`, `tests/project_origins.rs`,
   `tests/plugins.rs`, `tests/user_hooks.rs`).
3. `cargo test` green with no behavior change.

### Phase B — handlers
1. `config.rs`: `Hook.type`, `url`, `headers`, `allowed_env_vars`, `prompt`. Make `command`
   optional. Add the validation from Design §2.
2. `user_hooks.rs`: `Handler`, `Answer`, `UserHook` (rename `CommandHook`). Port the five
   existing events onto `Answer`, keeping their observable behavior.
3. The `http` handler on the shared HTTP client (find the client builder that
   `corporate_network_support` introduced and reuse it). Add header interpolation.
4. The `prompt` handler: a `HookJudge` built in `agent/setup.rs` from the guardian model, the same
   way the goal evaluator is built. Factor the shared extraction and timeout code out of
   `goal.rs` instead of copying it (DRY).
5. Add `systemMessage` → `Approver::warn`, and the 10,000-character cap.
6. Add the trust drop for project `http`/`prompt` and for `IgnoredWithWarning` events in
   `build()`.

### Phase C — events
1. `post_tool_use_failure`: split `PostHookStage` by outcome. Send `error` for the failure event.
2. `user_prompt_submit`: `AgentRuntime::prompt_submit(&str) -> PromptGate { Blocked(reason) |
   Proceed { context } }`. Call it from the TUI submit path, headless, and the scheduled-prompt
   path, before `record_run_start`. The context is prefixed to the run's model prompt as a
   harness note.
3. Run `claude -p` with a blocking `UserPromptSubmit` hook to observe its exit code and result
   subtype, and match them in headless (record the observation in the plan's Decisions).
4. `stop` and `stop_failure`: the gate next to the goal step; the continuation cap; both surfaces.
5. `session_start` and `session_end`: fire from `record_session_start` (source from
   `SessionOrigin`), `clear_conversation` (end `clear` before, start `clear` after), after
   compaction (start `compact`), TUI exit and headless end. Queue pending context on
   `SessionState` and drain it into the next run's prompt.
6. `pre_compact` and `post_compact` in `Session::compact`. Pass the hooks in through the runtime
   (`agent/goal.rs` `compact_with`), not into `Session`, which stays hook-free.
7. `subagent_start` and `subagent_stop`: `LifecycleGate` in `fontaine-agents/src/spawn.rs`. Call
   it in `continue_agent` (as a new `Reason::Hook` source, ahead of `teammate_idle`) and in
   `child_runner.rs` before the first run. Implement it in `user_hooks.rs`. Install it in
   `setup.rs`.
8. `task_created`: extend `QualityGate` with `task_created`. Call it from the `board_update`
   create path before `Board::create`, with no board lock held.
9. `notification`: give the TUI a core handle `AgentRuntime::notify_hooks(kind, message, title)`
   that spawns the hooks without awaiting them. Call it where `Notifier::observe` reports a new
   prompt (`screen.rs:424`; the notifier already detects a new prompt edge).

### Phase D — surfaces
1. `/hooks`, `fontaine plugin` show and doctor over the enum, with types and ignore markers.
2. Plugin `hooks.json`: the new events, `http`/`prompt`, the skip warnings for `agent`,
   `mcp_tool`, `model` and unbuilt events.

### Phase E — documentation and records
1. `docs/hooks.md`: rewrite the event table. Add per-event sections for the new events, a
   "Handler types" section (command, http, prompt), the trust table, `systemMessage`, loop caps,
   and "what is not built".
2. `docs/configuration.md` (hook keys), `docs/plugins.md` (hook types and events),
   `docs/agents.md` (subagent hooks), `docs/tui.md` (`/hooks` and the notification hook).
3. `tasks/design/permissions.md` §User hooks: the event set and handler trust rules.
4. `research/research-claude-code-parity.md`: update the two hook rows.
5. Master plan `## Decisions` → `### Extensibility`: append the dated lines listed under this
   plan's Decisions that deviate from Claude Code. Mark the 2026-10-28 command-only line and the
   2026-11-04 Notification line as superseded.

## Files to Modify

- `rust/fontaine-core/src/config.rs`, `config_catalog.rs`
- `rust/fontaine-core/src/user_hooks.rs` (major), `permissions.rs` (hook traits if signatures
  move)
- `rust/fontaine-core/src/goal.rs` (shared extraction helper)
- `rust/fontaine-core/src/agent.rs`, `agent/setup.rs`, `agent/goal.rs`, `session.rs`,
  `child_runner.rs`, `gate.rs` (`SessionState` pending hook context)
- `rust/fontaine-core/src/plugins.rs`, `doctor_report.rs`
- `rust/fontaine-agents/src/spawn.rs`, `tasks.rs`, plus the `board_update` tool
- `rust/fontaine/src/headless.rs`, `rust/fontaine/src/plugin.rs`
- `rust/fontaine-tui/src/ui/turn.rs`, `ui/mod.rs`, `reports.rs`, `screen.rs`
- tests: `rust/fontaine-core/tests/{user_hooks,config,plugins,project_origins,teams}.rs`, new
  `rust/fontaine-core/tests/lifecycle_hooks.rs`, `rust/fontaine-tui/tests/` (notification)
- `docs/hooks.md`, `docs/configuration.md`, `docs/plugins.md`, `docs/agents.md`, `docs/tui.md`
- `tasks/design/permissions.md`, `tasks/fontaine_harness_plan.md`,
  `research/research-claude-code-parity.md`

## Trade-offs

- **Enum-keyed map vs. more named fields.** Seventeen named fields would mean seventeen edits at
  every one of the six sites that list events. The enum puts per-event policy in one `match`, and
  an unknown key is still rejected.
- **A normalized `Answer` vs. per-handler interpretation.** Interpreting each handler inside each
  event would be up to 17 × 3 code paths. One normalized answer keeps "what is a block" in one
  place.
- **Guardian model for prompt hooks vs. a `model` key.** Fontaine has no "background model"
  setting. The guardian model is the one cheap model already configured and budgeted, and the
  goal evaluator already uses it. A per-hook `model` would need account resolution and pricing
  per hook.
- **Hook context as a harness note vs. a system-prompt addition.** A system-prompt change would
  break prompt caching and could be mistaken for operator instructions. A harness note in the
  turn is cache-friendly and explicitly marked as not the user's words.
- **Blocks never end the turn.** Claude Code ends the turn on some prompt-hook blocks. Fontaine
  feeds the reason back instead. A block still denies or blocks, and an ended turn would stall an
  AFK run on a hook the user meant as a guard.

## Decisions

- 2026-11-08: Not built, each skipped with a warning when a plugin names it:
  - `PermissionRequest`: an `allow` there answers the human prompt, which would be a second
    loosening path next to `pre_tool_use` `allow`.
  - `Setup` and `UserPromptExpansion`: no `--init`, and skills expand before submit.
  - `PostToolBatch`: rig dispatches calls without a batch boundary Fontaine owns.
  - `MessageDisplay`: display-only rewrites of model text are out of scope.
  - `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `DirectoryAdded` and `FileChanged`: no
    mid-session reload or file watcher. `/add-dir` and `cd` are already visible to `pre_tool_use`.
  - `WorktreeCreate`/`WorktreeRemove`: no worktrees.
  - `PreModelSwitch`/`PostModelSwitch`: model switches are user commands.
  - `Elicitation`/`ElicitationResult`: no MCP elicitation.
- 2026-11-08: The `agent` and `mcp_tool` handler types are not built. An `mcp_tool` hook calls an
  MCP tool outside the per-call gate, which the floor-gates-every-call invariant forbids. An
  `agent` hook is a tool-using model run, experimental in Claude Code. Command-handler `args`,
  `async`, `asyncRewake`, `shell`, `onFailure`, `if`, `statusMessage` and `once` are not built
  either; plugins keep today's skip behavior for them.
- 2026-11-08: `http` and `prompt` hooks from project scope are ignored with a warning, as are
  project `stop` and `subagent_stop` hooks. Reason: they send session data off the machine or
  force model spend, and a repository may only tighten (extends the 2026-11-02 `teammate_idle`
  rule). Claude Code loads them.
- 2026-11-08: A prompt hook runs on the guardian's model, and its usage counts in the guardian's
  usage bucket. There is no `model` key. Reason: Fontaine has no background-model setting, and
  the goal evaluator already uses this model.
- 2026-11-08: A hook block never ends the turn. Prompt-hook `ok: false` takes the event's block
  path, as with Claude Code's `continueOnBlock: true`. `continue`, `stopReason`,
  `terminalSequence` and `suppressOutput` are ignored. Reason: hooks only tighten, and an ended
  turn would stall an AFK run.
- 2026-11-08: `post_tool_use` now fires only on success, and failures fire
  `post_tool_use_failure` (Claude Code parity; this changes the pre-release behavior where
  `post_tool_use` saw `is_error: true`).
- 2026-11-08: Hook text that becomes model context is truncated at 10,000 characters with a
  marker, where Claude Code spills the rest to a file. Reason: no transcript directory to spill
  into.
- 2026-11-08: `task_created` sends no `task_id` (the id is assigned by the create that the hook
  gates) and sends Fontaine's `task` object next to Claude Code's flat fields.
- 2026-11-08: `user_prompt_submit` fires for human-typed, headless and scheduled prompts, and not
  for turns the harness starts (notifications, stop and goal continuations), where Claude Code
  also fires on turns it starts itself. Reason: blocking a harness wake could wedge background
  work, and its text is not the user's.
- 2026-11-08: `notification` maps an approval to `permission_prompt` and any question prompt to
  `elicitation_dialog`. Fontaine's other notification kinds (auth, quota, agent completion) have
  no notifier edge today.

## Documentation

- `docs/hooks.md`: main rewrite (events, handler types, trust, caps, what is not built).
- `docs/configuration.md`: `[[hooks.<event>]]` keys (`type`, `url`, `headers`,
  `allowed_env_vars`, `prompt`).
- `docs/plugins.md`: which `hooks.json` events and types load.
- `docs/agents.md`: `subagent_start`/`subagent_stop`, `task_created`.
- `docs/tui.md`: `/hooks` output, the notification hook.
- `docs/telemetry.md`: `hook_type` on `fontaine.hook`.
- `tasks/design/permissions.md` §User hooks, the master plan Decisions, the parity matrix.

## Testing Strategy

Unit tests (`user_hooks.rs`, `config.rs`, `plugins.rs`):
- `HookEvent` round-trips. An unknown event key, a bad `Values` matcher, a matcher on a
  matcher-less event, a handler type the event rejects, mismatched type fields, and an
  `allowed_env_vars` naming a scrubbed variable are each a load error.
- `Answer` normalization for each handler: exit 0 with JSON, exit 0 with plain text, exit 2,
  other exit, JSON `decision: "block"`, http 2xx with an empty body, 2xx with JSON, 2xx with
  non-JSON, non-2xx, prompt `ok` true / false / `impossible`.
- Header interpolation: only allowed names. An unlisted name becomes empty.
- `build()` trust: project http/prompt dropped with a warning count; project `stop` and
  `subagent_stop` dropped; untrusted drops all; plugin http/prompt kept.
- The 10,000-character cap. `systemMessage` routing (shown except on the three discard events).
- Plugin mapping: new events, http/prompt load, agent/mcp_tool/`model`/unbuilt events skipped
  with one warning each, matcher pass-through and rejection.
- `reports::hooks` lists types, scopes and ignore markers.

Integration tests (`rust/fontaine-core/tests/lifecycle_hooks.rs`, driving `fontaine -p` and
the runtime with `rig_core::test_utils` deterministic models and shell-script hook doubles, as
`tests/user_hooks.rs` does):
- `user_prompt_submit` block: no model request, nothing in the ledger, the headless exit/subtype
  from step C3. Context reaches the model prompt.
- `stop`: a blocking hook resumes the turn with its reason. `stop_hook_active` flips to `true`.
  The cap stops at 8 with a warning. A prompt hook with `impossible` stops. `/goal` runs only
  after no stop hook blocks.
- `stop_failure` fires on a failed provider call, using the `wiremock` 500 that the existing
  turn-failure tests use.
- `session_start` context reaches the first model request; `source` is right for a fresh,
  `--continue` and `--fork-session` run. `session_end` fires at headless end.
- `pre_compact` blocks a `/compact`, and its stdout lands in the summary request's focus.
  `post_compact` receives the summary.
- `post_tool_use` no longer fires on a failed call. `post_tool_use_failure` does, and its
  context is in the error the model reads.
- `subagent_start` context is in the child's first prompt. `subagent_stop` resumes the child and
  respects the cap. Both fire with teams off.
- `task_created` blocks a create, and the tool error carries the reason.
- `http` handler against `wiremock`: the body is the event JSON, headers are interpolated, a
  `deny` answer on `pre_tool_use` denies, and a 500 decides nothing.
- `prompt` handler with a deterministic model: `ok: false` denies a `pre_tool_use` call and
  resumes a `stop`. A model error decides nothing.
- **Security regression (silent failure class):** for each context-adding event
  (`session_start`, `user_prompt_submit`, `stop`, `subagent_start`, `subagent_stop`,
  `post_tool_use_failure`), the ledger's user messages, and so the guardian's intent input, are
  unchanged. No new event can produce an allow: a `stop`/`user_prompt_submit` hook answering
  `permissionDecision: "allow"` changes nothing.

TUI test (`fontaine-tui/tests/`, `TestBackend`): a pending approval fires the `notification`
hook once with `permission_prompt`, also when `[tui] notify = "off"`.

The existing `tests/user_hooks.rs` stays green unchanged, except for the deliberate
`post_tool_use` on-failure change.

## Manual Verification

1. `cargo run -p fontaine` with a user `[[hooks.notification]]` that writes to a file, trigger an
   approval and check that the file got one line. The `/hooks` listing shows every configured
   event with its type. This is a quick look at the real terminal flow; the logic is unit-tested.

## Open Questions

- What `claude -p` reports (exit code, result subtype) when a `UserPromptSubmit` hook blocks.
  Step C3 observes it and records the answer in Decisions; the fallback is
  `error_during_execution`.
