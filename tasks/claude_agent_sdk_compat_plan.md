# Claude Agent SDK Drop-in Compatibility Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/582

## Overview

Let an unmodified TypeScript (`@anthropic-ai/claude-agent-sdk`) or Python (`claude-agent-sdk`)
Agent SDK application use `fontaine` as its Claude Code executable
(`pathToClaudeCodeExecutable` / `cli_path`). Fontaine serves the server side of Claude Code's
SDK stream-json protocol: Claude-shaped `system` / `assistant` / `user` / `stream_event` /
`result` messages, and `control_request` / `control_response` traffic in both directions. It
keeps its own agent loop, models, tools, sessions, guardian and permission pipeline. The
surface is separate from and additive to `headless-v1`, which stays byte-for-byte unchanged.
Host callbacks (`canUseTool`, `AskUserQuestion`, hook callbacks, SDK MCP servers) are wired
through the seams that already exist (`Approver`, the hook pipeline, the MCP supervisor), so
none of them adds a route around the floor, the guardian or the risk-class registry. Anything
the compatibility layer does not implement fails explicitly, before any work is done.

## Current State

- **Headless CLI.** `rust/fontaine/src/cli.rs:9-153` is one clap `Cli` with kebab-case flags
  only. `--allowed-tools`, `--disallowed-tools`, `--resume`, `--continue`, `--fork-session`,
  `--session-id`, `--model`, `--permission-mode` (`manual|acceptEdits|plan|auto|dontAsk|afk`,
  `cli.rs:484`), `--json-schema`, `--include-partial-messages`, `--add-dir`, `--system-prompt`,
  `--append-system-prompt`, `--max-turns` and `--max-budget-usd` exist. The following do not
  exist: `--verbose`, `--mcp-config`, `--strict-mcp-config`, `--permission-prompt-tool`,
  `--setting-sources`, `--tools`, `--allowedTools`/`--disallowedTools` (camelCase) and every
  other SDK-only flag. Without `-p` the binary starts the TUI.
- **Headless run.** `lib.rs:631 prepare_headless` → `lib.rs:1170 run_headless` →
  `agent_setup` (`lib.rs:1082`), which hard-codes `approver: Arc::new(DenyAll)` (`:1148`) and
  is passed `headless: true` by `run_headless`. It then calls `connect_mcp(&[])` (`:1225`) and `headless::run`.
- **Headless I/O.** `headless.rs` writes untyped `serde_json::Value` through `emit_line`
  (`:57`) and `event` (`:64`). The run's `SessionEvent`s are turned into `headless-v1` events
  inside a closure in `run_attempt` (`:664-724`). Stdin is read only between turns
  (`Input::next`, `:85`). `user_line_text` (`:111`) accepts only `type:"user"`, so any other
  line, `control_request` included, is a usage error. There is no cancel token: the only
  `select!` is against the AFK stop (`:772-776`). Background notifications become their own
  turns in `settle` (`:1084`) / `wait_for_news` (`:893`).
- **Approver seam.** `fontaine-core/src/permissions.rs:95 trait Approver` has `approve`,
  `ask_user`, `elicit`, `elicit_url`, `notice` and `warn`. `Answer` (`:87`) is
  `AllowOnce | AllowSession | AllowAlways | Deny{feedback}`, with no allow-with-changed-input.
  Headless sets `PipelineParams.headless` (`:607`), so `no_human()` (`:1563`) and
  `would_prompt` (`:1590`) turn every Prompt into a Deny. `ask_human` (`:2540`) re-checks
  before it calls `approver.approve` (`:2625`). Grants persist through `state.grant`
  (session) and `user_config::append_allow_rule` (always) (`:2639-2690`).
- **Pipeline order.** `adjudicate` (`:1843`): unregistered-agent deny → AFK stop →
  guardian-off escape hatch → `run_hooks` (PreToolUse `updated_input` replaces args, `:1930`)
  → `evaluate` (`:1036`): the floor (deny → ask → critical → protected), the fast paths, rules
  and mode → Allow / Deny / `prompt_or_ask` → `ask_human` / `guardian_branch` (`:1998`).
- **Questions.** `questions.rs:54 HumanQuestions` → `approver.ask_user`. Headless returns
  `QuestionReply::Unavailable` (`fontaine-tools/src/ask_user.rs:60`). The `Question` shape
  (`ask_user.rs:32`) already mirrors Claude's `AskUserQuestion` (`question`, `header`,
  `options`, `multiSelect`).
- **Tool names.** `fontaine-tools/src/descriptor.rs` gives every builtin row its Claude Code
  names; the first is canonical (`shell`↔`Bash`, `read`↔`Read`, `agent`↔`Agent`/`Task`,
  `ask_user`↔`AskUserQuestion`, …). `find_claude` (`:372`) and
  `extensions.rs:140 map_tool_entry` already turn Claude `Tool(spec)` entries into Fontaine
  rules, with an explicit error for an unknown name.
- **MCP.** MCP tools always register as `RiskClass::Arbitrary`
  (`fontaine-tools/src/mcp/register.rs:199`). `servers.rs:51 enum Transport { Stdio, Http }`.
  `connect_transport` (`servers.rs:850`) already accepts any rmcp `IntoTransport` (kind
  `"pipe"`), though only tests use it today. The core path is `agent/mcp.rs:409 server_spec`
  → `McpSupervisor::start` through `trait McpConnector` (`agent/mcp_supervisor.rs:80`).
- **Hooks.** `user_hooks.rs:369 enum Handler { Command, Http, Prompt }`. `Http` is already
  JSON in, JSON out, through the same output parser that command hooks use. The pipeline
  consumes `PreToolUseHook` / `PermissionDeniedHook` / `ElicitationHook`, and
  `LifecycleHooks` (`:1486`) fires the rest.
- **Modes and models at runtime.** `gate.rs:597 set_mode` is the one mode switch: it records
  the change in the ledger and enforces the AFK refusals. The TUI's `/model` path switches the
  main model.
- **Client-side precedent.** `providers/claude_subscription/` speaks the client half of the same
  protocol, with untyped `Value` frames and `pub(crate)` items. Only the bounded line framing
  (`cli.rs:953 LineBuffer`, `:1003 LineReader`) is worth reusing.
- **Tests.** `fontaine/tests/common/mod.rs` scripts a wiremock model (`Turn::{Text, Tool, …}`)
  and builds a sandbox through `FONTAINE_CONFIG_DIR` / `FONTAINE_DATA_DIR`. Core pipeline tests
  use the `Scripted` approver (`fontaine-core/tests/common/pipeline.rs:82`).
- **Decisions this supersedes** (master plan, `### Headless`): 2026-10-21 "No external
  approval delegate (`--permission-prompt-tool`) in v1". The 2026-10-21 line that the
  stream-json events are Fontaine's own schema stays true for `headless-v1`; the compatibility
  surface is a second schema, not a change to that one.

### What the SDKs actually send (pinned versions)

Read from the released packages: TS `@anthropic-ai/claude-agent-sdk@0.3.325`, Python
`claude-agent-sdk==0.2.182`, both against Claude Code `2.1.325`.

- **Command.** Always `--output-format stream-json --verbose --input-format stream-json`, with
  **no `-p`**. On top of that, according to the options set: `--system-prompt <s>` (Python sends
  `--system-prompt ""` when none is set; `--system-prompt-file <path>` for a `file` system prompt),
  `--append-system-prompt`, `--tools`,
  `--allowedTools a,b`, `--disallowedTools`, `--max-turns`, `--max-budget-usd`, `--model`,
  `--fallback-model`, `--permission-mode`, `--permission-prompt-tool stdio` (TS:
  `--permission-prompt-tool=stdio` whenever `canUseTool` is set), `--continue`,
  `--resume=<id>`, `--session-id=<id>`, `--fork-session`, `--settings`, `--add-dir`,
  `--mcp-config <json>` (SDK servers appear as `{"type":"sdk","name":…}`),
  `--strict-mcp-config`, `--include-partial-messages`, `--include-hook-events`,
  `--setting-sources=<list>`, `--plugin-dir`, `--thinking`, `--max-thinking-tokens`,
  `--effort`, `--json-schema`, `--betas`, `--task-budget`, `--resume-session-at`,
  `--session-mirror`, `--await-initialize`, `--channels`, plus any `extraArgs`.
- **TS `initialize` channel.** The TS SDK never passes `--system-prompt` on argv.
  `buildInitializeRequest` sends `systemPrompt` (default `[""]`, an array form),
  `appendSystemPrompt`, `jsonSchema`, `agents`, `sdkMcpServers`, `sdkMcpServerConfigs` and
  `sdkMcpServerManifests` in `initialize`, plus, when set, `toolAliases`, `planModeInstructions`,
  `title`, `promptSuggestions`, `plugins`, `managedSettings` and `workspaceTrust`. Python sends
  none of the unsupported fields by default.
- **Env.** `CLAUDE_CODE_ENTRYPOINT=sdk-ts|sdk-py` and `CLAUDE_AGENT_SDK_VERSION`.
- **Host → CLI control.** `initialize` (`hooks` as `{event: [{matcher, hookCallbackIds,
  timeout, onFailure}]}`, `agents`, `skills`, `excludeDynamicSections`, …), `interrupt`,
  `set_permission_mode`, `set_model`, `mcp_status`, `get_context_usage`, `rewind_files`,
  `mcp_reconnect`, `mcp_toggle`, `stop_task`.
- **CLI → host control.** `can_use_tool` (`tool_name`, `input`, `permission_suggestions`,
  `tool_use_id`, `agent_id`, `blocked_path`, `decision_reason`, `title`, `display_name`,
  `description`) → `{behavior:"allow", updatedInput, updatedPermissions?}` or
  `{behavior:"deny", message, interrupt?}`; `hook_callback` (`callback_id`, `input`,
  `tool_use_id`) → hook JSON output; `mcp_message` (`server_name`, JSON-RPC `message`) →
  `{mcp_response}`; `control_cancel_request` (`request_id`) abandons a pending request.
- **Version check.** The Python SDK only warns, never fails, when `claude_code_version` in
  `system/init` is missing or not `x.y.z`; `2.1.248` matters only for verbatim prompts (also a
  warning). Setting `CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK` skips the check.
- **Message parsing.** The Python parser indexes these keys directly, so they are required:
  `assistant.message.{content, model}`, `result.{subtype, duration_ms, duration_api_ms,
  is_error, num_turns, session_id}`, `stream_event.{uuid, session_id, event}`, and the
  `system/task_*` fields. A one-turn baseline captured from `claude 2.1.325` (initialize →
  `control_response`, `system/init`, `assistant`, `rate_limit_event`, `result`) is the
  reference shape.

## Design

### 1. Selecting the compatibility surface

`fontaine` enters **Claude SDK mode** when its raw argv contains all of
`--output-format stream-json`, `--input-format stream-json` and `--verbose` (each in either
`--flag value` or `--flag=value` form), and contains neither `-p` nor `--print`. Every SDK
invocation has exactly this shape. Today that shape is meaningless to Fontaine (those flags
only act under `-p`), so no existing `headless-v1` or TUI invocation changes meaning.
`fontaine -p --output-format stream-json …` remains `headless-v1`.

`lib.rs::run` checks the argv shape next to the existing pre-clap hidden-mode check, before the
clap parse, and hands argv to a separate clap struct, `claude_compat::Cli`. The raw argv scan skips the operand after known value-taking flags (e.g.
`--system-prompt`, `--append-system-prompt`), so a value of `-p`, `--verbose` or `stream-json`
cannot flip detection. That struct carries Claude's exact flag spellings (camelCase
included). Its value enums and the unsupported list come from the capability matrix (§9). Any
unknown flag, and any flag classed as unsupported, exits with code 2 and a one-line stderr
message naming the flag and pointing to `docs/agent-sdk.md`, before stdin is read or a model
is contacted. The TS SDK appends the tail of the CLI's stderr to its exit error, so the message
reaches the caller in the raised error; the Python SDK raises `ProcessError` with the exit code
and a placeholder stderr, and the message reaches the parent's stderr or the caller's `stderr`
callback option.

### 2. Module layout

All new code lives in `rust/fontaine/src/claude_compat/`. Core and headless gain only the seams listed in
§3 and §5–§8: the event sink (in `rust/fontaine/src/headless.rs`), the human channel, callback hooks, injected MCP transports and
session agents, and `AgentSetup` inputs for CLI-only MCP servers, the root toolset and a
non-persisting model switch.

```
claude_compat/
  mod.rs        entry: run(argv) -> ExitCode; wires the pieces below
  cli.rs        clap struct with Claude flag names; flag -> Options mapping; unsupported table
  wire.rs       typed serde structs: inbound (User, ControlRequest, ControlResponse,
                ControlCancel) and outbound (SystemInit, Assistant, User, StreamEvent, Result,
                SystemStatus, PermissionDenied, Task*, ControlRequest/Response); inbound types
                ignore unknown fields (no `deny_unknown_fields`)
  router.rs     stdin demux task + single stdout writer task; outbound request ids,
                pending map, cancel, EOF fail-closed
  translate.rs  SessionEvent -> wire messages (tool-name mapping, parent_tool_use_id,
                Anthropic stream_event synthesis, usage/modelUsage/result mapping)
  approver.rs   SdkApprover: Approver over can_use_tool / AskUserQuestion
  hooks.rs      callback hook handler (initialize.hooks -> hook_callback)
  mcp.rs        SDK MCP servers: an rmcp IntoTransport tunnelled over mcp_message
  control.rs    host->CLI control handlers (initialize, interrupt, set_permission_mode,
                set_model, mcp_status, unsupported -> error response)
```

```
 SDK host ──stdin──▶ router (demux) ─┬─ user msg ───────▶ input queue ─▶ session loop
                                     ├─ control_request ─▶ control.rs
                                     └─ control_response ▶ pending[request_id] (approver/
                                                            hooks/mcp waiters)
 session loop ── SessionEvent ─▶ EventSink ─▶ translate.rs ─┐
 approver/hooks/mcp ── outbound control_request ────────────┼─▶ writer ──stdout──▶ SDK host
 control.rs ── control_response ────────────────────────────┘
```

### 3. Shared session loop: an `EventSink` seam in `headless.rs`

Today `run_attempt` turns `SessionEvent`s into `headless-v1` JSON inline. Extract a trait:

```rust
pub(crate) trait EventSink: Send {
    fn init(&mut self, info: &InitInfo); // called at each turn start
    fn event(&mut self, session_id: &str, event: &SessionEvent);
    fn result(&mut self, outcome: &RunOutcome);
    // usage_paused/resumed, goal: default no-op where the compat surface has no equivalent
}
```

`RunOutcome` is the existing private `Outcome` (headless.rs), extended; `InitInfo` is new.
`HeadlessV1Sink::init` emits only on the first call (today's once-per-session output);
`ClaudeSink::init` emits `system/init` each time (§4). The usage-pause observer takes an
`Arc<dyn Fn(&PauseEvent) + Send + Sync>` (`usage_pause.rs`), which a `&mut self` sink cannot be
moved into, so the sink hands out a cloneable writer/observer for that site (`ClaudeSink`'s is a
no-op). The sink takes over every emission site, not only the `run_attempt` closure: goal, init, the
usage-pause observer and both result emissions. It replaces the `stream_events` bool threaded
through `run_one`, `continue_on_stop`, `settle` and `guarded_turn`. `HeadlessV1Sink` holds today's code unchanged, and the existing `headless_contract.rs` / schema
tests are the guard that its output stays byte-identical. `ClaudeSink` (in `translate.rs`)
writes through the router's writer. `headless::run_session` (made `pub(crate)`) takes the sink plus an input source
trait (`next_user_message`, which today is `Input`; in SDK mode it is the router's queue) and an
optional cancel receiver. That is all the sharing needed: session open/continue/resume/fork,
budgets, `settle` and teardown are reused as they are.

### 4. Output translation (`translate.rs`)

- **`system/init`** goes out once per turn start, as in the baseline. It carries `session_id`,
  `uuid`, `cwd`, `model` (the Fontaine model reference), `permissionMode` (Claude spelling, see
  §6), `tools` (wire names), `mcp_servers` (`{name, status}`), `agents`, `skills: []`,
  `slash_commands: []`, `apiKeySource: "none"`, `output_style: "default"` and
  `claude_code_version`, plus Fontaine additions `fontaine_version` and
  `compat_protocol: "claude-sdk-1"`.
- **`claude_code_version`** reports the Claude Code version whose protocol the conformance suite
  was captured against (`2.1.325` initially), held in one constant next to the capability
  matrix. It describes the protocol level served, so the SDKs' feature gates read it correctly.
  `fontaine_version` reports the real binary version.
- **`assistant`** carries `message: {id, type:"message", role:"assistant", model, content,
  stop_reason, usage}`, `parent_tool_use_id` (the spawning `agent` call's id for subagent
  output, otherwise null), `session_id` and `uuid`. Content blocks are `text` and `tool_use`.
  Reasoning stays out, as the 2026-10-31 decision already says.
- **`user`** carries `tool_result` blocks (`tool_use_id`, `content`, `is_error`) with the same
  attribution. Replayed user prompts are not echoed (no `--replay-user-messages` in the
  matrix).
- **`stream_event`** goes out only with `--include-partial-messages`. Fontaine's text deltas are
  wrapped as Anthropic raw events (`message_start`, `content_block_start`,
  `content_block_delta{text_delta}`, `content_block_stop`, `message_delta`, `message_stop`).
  Tool-call argument streaming is sent as one `input_json_delta` per block. That is a faithful
  subset, and it is documented.
- **`system/permission_denied`** goes out for each gate deny, as Claude does. Each one also
  lands in `result.permission_denials[{tool_name, tool_use_id, tool_input}]`. Fontaine's
  `gate_decision` has no Claude equivalent and is not emitted.
- **Subagents.** A spawn's `tool_use` uses the `Agent` name. The child's messages carry
  `parent_tool_use_id`. Background spawns emit `system/task_started` / `task_notification` with
  the fields the Python parser requires.
- **`result`** carries `subtype` (Claude's: `success`, `error_max_turns`,
  `error_max_budget_usd`, `error_during_execution`, `error_max_structured_output_retries`, plus
  Fontaine's `error_usage_limit`, which the SDKs pass through), `is_error`, `duration_ms`,
  `duration_api_ms`, `num_turns`, `result`, `session_id`, `stop_reason`, `total_cost_usd`
  (`null` when unpriced), `usage` (Anthropic shape from Fontaine's main usage),
  `modelUsage{model: {inputTokens, outputTokens, cacheReadInputTokens,
  cacheCreationInputTokens, costUSD}}` (guardian and subagent models included),
  `permission_denials`, `structured_output` and `uuid`. Fontaine's guardian usage appears under
  an additive `fontaine_usage` key with the `headless-v1` bucket shape.
- **Tool names on the wire.** A builtin is shown under its descriptor's canonical Claude name
  (`shell` → `Bash`); MCP tools keep `mcp__server__tool` (the same convention); a Fontaine tool
  with no Claude name keeps its own name. **Input objects stay in Fontaine's own argument
  schema**, never rewritten. The docs list each mapped tool's argument differences. The same table
  maps names back on inbound `--allowedTools` / `--disallowedTools` / `--tools` entries through
  `map_tool_entry`.

### 5. Permission callback (`canUseTool`) without a bypass

**Opt-in.** Only `--permission-prompt-tool stdio` installs `SdkApprover`. Without it, the
session keeps `DenyAll` and today's deny-on-would-prompt behaviour. A named MCP permission tool
(`--permission-prompt-tool mcp__…`) is unsupported and exits 2.

**Placement.** Core gains a third value in the pipeline's "is a human available" state, which
today is a `headless: bool`. It becomes
`enum HumanChannel { Present, Absent, Delegate }`. `Delegate` behaves exactly like `Present`:
`would_prompt` does not convert, and `ask_human` calls `approver.approve`. The difference is
that `Delegate` (a) never offers `AllowAlways` (`ask_human` forces `offers.always = false` in
`ApprovalRequest.offers`, `permissions.rs:70`, `:80-84`, `:2574-2577`; an un-offered AllowAlways
is already downgraded, `:2690-2695`) and (b) records `delegate` in a new `DecisionRecord` field
(`human_channel`, audit.rs, `#[serde(default)]` so existing records and the deserializer at
`fontaine-tui/src/transcript.rs:2124` still load; not a `DecisionRecord::new` parameter) next to the
unchanged `headless` bool. `AgentSetup` gains a `human_channel: HumanChannel` field that is passed
to `PipelineParams` (`agent.rs:144`, `setup.rs:832-835`). `AgentSetup.headless` stays and keeps
meaning "no person at the terminal" (teams, `setup.rs:712` and `:943`, ledger SessionStart/project
info `agent.rs:869`, bug-report drafts `build.rs:180`): SDK mode sets `headless = true` together
with `human_channel = Delegate` when `--permission-prompt-tool stdio` is set (`Absent` otherwise);
the TUI sets `Present`, `headless-v1` `Absent`. Since the approver is reached only from `ask_human`, the
callback runs after the unregistered-agent check, the AFK stop, hooks, the floor, rules, mode
handling and (in `auto`/`afk`) the guardian. A floor deny, a guardian deny or a fail-closed
guardian outcome never reaches it. In `afk`, `AfkApprover` still wraps the delegate, so AFK's
would-prompt-is-deny semantics hold there as they do for a human. SDK mode passes
`confirm_project_servers=false` and `scheduling=false` (the headless values, `lib.rs:1159-1160`),
independent of `HumanChannel`. `SdkApprover` keeps the trait-default `guardian_unavailable`
(Deny); the composition-stop forced prompt and a guardian Escalate reach the delegate via
`ask_human`. `/guardian-disable` cannot be
reached from SDK mode: it is a TUI command, and SDK mode has no slash commands.

**Request.** `SdkApprover::approve` sends `can_use_tool` with `tool_name` (wire name), `input`
(normalized Fontaine args), `tool_use_id`, `agent_id` (for subagents), `decision_reason` (the
Prompt's `reason`), `description` (the summary) and `permission_suggestions`. `blocked_path`,
`title` and `display_name` are omitted (the pipeline has no source for them) and marked so in
the matrix. The suggestions are built from
the request's `proposed_rule`, as at most one `{type:"addRules", rules:[{toolName,
ruleContent}], behavior:"allow", destination:"session"}`.

**Response mapping.**

| SDK answer | Fontaine outcome |
|---|---|
| `allow`, `updatedInput` equal to the sent input, no `updatedPermissions` | `AllowOnce` |
| `allow` with `updatedInput` absent, no `updatedPermissions` | `AllowOnce` (absent is treated as equal to the sent input; unknown extra keys such as `toolUseID` and `decisionClassification` are tolerated) |
| `allow` + `updatedPermissions` = addRules/allow/`session` naming the proposed rule | `AllowSession` (existing grant path) |
| `allow` + `updatedPermissions` naming an accepted session allow rule other than the proposed one | `AllowOnce` plus those session grants |
| `allow` + `updatedInput` different from the sent input | `AllowModified(args)` → re-adjudication (below) |
| `deny` (`message`, `interrupt?`) | `Deny{feedback: message}`; `interrupt:true` also cancels the turn (§7) |
| error response, malformed JSON, unknown `behavior`, cancelled, EOF/disconnect | `Deny`, fail-closed, reason logged to the audit |

**Modified input.** `Answer` gains `AllowModified { args: Value, grant: Option<Rule> }`, where `grant` is the
session allow from `updatedPermissions`, applied only if the re-run allows. `ask_human` does not
allow it directly: its and `prompt_or_ask`'s return types change to carry a `Readjudicate(args)`
outcome, and `adjudicate` restarts from the top (hooks included) with the new args through an
owned call view, after the same normalization as any model call. The restart records the
original args digest, as the hook `updated_input` rewrite does. The re-run can still be denied by the floor, scope, destructive analysis or the guardian.
If the re-run would prompt again, the callback is asked once more about the new args. An answer
that changes the args a second time is denied.

**`updatedPermissions`** is accepted only where it is no looser than what Fontaine's TUI offers, except `setMode`, which is
equivalent to the host's own `set_permission_mode` control (bypass still refused):

- `addRules` with `behavior: "allow"` and destination `session` or `cliArg`, for rules that
  `map_tool_entry` accepts. These become session grants (`RuleSource::Session`) and still sit
  below the floor.
- `addRules` with `deny` / `ask` at any destination. These tighten, as session rules (Fontaine
  does not write the host's choice into config files).
- `setMode` → the same path as the `set_permission_mode` control (§7).
- Anything else fails the whole answer closed (Deny), with the reason in the deny feedback and
  the audit: an allow rule at `userSettings` / `projectSettings` / `localSettings`,
  `replaceRules`, `removeRules`, `addDirectories` or `removeDirectories`. The suggestions
  Fontaine sends only ever target `session`, so a host that echoes suggestions back never hits
  this.

**Questions.** `SdkApprover::ask_user` sends `can_use_tool` with
`tool_name: "AskUserQuestion"` and `input: {questions}` (Fontaine's `Question` already has
Claude's field names). An `allow` whose `updatedInput.answers` maps every question to an answer
becomes `QuestionReply::Answered`, and these count as human answers for the existing
user-intent recording. A `deny` becomes `Declined`, as in the TUI. Malformed answers, a
disconnect or a cancel give `Unavailable`, the existing headless text. Without the delegate,
behaviour is unchanged. `elicit` / `elicit_url` (MCP elicitation) stay declined in SDK mode,
which is documented.

**Waiting.** `can_use_tool` and question requests wait without a time limit, as a TUI prompt
does. They end on an answer, a disconnect, an interrupt, or the end of the run. In each
non-answer case Fontaine sends `control_cancel_request` (where the pipe is still open) and fails
closed.

### 6. Modes, models and other flags

- **`--permission-mode` / `set_permission_mode`.** `default`→`manual`, `acceptEdits`,
  `plan`, `dontAsk`, `auto`. Fontaine's `afk` is accepted under its own name.
  `bypassPermissions` and `--allow-dangerously-skip-permissions` exit 2 at the CLI, or return a
  control error at runtime: there is no bypass mode. A runtime change goes through
  `gate.rs set_mode` (ledger record, AFK refusals) and answers with an error when refused.
  Running subagents keep the mode they were spawned with (the existing invariant), and new
  spawns inherit the new one.
- **Model.** `--model` / `set_model` take a Fontaine model reference or role (Claude aliases such
  as `sonnet` work only if the user configured a role of that name) through a session-only switch
(`AgentRuntime::switch_main_model` persists `[model] model` via `user_config::set_model`, so SDK
mode never uses it as is; `agent/models.rs` gains a non-persisting variant). An unknown model is a control error, or exit 2 at startup. `set_model` with no
  model resets to the configured default.
- **Tools.** `--tools ""` means no builtins, `--tools default` means all, and a list restricts the
  base set (mapped names; an unknown name exits 2). The restriction is an `AgentSetup`
  root-toolset field applied where the root tool server is built, so excluded tools are not
  advertised. `--allowedTools` / `--disallowedTools` are
  comma lists; each entry goes through `map_tool_entry` before the CLI-scope rule path. `--tools` entries with a specifier exit 2.
- **`--setting-sources`.** Fontaine reads its own TOML config, not Claude's settings. The flag is
  accepted, but it never removes Fontaine's project-scope deny/ask tightening, because dropping
  it would loosen from the host side. User and project Fontaine config always load, and this
  difference is documented. `--settings` with anything other than an empty object exits 2, since
  Claude's JSON settings schema is not Fontaine's.
- **`--system-prompt` / `--append-system-prompt`** map to Fontaine's existing flags.
  `--system-prompt-file` reads the file. The TS SDK sends the same values as `initialize`
  `systemPrompt` / `appendSystemPrompt` (§7), which map onto the same options; empty and absent
  handling is decided in Decisions.
- **`--mcp-config <json|path>`** (repeatable): `stdio` / `http` servers map to Fontaine
  `[mcp.servers]` entries at CLI scope. `sse` entries are unsupported (exit 2; Fontaine's http
  transport is streamable HTTP). `sdk` servers are covered in §8 (the TS SDK declares them in `initialize`, §7). `--strict-mcp-config` sets an
  `AgentSetup` "CLI MCP servers only" switch, so no configured server starts.
- **Accepted and mapped:** `--continue`, `--resume`, `--session-id`, `--fork-session`,
  `--max-turns`, `--max-budget-usd`, `--add-dir`, `--json-schema`, `--effort`,
  `--include-partial-messages`, `--verbose` (required; no other effect).
- **Unsupported (exit 2):** `--fallback-model`, `--betas`, `--thinking`,
  `--max-thinking-tokens`, `--thinking-display`, `--task-budget`, `--plugin-dir`,
  `--resume-session-at`, `--resume-drops-turn`, `--session-mirror`, `--await-initialize`,
  `--channels`, `--sdk-url`, `--include-hook-events`, `--no-session-persistence`,
  `--permission-prompts`, `--agent`, `--debug*`, `--hard-fail` and any unknown flag. Each row in
  the matrix says why, and a later change can promote a row with its tests.

### 7. Bidirectional control (`router.rs`, `control.rs`)

- **I/O.** One stdin reader task parses each line (bounded, reusing the `LineReader` framing,
  lifted to a shared module) and routes it: a `user` message goes to the input queue, a
  `control_request` to `control.rs`, a `control_response` resolves the pending outbound request
  by `request_id`, and a `control_cancel_request` aborts the matching inbound handler. A
  `keep_alive` line is ignored, as in Claude Code. `update_environment_variables` is
  unsupported and, like any other malformed or unknown-type line, ends the run with `result.subtype =
  error_during_execution`, a stderr reason and exit 1. One writer task owns stdout, so messages
  and control frames never interleave mid-line.
- **Startup read.** The runtime is built before `headless::run` opens stdin, but callback hooks,
  session agents and SDK MCP transports arrive on stdin. SDK mode therefore reads stdin up to
  the first `initialize` (or the first user message, if none) before building the runtime, and
  passes what it found into `AgentSetup` (§8). User lines arriving before the `initialize` reply
  are buffered (the TS SDK writes the first user line right after `initialize` without waiting).
  Unknown fields on inbound `user` lines (`session_id` as `""`/`"default"`, `parent_tool_use_id`,
  `client_composed`) are ignored.
- **`initialize`** must come before the first user message if it comes at all; Claude Code
  tolerates its absence and so does Fontaine. Its response mirrors the baseline's keys that have
  meaning here: `commands: []`, `agents`, `models` (configured), `current_permission_mode`,
  `claude_code_version`, `pid`, `hook_on_failure_supported: true` and
  `hooks_applied: true`. `initialize.hooks` registers callback hooks (§8) and
  `initialize.agents` registers session agents (§8). `systemPrompt`, `appendSystemPrompt`,
  `jsonSchema` and `sdkMcpServers` / `sdkMcpServerConfigs` / `sdkMcpServerManifests` map onto the
  same options as their argv forms (§6, §8). Fields set to a non-default value that Fontaine
  cannot honor (`excludeDynamicSections`, `systemPromptSnapshot`, `skills` as a non-empty list,
  `forwardSubagentText`, `toolAliases`, `planModeInstructions`, `title`, `promptSuggestions`,
  `plugins`, `managedSettings`, `workspaceTrust`) get an error response naming them; default
  values never do. The SDK raises on an error response to `initialize`, so the caller sees it.
- **`interrupt`.** `run_attempt` gains a `select!` arm on a cancel receiver, next to the AFK
  stop. Interrupt drops the turn future (the TUI's mechanism); that kills foreground
  children through their drop guards and `kill_on_drop`, as in the TUI. It also cancels pending
  outbound requests (each fails closed). The turn's `result` then goes out with Claude Code
  2.1.325's aborted-turn shape: `subtype:"error_during_execution"`, `is_error:true`,
  `terminal_reason` `aborted_streaming` or `aborted_tools`, and `errors:["[ede_diagnostic] …"]`,
  except `subtype:"success"` when the last message is assistant text, or all tool results with
  `end_turn`. The Step 1 baseline assertions pin `terminal_reason` and `errors`. The control response is sent after the turn has stopped. Queued user
  messages stay queued. The arm calls `keep_unsettled_turn(Unsettled::User, …)` as the AFK arm
  does.
- **Streaming input while a turn runs.** A user message that arrives mid-turn goes to the root's
  `Steering` queue (`steering.rs:51`, hooked up in headless, which has none today) and is delivered at the next tool result, as in the TUI.
  If the turn ends first, it becomes the next turn. Ordering: input-queue order is arrival
  order. Background notifications (`settle`) run only when the input queue is empty, so a host
  message is never starved by notification turns. Control requests are handled concurrently
  with turns, and never wait behind one.
- **`mcp_status`** answers from the supervisor's server states. `get_context_usage`,
  `rewind_files`, `mcp_reconnect`, `mcp_toggle`, `stop_task` and unknown subtypes return a
  control error naming the subtype.
- **Disconnect and shutdown.** EOF on stdin with no turn running ends the run (exit 0 after the
  last `result`). EOF during a turn lets the turn finish, but every pending outbound request
  fails closed at once. A broken stdout pipe ends the run. SIGTERM/SIGINT cancel like
  `interrupt`, then use the existing teardown of jobs and subagents.

### 8. Host-provided tools, hooks and agents

- **SDK MCP servers.** `--mcp-config` entries (or `initialize` `sdkMcpServers` / configs / manifests) with `type:"sdk"` become a new
  `Transport::Sdk { name }`. Its connector builds an rmcp `IntoTransport` over a channel pair.
  Outbound JSON-RPC goes out as `mcp_message` control requests, and the `mcp_response` comes
  back through the router. A public transport-factory trait
  on `AgentSetup` supplies the channel pair (the connector types are `pub(crate)` today), and
  the server specs come from a non-Config source, since `server_spec` returns nothing without a
  command or url. Servers named in `--mcp-config` or `initialize` are known by argv or
  `initialize`, both before the runtime is built (startup read, §7); only their transports are
  bound afterwards. It enters `McpSupervisor` through the existing `ServerConnector` via
  `connect_transport` for `Transport::Sdk`, so registration, deferral, `list_changed` and naming are the normal
  MCP path. **These tools register as `RiskClass::Arbitrary` and cross the full gate and result
  pipeline like any MCP tool.** Results are untrusted tool output (scanner included).
- **Callback hooks.** `user_hooks.rs` gains `Handler::Callback { callback_id, timeout }`,
  executed by sending `hook_callback` (`input` = the same JSON a command hook receives on stdin,
  `tool_use_id`) and parsing the reply with the existing hook-output parser. The handler is
  injected through a `HookTransport` trait object in `LifecycleHooks` / `UserHook`, so
  `fontaine-core` gains no knowledge of the wire. It is built by a constructor that bypasses
  `HandlerKind` (which deserializes from TOML `type`), so a callback is not declarable from
  config. `AgentSetup` gains an input for callback hooks, feeding both the pipeline's
  pre-hooks (frozen in `PipelineParams.hooks`) and `LifecycleHooks`, which are built before the
  runtime is shared. Callback hooks are registered at Fontaine's CLI scope
  (`Slot::Cli`, user trust), so a callback hook's `allow` is a trusted hook allow after the
  floor (`Tier::Hook`), which skips `canUseTool` and, for narrow matchers in auto, the guardian,
  as a PreToolUse allow does in Claude Code. `initialize.hooks` matchers use Claude syntax (`"Bash"`, `"Write|Edit"`) while Fontaine hook
  matchers go through `parse_rule`, where an unparsed matcher runs for every call with a non-narrow
  allow. So each matcher is split on `|` and each alternative is mapped through `map_tool_entry`
  into one callback hook; an alternative that cannot be mapped fails `initialize` with an error
  naming it; an empty or `*` matcher stays deliberately broad (its allow is non-narrow).
  Timeout is the matcher's `timeout`, or the
  command-hook default. An error or timeout follows `onFailure`: `block` makes a PreToolUse
  deny, and `continue` or absent gives command-hook non-blocking-error semantics. The callback constructor sets the existing `fails_closed`
  (`PreToolUseHook::fails_closed`, `UserHook.fails_closed`) from `onFailure: block`. A hook's output
  is data, never a human answer. Events with no Fontaine equivalent get an `initialize` error
  naming the event.
- **Programmatic agents** (`initialize.agents`, `--agents` is not a flag the SDK sends) are
  mapped from the JSON definition into agent types by a new function in `extensions.rs` (today
  it maps only frontmatter files), under a new explicit session origin on `TypeOrigin` only (`fontaine-agents/src/registry.rs`;
  the extension `Scope` is shared with skills and styles and gains no variant). The
  toolset, model and effort restrictions that apply to tightened scopes apply to them. Tool
  lists go through `map_tool_entry` and cannot exceed the parent's toolset. `AgentSetup` gains a
  session-agents input, kept across plugin reload (which overwrites the live agent-type cell).

### 9. Capability matrix

`claude_compat/cli.rs` holds one table of every SDK flag, control subtype and message field
group, each with a state of `supported`, `mapped (difference)` or `unsupported (reason)`. The
CLI parser and control router read it, so the code cannot drift from the published matrix, and
a unit test renders it and compares the result with the table in `docs/agent-sdk.md`. Tested
versions are TS `0.3.325`, Python `0.2.182` and Claude Code protocol baseline `2.1.325`.

## Implementation Steps

**Step 1: Baselines and consumer programs (first; later steps depend on them).**
1. `evals/claude_sdk/capture.sh` (manual, never in CI): runs each consumer program against the
   real `claude` and writes reference transcripts into
   `rust/fontaine/tests/fixtures/claude_sdk/`. It must be run once for this change, to pin
   shapes such as the interrupt result and the mid-turn input delivery.
2. `rust/fontaine/tests/sdk_compat/ts/` (`package.json` + lockfile pinning
   `@anthropic-ai/claude-agent-sdk@0.3.325`) and `sdk_compat/py/` (`requirements.txt` pinning
   `claude-agent-sdk==0.2.182` with hashes): one unmodified consumer program per scenario,
   selecting the executable only through `pathToClaudeCodeExecutable` / `cli_path`. Add
   `node_modules/` and `.venv/` to `.gitignore` and `.ignore`.

**Step 2: Seams in core and headless, with no behaviour change.**
3. `rust/fontaine/src/headless.rs`: extract `EventSink`, an input-source trait and an optional
   cancel receiver; extend the private `Outcome` into `RunOutcome`; move today's emission into
   `HeadlessV1Sink`, covering every emission site listed in §3 and replacing the `stream_events`
   flag; make `run_session` `pub(crate)`. Hook up the root `Steering` queue in headless, and
   call `keep_unsettled_turn(Unsettled::User, …)` in the interrupt arm as the AFK arm does.
   Every existing headless test must stay green and unmodified.
4. `fontaine-core/src/permissions.rs` and `fontaine-core/src/agent.rs`: `PipelineParams.headless:
   bool` → `HumanChannel`, fed from a new `AgentSetup.human_channel` (`AgentSetup.headless` stays). Add
   `Answer::AllowModified`, the `Readjudicate(args)` return of `ask_human` / `prompt_or_ask` and
   the restart in `adjudicate`. Add `human_channel` to `DecisionRecord` (`audit.rs`; `tier.rs`
   if a tier is needed) and carry it into the ledger and spans. Update the callers this breaks in
   the same step: `fontaine-evals/src/none.rs` (~:165), `fontaine-core/tests/common/pipeline.rs`
   (~:307), `fontaine-tui/src/app.rs` (the `Answer` match ~:1835 and the `DecisionRecord`
   literal ~:2110-2143), `fontaine-core/src/statusline.rs` (~:487-520),
   `fontaine-core/tests/audit_telemetry.rs`, the `AgentSetup` literals at `rust/fontaine/src/lib.rs:1130`,
   `fontaine-core/tests/common/runtime.rs:454`, `fontaine-tui/tests/common/mod.rs:374` and
   `fontaine-tui/src/ui/tests.rs:865`.
5. `fontaine-core/src/user_hooks.rs`: add `Handler::Callback` behind a `HookTransport` trait,
   built by a constructor that bypasses `HandlerKind` and sets `fails_closed` from `onFailure`.
6. `fontaine-tools/src/mcp/servers.rs` and `fontaine-core/src/agent/mcp*.rs`: add
   `Transport::Sdk`, a public transport-factory trait, a non-Config spec source, and routing in
   `ServerConnector` via `connect_transport`.
7. `fontaine-core/src/agent/setup.rs`, `build.rs`, `models.rs`, `reload.rs` and
   `mcp.rs`: `AgentSetup` inputs for callback hooks (pipeline pre-hooks and `LifecycleHooks`),
   session agents (kept across reload), a "CLI MCP servers only" switch, a root-toolset
   restriction applied where the root tool server is built, and a non-persisting model switch.
8. `fontaine-agents/src/registry.rs` and `rust/fontaine-core/src/extensions.rs`: a session
   `TypeOrigin`, and the JSON agent-definition mapping with the toolset, model and
   effort restrictions of tightened scopes applied.
9. `rust/fontaine/src/lib.rs`: `agent_setup` and `run_headless` take an approver and a
   `HumanChannel` parameter (today `DenyAll` is hard-coded and `headless` also drives
   `confirm_project_servers` and scheduling; SDK mode passes both as false, independent of
   `HumanChannel`), and build their inputs from `headless::Options`
   plus compat extras instead of Fontaine's `&Cli`. That covers `Launch { cli: &Cli, … }`
   (`lib.rs:1055`), the `headless: bool` parameter shared with the TUI (`:1087`, false at `:1331`),
   `resolve_models(cli, …)` (`:1180`) and `start_scanner(&launch, …)` (`:1207`): a `Launch` is
   built without a Fontaine `Cli`.
10. Lift `LineReader` from `claude_subscription/cli.rs` into a new `pub` module,
    `fontaine-core/src/line_reader.rs`, generic over `AsyncRead`, moving `LineBuffer` and
    `ReadFail` with it, and use it from both sides.

**Step 3: The surface.**
11. `rust/fontaine/src/lib.rs`: argv-shape detection in `run`, next to the pre-clap hidden-mode
    check → `claude_compat::run`.
12. `claude_compat/cli.rs`: Claude flag struct, the capability matrix, and mapping to the
    headless `Options` plus the compat extras.
13. `claude_compat/wire.rs` and `translate.rs`: typed messages and `ClaudeSink`.
14. `claude_compat/router.rs` and `control.rs`: startup read, demux, writer, initialize,
    interrupt, set_permission_mode, set_model, mcp_status, cancel, EOF.

**Step 4: Callbacks.**
15. `claude_compat/approver.rs`: `SdkApprover` (approve, ask_user, updatedPermissions validation,
    suggestions).
16. `claude_compat/hooks.rs` and `mcp.rs`: the callback-hook `HookTransport` and the SDK MCP
    transport factory, plus `initialize.agents` mapping.

**Step 5: Conformance.**
17. `rust/fontaine/tests/claude_sdk_compat.rs`: builds the wiremock sandbox from
    `tests/common`, runs each consumer program (its env reaches `fontaine` through the SDK's
    inherited environment), and asserts on the program's JSON report and on Fontaine's
    audit/ledger. When `node` / `python3` / the pinned packages are missing it prints a skip
    notice, unless `FONTAINE_SDK_COMPAT=required`, in which case it fails.
18. `.github/workflows/ci.yml`: an `sdk-compat` job (Linux) that installs the pinned packages
    and runs that test with `FONTAINE_SDK_COMPAT=required`. It needs no provider keys, since the
    model is wiremock.

**Step 6: Docs and plan.**
19. `docs/agent-sdk.md` (new), `docs/headless.md`, `docs/permissions.md`, `docs/mcp.md`,
    `docs/hooks.md`, `docs/telemetry.md`, `docs/guardian.md`, `docs/agents.md`,
    `docs/extensions.md`, `docs/SUMMARY.md`, `docs/index.md`, `CHANGELOG.md`.
20. Master plan `## Decisions`: the lines listed under Decisions below, in their subsections;
    also the workspace-layout `ci.yml` step list (`tasks/fontaine_harness_plan.md` ~:205-207),
    which gains the `sdk-compat` job.

## Files to Modify

- `rust/fontaine/src/{lib.rs,headless.rs}`, `rust/fontaine-core/src/{extensions.rs,agent.rs}`
- `rust/fontaine/src/claude_compat/` (new: `mod.rs`, `cli.rs`, `wire.rs`, `router.rs`,
  `translate.rs`, `approver.rs`, `hooks.rs`, `mcp.rs`, `control.rs`)
- `rust/fontaine-core/src/permissions.rs`, `tier.rs`, `ledger.rs`, `audit.rs`,
  `user_hooks.rs`, `agent/mcp.rs`, `agent/mcp_supervisor.rs`, `agent/setup.rs`,
  `agent/build.rs`, `agent/models.rs`, `agent/reload.rs`, `statusline.rs`
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (LineReader moved out), `rust/fontaine-core/src/line_reader.rs` (new)
- `rust/fontaine-tools/src/mcp/servers.rs`
- `rust/fontaine-agents/src/registry.rs`
- `rust/fontaine-tui/src/app.rs`, `rust/fontaine-tui/src/ui/tests.rs`,
  `rust/fontaine-tui/tests/common/mod.rs`,
  `rust/fontaine-evals/src/none.rs`
- `rust/fontaine/tests/claude_sdk_compat.rs`, `rust/fontaine/tests/sdk_compat/**`,
  `rust/fontaine/tests/fixtures/claude_sdk/**`
- `rust/fontaine-core/tests/` (`audit_telemetry.rs`, `common/pipeline.rs`, `common/runtime.rs`, pipeline tests for `Delegate` and `AllowModified`)
- `rust/fontaine-tools/tests/mcp_registration.rs`
- `evals/claude_sdk/capture.sh`
- `.github/workflows/ci.yml`, `.gitignore`, `.ignore`
- `docs/agent-sdk.md`, `docs/headless.md`, `docs/permissions.md`, `docs/mcp.md`,
  `docs/hooks.md`, `docs/telemetry.md`, `docs/guardian.md`, `docs/agents.md`,
  `docs/extensions.md`, `docs/SUMMARY.md`, `docs/index.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Argv-shape detection vs a second binary (`fontaine-agent-sdk`).** A second binary is more
  explicit, but it adds a `dist` artifact, a signature and a `fontaine update` target. The SDK
  argv shape is fixed, distinctive, and meaningless to Fontaine today, so detection on it is
  unambiguous and keeps one binary. An env-var trigger (`CLAUDE_CODE_ENTRYPOINT=sdk-*`) was
  rejected because it is inherited by any `fontaine -p` a Claude Code session's shell spawns, so
  it would silently flip `headless-v1` consumers.
- **A separate compat sink vs evolving `headless-v1`.** The issue requires `headless-v1` to stay
  unchanged, and the eval runner depends on it. A sink seam keeps one session loop.
- **Claude tool names with Fontaine inputs vs full input translation.** Translating each tool's
  arguments both ways would double the attack surface of `updatedInput` (every translation is
  something the re-adjudication would have to trust). Mapping names only keeps a host's
  name-based deny policy working, the case that would otherwise fail open silently. Input
  differences are documented per tool.
- **Re-running `adjudicate` for modified input vs a narrower re-check.** A narrower re-check
  (floor plus guardian only) would be a second, partial pipeline. A full restart reuses the one
  that exists and cannot skip a stage.
- **Steering for mid-turn input vs strictly queueing.** Steering matches the TUI and Claude
  Code's interactive delivery. Strict queueing would make long tool loops unresponsive to the
  host.

## Decisions

- 2026-11-13: SDK mode is selected by argv shape (stream-json in and out, `--verbose`, no
  `-p`), not by env or a second binary.
- 2026-11-13: Builtin tools carry their canonical Claude name on the SDK wire; argument objects
  stay in Fontaine's schema.
- 2026-11-13: `claude_code_version` in SDK-mode `init`/`initialize` reports the conformance
  baseline's Claude Code protocol version; `fontaine_version` carries the real one.
- 2026-11-13: The `canUseTool` delegate is reached only from `ask_human`, never offers
  `AllowAlways`, and accepts `updatedPermissions` only as session allows or tightenings;
  anything looser fails the answer closed.
- 2026-11-13: Modified input restarts `adjudicate`; a second modification is denied.
- 2026-11-13: `can_use_tool` and question requests have no timeout (as a TUI prompt); hook
  callbacks use the matcher or command-hook timeout.
- 2026-11-13: `--setting-sources` never drops Fontaine project-scope tightening.
- 2026-11-13: Valid Claude flags Fontaine does not implement are classed unsupported and exit 2.
- 2026-11-13: SDK mode keeps `AgentSetup.headless = true`; only the pipeline's human channel
  becomes Delegate.

Master-plan lines to add (`### Headless` unless noted):
- "(#582, supersedes the 2026-10-21 no-external-approval-delegate line) Claude Agent SDK mode
  accepts `--permission-prompt-tool stdio` as an opt-in `canUseTool` delegate, reached only
  where the TUI would ask a human: after hooks, floor, rules, mode and guardian; modified input
  is re-adjudicated from the top; without the flag every would-prompt is still Deny."
- "(#582) Claude Agent SDK mode is a second stream-json surface (Claude's message and control
  shapes), selected by the SDK's argv shape; `headless-v1` is unchanged."
- "(#582) SDK mode reports builtins under their Claude names on the wire with Fontaine's own
  argument schema; `claude_code_version` reports the conformance baseline's protocol version
  and `fontaine_version` the real one; `can_use_tool` and question requests have no timeout."
- "(#582) Valid Claude flags Fontaine does not implement are classed unsupported and exit 2."
- `### Config`: "(#582) `--setting-sources` never drops project-scope tightening, Claude settings
  files are not read, and `--settings` other than `{}` exits 2."
- `### Permissions`: "(#582) `HumanChannel::Delegate`: a host-delegated approver never offers
  Allow-always and cannot persist rules beyond the session."

## Documentation

- **`docs/agent-sdk.md`** (new): selecting `fontaine` as the executable (TS
  `pathToClaudeCodeExecutable`, Python `cli_path`); tested SDK versions; the capability matrix
  (generated table); permission behaviour (where `canUseTool` sits, modified input,
  `updatedPermissions`, no bypass mode); AskUserQuestion; SDK MCP servers and callback hooks;
  session flows (continue/resume/fork; the SDK's `listSessions`/`getSessionMessages` read
  Claude's own transcript files and do not see Fontaine sessions); tool-name mapping and
  per-tool argument differences; remaining differences from Claude Code.
- **`docs/headless.md`**: the Denials paragraph ("External approval delegates are not
  supported") now points to SDK mode; a cross-link from "Using it as a driver".
- **`docs/permissions.md`**: the delegate as a human channel.
- **`docs/mcp.md`**: SDK servers.
- **`docs/hooks.md`**: callback hooks.
- **`docs/telemetry.md`**: `tool_decision` attributes gain `human_channel`.
- **`docs/guardian.md`**: in SDK mode a headless escalation can reach `canUseTool` (~:428, :681).
- **`docs/agents.md`** and **`docs/extensions.md`**: the session agent origin.
- **`docs/SUMMARY.md`** and **`docs/index.md`**: list the new page.
- **`CHANGELOG.md`**: an entry for SDK mode.

## Testing Strategy

**Unit tests (in the modules):**
- `cli.rs`: argv-shape detection (every shape the SDKs send → SDK mode; `-p` forms and the TUI
  → not; a value-taking flag whose operand is `-p`, `--verbose` or `stream-json` does not flip
  detection); every matrix row (supported flags map to the expected `Options`, unsupported ones exit
  2 with the flag named, an unknown flag exits 2); permission-mode mapping, with
  `bypassPermissions` rejected; the matrix renders to the docs table.
- `wire.rs`: serde round-trip of every message and control shape against the checked-in
  `claude` baseline fixtures (same keys, same types for the fields the SDKs read).
- `translate.rs`: `SessionEvent` sequences → expected messages (tool-name mapping, subagent
  `parent_tool_use_id`, `stream_event` synthesis, `result` subtypes, usage/modelUsage, unpriced
  → `total_cost_usd: null`, denials in both places).
- `approver.rs`: the response-mapping table row by row; `updatedPermissions` acceptance and
  rejection per type and destination; suggestions only at `session`; error, malformed, cancel
  and EOF → Deny / `Unavailable`.
- `user_hooks.rs` (beside `a_timed_out_command_hook_loses_its_whole_tree`): the callback
  constructor sets `fails_closed` from `onFailure`; a transport timeout or error maps to
  `HookError::Failed` (`fontaine-core/tests/hook_slot.rs:273` already covers fail-closed denial).
- `hooks.rs`: matcher mapping: `Write|Edit` → two callback hooks via `map_tool_entry`, an
  unmappable alternative fails `initialize` naming it, empty and `*` stay broad.
- `router.rs`: request-id correlation, cancel, malformed line → error result, EOF with pending
  requests → all fail closed, a control request served while a turn runs, input ordering against
  notification turns.

**Core pipeline tests** (`fontaine-core/tests`, `Scripted` approver with `Delegate`):
- The delegate is never asked for a floor deny, a protected path, a guardian deny, a guardian
  error or timeout, or an AFK-stopped run, and is asked for a manual-mode Prompt.
- `AllowModified` whose new args hit a floor deny is denied; one whose new args are out of
  scope goes back through scope; one a second time is denied; one that is clean is allowed with
  the new args (the executed args are asserted).
- Delegate answers are recorded with `human_channel = delegate`; `AllowAlways` is never offered.
- An SDK MCP tool is registered `Arbitrary` and gated (asserted beside
  `fontaine-tools/tests/mcp_registration.rs` with a `Transport::Sdk` server, or in a core-side
  test).

**Regression guard:** every existing `headless-v1` test, schema validation included, passes
unmodified.

**Real-SDK conformance** (`claude_sdk_compat.rs`, the `sdk-compat` CI job). Unmodified TS and
Python consumer programs, deterministic wiremock model, covering the issue's list: one-shot
`query()` with streamed output; sequential and mid-turn input; continue, resume and fork;
`canUseTool` allow, deny, modified input, permission update and disconnect; AskUserQuestion; an
SDK MCP custom tool and a callback hook; interrupt; mode and model controls; malformed input;
unsupported options (TS: the raised error names the flag; Python: `ProcessError` with exit code 2 and, via its `stderr` callback, the flag named); process shutdown; subagent attribution
and result/usage shapes. The CI job installs pinned
packages from the registries and contacts no model provider.

## Open Questions

- Whether Claude Code delivers mid-turn user messages at the next tool boundary or only at turn
  end. The early baseline capture (Step 1) settles it before `control.rs` is written. The plan assumes the
  tool-boundary delivery described in §7.
