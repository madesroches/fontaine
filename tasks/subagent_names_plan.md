# Stable Human-Readable Subagent Names Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/290

## Overview
Subagents are identified only by synthetic ids (`agent-1`, `agent-2`). With several running in the
background, the panel, spawn result, report notifications and `send_message` all show an id that
says nothing about what the agent does. This adds a stable, human-readable `name` to every
subagent and threads it through every surface a person or a model reads, while `agent-N` stays the
canonical key for the ledger, audit log, telemetry and budgets.

## Current State
- `AgentTree::reserve` mints `agent-{n}` from an atomic counter and stores it in `NodeInfo.id`
  (`fontaine-agents/src/tree.rs`, `reserve`). `NodeInfo` and `AgentRow` carry no label.
- The `agent` tool takes `agent_type`, `prompt`, `model` and returns `{"agent_id": ...}`
  (`fontaine-agents/src/spawn.rs`, `AgentTool`).
- `send_message` addresses by id or `main`; `AgentTree::deliver` / `resolve_target` map `main` to
  the root. `Notification::render` embeds `from` in the "Message from agent …" / "Report from
  agent …" text the recipient reads.
- `job_output` routes ids by the `agent-` prefix: other ids go to `read_job`, and subagent callers
  are refused ids lacking the prefix (`fontaine-tools/src/jobs/tools.rs`). Job ids are `job-` plus
  4 hex. `JobOutputTool.agents: Option<Arc<dyn AgentWaits>>` is already the seam to the tree;
  `AgentTree` implements `AgentWaits` and `wait(caller, id, ..)` takes an opaque string, with
  unknown ids already mapping to `AgentWaitError::Unknown`.
- A fresh process builds `AgentTree::new` seeded only with `ledger.view().next_agent`
  (`fontaine-core/src/agent/setup.rs`); `LedgerView.agents` is not read back. In-process
  `resume_child` keeps the node and its name.
- Claude Code's naming and addressing semantics have not been observed yet (step 1).
- The agents panel draws `{marker} {indent}{id} {agent_type} {status} {age}`
  (`fontaine-tui/src/agent_list.rs`).
- `fontaine-core/src/ledger.rs` parses the numeric part of `agent-N`; audit, telemetry and todos
  key on the id (`headless.rs`, `todos.rs`, `audit.rs`).
- Issue #114 (open) covers a `description` parameter and readable inline rows; #40 covers named
  long-lived teams. This plan covers naming only.

## Design
**The id stays the identity; the name is a unique alias.** The name is a label plus an
address that resolves to an id.

- `NodeInfo` gains `name: String`; `AgentRow` gains `name: String`; the TUI `PanelRow` gains
  `name: String`.
- `agent` tool gains an optional `name` argument: 1–32 chars, `[a-z0-9-]`, starting with a
  letter, e.g. `survey-tech-debt`. The description tells the model to give a short role-describing
  name when it starts several agents.
- Fallback when omitted: `<agent_type>-<k>`, where `k` is the next unused number for that type in
  this session (`explore-1`, `explore-2`, `worker-1`). Deterministic, so it needs no model call.
- Uniqueness is per session, across the whole tree and across ended agents (a name is never
  reused, so a late message cannot reach the wrong agent). A model-supplied name that is taken,
  is `main` or `root` (the root's `ROOT_AGENT` id), starts with `job-` (job ids share
  `job_output`'s id space), or matches `agent-<digits>` is refused as invalid args with the reason, so the model
  fixes it; it is never silently renamed. Validation and the per-type counter live in
  `AgentTree::reserve`, under the tree lock, so racing spawns cannot collide. The taken-names set
  and per-type counters live on `Inner`, separate from `nodes`, and `release` does not free them.
- `reserve` returns the id as today; callers read the name from the node. The `agent` tool result
  becomes `{"agent_id": "agent-3", "name": "survey-tech-debt"}`.
- **Addressing:** `AgentTree::resolve_target` resolves `main`, then an exact id, then a name, to an
  id before the reachability check, so `deliver`'s rules (parent, children, siblings, `main`) are
  unchanged and names confer no extra reach, and the permission and guardian surface is unchanged.
  `deliver` returns the resolved id, and `AgentEvent::Message` (`spawn.rs`) records that id rather
  than the raw `to`. `job_output` routes ids starting with `agent-`, and anything else not
  starting with `job-`, to agent waits, both in its dispatch arm and in the subagent restriction in
  `call`; `AgentTree::agent_state` resolves a name through `resolve_target` first, and unknown
  names map to `AgentWaitError::Unknown`.
- **Rendering for people:** the panel (list, `agents_panel.rs`, view header and `Message {id}…`
  placeholder, status bar) shows `{name}` where it showed the id, with the id dimmed after it
  (`survey-tech-debt (agent-3)`) so `/agents`-style references and logs still match; internal
  keys stay ids. `Notification::render` leads with the name and carries the id:
  `Report from agent survey-tech-debt (agent-3)`. The `gate_decision` stream-json event gains
  `agent_name` beside `agent_id`.
- **Persistence:** `Record::Agent` gains `name: Option<String>` with `#[serde(default)]`, set by
  the `AgentEvent::Spawned` mapping in `child_runner.rs` and folded into `AgentRecord.name`;
  records from before this change load with the name falling back to the id. In-process resume
  keeps the node's name. A new process seeds the taken names and per-type fallback counters from
  `LedgerView.agents` through `AgentTree::new`, as `next_agent` is seeded, so names stay never
  reused. The export index shows the name. `DecisionRecord` gains `agent_name`, filled in
  `permissions.rs` like `type_of` (absent for root and `user` records). Audit and headless JSON
  add `agent_name` (snake_case) next to `agent_id`; telemetry (tracing) fields use `agent.name`
  beside `agent.id`/`agent.type` (`spawn.rs`). Ids remain the join key.
- **Untrusted text:** names are model-chosen and flow into notifications read by other agents and
  into the TUI. The charset restriction makes them safe to embed without escaping, and the render
  text keeps the existing "untrusted data" framing.

## Implementation Steps
1. Observe Claude Code's naming and addressing with `claude -p` and state the findings in Current
   State and Design.
2. `fontaine-agents/src/tree.rs`: add `name` to `NodeInfo`/`AgentRow`; name validation, the
   per-type counter and the taken-names set (on `Inner`) inside `reserve`; a `Refusal::Name(String)`
   variant; `resolve_target` resolving ids and names; `agent_state` resolving names;
   `Notification::render` with the name; `AgentTree::new` seeded with taken names and counters.
3. `fontaine-agents/src/spawn.rs`: `name` in `AgentArgs` and the schema, the tool description,
   map `Refusal::Name` to `invalid_args`, return `name` in the result, resolve names in
   `SendMessageTool` and update its `to` description; `deliver` returns the resolved id for
   `AgentEvent::Message`; `agent.name` telemetry field.
4. `fontaine-core/src/ledger.rs`, `root_ledger.rs`, `child_runner.rs`, `export.rs`,
   `agent/setup.rs`, `permissions.rs`: `name` on `Record::Agent` (serde default) and
   `AgentRecord`, set from `AgentEvent::Spawned`; seed `AgentTree::new` from `LedgerView.agents`;
   export index shows the name; `DecisionRecord.agent_name`.
5. `fontaine-tui/src/{agent_list,agents_panel,view,statusbar}.rs`, `ui/mod.rs`: show
   `name (id)` with the dimmed id, `name` on `PanelRow`; keep selection and dismissal keyed on id.
6. `fontaine/src/headless.rs`, audit emitters: add `agent_name`.
7. `fontaine-tools/src/jobs/tools.rs`: route by the `agent-`/`job-` rule and resolve names via
   `AgentWaits`.
8. Append a dated line to `### Agents` in `tasks/fontaine_harness_plan.md` `## Documentation
`docs/agents.md`: the `name` argument, fallback names, `send_message` / `job_output` addressing,
the panel display. `docs/telemetry.md`: the `agent.name` field. `docs/tui.md`: the `name (id)`
display. `docs/headless.md` and `docs/schemas/headless-v1.schema.json`: `agent_name` on
`gate_decision`. `docs/permissions.md`: `agent_name` in decision records. `CHANGELOG.md` entry.

## Testing Strategy
Unit tests (`tree.rs`): name validation (charset, length, `main`, `root`, `job-*`, `agent-<n>` rejected); fallback
numbering per type; duplicate refused; names unique under racing `reserve` calls; `resolve_target`
by id and name; a name does not widen `deliver` reachability (non-sibling by name is
`NotReachable`); an ended agent's name stays taken. `spawn.rs`: schema and result contain `name`;
`Refusal::Name` becomes `invalid_args`. Ledger: a legacy spawn event without `name` loads with the
id as its name; a new process seeds taken names and counters from `LedgerView.agents`. TUI:
`agent_list.rs` unit test `a_named_row_shows_the_name_and_a_dimmed_id` asserts the text
`survey-tech-debt (agent-1)` and `Modifier::DIM` on the id span only, plus a narrow-width
truncation case. `job_output`: `job-` ids still reach `read_job`, names and `agent-` ids reach
agent waits. Notification render includes name and id.
`rust/fontaine-agents/tests/seam.rs` spawns two named agents, sends by name and asserts the
rendered notification. Validation tests cover `root` and `job-` names.
