# Coordination Tools Skip the Guardian Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/394

## Overview

`send_message` and `board_update` are agent-coordination tools. They are classified `Arbitrary` /
`Destructive::Always`, so every call goes to the guardian and `manual`/`acceptEdits` prompt for it.
Both reach only agents of the same tree (a message queue, the team's task board), and every call
those agents make is already gated, so guardian review and prompts add latency with no security
value. After the deterministic floor, allow hooks and allow rules, both tools run in every mode with
no guardian verdict and no prompt. Issue #394 asked for `send_message`; its owner comment extends
it to `board_update` ("task management is there to help the model").

## Current State

- `rust/fontaine-tools/src/descriptor.rs` (~L219, ~L235 and the mirrored test table ~L491, ~L507):
  `send_message` and `board_update` are `Arbitrary`, not read-only, `Destructive::Always`.
  `board_show` is already `ReadOnly` and takes the read-only fast path.
- `rust/fontaine-core/src/permissions.rs` `evaluate` (~L1008): the `agent` branch sits after the
  allow rules, hook allow and out-of-scope handling, and before the mode `match`. Everything
  else `Arbitrary` falls to the mode: `manual`/`acceptEdits` prompt, `auto`/`plan` the guardian,
  `dontAsk` denies.
- `rust/fontaine-core/src/adjudication.rs` `spawn_changes_state` (~L79): `send_message` is
  hard-coded as non-state-changing in a child's toolset; `board_update` is not, so a spawn holding
  it is treated as state-changing.
- `rust/fontaine-core/src/tier.rs`: `Tier` enum (18 variants); `Spawn` is the precedent for a tier
  that records why an `Arbitrary` call skipped the guardian (`tasks/completed/agent_spawn_guardian_skip_plan.md`).

## Design

### A `coordination` tier in `evaluate`

Add `Tier::Coordination` (`"coordination"`) to the enum, `Tier::ALL` (18 → 19) and `as_str`.

In `evaluate`, next to the `agent` branch (same position: after allow rules, hooks and the
out-of-scope handling, before the mode dispatch), a branch for coordination tools returns
`Verdict::Allow { tier: Tier::Coordination, rule: None }` in **every** mode, `dontAsk` included
(an unattended `dontAsk` team run could otherwise never message or use the board). Everything
above stays in force, so `deny`/`ask` rules naming either tool, the floor, hooks and the
child-toolset restriction still act first. The `task_completed` hook still runs inside
`board_update complete`.

The set lives once in `fontaine-tools/src/descriptor.rs` as `pub fn is_coordination(name: &str)
-> bool` over a `COORDINATION` const (`send_message`, `board_update`). Their descriptor rows stay
`Arbitrary`/`Always`, so the guardian crate's view of them, the choke-point test and `allow_rule`
matching are unchanged: this is a gate-level skip, not a reclassification (read-only ≠ safe).

`spawn_changes_state` replaces its `tool == "send_message"` clause with
`descriptor::is_coordination(tool)`, so a child holding only read-only tools plus the
coordination tools keeps the read-only spawn fast path. This is the same containment argument:
neither tool changes anything outside the tree.

`Tier::Coordination` is excluded from `guardian_allowed()` like the other skip tiers and, like
`Spawn`, returns before `guardian_branch`, so it ignores the auto-pause circuit breaker.

`fontaine-guardian` is untouched (lift-out-able); embedders may still route these calls to it.

## Implementation Steps

1. `rust/fontaine-tools/src/descriptor.rs`: add `COORDINATION` and `is_coordination`, with a
   `why` comment (reaches only agents of the same tree; every call they make is gated).
2. `rust/fontaine-core/src/tier.rs`: add `Coordination`, its `as_str` arm, extend `Tier::ALL`.
3. `rust/fontaine-core/src/permissions.rs`: the branch above, before the `agent` branch.
4. `rust/fontaine-core/src/adjudication.rs`: use `is_coordination`; update the doc comment and
   its unit tests (a child with `board_update` no longer changes state).
5. Update tests that expected a guardian/prompt for these tools: `rust/fontaine-core/tests/`
   `permissions_auto.rs`, `permission_rules.rs`, `teams.rs`, `agent_pipeline.rs`, and
   `rust/fontaine/tests/{parallel_agents,headless_smoke}.rs`, `rust/fontaine-agents/tests/seam.rs`
   where they assert on gating of `send_message`/`board_update` (rg for them). `choke_point.rs`
   must stay green unchanged.
6. `docs/schemas/headless-v1.schema.json`: add `"coordination"` to `$defs.tier.enum`.
7. Evals: add `evals/dataset` cases (pipeline kind) for `send_message` and `board_update` in
   `auto`, `expected_tier: "coordination"`; add `coordination` to the tier name list in
   `evals/README.md`. Run the fixture evals and the live guardian eval (see Testing).
8. Docs: `docs/permissions.md` (pipeline step, tier list, the `board_update`/`send_message`
   paragraph ~L182, spawn paragraph ~L79), `docs/agents.md` (~L185, ~L247, ~L262), `docs/tools.md`
   (rows ~L34-36), `docs/headless.md` (tier list), `docs/architecture.md` (pipeline diagram),
   `tasks/design/permissions.md` (~L60 tier list), `AGENTS.md` (the fast-path exceptions: add the
   coordination tools, and the `send_message` mention in the read-only-spawn clause becomes
   "coordination tools"). `tasks/fontaine_harness_plan.md` `## Decisions`: add a dated line at
   the end of `### Permissions`; the ~L404 board line is amended by reference, not rewritten.
   `CHANGELOG.md` is updated by the `pr` step.

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs`
- `rust/fontaine-core/src/{tier,permissions,adjudication}.rs`
- the tests listed in step 5
- `docs/schemas/headless-v1.schema.json`, `evals/dataset/*.jsonl`, `evals/README.md`
- `docs/{permissions,agents,tools,headless,architecture}.md`, `tasks/design/permissions.md`,
  `AGENTS.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **New tier vs reclassifying as `ReadOnly`.** Reclassifying would put the tools on the read-only
  fast path but would lie in the descriptor table, feed "read-only" to every consumer (guardian
  prompt, MCP-style heuristics) and contradict "read-only ≠ safe". A tier keeps the audit record.
- **New tier vs reusing `Spawn`/`Mode`.** One enum variant buys an audit trail of why the call ran.

## Decisions

- 2026-11-02 (design): `dontAsk` allows the coordination tools too, since the issue says "every
  mode" and a headless team run would otherwise stall on its own messaging.
- 2026-11-02 (design): `board_update` joins `send_message` per the issue-owner comment; both
  are bounded to the agent tree, and task text stays untrusted data, never guardian intent.

## Documentation

See step 8.

## Testing Strategy

- Unit tests in `permissions` integration tests: for each of `send_message` and `board_update`,
  in every mode, the verdict is `Allow` at `coordination` with 0 guardian calls and 0 prompts.
- A `deny` rule and an `ask` rule naming each tool still win; a hook deny still wins; a
  critical/protected-path floor is unaffected (not reachable by these tools, covered by existing
  tests).
- `spawn_changes_state`: a child with `read` + `board_update` is not state-changing.
- Tier round-trip tests (`Tier::ALL`, schema validation in `headless_contract.rs`) cover the new
  value.
- Choke-point test stays green; fixture evals (`--guardian fixture`, default and
  `--profile minimal`) pass with the new cases.
- Live Haiku guardian eval run before push (per project practice) to confirm no regression; no
  golden case is edited to make a model pass.

## Open Questions

None.
