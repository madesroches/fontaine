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
- `job_output` accepts `agent-*` ids (`fontaine-tools/src/jobs/tools.rs`).
- The agents panel draws `{marker} {indent}{id} {agent_type} {status} {age}`
  (`fontaine-tui/src/agent_list.rs`).
- `fontaine-core/src/ledger.rs` parses the numeric part of `agent-N`; audit, telemetry and todos
  key on the id (`headless.rs`, `todos.rs`, `audit.rs`).
- Issue #114 (open) covers a `description` parameter and readable inline rows; #40 covers named
  long-lived teams. This plan covers naming only.

## Design
**The id stays the identity; the name is a unique alias.** Ids are parsed, persisted and
audited; changing them breaks resume and telemetry for no gain. The name is a label plus an
address that resolves to an id.

- `NodeInfo` gains `name: String`; `AgentRow` gains `name: String`.
- `agent` tool gains an optional `name` argument: 1–32 chars, `[a-z0-9-]`, starting with a
  letter, e.g. `survey-tech-debt`. The description tells the model to give a short role-describing
  name when it starts several agents.
- Fallback when omitted: `<agent_type>-<k>`, where `k` is the next unused number for that type in
  this session (`explore-1`, `explore-2`, `worker-1`). Deterministic, so it needs no model call.
- Uniqueness is per session, across the whole tree and across ended agents (a name is never
  reused, so a late message cannot reach the wrong agent). A model-supplied name that is taken,
  is `main`, or matches `agent-<digits>` is refused as invalid args with the reason, so the model
  fixes it; it is never silently renamed. Validation and the per-type counter live in
  `AgentTree::reserve`, under the tree lock, so racing spawns cannot collide.
- `reserve` returns the id as today; callers read the name from the node. The `agent` tool result
  becomes `{"agent_id": "agent-3", "name": "survey-tech-debt"}`.
- **Addressing:** `AgentTree::resolve_target` resolves `main`, then an exact id, then a name, to an
  id before the reachability check, so `deliver`'s rules (parent, children, siblings, `main`) are
  unchanged and names confer no extra reach. `job_output` resolves a name the same way through the
  same helper, if its `agent-*` branch can reach the tree (see Open Questions).
- **Rendering for people:** the panel shows `{name}` where it showed the id, with the id dimmed
  after it (`survey-tech-debt (agent-3)`) so `/agents`-style references and logs still match.
  Headless output and `Notification::render` lead with the name and carry the id:
  `Report from agent survey-tech-debt (agent-3)`.
- **Persistence:** the ledger's spawn event gains `name` with `#[serde(default)]`; records from
  before this change load with the name falling back to the id. Resume reuses the stored name and
  re-registers it as taken. Audit and telemetry events add a `agent_name` field next to `agent_id`;
  ids remain the join key.
- **Untrusted text:** names are model-chosen and flow into notifications read by other agents and
  into the TUI. The charset restriction makes them safe to embed without escaping, and the render
  text keeps the existing "untrusted data" framing.

## Implementation Steps
1. `fontaine-agents/src/tree.rs`: add `name` to `NodeInfo`/`AgentRow`; name validation, the
   per-type counter and the taken-names set inside `reserve`; a `Refusal::Name(String)` variant;
   `resolve_target` resolving ids and names; `Notification::render` with the name.
2. `fontaine-agents/src/spawn.rs`: `name` in `AgentArgs` and the schema, the tool description,
   map `Refusal::Name` to `invalid_args`, return `name` in the result, resolve names in
   `SendMessageTool` and update its `to` description.
3. `fontaine-core/src/ledger.rs`, `root_ledger.rs`: `name` on the spawn event with serde default;
   resume path restores names.
4. `fontaine-tui/src/agent_list.rs`: show the name with the dimmed id; keep selection and
   dismissal keyed on id.
5. `fontaine/src/headless.rs`, audit and telemetry emitters: add `agent_name`.
6. `fontaine-tools/src/jobs/tools.rs`: resolve names for `job_output` (if reachable).
7. Docs and changelog (below).

## Files to Modify
`rust/fontaine-agents/src/{tree,spawn,root_ledger}.rs`, `rust/fontaine-core/src/{ledger,audit}.rs`,
`rust/fontaine-tui/src/agent_list.rs`, `rust/fontaine/src/headless.rs`,
`rust/fontaine-tools/src/jobs/tools.rs`, `docs/agents.md`, `docs/telemetry.md`, `CHANGELOG.md`.

## Trade-offs
- **Replace the id with the name.** Rejected: ids are parsed (`ledger.rs`), persisted and joined on
  in audit and telemetry; a rename would be a migration with no user benefit.
- **Derive the name from a `description` argument (the issue's suggestion).** Rejected here:
  #114 owns `description` and its rendering, and a 3–5 word sentence is a poor address. A
  separate short slug is addressable; #114 can still add `description` beside it.
- **LLM-generated names.** Rejected: spend and latency per spawn, and the fallback is enough.
- **Silently de-duplicate a taken name.** Rejected: the model would not learn the name it
  must address, so a refusal with the reason is clearer.

## Decisions
- Names are never reused within a session, ended agents included, so a stale reference cannot
  reach a different agent.
- Names add no reach: reachability stays decided on ids by the existing parent/child/sibling/`main`
  rule, so the permission and guardian surface is unchanged.

## Documentation
`docs/agents.md`: the `name` argument, fallback names, `send_message` / `job_output` addressing,
the panel display. `docs/telemetry.md`: the `agent_name` field. `CHANGELOG.md` entry.

## Testing Strategy
Unit tests (`tree.rs`): name validation (charset, length, `main`, `agent-<n>` rejected); fallback
numbering per type; duplicate refused; names unique under racing `reserve` calls; `resolve_target`
by id and name; a name does not widen `deliver` reachability (non-sibling by name is
`NotReachable`); an ended agent's name stays taken. `spawn.rs`: schema and result contain `name`;
`Refusal::Name` becomes `invalid_args`. Ledger: a legacy spawn event without `name` loads with the
id as its name; a resumed agent keeps its name. TUI: `agent_list` snapshot with name and dimmed id.
Notification render includes name and id. An integration test in
`rust/fontaine/tests/parallel_agents.rs` spawns two agents and messages one by name.

## Manual Verification
1. Run `cargo run -p fontaine`, ask for two background `explore` agents with distinct names, and
   open the agents panel. Expect each row to show its name and dimmed id. Not automated: the look of
   the panel needs an eye (`TestBackend` does not show dimming faithfully).

## Open Questions
- Whether `job_output` (in `fontaine-tools`, which does not depend on `fontaine-agents`) can resolve
  names without a new trait seam. If not, ship name addressing for `send_message` only and leave
  `job_output` on ids.
