# Coordination Tools Skip the Guardian Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/394

## Overview

`send_message` and `board_update` are agent-coordination tools. They are classified `Arbitrary` /
`Destructive::Always`, so every call goes to the guardian and `manual`/`acceptEdits` prompt for it.
Both reach only agents of the same tree (a message queue, the team's task board; no model-chosen
effect lands outside the tree, `board_update` writes only the session's own `tasks.json` and runs
the user's `task_completed` hooks), and every call
those agents make is already gated, so guardian review and prompts add latency with no security
value. After the deterministic floor, allow hooks and allow rules, both tools run in every mode with
no guardian verdict and no prompt.

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
above stays in force, so `deny`/`ask` rules naming either tool, the floor, hooks and
child specifier deny rules still act first; a child without these tools never has them (its
toolset is removed at fork, `fork_tools` in `child_runner.rs`). The `task_completed` hook still runs inside
`board_update complete`.

The set lives once in `fontaine-tools/src/descriptor.rs` as `pub fn is_coordination(name: &str)
-> bool` over a `COORDINATION` const (`send_message`, `board_update`). Their descriptor rows stay
`Arbitrary`/`Always`, so the guardian crate's view of them, the choke-point test and `allow_rule`
matching are unchanged: this is a gate-level skip, not a reclassification (read-only ≠ safe).

`spawn_changes_state` replaces its `tool == "send_message"` clause with
`descriptor::is_coordination(tool)`, so a child holding only read-only tools plus the
coordination tools keeps the read-only spawn fast path. This is the same containment argument:
no model-chosen effect reaches outside the tree; `board_update` writes only the session's own
`tasks.json` and runs the user's `task_completed` hooks.

`Tier::Coordination` is excluded from `guardian_allowed()` like `Spawn` and the read-only fast
path, unlike `tracked_edit`/`scratch_write`, and returns before `guardian_branch`, so it ignores the
auto-pause circuit breaker. Coordination calls therefore no longer reset the denial run (today a
guardian allow of them does).

`fontaine-guardian` is untouched (lift-out-able); embedders may still route these calls to it.

## Implementation Steps

1. `rust/fontaine-tools/src/descriptor.rs`: add `COORDINATION` and `is_coordination`, with a
   `why` comment (reaches only agents of the same tree; every call they make is gated).
2. `rust/fontaine-core/src/tier.rs`: add `Coordination`, its `as_str` arm, extend `Tier::ALL`.
3. `rust/fontaine-core/src/permissions.rs`: the branch above, before the `agent` branch.
4. `rust/fontaine-core/src/adjudication.rs`: use `is_coordination`; update the doc comment and
   its unit tests (a child with `board_update` no longer changes state).
4b. Doc comments: `rust/fontaine-agents/src/tools.rs` (L1-4) and `rust/fontaine-agents/src/spawn.rs`
   (L3) call these "ordinary Arbitrary tools behind the permission pipeline"; reword to say the
   coordination tools skip the guardian at the `coordination` tier.
5. Update `rust/fontaine-core/tests/permissions_auto.rs`: tests
   `board_update_asks_in_manual_and_accept_edits_like_send_message` and
   `board_update_is_guardian_judged_in_auto_and_plan_with_no_fast_path` (~L1161-1190) break; the new
   per-mode tests belong there too. The `adjudication.rs` unit test is step 4; `choke_point.rs`
   stays green unchanged.
6. `docs/schemas/headless-v1.schema.json`: add `"coordination"` to `$defs.tier.enum`.
7. Evals: add `evals/dataset` cases (pipeline kind) for `send_message` and `board_update` in
   `auto`, `expected_tier: "coordination"`, category `spawn` (`case.rs` ~L202-207); also add `manual`
   and `dontAsk` cases for each tool so the branch is checked deterministically under
   `--guardian none` (auto cases are skipped there and fixture mode returns canned verdicts). Every
   case, in any mode, expects `allow`/`coordination`, so each carries
   `"fixture_verdict": {"decision": "allow", "tier": "coordination"}` (`case.rs` ~L231-242 rejects a
   pipeline case without a `{decision, tier}` verdict); add `coordination` to the tier name list in
   `evals/README.md`. Run the fixture evals and the live guardian eval (see Testing).
8. Docs: `docs/permissions.md` (pipeline step, tier list, the `board_update`/`send_message`
   paragraph ~L182, spawn paragraph ~L79, modes table ~L15-20 incl. the `dontAsk` row ~L19, and the
   "adds two tools and no new tier" sentence ~L179), `docs/guardian.md` (~L9-16, list of what the
   guardian never sees), `README.md` (~L90-93 "## Auto mode" list), `docs/evals.md` (~L126-127,
   `spawn.jsonl` pipeline description), `docs/agents.md` (~L185, ~L247, ~L262), `docs/tools.md`
   (rows ~L34-36), `docs/headless.md` (tier list), `docs/architecture.md` (pipeline diagram, ~L143, ~L148-149),
   `tasks/design/permissions.md` (~L60 read-only-spawn clause, ~L123, ~L171 tier list, ~L230), `AGENTS.md` (the fast-path exceptions: add the
   coordination tools, and the `send_message` mention in the read-only-spawn clause becomes
   "coordination tools"; change "four exceptions" to "five"). `tasks/fontaine_harness_plan.md` `## Decisions`: add a dated line at
   the end of `### Permissions`; it states explicitly that it supersedes the ~L404 board decision and
   the `send_message` guardian clauses at ~L343 and ~L714 of that file, which are amended by
   reference, not rewritten.
   `CHANGELOG.md` is updated by the `pr` step.

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs`
- `rust/fontaine-core/src/{tier,permissions,adjudication}.rs`
- `rust/fontaine-agents/src/{tools,spawn}.rs` (doc comments)
- `rust/fontaine-core/tests/permissions_auto.rs`
- `docs/schemas/headless-v1.schema.json`, `evals/dataset/*.jsonl`, `evals/README.md`
- `docs/{permissions,guardian,agents,tools,headless,architecture,evals}.md`, `README.md`, `tasks/design/permissions.md`,
  `AGENTS.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **New tier vs reusing `Spawn`/`Mode`.** One enum variant buys an audit trail of why the call ran.

## Decisions

- 2026-11-02 (design): `dontAsk` allows the coordination tools too, since the issue says "every
  mode" and a headless team run would otherwise stall on its own messaging.
- 2026-11-02 (design): `board_update` joins `send_message` per the issue-owner comment; both
  are bounded to the agent tree, and task text stays untrusted data, never guardian intent.
- 2026-11-02 (design): coordination calls do not reset the auto-pause breaker; messaging/board
  churn between denials is not progress, and the more conservative choice keeps the breaker from
  being evaded by cheap allowed calls.
- 2026-11-02 (design): the other `Arbitrary` tools were reviewed; `cron_create` stays
  guardian-judged (existing decision in `tasks/fontaine_harness_plan.md` ~L767) and `job_cancel`
  is not a coordination tool.
- 2026-11-02 (review): in `manual`, `acceptEdits` and `dontAsk` agents can now message and wake
  each other without a human prompt; spend is bounded only by the opt-in session and subagent
  budgets (all unset by default), and no wake cap is added.

## Documentation

See step 8.

## Testing Strategy

- Per-mode tests in `rust/fontaine-core/tests/permissions_auto.rs`: for each of `send_message` and `board_update`,
  in every mode, the verdict is `Allow` at `coordination` with 0 guardian calls and 0 prompts.
- A `deny` rule and an `ask` rule naming each tool still win; a hook deny still wins; a
  critical/protected-path floor is unaffected (not reachable by these tools, covered by existing
  tests).
- `spawn_changes_state`: a child with `read` + `board_update` is not state-changing.
- Add a unit assertion in `rust/fontaine/tests/headless_contract.rs` (it already loads the schema)
  that `$defs.tier.enum` equals `Tier::ALL`'s `as_str` names
  (existing `tier.rs` tests do not cover the new value).
- Choke-point test stays green; fixture evals (`--guardian fixture`, default and
  `--profile minimal`) pass with the new cases.
- Live Haiku guardian eval run before push (per project practice) to confirm no regression; no
  golden case is edited to make a model pass.

## Open Questions

None.
