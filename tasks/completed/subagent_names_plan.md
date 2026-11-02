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
- Claude Code (observed with `claude -p`): its Agent tool has no `name` parameter (it takes
  description, prompt, subagent_type, model, effort, run_in_background, isolation). SendMessage `to`
  accepts a name, `main` or an agentId; names stay addressable after the agent completes; duplicate
  names are allowed and the latest wins, with a ` [ref]` suffix to tell them apart; unnamed agents
  are addressed only by id.
- The agents panel draws `{marker} {indent}{id} {agent_type} {status} {age}`
  (`fontaine-tui/src/agent_list.rs`, from `ListRow`, built in `rows`). The view header and status
  bar read the same `ListRow` through `App::viewed_row()`; `PanelRow` (`agents_panel.rs`) feeds
  only the panel.
- `fontaine-core/src/ledger.rs` parses the numeric part of `agent-N`; audit, telemetry and todos
  key on the id (`headless.rs`, `todos.rs`, `audit.rs`).
- Issue #114 (open) covers a `description` parameter and readable inline rows; #40 covers named
  long-lived teams. This plan covers naming only.

## Design
**The id stays the identity; the name is a unique alias.** The name is a label plus an
address that resolves to an id.

- `NodeInfo` gains `name: String`; `AgentRow` gains `name: String`; the TUI `ListRow` (including
  `ListRow::main()`) and `PanelRow` gain `name: String`. The status bar falls back to the id when no
  row is viewed.
- `agent` tool gains an optional `name` argument: 1–32 chars, `[a-z0-9-]`, starting with a
  letter, e.g. `survey-tech-debt`. The description tells the model to give a short role-describing
  name when it starts several agents.
- Fallback when omitted: `<slug>-<k>`, where `<slug>` is the agent type slugged (lowercased, every
  char outside `[a-z0-9]` becomes `-`, prefixed with `a-` when it does not start with a letter,
  truncated so `-<k>` fits in 32 chars) and `k` is the first of 1, 2, … for which the candidate
  passes the same reserved-name checks as a model-supplied name and is not in the taken set
  (`explore-1`, `explore-2`, `worker-1`; a plugin type `Plug:Review_Bot` gives `plug-review-bot-1`;
  a type `agent` or `job` never yields `agent-1` / `job-1` because those fail the reserved checks,
  so the probe moves on). Deterministic, so it needs no model call.
- Uniqueness is per session, across the whole tree and across ended agents (a name is never
  reused, so a late message cannot reach the wrong agent). A model-supplied name that is taken,
  is `main` or `root` (the root's `ROOT_AGENT` id), starts with `job-` (job ids share
  `job_output`'s id space), or matches `agent-<digits>` is refused as invalid args with the reason, so the model
  fixes it; it is never silently renamed. Validation and the fallback probe live in
  `AgentTree::reserve`, under the tree lock, so racing spawns cannot collide. The taken-names set
  lives on `Inner`, separate from `nodes`. `release` of a reservation that never started (spawn
  failure, `ReleaseOnDrop`; both run before `AgentEvent::Spawned`) frees that reservation's name,
  since nothing could have messaged an agent that never ran; this matches the ledger-seeded rule,
  where a never-started agent's name is absent from the ledger and so free after restart. Names of
  agents that did start are never freed. Fallback probing makes consumed numbers irrelevant.
- `reserve` returns the id as today; callers read the name from the node. The `agent` tool result
  becomes `{"agent_id": "agent-3", "name": "survey-tech-debt"}`.
- **Addressing:** `AgentTree::resolve_target` (now taking `&Inner` and returning an owned id,
  resolved by `deliver` under its lock) resolves `main`, then an exact id, then a name, to an
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
  keys stay ids. The row is built and truncated as spans (not one flat string); at narrow width the
  dimmed id gives way before status and age. `Notification` gains a separate `from_name` field for the sender's name, filled in
  `deliver` / `push` from the sender node under the tree lock (the root node is named `main`, so
  main-to-child messages read `main`); `from` stays the id, since `push` dedups Job notes by `from`
  and `agent_state` matches reports by `note.from == id`. `render` leads with `from_name` and
  carries the id:
  `Report from agent survey-tech-debt (agent-3)`. Other surfaces that showed a bare id show the
  name too: the approval prompt `Allow {tool} ({agent})?` and its outcome notice (the name rides on
  `ApprovalRequest`), kill/resume notices (tree lookup by id), `MessageRefusal::notice` and the
  `watch_report` body (`Agent {id} ({type_name}) finished`; tree lookup by id). The `gate_decision` stream-json event gains
  `agent_name` beside `agent_id`.
- **Persistence:** `Record::Agent` gains `name: Option<String>` with `#[serde(default)]`, set by
  the `AgentEvent::Spawned` mapping in `child_runner.rs` and folded into `AgentRecord.name`;
  records from before this change load with the name falling back to the id. In-process resume
  keeps the node's name. A new process seeds the taken names from `LedgerView.agents` through a
  separate seeding call from `setup.rs` (`AgentTree::new` and its callers stay unchanged), so names
  stay never reused. The export index shows the name. `DecisionRecord` gains `agent_name`, filled in
  `permissions.rs` like `type_of` (absent for root and `user` records). Audit and headless JSON
  add `agent_name` (snake_case) next to `agent_id`; telemetry (tracing) fields use `agent.name`
  beside `agent.id`/`agent.type` (`spawn.rs`). Ids remain the join key.
- **Untrusted text:** names are model-chosen and flow into notifications read by other agents and
  into the TUI. The charset restriction makes them safe to embed without escaping, and the render
  text keeps the existing "untrusted data" framing.

## Implementation Steps
1. `fontaine-agents/src/tree.rs`: add `name` to `NodeInfo`/`AgentRow`; name validation, the
   fallback probe and the taken-names set (on `Inner`) inside `reserve`; a `Refusal::Name(String)`
   variant; `resolve_target` taking `&Inner` and returning an owned id (the `deliver` callers in
   the tests update for the new return type); `agent_state` resolving names; `name` on
   `Notification`, filled in `deliver`/`push`, with `render` using it; a seeding call for taken
   names (`AgentTree::new` unchanged).
2. `fontaine-agents/src/spawn.rs`: `name` in `AgentArgs` and the schema, the tool description,
   map `Refusal::Name` to `invalid_args`, return `name` in the result, resolve names in
   `SendMessageTool` and update its `to` description; `deliver` returns the resolved id for
   `AgentEvent::Message`; `agent.name` telemetry field; the `watch_report` body shows the name (tree
   lookup by id).
3. `fontaine-core/src/ledger.rs`, `root_ledger.rs`, `child_runner.rs`, `export.rs`,
   `agent/setup.rs`, `permissions.rs`: `name` on `Record::Agent` (serde default) and
   `AgentRecord`, set from `AgentEvent::Spawned`; seed taken names from `LedgerView.agents`;
   export index shows the name; `DecisionRecord.agent_name`; `MessageRefusal::notice`
   (`agent/messaging.rs`) shows the name.
4. `fontaine-tui/src/{agent_list,agents_panel,view,statusbar}.rs`, `ui/mod.rs`: show
   `name (id)` with the dimmed id as spans, `name` on `ListRow` (and its test helpers) and
   `PanelRow`; the approval prompt (`approval.rs`, name on `ApprovalRequest`), approval outcome
   notice (`app.rs`) and kill/resume notices (`ui/mod.rs`, tree lookup by id); keep selection and
   dismissal keyed on id.
5. `fontaine/src/headless.rs`, audit emitters: add `agent_name`.
6. `fontaine-tools/src/jobs/tools.rs`, `jobs.rs`: route by the `agent-`/`job-` rule and resolve
   names via `AgentWaits`; apply the same rule to the `JobError::UnknownJob` "is a subagent, not a
   job" hint (now only for `agent-` ids). Update the `job_output` description and `id` parameter
   text, and in `spawn.rs` the `agent` tool description (returns `agent_id` and `name`; call
   `job_output` with either) and the `send_message` description ("marked with your agent id"), to
   say an id or a name is accepted.
7. Append dated lines to `### Agents` in `tasks/fontaine_harness_plan.md` recording the deviations
   from Claude Code: (a) a taken name is refused and names are never reused (Claude Code lets the
   latest win; reason: a late message cannot reach the wrong agent); (b) the model picks the name
   through a new `name` argument (Claude Code's Agent tool has none; reason: addressable roles
   without a naming step in the harness); (c) unnamed agents get generated `<type>-<k>` names
   (Claude Code addresses them only by id; reason: every agent is readable and addressable);
   (d) charset, length and reserved-word rules (`[a-z0-9-]`, 1-32 chars, not `main`/`root`/`job-*`/
   `agent-<n>`; reason: safe to embed untrusted, no collision with ids or job ids).

## Documentation
`docs/agents.md`: the `name` argument, fallback names, `send_message` / `job_output` addressing,
the panel display. `docs/telemetry.md`: the `agent.name` field. `docs/tui.md`: the `name (id)`
display. `docs/headless.md` and `docs/schemas/headless-v1.schema.json`: `agent_name` on
`gate_decision`. `docs/permissions.md`: `agent_name` in decision records. `CHANGELOG.md` entry.

## Testing Strategy
Unit tests (`tree.rs`): name validation (charset, length, `main`, `root`, `job-*`, `agent-<n>` rejected); fallback
numbering per type; fallback for a plugin-namespaced/uppercase type (`Plug:Review_Bot`) is slugged and
fits 32 chars, and types `agent` and `job` skip the reserved `agent-1` / `job-1`; a released
(never started) reservation's name is free again while a started agent's name stays taken; duplicate refused; `resolve_target`
by id and name; a name does not widen `deliver` reachability (non-sibling by name is
`NotReachable`); an ended agent's name stays taken. `spawn.rs`: schema and result contain `name`;
`Refusal::Name` becomes `invalid_args`. Ledger: a legacy spawn event without `name` loads with the
id as its name; a new process seeds taken names from `LedgerView.agents`. TUI:
`agent_list.rs` unit test `a_named_row_shows_the_name_and_a_dimmed_id` asserts the text
`survey-tech-debt (agent-1)` and `Modifier::DIM` on the id span only, plus a narrow-width
truncation case. `job_output`: `job-` ids still reach `read_job`, names and `agent-` ids reach
agent waits. Notification render includes name and id.
`rust/fontaine-agents/tests/seam.rs` spawns two named agents, sends by name and asserts the
rendered notification.

## Decisions
- AFK: Fallback name is `<slug>-<k>` with the type slugged (lowercase, non-`[a-z0-9]` to `-`, `a-` prefix if not letter-first, truncated so `-k` fits in 32), run through the same reserved-name checks, probing k=1,2,… against the taken set; unit tests cover a plugin-namespaced/uppercase type and types `agent`/`job`.
- AFK: `Notification` gets a separate `from_name` field filled in `deliver`/`push` from the sender node under the lock; `from` stays the id (push dedups Job notes by `from`, `agent_state` matches `note.from == id`).
- AFK: `release` frees the released reservation's name (it runs before `AgentEvent::Spawned`, so the agent never ran), consistent with ledger seeding; names of agents that started are never reused; a test asserts a released name is free again.
