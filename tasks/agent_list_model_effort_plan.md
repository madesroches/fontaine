# Agent List Model and Effort Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/464

## Overview
The agent list below the prompt box names each running subagent but says nothing about which
model or reasoning effort it runs at, so an agent definition override (`model:`, `effort:` in an
agent file or `[agents.types.<name>]`) is invisible until the run is over. This plan records the
effective model and effort per agent in the agent tree and renders both in the list's subagent
rows, after the type: `◯ survey-tech-debt (agent-3) worker acct/sonnet effort high running 1m02s · 1.5k tok`.

## Current State
- The compact list (`rust/fontaine-tui/src/agent_list.rs`, `ListRow` + `AgentList::lines`) renders
  `◯ name (id) type status age · spend`, giving way id → tail (with an unpriced-spend
  front-cut exception). `ListRow` has no model or effort fields.
- `AgentRow` (`rust/fontaine-agents/src/tree.rs:528`) already carries `model: String`:
  `reserve_named` seeds it with the spec text (`$main` and the like) and
  `AgentTree::set_model` (`tree.rs:771`) records the resolved reference at assemble
  (`rust/fontaine-core/src/child_runner.rs:574`). The full agents panel shows it
  (`agents_panel.rs` `PanelRow.model`, rendered after the type); the compact list does not.
- Effort is not recorded anywhere per agent. `CoreChildRunner::assemble` resolves the level a
  child runs at — `agent_type.effort.or(context.state.effort())` (`child_runner.rs:592`) — and
  passes it straight into `generation_options_warned`; nothing stores it.
  `fontaine_agents::effort::Effort` (`Off | Low | Medium | High | Max`, `Display`) is re-exported
  as `fontaine_core::effort::Effort`.
- The main conversation's model and effort already show in the status bar
  (`statusbar.rs:276-280`, `· m · effort high ·`, effort omitted when unset).
- The tree snapshot `AgentTree::rows()` (`tree.rs:1916`) maps `NodeInfo` → `AgentRow`;
  `NodeInfo` literals exist at `tree.rs:594` (root), `:928` (reserve), `:1209` (rebuild from
  `RebuildRecord`, which carries `model: Option<String>` but no effort — the ledger does not
  record it).
- `ListRow` struct literals outside `agent_list.rs`: `app.rs:2022,2039`, `view.rs:894`,
  `spinner.rs:417` (tests included).

## Design
- **Record the resolved effort in the tree, like the model.** `NodeInfo` gains
  `effort: Option<Effort>` (`None` = unset, sends no reasoning parameter — same convention as the
  session level). `AgentTree::set_effort(agent, effort)` mirrors `set_model`.
  `CoreChildRunner::assemble` calls it where the level is computed
  (`child_runner.rs:592-593`), recording the same value that goes into the generation options:
  the type's own level, else the session level at assemble. An unsupported level dropped by
  `generation_options_warned` is still the configured level (the status bar shows the session's
  configured level the same way). Display state: not part of `Spend`, never budgeted, never
  persisted. A re-assembled run (a revived or resumed agent's next run) records the level
  resolved then, so the row always shows the latest run's level — the same freshness the model
  column already has. Root and rebuild literals start at `None`; the root's level lives in
  session state (status bar), and a rebuilt agent's first wake re-assembles and records it.
- **Expose it.** `AgentRow` gains `effort: Option<Effort>`, populated in the `rows()` snapshot.
  `RebuildRecord` is unchanged (no ledger/schema churn).
- **Render in the compact list, subagent rows only.** `ListRow` gains `model: String` and
  `effort: Option<Effort>`, mapped in `agent_list.rs::rows()`. `AgentList::lines` inserts both
  after the agent type: the model bare (as in the agents panel), the effort as
  `effort <level>` (as in the status bar; a bare level would read as a status word). Both
  segments are omitted when empty/`None`. An unresolved placeholder reference (e.g. `$main`)
  is not special-cased — the row shows whatever
  the tree holds, exactly as the panel does. The `main` row is unchanged: its model and effort
  sit in the status bar directly above the list, and the list only draws while a subagent row
  exists anyway.
- **Give-way order.** The row must fit `width` as today: everything → drop the `(id)` tag
  (unchanged) → drop the `model`/`effort` segments together → the existing tail cut (front-cut
  when the spend is unpriced, plain tail cut otherwise; the `· writing N chars` nicety rule is
  last, unchanged). Model and effort survive ahead of status/age on a moderately narrow
  terminal; the id still gives way before them.
- **Agents panel: out of scope.** It already shows the model; adding effort there can be a
  follow-up issue. The issue targets the list below the prompt box.

## Implementation Steps
1. `rust/fontaine-agents/src/tree.rs`: `effort` on `NodeInfo` (three literals: root `None`,
   reserve `None`, rebuild `None`), `AgentTree::set_effort`, `AgentRow.effort`, snapshot in
   `rows()`.
2. `rust/fontaine-core/src/child_runner.rs`: `assemble` records the resolved level next to
   `set_model`.
3. `rust/fontaine-tui/src/agent_list.rs`: `ListRow.model`/`ListRow.effort`, mapping in `rows()`,
   rendering and give-way order in `lines()`; update the `AgentRow` literal in the tests' helper.
4. Update every `ListRow` literal: `app.rs:2022,2039`, `view.rs:894`, `spinner.rs:417`.
5. Docs: `docs/tui.md` "The agent list and the agent view" (row shape and the new give-way
   order), `CHANGELOG.md`.

## Files to Modify
- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/child_runner.rs`
- `rust/fontaine-tui/src/agent_list.rs`, `rust/fontaine-tui/src/app.rs`,
  `rust/fontaine-tui/src/view.rs`, `rust/fontaine-tui/src/spinner.rs`
- `docs/tui.md`, `CHANGELOG.md`

## Trade-offs
- Tree field vs. resolving effort at render time from the registry + session state: render-time
  resolution cannot know the session level at the child's spawn (session effort moves with
  `/effort`), and a running child's baked options never re-resolve — recording at assemble is
  the only place the effective level is known. It also matches how the model is recorded.
- Recording the configured level rather than the post-validation wire level: matches the status
  bar's convention for main and keeps the tree free of model-catalog concerns.

## Decisions
- The display is subagent-rows-only; `main`'s model and effort already sit in the status bar
  directly above the list.

## Documentation
`docs/tui.md` "The agent list and the agent view": the example row gains the model and
`effort <level>` after the type, a clause that both are omitted when unset, and the give-way
order becomes id → model/effort → tail. `CHANGELOG.md`.

## Testing Strategy
All automated, no live APIs (unit and integration tiers; nothing here needs a real service):
- Unit (tree): `set_effort` records and `rows()` exposes it; unknown agent id is a no-op; root
  and rebuilt nodes read `None`.
- Integration (core, `agent_resume.rs`-style world with `MockStreamEvent` turns): a child of a
  type with `effort: Some(..)` records that level; a type without one records the session level;
  with neither set the row reads `None` — asserted through `agents().rows()` after the spawn
  record lands, the same seam `agent_resume.rs:398` uses for the model.
- Unit (tui): a wide row shows `type model effort status age · spend`; `effort` omitted when
  `None`, model segment omitted when empty; give-way order at narrow widths — id drops before
  model/effort, model/effort drop before status/age, and the unpriced front-cut still protects
  the spend marker; `main`'s row text is unchanged.

## Open Questions
None.
