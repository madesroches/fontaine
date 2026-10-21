# Phase 5 — Extensibility: MCP, sessions, parallel agents

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

### TUI extension — background-agents panel

One row per running subagent — type, model, status (running / waiting-on-permission / done /
killed), token/cost spend vs budget — with keys to inspect a row's transcript, kill/pause it, and
see its pending approval prompts; per-agent guardian decisions (including Tier-0.5 audit entries)
are listed per row, so a background subagent never hides an approval behind the parent's
transcript.

## Steps

1. `fontaine-mcp` (rig-rmcp): stdio + streamable-HTTP servers from config. `McpClientHandler::new(config, sink)`
   registers raw `DynamicTool`s with whatever sink it is given — again on every list-changed
   refresh — so Fontaine passes a wrapping sink that registers every MCP tool, initial and
   refreshed, as a `GuardedTool` (DynamicTool path) with `Arbitrary` risk under the name
   `mcp__<server>__<tool>` (Claude Code naming); the agent's `ToolServerHandle` is never handed to
   rig-rmcp directly. rig-rmcp enables only rmcp's `client` feature, so the transports need a direct
   `rmcp` dependency, pinned to an exact version inside rig-rmcp 0.45.0's declared 3.5 range
   (one rmcp in the tree) with the transport features enabled. Server annotations surfaced to the
   guardian as hints.
2. Session persistence via rig's serializable `AgentRun` + `rig-memory` save/resume (JSONL on
   disk, our own transcript log alongside). (Headless `-p` moved to Phase 1 step 2 — user
   decision 2026-10-21.)
3. `fontaine-agents`: `agent` spawn tool over rig-agent — concurrent background subagents with
   own context/model/per-type restricted registries; list/stream-status/interrupt; spawn call
   gated as a pipeline request; subagent tool calls re-enter the permission pipeline with
   one-way mode inheritance (child mode ≤ parent mode); agent-to-agent messages through the same
   guardian gate; results returned as ordinary tool results through the shared result-handling hook (where the
   Phase 6 scanner attaches); `[agents]` budgets
   enforced; TUI background-agents panel (status, kill/pause, per-agent decisions).
4. Docs — `docs/mcp.md`: declaring stdio / streamable-HTTP servers in config, and the MCP trust
   model (server annotations are hints surfaced to the guardian, never gates; all MCP tools
   register `Arbitrary`); `docs/agents.md`: the `agent` tool, `[agents]` budgets, `[agents.types]`
   (model / restricted toolset / mode with the ≤-parent ladder), nesting depth cap, spawn gating,
   per-agent audit in the background-agents panel; extend `docs/tui.md` with the panel; add the
   session persistence/resume surface to `docs/architecture.md`; extend `docs/configuration.md`
   with the MCP server keys and every `[agents]`/`[agents.types]` key; append a CHANGELOG entry.

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
`rust/fontaine-core/src/session.rs` (persistence); `rust/fontaine-tui/src/agents_panel.rs`;
`docs/{mcp,agents,tui,configuration,architecture,index}.md`.

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
- Persistence: save/resume round-trip through `AgentRun`.

## Definition of done

- Manual Verification item 5 passes (agents-panel visual check; gating and audit are covered by the
  automated parallel-orchestration test).
- Item 6 ships **with the sandbox slot when taken** (minimal-profile escape denies + probe
  fail-closed); the build steps above do not depend on it.
- `docs/mcp.md` + `docs/agents.md` complete; every new config key documented at ship.
- CHANGELOG entry appended.
- Log the phase in the master plan's Progress log.
