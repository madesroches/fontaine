# Phase 5 — Extensibility: MCP, sessions, parallel agents, hooks, extension files

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

## Design (excerpt owned by this phase)

### Parallel agent orchestration (fontaine-agents)

Managing many agents concurrently in one session is a **v1 requirement** (user directive 2026-10-21, superseding
"no subagents in v1"), patterned on Claude Code's documented subagents/background tasks per the design tie-breaker:
each subagent runs in its own context window, background subagents run concurrently, their permission prompts surface
in the main session, and their results return to the parent as a summary.

- **The `agent` tool** (Claude Code's `Agent`/`Task` tool): a first-party tool that spawns a subagent. Each subagent is
  a separate **rig-agent** conversation (`AgentBuilder` + `AgentRun`, its own `rig-memory` context), with its own
  selectable model and a **per-agent-type restricted tool registry** drawn from Fontaine's risk registry — a subagent's
  registry is a subset of the parent's, never a superset. The `agent` tool blocks until the child finishes and returns its report as the tool result (Claude Code parity); with `background: true` it returns `{agent_id}` immediately and the child's report is delivered later as a notification injected into the parent's next turn, passing through `ResultPipeline` like any tool result. With `tool_concurrency = 1`, concurrency comes from background children. Listing,
  status streaming and interrupt/kill/pause are TUI-only user controls (the agents panel), not model tools.
- **Results re-enter through the gate.** A subagent's report comes back to the parent as an ordinary *tool result*, so
  when a scanner is configured (`[guardian] scanner`, off by default) it passes the tool-result scanner like any other
  result — only then is injection text surviving inside a subagent's output flagged before the parent reads it. The
  classifier does not screen reports at handoff — the guardian by construction never sees tool outputs (Deviation
  recorded in Decisions).
- **Guardian gating.** (i) The **spawn request itself is a pipeline request**: task text × toolset × model is a risky
  composite — adjudicated by the guardian in `auto`, prompted in manual modes. (ii) **Every subagent tool call goes
  through the same permission pipeline** as the parent's (including Tier 0.5). Mode inheritance is one-way: a
  subagent's effective mode is **≤ the parent's** — never looser (a `manual` parent cannot hand a child `auto`).
  Mechanism (redesign 2026-10-21): the `[agents.types]` `mode` key defaults to `"inherit"` = the
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
  (iii) **Parent↔child and peer messages** (the `send_message` tool, Claude Code's `SendMessage`-style) go through the same guardian gate — the
  2026-10-20 decision, which stands and now applies in v1. Addressing: any agent may message its parent, its children, or siblings in the same root tree; `"main"` addresses the root session. Delivery: the message is queued and injected into the recipient's next turn as a notification marked with the sender's agent id, treated as untrusted (it reaches the guardian only as untrusted context); the sender gets `{status: "queued"}`. A target that has finished or been killed returns an error tool result, `agent not running`.
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
  agents across the **whole session tree**, not just the parent's direct children. A cost cap (per-subagent or session) on a model with no catalog price is inactive, with a one-time notice; the token cap still binds. An explicit `--max-budget-usd` on an unpriced model stays a usage error (exit 2).

### Workspace trust and project executables

Project and project-local MCP servers (`[mcp.servers.*]` in `.fontaine/settings{,.local}.toml`), hooks, and
skills' `!cmd` dynamic context are project-local executables (`tasks/design/config.md` §Config, Workspace trust). In
the TUI, before any of them loads, an untrusted primary root gets the trust prompt (trust / don't
trust; the answer is written to the trust store from Phase 0); each project MCP server is then
approved individually at first load, and the repository cannot pre-approve its own servers. Declining
loads the session without them. Headless `-p` loads them without a prompt, as Claude Code does. In
both surfaces they never change a permission decision (`tasks/design/config.md` §Config, Workspace trust): a project
hook's `allow` is ignored and project MCP tools are gated like any MCP tool.

### User hooks

Built to `tasks/design/permissions.md` §User hooks, into the step-1a slot Phase 3 left: command handlers for
`pre_tool_use`, `post_tool_use` and `permission_denied` (`fontaine-core/src/user_hooks.rs`). The
event goes to the handler as JSON on stdin (session id, agent id, tool, args, mode, roots); the
answer comes back as JSON on stdout or exit code 2 (= deny, stderr as the reason). `pre_tool_use`
returns `decision` (deny / ask / allow), `updated_input`, `guardian_context`; `post_tool_use` may
append a note to the tool result the agent sees (a `ResultPipeline` stage that runs after the Phase 6 scanner stage); `permission_denied` may return `retry` (the agent is
told it may retry, as with `/denied` → `r`). Handlers run (through Git Bash on Windows, found by the same discovery as the `shell` tool, including `FONTAINE_GIT_BASH_PATH`) with the scrubbed child environment and a
`timeout_secs` cap, in a `fontaine.hook` span. A user-scope `allow` in `auto` follows Claude Code: a
narrow matcher skips the guardian, a broad one is suspended. Every subagent tool call runs the same hooks.

### Extension files (skills, agent files, output styles)

Layout and scope per `tasks/design/config.md` §Config: `skills/<name>/SKILL.md`, `agents/<name>.md`,
`output-styles/<name>.md` under `<config dir>/` and `<root>/.fontaine/`, with Claude
Code-compatible frontmatter (`name`, `description`, `tools`, `model`, `permissionMode` ↦ `mode`
with `default` ↦ `manual`, `allowed-tools`, `disable-model-invocation`), so `.claude/` skills and
agents port by copying; `.claude/` is not read directly.

- **Skills** are progressive-disclosure prompt content: names and descriptions go into the system
  prompt; the body loads through the first-party `skill` tool (`fontaine-tools/src/skill.rs`,
  `RiskClass::ReadOnly`, on the fast path: its only argument is a skill name resolved against the index loaded at
  start, so it can read nothing else and only loads skill text into context). A user-invocable skill is also a `/name` slash command
  (custom slash commands are skills). `allowed-tools` from a user-trust-scope skill is a set of
  session-scope allow rules while the skill is active (same suspension in `auto`); from project scope
  it is ignored. `!cmd` dynamic context runs through the permission pipeline as a `shell` call.
- **Agent files** load into the step-3 agent-type registry beside `[agents.types]` (project overrides
  user on a name clash); project files only narrow — toolset a subset of the parent's, `mode`
  `manual` (default) or `dontAsk`, no `model` key (session model) — and a project file breaking that
  is rejected at load (exit 2). User-scope agent files follow the `[agents.types]` mode rule: `permissionMode` is accepted only as `inherit`, `default` (↦ `manual`) or `dontAsk`; any other value (`acceptEdits`, `auto`, `plan`, `bypassPermissions`) is a load error (exit 2) naming the file.
- **Output styles** are system-prompt overlays selected per session with `/output-style`. Output styles never alter the guardian's prompt or `AdjudicationRequest`.

### TUI extension — background-agents panel

One row per running subagent — type, model, status (running / waiting-on-permission / done /
killed), token/cost spend vs budget — with keys to inspect a row's transcript, kill/pause it, and
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
   (one rmcp in the tree) with the transport features enabled. Server annotations surfaced to the
   guardian as hints.
2. Session persistence via rig's serializable `AgentRun` + `rig-memory` save/resume, with the
   Phase-2 ledger as the second store (`tasks/design/sessions.md` §Session ledger): `--continue` / `--resume <id>`
   restore context and ledger, start in the resolved default mode and drop session grants;
   `--fork-session` copies both stores under a new session id without session grants;
   `fontaine.session.count{start_type=resume}`.
3. `fontaine-agents`: defines the `ChildRunner` trait (`rust/fontaine-agents/src/runner.rs`: spawn a gated child
   run from agent type, effective mode, budget and a `LedgerView` handle; returns a handle for status, kill
   and report) which `fontaine-core` implements (`rust/fontaine-core/src/child_runner.rs`) and injects at startup, like
   `ToolGate`; the core impl re-evaluates the child's mode as the tighter of its own and the parent's on every call. `agent` spawn tool over rig-agent — concurrent background subagents with
   own context/model/per-type restricted registries; `LedgerView` is a small trait defined in `fontaine-agents` (`rust/fontaine-agents/src/ledger_view.rs`: read root human messages, append child events) and implemented by `fontaine-core`'s ledger, the same inversion as `ToolGate`/`ChildRunner`; list/stream-status/interrupt as TUI-only controls; spawn call
   gated as a pipeline request; subagent tool calls re-enter the permission pipeline with
   one-way mode inheritance (child mode ≤ parent mode); agent-to-agent messages (`send_message`) through the same
   guardian gate; results returned as ordinary tool results through `ResultPipeline` (Phase 2; the Phase 6 scanner
   stage attaches there); `[agents]` budgets
   enforced; TUI background-agents panel (status, kill/pause, per-agent decisions). Telemetry:
   `fontaine.subagent` spans (`agent.id` / `agent.type`) parented to the spawning tool call,
   `subagent` lifecycle events (incl. `budget_exhausted`), `mcp_server_connection` and
   `fontaine.mcp.call` (server and tool names only under `log_tool_details`, otherwise
   `"third-party"`).
4. User hooks (design above); `post_tool_use` attaches to `ResultPipeline` as its hook stage.

   New registered model tools this phase adds (each wrapped; the exact set asserted by `rust/fontaine-core/tests/choke_point.rs` is
   extended to the Phase-2 table plus exactly these, plus dynamic `mcp__<server>__<tool>` names):

   | Tool | RiskClass | Args |
   |---|---|---|
   | `agent` | Arbitrary | `agent_type`, `prompt`, optional `model`, optional `background` (bool, default false: blocks and returns the report; true: returns `{agent_id}` at once, report delivered later as a notification); withheld from subagents at the depth cap |
   | `skill` | ReadOnly | `name` (resolved against the start-time index) |
   | `send_message` | Arbitrary | `to` (agent id of a parent, child or sibling in the same root tree, or `"main"` for the root session), `message`; returns `{status: "queued"}`, or error `agent not running` for a finished or killed target |
5. Extension files (design above): loader in `fontaine-core/src/extensions.rs`; `skill` tool and
   skill slash commands; agent files into the agent-type registry; output styles.
6. Docs — `docs/mcp.md`: declaring stdio / streamable-HTTP servers in config, project servers
   behind workspace trust and per-server approval, and the MCP trust model (server annotations are hints surfaced to the guardian, never gates; all MCP tools
   register `Arbitrary`); `docs/agents.md`: the `agent` tool, `[agents]` budgets, `[agents.types]`
   (model / restricted toolset / mode with the ≤-parent ladder), agent files and project-agent
   narrowing, nesting depth cap, spawn gating, per-agent audit in the background-agents panel;
   `docs/hooks.md`: events, handler I/O, place in the pipeline, composition, failure semantics,
   scopes and trust; `docs/extensions.md`: skills, slash commands, output styles, layout,
   frontmatter, porting from `.claude/`; extend `docs/tui.md` with the panel, the trust prompt and
   `/output-style`; add resume/fork to `docs/architecture.md` and `--continue` / `--resume` /
   `--fork-session` to `docs/headless.md`; add subagent/MCP telemetry to `docs/telemetry.md`; extend
   `docs/configuration.md` with `[mcp.servers.*]`, `[hooks.*]` and every
   `[agents]`/`[agents.types]` key; append a CHANGELOG entry.

## Roadmap slot (not urgent)

Amendments A3–A4 (analysis-strategic-verdict; folded in per user decision 2026-10-21): a
**sandbox-interop spike** for running Fontaine under Anthropic's open-source
`@anthropic-ai/sandbox-runtime` (srt) / bubblewrap — complements, does not replace, the guardian
(sandbox = blast radius, guardian = intent). Phase 4 unlocks `[guardian] profile = "minimal"` through
`--in-container` plus the Linux runtime probe; this slot adds srt/bubblewrap launch detection to that
probe (`rust/fontaine-core/src/containment.rs`) as a second verification route. When taken, its deliverables:
the spike, the **escape-detection test suite** (mount/umount, docker.sock, cgroup-control writes,
bind-mount symlink escape, proxy bypass → denied even under minimal — the srt/bubblewrap leg of `tasks/design/testing.md` Manual Verification item 6),
and **`docs/sandboxing.md`** documenting the integration path, the container containment probe
(`--in-container` verification), and the escape classes minimal must still deny. And keep
`fontaine-guardian` **dependency-light and lift-out-able** as an embeddable crate, so other
harnesses can embed the adjudicator via adapters (see the rig contingency in Decisions).

## Files created or modified

`rust/Cargo.toml` (direct `rmcp` pin); `rust/fontaine-mcp/src/{lib,servers,register}.rs`; `rust/fontaine-agents/src/{lib,spawn,registry,budget,runner,ledger_view}.rs`;
`rust/fontaine-core/src/{session,user_hooks,extensions,child_runner}.rs`; `rust/fontaine-tools/src/skill.rs`;
`rust/fontaine-tui/src/{agents_panel,trust_prompt}.rs`;
`docs/{mcp,agents,hooks,extensions,tui,configuration,architecture,headless,telemetry,index}.md`.

## Tests owned by this phase

- **parallel agents** (`tasks/design/testing.md` Testing Strategy; `rust/fontaine/tests/parallel_agents.rs`): spawn-call gating (task × toolset × model
  composite); mode-inheritance invariant (child ≤ parent, never looser), including a test where the
  parent switches mode mid-run and the child's next call follows the tighter mode; a `plan` parent blocks every child edit and drops an `auto`/`acceptEdits` child's non-edit calls to `manual`, including when the parent enters `plan` mid-run; per-type registry
  is a subset of the parent's; budget caps stop a subagent; a blocking `agent` call returns the child's report as its tool result through `ResultPipeline`; `background: true` returns `{agent_id}` at once and the report arrives later as a notification in the parent's next turn, passing through `ResultPipeline`; N background children run concurrently; `send_message` hits the
  same gate; delivery to a parent, a child, a sibling and `"main"` arrives in the recipient's next turn marked with the sender's agent id and appears to the guardian only as untrusted context; the sender gets `{status: "queued"}`; a finished or killed target returns error `agent not running`.
- **unpriced-model cost cap** (`rust/fontaine/tests/parallel_agents.rs`): a subagent on an `openai-compatible` model with `subagent_cost_budget_usd` set runs with the cost cap inactive and one notice, and the token cap still stops it; explicit `--max-budget-usd` against that model is a usage error (exit 2).
- **ChildRunner seam** (`rust/fontaine-agents/tests/`, fake `ChildRunner`): the spawn tool, registry and budget logic run with no `fontaine-core` dependency; the fake receives type, effective mode, budget and a fake `LedgerView`; status/kill/report flow through the returned handle.
- **system prompt** (`rust/fontaine-core/tests/`): the Phase 2 snapshot of the assembled prompt is extended with the skill index and an active output style in the layering order (base → `FONTAINE.md` → skill index → output style → append).
- **output styles** (`rust/fontaine-core/tests/`): the guardian's `AdjudicationRequest` and rendered prompt are byte-identical with and without an active output style.
- **agents panel** (`rust/fontaine-tui/tests/`, ratatui `TestBackend` with a scripted model): a killed subagent's row shows `killed` and its token/cost spend against budget.
- **subagent intent** (`rust/fontaine/tests/parallel_agents.rs`): an injected spawn prompt claiming user authorization does not appear as `task_intent` or in `recent_user_messages` in any `AdjudicationRequest`; it appears only inside an untrusted-delimited block.
- **choke point** (`rust/fontaine-core/tests/choke_point.rs`, over core's registry-assembly function — the one place built-ins, `agent`/`send_message`/`skill` and MCP tools are all assembled; the Phase-2 builtins-only check stays in `fontaine-tools`): the exact registered set is the Phase-2 table plus `agent` (Arbitrary), `skill` (ReadOnly) and `send_message` (Arbitrary) — no other names, no duplicates, every one with a `RiskClass`; no list/status/kill tool is registered for the model.
- Parallel orchestration (deterministic `test_utils` models or cassettes; scripted endpoints via the Phase-1 `common/mod.rs` helper): spawn N subagents, kill
  one, assert the others complete and every tool call has a gate decision in the audit log keyed by
  agent id.
- MCP (`rust/fontaine-mcp/tests/mcp_registration.rs`): servers from config register wrapped `Arbitrary`; a declared `readOnlyHint` never earns a
  fast path (`rust/fontaine-core/tests/choke_point.rs` extended to MCP-imported tools and to a list-changed refresh: tools added or
  replaced by a refresh are registered wrapped as `Arbitrary` under `mcp__<server>__<tool>`, none
  reachable raw).
- Persistence: save/resume round-trip through `AgentRun` + ledger; resume starts in the default mode
  without session grants; fork copies both stores under a new id; neither carries session grants.
- **workspace trust** (TUI `TestBackend`): an untrusted root's project and project-local MCP servers and hooks do not
  start before the trust prompt is accepted; declining starts none; a trusted root still asks per
  new project server; headless `-p` loads them, and in both surfaces a project hook's `allow` is
  ignored and a project MCP tool call is gated like any `Arbitrary` call.
- **user hooks** (`rust/fontaine-core/tests/user_hooks.rs`, script handlers in a temp dir): `updated_input` is what the floor, Tier 0.5, rules, guardian and audit see; deny > ask > allow; hook
  `allow` never passes the deny rule or critical path and is suspended in `auto` for a broad matcher; error / timeout / malformed output ⇒ no
  decision; project hook `allow` ignored; hook children get the scrubbed environment (no configured `api_key_env` variable, `scanner_hf_token_env`, telemetry credential or `MICROMEGAS_*`); exit code 2 = deny.
- **extensions**: frontmatter parsing of Claude Code-style skills and agents; project agent file
  with `model`, `mode = "inherit"`/`auto`, or a tool outside the parent's set is rejected; project
  agent overrides a same-name user agent and stays narrowed; project skill `allowed-tools` ignored;
  `!cmd` runs through the pipeline as `shell`; `skill` in the choke-point test with its risk class; a user-scope agent file with `permissionMode` `acceptEdits`, `auto`, `plan` or `bypassPermissions` is rejected at load (exit 2) naming the file, while `inherit` / `default` / `dontAsk` load.
- **telemetry**: N subagents produce per-agent `tool_decision` events keyed like the audit log;
  MCP names appear only under `log_tool_details`.

## Definition of done

- Offline gates (no keys, no human): `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` /
  `cargo test` green from `rust/`, including `rust/fontaine/tests/parallel_agents.rs` (parallel-agents and
  parallel-orchestration tests), `rust/fontaine-mcp/tests/mcp_registration.rs` (MCP tests),
  `rust/fontaine-core/tests/user_hooks.rs`, and the trust, persistence, extensions and telemetry tests.
- Needs a human: `tasks/design/testing.md` Manual Verification item 5 passes (live look-and-feel only; the killed-row state and budget display are covered by the agents-panel `TestBackend` test, gating and audit by the
  automated parallel-orchestration test).
- Needs live keys (not CI): the `claude -p` baseline check (`fontaine-evals` `baseline capture` / `diff`) that a hook `allow` in `auto` skips the
  classifier for a narrow matcher and is suspended for a broad one.
- Item 6's srt/bubblewrap leg ships **with the sandbox slot when taken** (minimal-profile escape denies + probe
  fail-closed under srt/bubblewrap); its Docker leg is owned by Phase 4; the build steps above do not depend on it.
- `rust/fontaine-core/tests/docs.rs` green with `docs/mcp.md`, `docs/agents.md`, `docs/hooks.md`, `docs/extensions.md` in place; every new
  config key documented at ship.
- Trust, hooks, extensions, persistence and telemetry tests above green.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
