# Phase 5 — Extensibility: MCP, sessions, parallel agents, hooks, extension files

**Status:** not started · **Depends on:** [Phase 4](phase4_guardian.md) · **Master:** [fontaine_harness_plan.md](../fontaine_harness_plan.md)

Parallel multi-agent management is a **v1 requirement** (user directive 2026-10-21) — this is
build work, not backlog.

## Design (excerpt owned by this phase)

### Parallel agent orchestration (fontaine-agents)

Managing many agents concurrently in one session is a **v1 requirement** (user directive 2026-10-21, superseding
"no subagents in v1"), patterned on Claude Code's documented subagents/background tasks per the design tie-breaker:
each subagent runs in its own context window, background subagents run concurrently, their permission prompts surface
in the main session, and their results return to the parent as a summary.

- **The `agent` tool** (Claude Code's `Agent`/`Task` tool): a first-party tool that spawns a subagent. Each subagent is
  a separate **rig-agent** conversation (`AgentBuilder` + `AgentRun`, its own `rig-memory` context), with its own
  selectable model and a **per-agent-type restricted tool registry** drawn from Fontaine's risk registry — a subagent's
  registry is a subset of the parent's, never a superset. N subagents run concurrently in the background; the parent
  can list them, stream their status, interrupt/kill any one, and receives summarized results back.
- **Results re-enter through the gate.** A subagent's report comes back to the parent as an ordinary *tool result*, so
  when scanning is enabled it passes the tool-result scanner (PromptGuard 2 default, off by default) like any other
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
  `plan`/`acceptEdits`/`auto` are reachable for a child only via `"inherit"` from a parent already
  there — so a `manual` parent can never produce a silently-adjudicating `plan` child. The child's mode is
  not fixed at spawn: it is re-evaluated on every call as the tighter of its own setting and the
  parent's current mode.
  (iii) **Parent↔child and peer messages** (Claude Code's `SendMessage`-style) go through the same guardian gate — the
  2026-10-20 decision, which stands and now applies in v1.
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
  agents across the **whole session tree**, not just the parent's direct children.

### Workspace trust and project executables

Project MCP servers (`[mcp.servers.*]` in `.fontaine/settings.toml`), project hooks, and project
skills' `!cmd` dynamic context are project-defined executables (master §Config, Workspace trust). In
the TUI, before any of them loads, an untrusted primary root gets the trust prompt (trust / don't
trust; the answer is written to the trust store from Phase 0); each project MCP server is then
approved individually at first load, and the repository cannot pre-approve its own servers. Declining
loads the session without them. Headless `-p` loads them without a prompt, as Claude Code does.

### User hooks

Built to the master plan's §User hooks, into the step-1a slot Phase 3 left: command handlers for
`pre_tool_use`, `post_tool_use` and `permission_denied` (`fontaine-core/src/user_hooks.rs`). The
event goes to the handler as JSON on stdin (session id, agent id, tool, args, mode, roots); the
answer comes back as JSON on stdout or exit code 2 (= deny, stderr as the reason). `pre_tool_use`
returns `decision` (deny / ask / allow), `updated_input`, `guardian_context`; `post_tool_use` may
append a note to the tool result the agent sees; `permission_denied` may return `retry` (the agent is
told it may retry, as with `/denied` → `r`). Handlers run with the scrubbed child environment and a
`timeout_secs` cap, in a `fontaine.hook` span. Every subagent tool call runs the same hooks.

### Extension files (skills, agent files, output styles)

Layout and scope per the master plan's §Config: `skills/<name>/SKILL.md`, `agents/<name>.md`,
`output-styles/<name>.md` under `~/.config/fontaine/` and `<root>/.fontaine/`, with Claude
Code-compatible frontmatter (`name`, `description`, `tools`, `model`, `permissionMode` ↦ `mode`
with `default` ↦ `manual`, `allowed-tools`, `disable-model-invocation`), so `.claude/` skills and
agents port by copying; `.claude/` is not read directly.

- **Skills** are progressive-disclosure prompt content: names and descriptions go into the system
  prompt; the body loads through the first-party `skill` tool (`fontaine-tools/src/skill.rs`,
  `RiskClass::ReadOnly`; its only argument is a skill name resolved against the index loaded at
  start, so it can read nothing else). A user-invocable skill is also a `/name` slash command
  (custom slash commands are skills). `allowed-tools` from a user-trust-scope skill is a set of
  session-scope allow rules while the skill is active (same suspension in `auto`); from project scope
  it is ignored. `!cmd` dynamic context runs through the permission pipeline as a `shell` call.
- **Agent files** load into the step-3 agent-type registry beside `[agents.types]` (project overrides
  user on a name clash); project files only narrow — toolset a subset of the parent's, `mode`
  `manual` (default) or `dontAsk`, no `model` key (session model) — and a project file breaking that
  is rejected at load (exit 2).
- **Output styles** are system-prompt overlays selected per session with `/output-style`.

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
   `rmcp` dependency, pinned to an exact version inside rig-rmcp 0.45.0's declared 3.5 range
   (one rmcp in the tree) with the transport features enabled. Server annotations surfaced to the
   guardian as hints.
2. Session persistence via rig's serializable `AgentRun` + `rig-memory` save/resume, with the
   Phase-2 ledger as the second store (master §Session ledger): `--continue` / `--resume <id>`
   restore context and ledger, start in the resolved default mode and drop session grants;
   `--fork-session` copies both stores under a new session id without session grants;
   `fontaine.session.count{start_type=resume}`. (Headless `-p` moved to Phase 1 step 2 — user
   decision 2026-10-21.)
3. `fontaine-agents`: `agent` spawn tool over rig-agent — concurrent background subagents with
   own context/model/per-type restricted registries; list/stream-status/interrupt; spawn call
   gated as a pipeline request; subagent tool calls re-enter the permission pipeline with
   one-way mode inheritance (child mode ≤ parent mode); agent-to-agent messages through the same
   guardian gate; results returned as ordinary tool results through the shared result-handling hook (where the
   Phase 6 scanner attaches); `[agents]` budgets
   enforced; TUI background-agents panel (status, kill/pause, per-agent decisions). Telemetry:
   `fontaine.subagent` spans (`agent.id` / `agent.type`) parented to the spawning tool call,
   `subagent` lifecycle events (incl. `budget_exhausted`), `mcp_server_connection` and
   `fontaine.mcp.call` (server and tool names only under `log_tool_details`, otherwise
   `"third-party"`).
4. User hooks (design above).
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
(sandbox = blast radius, guardian = intent). Fontaine-verified srt/bubblewrap launch is what
unlocks `[guardian] profile = "minimal"` (see the Phase 4 plan). When taken, its deliverables:
the spike, the **escape-detection test suite** (mount/umount, docker.sock, cgroup-control writes,
bind-mount symlink escape, proxy bypass → denied even under minimal — Manual Verification item 6),
and **`docs/sandboxing.md`** documenting the integration path, the container containment probe
(`--in-container` verification), and the escape classes minimal must still deny. And keep
`fontaine-guardian` **dependency-light and lift-out-able** as an embeddable crate, so other
harnesses can embed the adjudicator via adapters (see the rig contingency in Decisions).

## Files created or modified

`rust/Cargo.toml` (direct `rmcp` pin); `rust/fontaine-mcp/src/{lib,servers,register}.rs`; `rust/fontaine-agents/src/{lib,spawn,registry,budget}.rs`;
`rust/fontaine-core/src/{session,user_hooks,extensions}.rs`; `rust/fontaine-tools/src/skill.rs`;
`rust/fontaine-tui/src/{agents_panel,trust_prompt}.rs`;
`docs/{mcp,agents,hooks,extensions,tui,configuration,architecture,headless,telemetry,index}.md`.

## Tests owned by this phase

- **parallel agents** (master Testing Strategy): spawn-call gating (task × toolset × model
  composite); mode-inheritance invariant (child ≤ parent, never looser), including a test where the
  parent switches mode mid-run and the child's next call follows the tighter mode; per-type registry
  is a subset of the parent's; budget caps stop a subagent; subagent reports return as ordinary
  tool results through the shared result-handling hook; `SendMessage`-style peer messages hit the
  same gate.
- Parallel orchestration (deterministic `test_utils` models or cassettes): spawn N subagents, kill
  one, assert the others complete and every tool call has a gate decision in the audit log keyed by
  agent id.
- MCP: servers from config register wrapped `Arbitrary`; a declared `readOnlyHint` never earns a
  fast path (choke-point test extended to MCP-imported tools and to a list-changed refresh: tools added or
  replaced by a refresh are registered wrapped as `Arbitrary` under `mcp__<server>__<tool>`, none
  reachable raw).
- Persistence: save/resume round-trip through `AgentRun` + ledger; resume starts in the default mode
  without session grants; fork copies both stores under a new id; a rewind keeps ledger history.
- **workspace trust** (TUI `TestBackend`): an untrusted root's project MCP servers and hooks do not
  start before the trust prompt is accepted; declining starts none; a trusted root still asks per
  new project server; headless `-p` loads them.
- **user hooks** (`rust/fontaine-core/tests/user_hooks.rs`, script handlers in a temp dir): the
  master Testing Strategy bullet — rewritten input judged everywhere; deny > ask > allow; hook
  `allow` never passes the floor and is suspended in `auto` for a broad matcher; failure ⇒ no
  decision; project hook `allow` ignored; scrubbed environment; exit code 2 = deny.
- **extensions**: frontmatter parsing of Claude Code-style skills and agents; project agent file
  with `model`, `mode = "inherit"`/`auto`, or a tool outside the parent's set is rejected; project
  agent overrides a same-name user agent and stays narrowed; project skill `allowed-tools` ignored;
  `!cmd` runs through the pipeline as `shell`; `skill` in the choke-point test with its risk class.
- **telemetry**: N subagents produce per-agent `tool_decision` events keyed like the audit log;
  MCP names appear only under `log_tool_details`.

## Definition of done

- Manual Verification item 5 passes (agents-panel visual check; gating and audit are covered by the
  automated parallel-orchestration test).
- Item 6 ships **with the sandbox slot when taken** (minimal-profile escape denies + probe
  fail-closed); the build steps above do not depend on it.
- `docs/mcp.md`, `docs/agents.md`, `docs/hooks.md`, `docs/extensions.md` complete; every new
  config key documented at ship.
- Trust, hooks, extensions, persistence and telemetry tests above green.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
