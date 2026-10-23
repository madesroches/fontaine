# Phase 5 — Extensibility: MCP, sessions, parallel agents, hooks, extension files

**Status:** done (offline gates green; live steps are human) · **Depends on:** [Phase 4](phase4_guardian.md) · **Master:** [fontaine_harness_plan.md](../../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Parallel agent orchestration (fontaine-agents)

Managing many agents concurrently in one session is a **v1 requirement**, patterned on Claude Code's documented subagents/background tasks per the design tie-breaker:
each subagent runs in its own context window, background subagents run concurrently, their permission prompts surface
in the main session, and their results return to the parent as a summary.

- **The `agent` tool** (Claude Code's `Agent`/`Task` tool): a first-party tool that spawns a subagent. Each subagent is
  a separate **rig-agent** conversation (`AgentBuilder` + `AgentRun`, its own `rig-memory` context), with its own
  selectable model and a **per-agent-type restricted tool registry** drawn from Fontaine's risk registry — a subagent's
  registry is a subset of the parent's, never a superset. The `agent` tool blocks until the child finishes and returns its report as the tool result (Claude Code parity); with `background: true` it returns `{agent_id}` immediately and the child's report is delivered later as a notification injected into the parent's next turn, passing through `ResultPipeline` like any tool result. With `tool_concurrency = 1`, concurrency comes from background children. Listing,
  status streaming and interrupt/kill are TUI-only user controls (the agents panel), not model tools.
- **Results re-enter through the gate.** A subagent's report comes back to the parent as an ordinary *tool result*, so
  when a scanner is configured (`[guardian] scanner`, off by default) it passes the tool-result scanner like any other
  result — only then is injection text surviving inside a subagent's output flagged before the parent reads it. The
  classifier does not screen reports at handoff — the guardian by construction never sees tool outputs (Deviation
  recorded in Decisions).
- **Guardian gating.** (i) The **spawn request itself is a pipeline request**: task text × toolset × model is a risky
  composite — adjudicated by the guardian in `auto`, prompted in manual modes. (ii) **Every subagent tool call goes
  through the same permission pipeline** as the parent's (including Tier 0.5). Mode inheritance is one-way: a
  subagent's effective mode is **≤ the parent's** — never looser (a `manual` parent cannot hand a child `auto`).
  The `[agents.types]` `mode` key defaults to `"inherit"` = the
  parent's effective mode; an **explicit** per-type `mode` may only be `manual` or `dontAsk` —
  never looser than inherited, on the session ladder `dontAsk < manual < acceptEdits < auto`
  (`plan` is off the ladder: a session-root interactive posture, not a child mode).
  `acceptEdits`/`auto` are reachable for a child only via `"inherit"` from a parent already
  there. While the parent is in `plan`, every child's edits are structurally blocked (plan's edit block is inherited) and the child's non-edit calls use the tighter of the child's own effective mode and `manual` (so `auto`/`acceptEdits` children drop to `manual` for non-edits). The child's mode is
  not fixed at spawn: it is re-evaluated on every call as the tighter of its own setting and the
  parent's current mode, including a parent that switches into `plan` mid-run.
  The guardian's intent for every agent in the tree comes from the **root session's ledger**: `task_intent` and
  `recent_user_messages` are built from the root's human messages only. Spawn prompts and `send_message` bodies reach
  the guardian only as untrusted-delimited context, never as intent.
  (iii) **Parent↔child and peer messages** (the `send_message` tool, Claude Code's `SendMessage`-style) go through the same guardian gate. Addressing: any agent may message its parent, its children, or siblings in the same root tree; `"main"` addresses the root session. Delivery: the message is queued and injected into the recipient's next turn as a notification marked with the sender's agent id, treated as untrusted (it reaches the guardian only as untrusted context); the sender gets `{status: "queued"}`. A target that has finished or been killed returns an error tool result, `agent not running`.
- **Budgets.** `[agents] max_parallel` (default small, e.g. 4) plus per-subagent token/cost budgets, so fan-out cannot
  silently multiply guardian round-trips and model spend (Claude Code documents that each subagent spends tokens of
  its own and running many multiplies usage). The per-subagent cap **always** binds; a root-of-tree total binds
  **when set**: `[agents] session_token_budget` / `session_cost_budget_usd` (default unset) — when set, every
  agent in the tree debits this root session total (a hard fan-out ceiling); when unset, tree exposure is
  structurally bounded by `max_parallel` (tree-wide concurrency) × the per-subagent caps × the 3-layer depth cap.
  A subagent hitting its cap **stops**, returns its result as **partial**, and its pending
  escalation/approval prompts are **cancelled as denied**. Nesting is capped, mirroring Claude Code's
  documented rules: a subagent may itself spawn subagents up to a default depth of **three layers below the main
  conversation**, and at the cap the `agent` tool is withheld from further subagents. `max_parallel` bounds concurrent
  agents across the **whole session tree**, not just the parent's direct children. A cost cap (per-subagent or session) on a model with no catalog price is inactive, with a one-time notice; the token cap still binds. An explicit `--max-budget-usd` on an unpriced model stays a usage error (exit 2). Per-subagent and session token/cost caps include the child's guardian spend, tallied per agent (headless `--max-budget-usd`/`total_cost_usd` already include guardian spend via `session_cost` in `rust/fontaine/src/headless.rs`; today guardian usage is one session-wide tally in `rust/fontaine-core/src/gate.rs`, which step 3 makes per agent).

### Workspace trust and project executables

Project and project-local MCP servers (`[mcp.servers.*]` in `.fontaine/settings{,.local}.toml`), hooks, and
skills' `!cmd` dynamic context are project-local executables (`tasks/design/config.md` §Config, Workspace trust). In
the TUI, before any of them loads, an untrusted primary root gets the trust prompt (trust / don't
trust; "trust" is written to the trust store from Phase 0; "don't trust" applies to the current session only and is re-prompted next session; trusting a root also enables its untracked `settings.local.toml` loosening keys); each project MCP server is then
approved individually at first load, and the repository cannot pre-approve its own servers. Declining
loads the session without them. Headless `-p` loads them without a prompt, as Claude Code does. In
both surfaces they never change a permission decision (`tasks/design/config.md` §Config, Workspace trust): a project
hook's `allow` is ignored and project MCP tools are gated like any MCP tool.

### User hooks

Built to `tasks/design/permissions.md` §User hooks, into the step-1a slot Phase 3 left: command handlers for
`pre_tool_use`, `post_tool_use` and `permission_denied` (`fontaine-core/src/user_hooks.rs`). The
event goes to the handler as one JSON object on stdin with Claude Code's field names: `session_id`, `cwd`, `hook_event_name`, `tool_name` (Fontaine tool names), `tool_input`, plus `tool_response` for `post_tool_use`, and the Fontaine extensions `agent_id`, `permission_mode` (Fontaine mode names) and `workspace_roots`; there is no `transcript_path` (Fontaine has no transcript file equivalent). The
answer is one JSON object on stdout: `hookSpecificOutput` with `hookEventName`, `permissionDecision` (`allow` | `deny` | `ask`), `permissionDecisionReason`, `updatedInput`, plus the Fontaine extension `guardian_context`. Exit codes as Claude Code: 0 = parse stdout; 2 = block, stderr as the reason (a deny); any other code = non-blocking error that contributes no decision and never loosens. `pre_tool_use`
returns the decision, `updatedInput` and `guardian_context` above; `post_tool_use` may
append a note to the tool result the agent sees, returned as `hookSpecificOutput.additionalContext` (string; a `ResultPipeline` stage that runs after the Phase 6 scanner stage); `permission_denied` may return `hookSpecificOutput.retry` (bool; the agent is
told it may retry, as with `/denied` → `r`). Handlers run (through Git Bash on Windows, found by the same discovery as the `shell` tool, including `FONTAINE_GIT_BASH_PATH`) with the scrubbed child environment and a
`timeout_secs` cap, in a `fontaine.hook` span. A user-scope `allow` in `auto` follows Claude Code: a
narrow matcher skips the guardian, a broad one is suspended. `matcher` is optional (absent counts as broad, so its `allow` is suspended in `auto`), and `timeout_secs` defaults to 30. Every subagent tool call runs the same hooks.

### Extension files (skills, agent files, output styles)

Layout and scope per `tasks/design/config.md` §Config: `skills/<name>/SKILL.md`, `agents/<name>.md`,
`output-styles/<name>.md` under `<config dir>/` and `<root>/.fontaine/`, with Claude
Code-compatible frontmatter (`name`, `description`, `tools`, `model`, `permissionMode` ↦ `mode`
with `default` ↦ `manual`, `allowed-tools`, `disable-model-invocation`, `user-invocable`, `argument-hint`), so `.claude/` skills and
agents port by copying; `.claude/` is not read directly. `user-invocable` (default true) set to false hides the skill from the `/` menu (Claude Code semantics). Unknown frontmatter keys are ignored (Claude Code parity), except permission-related keys: agent files honour `disallowedTools` as a subtraction from the type's toolset, and `hooks`, `mcpServers` (agent or skill) and a skill's `permissionMode` are a load error (exit 2) naming the file.

At load, Claude Code tool names in `tools`, `disallowedTools` and `allowed-tools` (a YAML list or one comma-separated string) are renamed — `Read`→`read`, `Edit`/`MultiEdit`→`edit`, `Write`→`write`, `Glob`→`glob`, `Grep`→`grep`, `LS`→`ls`, `Bash`→`shell`, `WebFetch`→`webfetch`, `Task`/`Agent`→`agent`, `Skill`→`skill`, `SendMessage`→`send_message` — with any `(specifier)` kept verbatim and then checked by the ordinary rule validator; any other name, or a specifier the validator rejects (e.g. the `Bash(git add:*)` colon form), is a load error (exit 2) naming the file. `model: inherit` means no `model` key; any other `model` must be `$main`, `$guardian_default` or `<account>/<model>` (the `[agents.types]` check), so Claude Code aliases like `sonnet` are a load error naming the file.

- **Skills** are progressive-disclosure prompt content: names and descriptions go into the system
  prompt; the body loads through the first-party `skill` tool (`fontaine-tools/src/skill.rs`,
  `RiskClass::ReadOnly`, on the fast path: its only argument is a skill name resolved against the index loaded at
  start, so it can read nothing else and only loads skill text into context). A user-invocable skill is also a `/name` slash command
  (custom slash commands are skills); arguments follow Claude Code: `$ARGUMENTS` is replaced by the full argument string, `$ARGUMENTS[N]`/`$N` are positional, and when the body has no placeholder `ARGUMENTS: <args>` is appended; `argument-hint` is shown in the `/` menu. Substitution happens before `!cmd` blocks run. `allowed-tools` from a user-trust-scope skill is a set of
  allow rules while the skill is active — from the invocation to the end of that user turn (a headless run's single turn), for the invoking agent only (never its subagents), the same for `skill`-tool and `/name` invocation, acting after the floor and the mode like any allow rule (same suspension in `auto`); from project scope
  it is ignored. `!cmd` dynamic context runs through the permission pipeline as a `shell` call.
- **Agent files** load into the step-3 agent-type registry beside `[agents.types]` (project overrides
  user on a name clash); project files only narrow — toolset a subset of the parent's, `mode`
  `manual` (default) or `dontAsk`, no `model` key (session model) — and a project file breaking that
  is rejected at load (exit 2). User-scope agent files follow the `[agents.types]` mode rule: `permissionMode` is accepted only as `inherit`, `default` (↦ `manual`) or `dontAsk`; any other value (`acceptEdits`, `auto`, `plan`, `bypassPermissions`) is a load error (exit 2) naming the file.
- **Output styles** are system-prompt overlays selected per session with `/output-style`. Output styles never alter the guardian's prompt or `AdjudicationRequest`.

### TUI extension — background-agents panel

One row per running subagent — type, model, status (running / waiting-on-permission / done /
killed), token/cost spend vs budget — with keys to inspect a row's transcript, kill it, and
see its pending approval prompts; per-agent guardian decisions (including Tier-0.5 audit entries)
are listed per row, so a background subagent never hides an approval behind the parent's
transcript.

## Steps

1. `fontaine-mcp` (rig-rmcp): stdio + streamable-HTTP servers from `[mcp.servers.*]` (user,
   project-local, CLI; project servers only after workspace trust + per-server approval in the TUI —
   design above; the trust prompt is `rust/fontaine-tui/src/trust_prompt.rs`). `McpClientHandler::new(config, sink)`
   registers raw `DynamicTool`s with whatever sink it is given — again on every list-changed
   refresh — so Fontaine passes a wrapping sink that registers every MCP tool, initial and
   refreshed, as a `GuardedTool` (DynamicTool path) with `Arbitrary` risk under the name
   `mcp__<server>__<tool>` (Claude Code naming); the agent's `ToolServerHandle` is never handed to
   rig-rmcp directly. rig-rmcp enables only rmcp's `client` feature, so the transports need a direct
   `rmcp` dependency, pinned `=3.6.0`, inside rig-rmcp 0.45.0's declared 3.5 range
   (one rmcp in the tree) with `rmcp = { version = "=3.6.0", default-features = false, features = ["client", "transport-child-process", "transport-streamable-http-client-reqwest"] }`; TLS comes from the workspace reqwest `rustls` feature. MCP tool annotations travel to the
   guardian as untrusted text inside the `tool_call` block (rendered within the prompt's existing untrusted
   delimiting, never as trusted facts and never skipping the guardian): an optional annotations field on
   `ToolCallSummary` (`rust/fontaine-guardian/src/request.rs`) with the render in
   `rust/fontaine-guardian/src/prompt_builder.rs` and a snapshot test. Telemetry: `mcp_server_connection` and
   `fontaine.mcp.call` (server and tool names only under `log_tool_details`, otherwise `"third-party"`).
2. Session persistence over the session's `rig-memory` `DirStore` (the conversation log the agent is built with via `AgentBuilder::memory(..).conversation(..)`), with the
   Phase-2 ledger as the second store (`tasks/design/sessions.md` §Session ledger): `--continue` / `--resume <id>`
   restore context and ledger by reopening both stores (through new session/ledger open/load constructors beside today's `Session::create` and `Ledger::create`), start in the resolved default mode and drop session grants;
   `--fork-session` copies both stores (or uses rig-memory's `Writer::fork_log` for the conversation log) under a new session id without session grants (`--session-id` sets that new id under `--fork-session`, as Claude Code allows);
   `--continue` picks the most recently updated session whose primary root is the launch cwd and starts a fresh session when there is none (Claude Code parity); `--session-id` with `--continue` or `--resume` but without `--fork-session` is a usage error (exit 2), and the `--session-id` help text in `rust/fontaine/src/cli.rs` says so;
   `fontaine.session.count` `start_type` takes Claude Code's values `fresh` | `resume` | `continue` (a fork takes the value of the flag it forks from; `--continue` with nothing to continue is `fresh`), and today's surface value moves to a `headless` (`true` | `false`) attribute (`rust/fontaine/src/lib.rs`, `rust/fontaine/src/telemetry/span_bridge.rs` and its `start_type = "headless"` test).
3. `fontaine-agents`: defines the `ChildRunner` trait (`rust/fontaine-agents/src/runner.rs`: spawn a gated child
   run from agent type, effective mode, budget and a `RootLedger` handle; returns a handle for status, kill
   and report) which `fontaine-core` implements (`rust/fontaine-core/src/child_runner.rs`) and injects at startup, like
   `ToolGate`; the core impl re-evaluates the child's mode as the tighter of its own and the parent's on every call. `agent` spawn tool over rig-agent — concurrent background subagents with
   own context/model/per-type restricted registries; `RootLedger` is a small trait defined in `fontaine-agents` (`rust/fontaine-agents/src/root_ledger.rs`: read root human messages, append child events) and implemented by `fontaine-core`'s ledger, the same inversion as `ToolGate`/`ChildRunner`; list/stream-status/interrupt as TUI-only controls; spawn call
   gated as a pipeline request; subagent tool calls re-enter the permission pipeline with
   one-way mode inheritance (child mode ≤ parent mode); agent-to-agent messages (`send_message`) through the same
   guardian gate; results returned as ordinary tool results through `ResultPipeline` (Phase 2; the Phase 6 scanner
   stage attaches there); `[agents]` budgets
   enforced; TUI background-agents panel (status, kill, per-agent decisions). Telemetry:
   `fontaine.subagent` spans (`agent.id` / `agent.type`) parented to the spawning tool call,
   `subagent` lifecycle events (incl. `budget_exhausted`).

   New registered model tools this phase adds (each wrapped; the exact set asserted by `rust/fontaine-core/tests/choke_point.rs` (created over `fontaine_core::tools::registry`, extended to agent, skill, send_message and MCP tools) is
   the Phase-2 table plus exactly these, plus dynamic `mcp__<server>__<tool>` names):

   | Tool | RiskClass | Args |
   |---|---|---|
   | `agent` | Arbitrary | `agent_type`, `prompt`, optional `model`, optional `background` (bool, default false: blocks and returns the report; true: returns `{agent_id}` at once, report delivered later as a notification); withheld from subagents at the depth cap |
   | `skill` | ReadOnly | `name` (resolved against the start-time index) |
   | `send_message` | Arbitrary | `to` (agent id of a parent, child or sibling in the same root tree, or `"main"` for the root session), `message`; returns `{status: "queued"}`, or error `agent not running` for a finished or killed target |

   The three risk classes are declared in a second table beside `BUILTIN_RISK` in `rust/fontaine-tools/src/risk.rs`, `EXTENSION_RISK` (`agent` Arbitrary, `skill` ReadOnly, `send_message` Arbitrary), which core's registry assembly reads when wrapping them; `BUILTIN_RISK` and the `fontaine-tools` builtins-only choke-point test stay at the 8 Phase-2 tools, core's choke-point test checks `BUILTIN_RISK` ∪ `EXTENSION_RISK`, and `the_tools_page_names_every_registered_built_in_tool` (`rust/fontaine-core/tests/docs.rs`) iterates both tables.

   The `agent` tool's per-call `model` wins over the agent type's `model`, which wins over the session model (Claude Code's precedence); it accepts the `[agents.types]` values (`$main`, `$guardian_default`, `<account>/<model>`), and an invalid value — or any per-call `model` for a type defined by a project agent file, which stays on the session model — is an error tool result. A spawn that would exceed `[agents] max_parallel` (tree-wide running count, blocking or background) is not queued: it returns the error tool result `concurrent subagent limit reached (max_parallel = N)` telling the model not to retry (Claude Code parity). In headless `-p`, when the root's turn ends with background children still running, the run waits for them and gives the root another turn with their report notifications, repeating until none runs, before emitting `result`; `--max-turns` and `--max-budget-usd` count those turns, and a run stopped by either kills the running children, whose partial reports are not delivered.

4. User hooks (design above): extend `PreToolUseEvent` (`rust/fontaine-core/src/permissions.rs`) with the wire fields `session_id`, `cwd`, `permission_mode`, `workspace_roots`; wire configured hooks into the agent in `rust/fontaine-core/src/agent.rs` (today `hooks: Vec::new()`); `post_tool_use` attaches to `ResultPipeline` as its hook stage, where `rust/fontaine-core/src/tools.rs` builds `ResultPipeline::new()`; `permission_denied` is invoked from the pipeline's deny paths (`rust/fontaine-core/src/permissions.rs`) and its `retry` feeds the existing gate retry grant (`approve_retry`/`take_retry` in `rust/fontaine-core/src/gate.rs`), via a new non-index retry entry beside `approve_retry`.
5. Extension files (design above): loader in `fontaine-core/src/extensions.rs`; `skill` tool and
   skill slash commands (the `skill` tool is in the step-3 table; it is constructed and wrapped by `fontaine_core::tools::registry`, not by `fontaine-tools`' `builtins::build`, so the Phase-2 builtins-only check stays at 8 entries); agent files into the agent-type registry; output styles.
6. Docs — `docs/mcp.md`: declaring stdio / streamable-HTTP servers in config, project servers
   behind workspace trust and per-server approval, and the MCP trust model (server annotations reach the guardian only as untrusted text, never as gates; all MCP tools
   register `Arbitrary`); `docs/agents.md`: the `agent` tool, `[agents]` budgets, `[agents.types]`
   (model / restricted toolset / mode with the ≤-parent ladder), agent files and project-agent
   narrowing, nesting depth cap, spawn gating, per-agent audit in the background-agents panel;
   `docs/hooks.md`: events, handler I/O, place in the pipeline, composition, failure semantics,
   scopes and trust; `docs/extensions.md`: skills, slash commands, output styles, layout,
   frontmatter, porting from `.claude/`; extend `docs/tui.md` with the panel, the trust prompt and
   `/output-style`; add the four new pages to `docs/index.md` and to the required-pages list in `rust/fontaine-core/tests/docs.rs`; add resume/fork to `docs/architecture.md` and `--continue` / `--resume` /
   `--fork-session` to `docs/headless.md`; add subagent/MCP telemetry to `docs/telemetry.md` and change its `fontaine.session.count` row to `start_type` (`fresh`/`resume`/`continue`) plus `headless`; extend
   `docs/configuration.md` with `[mcp.servers.*]`, `[hooks.*]` and every
   `[agents]`/`[agents.types]` key; extend `docs/tools.md` with inventory rows for `agent`, `skill` and `send_message` (required by `the_tools_page_names_every_registered_built_in_tool`); in `docs/permissions.md` replace the sentence "No hook is configurable in this build; the slot exists and is tested" with a link to `docs/hooks.md`; append a CHANGELOG entry.

## Roadmap slot (not urgent)

Amendments A3–A4 (analysis-strategic-verdict; folded in per user decision 2026-10-21): a
**sandbox-interop spike** for running Fontaine under Anthropic's open-source
`@anthropic-ai/sandbox-runtime` (srt) / bubblewrap — complements, does not replace, the guardian
(sandbox = blast radius, guardian = intent). Phase 4 unlocks `[guardian] profile = "minimal"` through
`--in-container` plus the Linux runtime probe; this slot adds srt/bubblewrap launch detection to that
probe (`rust/fontaine-core/src/containment.rs`) as a second verification route. When taken, its deliverables:
the spike, the **escape-detection test suite** (mount/umount, docker.sock, cgroup-control writes,
bind-mount symlink escape, proxy bypass → denied even under minimal — the srt/bubblewrap leg of `tasks/design/testing.md` Manual Verification item 6),
and **`docs/sandboxing.md`** documenting the srt/bubblewrap integration path and the escape classes minimal must still deny.

## Files created or modified

`rust/Cargo.toml` (`rig-rmcp =0.45.0`; direct `rmcp` pin `=3.6.0`, features `client`, `transport-child-process`, `transport-streamable-http-client-reqwest`, `default-features = false`; TLS via the workspace reqwest `rustls` feature); `rust/fontaine-mcp/src/{lib,servers,register}.rs`; `rust/fontaine-agents/src/{lib,spawn,registry,budget,runner,root_ledger}.rs`;
`rust/fontaine-core/src/{session,user_hooks,extensions,child_runner,permissions,agent,tools,gate}.rs`; `rust/fontaine-guardian/src/{request,prompt_builder}.rs`; `rust/fontaine-tools/src/{lib,skill,risk}.rs` (`pub mod skill;`; `EXTENSION_RISK`); `rust/fontaine/src/{cli,lib,headless}.rs` and `rust/fontaine/src/telemetry/span_bridge.rs` (resume flags, `start_type`, headless wait for background children); `rust/fontaine-core/tests/docs.rs` (tools-page test over both risk tables);
`rust/fontaine-tui/src/{agents_panel,trust_prompt}.rs`;
`docs/{mcp,agents,hooks,extensions,tools,permissions,tui,configuration,architecture,headless,telemetry,index}.md`.

## Tests owned by this phase

- **parallel agents** (`tasks/design/testing.md` Testing Strategy; `rust/fontaine/tests/parallel_agents.rs`): spawn-call gating (task × toolset × model
  composite); mode-inheritance invariant (child ≤ parent, never looser), including a test where the
  parent switches mode mid-run and the child's next call follows the tighter mode; a `plan` parent blocks every child edit and drops an `auto`/`acceptEdits` child's non-edit calls to `manual`, including when the parent enters `plan` mid-run; per-type registry
  is a subset of the parent's; budget caps stop a subagent; a blocking `agent` call returns the child's report as its tool result through `ResultPipeline`; `background: true` returns `{agent_id}` at once and the report arrives later as a notification in the parent's next turn, passing through `ResultPipeline`; N background children run concurrently; `send_message` hits the
  same gate; delivery to a parent, a child, a sibling and `"main"` arrives in the recipient's next turn marked with the sender's agent id and appears to the guardian only as untrusted context; the sender gets `{status: "queued"}`; a finished or killed target returns error `agent not running`; a spawn past `max_parallel` (blocking or background) returns the `concurrent subagent limit reached` error tool result and starts nothing; the per-call `model` overrides the type's `model`, an invalid value or one on a project-file type is an error tool result; headless `-p` with a background child still running at the root's final answer waits for it, runs a further root turn with its notification, and emits `result` after it, and a `--max-budget-usd` stop kills the child.
- **unpriced-model cost cap** (`rust/fontaine/tests/parallel_agents.rs`): a subagent on an `openai-compatible` model with `subagent_cost_budget_usd` set runs with the cost cap inactive and one notice, and the token cap still stops it.
- **ChildRunner seam** (`rust/fontaine-agents/tests/`, fake `ChildRunner`): the spawn tool, registry and budget logic run with no `fontaine-core` dependency; the fake receives type, effective mode, budget and a fake `RootLedger`; status/kill/report flow through the returned handle.
- **system prompt** (`rust/fontaine-core/tests/`): the Phase 2 snapshot of the assembled prompt is extended with the skill index and an active output style in the layering order (base → `FONTAINE.md` → skill index → output style → append).
- **output styles** (`rust/fontaine-core/tests/`): the guardian's `AdjudicationRequest` and rendered prompt are byte-identical with and without an active output style.
- **agents panel** (`rust/fontaine-tui/tests/`, ratatui `TestBackend` with a scripted model): a killed subagent's row shows `killed` and its token/cost spend against budget.
- **subagent intent** (`rust/fontaine/tests/parallel_agents.rs`): an injected spawn prompt claiming user authorization does not appear as `task_intent` or in `recent_user_messages` in any `AdjudicationRequest`; it appears only inside an untrusted-delimited block.
- **choke point** (create `rust/fontaine-core/tests/choke_point.rs`, over core's registry-assembly function — the one place built-ins, `agent`/`send_message`/`skill` and MCP tools are all assembled; the Phase-2 builtins-only check stays in `fontaine-tools`): the exact registered set is the Phase-2 table plus `agent` (Arbitrary), `skill` (ReadOnly) and `send_message` (Arbitrary) — no other names, no duplicates, every one with a `RiskClass`; the non-MCP names equal `BUILTIN_TOOL_NAMES` from `fontaine-tools` (`src/names.rs`); no list/status/kill tool is registered for the model.
- Parallel orchestration (deterministic `test_utils` models or cassettes; scripted endpoints via the Phase-1 `common/mod.rs` helper): spawn N subagents, kill
  one, assert the others complete and every tool call has a gate decision in the audit log keyed by
  agent id.
- MCP (`rust/fontaine-mcp/tests/mcp_registration.rs`): servers from config register wrapped `Arbitrary`; a declared `readOnlyHint` never earns a
  fast path (`rust/fontaine-core/tests/choke_point.rs` extended to MCP-imported tools and to a list-changed refresh: tools added or
  replaced by a refresh are registered wrapped as `Arbitrary` under `mcp__<server>__<tool>`, none
  reachable raw).
- Persistence: save/resume round-trip through the reopened `rig-memory` conversation log + ledger; resume starts in the default mode
  without session grants; fork copies both stores under a new id; neither carries session grants; `--continue` picks the most recently updated session of the launch cwd's primary root and starts fresh when there is none; `--session-id` with `--continue`/`--resume` without `--fork-session` exits 2; `fontaine.session.count` carries `start_type` `fresh`/`resume`/`continue` and `headless` `true`/`false`.
- **workspace trust** (TUI `TestBackend`): an untrusted root's project and project-local MCP servers and hooks do not
  start before the trust prompt is accepted; declining starts none; a trusted root still asks per
  new project server; headless `-p` loads them, and in both surfaces a project hook's `allow` is
  ignored and a project MCP tool call is gated like any `Arbitrary` call.
- **user hooks** (`rust/fontaine-core/tests/user_hooks.rs`, script handlers in a temp dir): the hook's `updatedInput` is what the floor, Tier 0.5, rules, guardian and audit see; deny > ask > allow; hook
  `allow` never passes the deny rule or critical path and is suspended in `auto` for a broad matcher; error / timeout / malformed output ⇒ no
  decision; project hook `allow` ignored; hook children get the scrubbed environment (no configured `api_key_env` variable, `scanner_hf_token_env`, telemetry credential or `MICROMEGAS_*`); exit code 2 = deny.
- **extensions**: frontmatter parsing of Claude Code-style skills and agents; project agent file
  with `model`, `mode = "inherit"`/`auto`, or a tool outside the parent's set is rejected; project
  agent overrides a same-name user agent and stays narrowed; project skill `allowed-tools` ignored;
  `!cmd` runs through the pipeline as `shell`; skill argument substitution (`$ARGUMENTS`, `$ARGUMENTS[N]`/`$N`, appended `ARGUMENTS: <args>` when no placeholder, before `!cmd`) and `user-invocable: false` hiding the skill from the `/` menu; unknown frontmatter keys ignored; `skill` in the choke-point test with its risk class from `EXTENSION_RISK`; Claude Code tool names (list and comma-separated string, with `Bash(git *)` ↦ `shell(git *)`) and `model: inherit` load, while an unmapped tool name, the `Bash(git add:*)` colon form and `model: sonnet` are load errors naming the file; `disallowedTools` removes tools from the type's toolset; `hooks`/`mcpServers` in an agent or skill file and a skill's `permissionMode` are load errors naming the file; a skill's `allowed-tools` grant ends with the user turn it was invoked in, is the same for `skill`-tool and `/name` invocation, and does not reach a subagent the invoking agent spawns; a user-scope agent file with `permissionMode` `acceptEdits`, `auto`, `plan` or `bypassPermissions` is rejected at load (exit 2) naming the file, while `inherit` / `default` / `dontAsk` load.
- **telemetry**: N subagents produce per-agent `tool_decision` events keyed like the audit log;
  MCP names appear only under `log_tool_details`.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including `rust/fontaine/tests/parallel_agents.rs` (parallel-agents and
  parallel-orchestration tests), `rust/fontaine-mcp/tests/mcp_registration.rs` (MCP tests),
  `rust/fontaine-core/tests/user_hooks.rs`, and the trust, persistence, extensions and telemetry tests.
- Needs a human: `tasks/design/testing.md` Manual Verification item 5 passes (live look-and-feel only; the killed-row state and budget display are covered by the agents-panel `TestBackend` test, gating and audit by the
  automated parallel-orchestration test).
- Agent step needing a logged-in `claude` CLI (manual or eval-side; never CI): the `claude -p` baseline check (`fontaine-evals` `baseline capture` / `diff`) that a hook `allow` in `auto` skips the
  classifier for a narrow matcher and is suspended for a broad one.
- Item 6's srt/bubblewrap leg ships **with the sandbox slot when taken** (minimal-profile escape denies + probe
  fail-closed under srt/bubblewrap); its Docker leg is owned by Phase 4; the build steps above do not depend on it.
- `rust/fontaine-core/tests/docs.rs` green with `docs/mcp.md`, `docs/agents.md`, `docs/hooks.md`, `docs/extensions.md` in place; every new
  config key documented at ship.
- Trust, hooks, extensions, persistence and telemetry tests above green.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.

## Decisions

- 2026-10-22 AFK: Headless `-p` waits for running background children at the root's final answer, runs a further root turn with their report notifications until none runs, then emits `result`; `--max-turns`/`--max-budget-usd` count those turns and a stop kills the children — Claude Code waits for a background subagent's notification before reporting, and waiting is deterministic for CI where killing loses work.
- 2026-10-22 AFK: Deviation (project-file types only) — the `agent` tool's per-call `model` overrides the type's `model`, which overrides the session model; it takes the `[agents.types]` values, and an invalid value or any per-call `model` on a project-agent-file type is an error tool result — Claude Code's precedence, while a repo-defined type must not end up on a model the repo steered.
- 2026-10-22 AFK: `fontaine.session.count` `start_type` takes Claude Code's values `fresh`/`resume`/`continue` (a fork takes its source flag's value) and the headless/interactive surface moves to a `headless` attribute — today's `start_type = headless|interactive` silently deviated from Claude Code's attribute.
- 2026-10-22 AFK: Extension files map Claude Code tool names to Fontaine's at load (`Read`→`read`, `Edit`/`MultiEdit`→`edit`, `Write`→`write`, `Glob`→`glob`, `Grep`→`grep`, `LS`→`ls`, `Bash`→`shell`, `WebFetch`→`webfetch`, `Task`/`Agent`→`agent`, `Skill`→`skill`, `SendMessage`→`send_message`; specifiers verbatim through the rule validator; comma strings accepted); `model: inherit` = no key; anything unmapped or invalid is a load error naming the file — keeps "port by copying" with a closed rename table and fail-closed on everything else.
- 2026-10-22 AFK: Deviation — `hooks` and `mcpServers` in agent or skill files and `permissionMode` in skill files are load errors naming the file, and agent `disallowedTools` is honoured as a subtraction — files load without workspace trust, so no per-file executables in v1, and ignoring a narrowing key would widen authority.
- 2026-10-22 AFK: The `fontaine-agents` root-ledger trait is named `RootLedger` (`src/root_ledger.rs`) — `LedgerView` is already a `fontaine-core` struct.
- 2026-10-22 AFK: `agent`/`skill`/`send_message` risk classes live in a second `EXTENSION_RISK` table in `fontaine-tools/src/risk.rs`; `BUILTIN_RISK` stays at the 8 builtins, core's choke-point test and the tools-page docs test cover both tables — `builtins::build` cannot register them and `risk_of`'s Arbitrary fallback would hide `skill`'s ReadOnly class.
- 2026-10-22 AFK: A spawn past `[agents] max_parallel` (blocking or background) is not queued; it returns an error tool result naming the limit and telling the model not to retry — Claude Code's `Concurrent subagent limit reached` behaviour, and queueing a blocking spawn can deadlock.
- 2026-10-22 AFK: A skill's `allowed-tools` grant lasts from invocation to the end of that user turn, applies to the invoking agent only (never its subagents), is the same for `skill`-tool and `/name` invocation, and acts after the floor and the mode — the narrowest reading of "while the skill is active".
- 2026-10-22 AFK: `--continue` picks the most recently updated session whose primary root is the launch cwd and starts fresh when there is none; `--session-id` with `--continue`/`--resume` without `--fork-session` is a usage error (exit 2) — mirrors observed `claude -p` (fresh start with exit 0; the `--session-id` combination rejected), with Fontaine's usage exit code.
- 2026-10-22: A stdio MCP server gets a scrubbed environment — a short base set plus the variables named in `env_vars` — and its stderr goes to the debug file; reason: `env_vars` is a pass-through list, so the rest of the parent's environment (provider keys, telemetry credentials) must not reach a third-party process.
- 2026-10-22: The built-in `explore` and `worker` agent types include `send_message`; reason: peer messaging between subagents is a v1 requirement and a type's toolset is closed.
- 2026-10-22: The hook wire uses Claude Code's `PreToolUse` / `PostToolUse` / `PermissionDenied` for `hook_event_name` and `hookEventName` while configuration keys stay snake_case; a `hookEventName` naming another event makes the answer unreadable (no decision).
- 2026-10-22: A `permission_denied` hook's `retry` feeds the retry grant only for guardian denials (the floor and the mode have no retry to give), and a repository hook's `retry` is ignored like its `allow`; reason: a retry grant loosens a decision, and project executables never change one.
- 2026-10-22: An agent file's `tools` entry with a specifier limits that tool's calls for the type, and a `disallowedTools` entry with a specifier forbids the calls it matches; reason: ignoring a narrowing specifier would widen authority.
- 2026-10-22: A personal skill outranks a repository skill of the same name (Claude Code), while a repository agent file replaces a user one (Claude Code and the plan); the `skill` tool takes only `name`, so arguments reach a skill by `/name` only.
- 2026-10-22: A repository's MCP server approval lives in `trust.toml` as `[[approved_servers]]` (root, name, definition digest); reason: the record must be outside the repository and a changed definition must ask again.
- 2026-10-22: `gen_ai.tool.name` and every other exported MCP server or tool name read `third-party` unless `log_tool_details` is on; reason: those names belong to a third party and can name internal systems.
- 2026-10-22: The agents panel opens with `/agents` or Ctrl-T (Ctrl-A is the input's Home key) and takes the keys over a pending approval, whose prompts it also lists per agent.
- 2026-10-22: Notices raised without an interface (such as the pause of `auto` or an inactive cost cap) go to stderr in a headless run.
