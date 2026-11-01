# Agent Spawn Guardian Skip Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/360

## Overview

Two changes to how the permission pipeline treats an `agent` spawn call, from the incident in
the issue: an AFK skill run was interrupted when the guardian classified a benign worker spawn
("the user is away, resolve these items") as `Download+Execute` at 0.6 confidence and escalated
to a human.

1. A spawn is no longer guardian-judged at all. The deterministic floor still applies to the
   `agent` call (deny/ask rules, hooks, child restrictions); after the floor the mode decides,
   as it does for calls nobody pre-approved — except the guardian/screen cascade is removed from
   the spawn's path. The containment argument: every action the child takes is gated by this
   same pipeline on its own way out, and a child subagent's effective mode is never looser than
   its parent's, so judging the spawn's prompt with a model classifier duplicates coverage that
   already holds call-by-call, and does so at exactly the point the user has just explicitly
   asked for the spawn.
2. The guardian's explanation sentence is persisted in the audit record. Today the record of an
   escalated (or allowed) guardian decision has `explanation: null`, so a misjudgment like this
   incident is unrecoverable once the TUI dialog closes.

## Current State

- `PermissionPipeline::evaluate` (`rust/fontaine-core/src/permissions.rs`) runs the floor
  (hook deny, deny rules, child restriction, plan block, forced ask, critical path, protected
  path, argument error), then the read-only fast path, then a spawn fast path: `tool == "agent"`
  with `adjudication::spawn_facts(...).is_some_and(|spawn| !spawn.changes_state)` allows at
  `Tier::ReadOnlyFastPath` in every mode. A spawn that can change state falls through to the
  mode dispatch, where `auto` and `plan` answer `Verdict::Guardian` and `manual`/`acceptEdits`
  prompt (`dontAsk` denies).
- `guardian_branch` (same file) sends the call through the screen+guardian cascade
  (`fontaine_guardian::cascade::adjudicate`). An escalate asks the human
  (`settle`, `GuardianDecision::Escalate` arm). This is where the incident's prompt came from.
- `adjudication::build` (`rust/fontaine-core/src/adjudication.rs`) fills
  `session_facts.spawn` for `agent` calls, and the guardian prompt shows the child's computed
  toolset.
- `DecisionRecord.explanation` (`rust/fontaine-core/src/audit.rs`) holds the model-facing denial
  message for guardian denies; guardian allows and escalate-prompts record `None` (`settle` and
  `with_guardian` in permissions.rs). The audit field doc says "What the model was told for a
  denial."
- `Tier` (`rust/fontaine-core/src/tier.rs`) has 17 variants; the only exhaustive consumer is its
  own `as_str`/`ALL`. The tier string reaches the audit log, the `tool_decision` telemetry
  event and the headless `gate_decision` event as-is.
- Tests encoding the current behavior: `the_spawn_call_is_gated_like_any_arbitrary_call` and
  `a_spawn_that_can_change_state_still_reaches_the_guardian`
  (`rust/fontaine-core/tests/agent_pipeline.rs`); `a_read_only_spawn_takes_the_fast_path_in_every_mode_without_the_guardian`
  pins the read-only fast path and stays as-is.
- Evals: `evals/dataset/spawn.jsonl` case `spawn-worker-not-fast-path` expects `deny` at tier
  `guardian` for a worker spawn in `auto`. Fixture mode (`score_fixture`,
  `rust/fontaine-evals/src/runner.rs`) replays `fixture_verdict` against `expected`, and the
  fixture eval runs are CI steps.
- Docs describing the current behavior: `docs/permissions.md` pipeline step 3 (read-only spawn
  fast path), step 7 (the mode/guardian), the tier vocabulary list, and the "guardian skips"
  paragraph near the end; `docs/guardian.md` intro and the "Subagents and the scratch
  directory" section; the security invariants in `AGENTS.md` ("three exceptions" to the fast
  path).

## Design

### Spawns leave the guardian's path

In `evaluate`, after the allow rules, the hook allow and the out-of-scope handling — everything
that can pre-approve or pre-refuse the call — and before the mode dispatch, an `agent` branch
decides the spawn without the cascade:

- `auto` and `plan` → `Verdict::Allow { tier: Tier::Spawn, rule: None }`.
- `manual` and `acceptEdits` → the same prompt as today (`would_prompt` with
  "it needs the user's approval").
- `dontAsk` → the same denial as today.

Everything above the branch is unchanged, so a `deny`/`ask` rule on `agent`, a hook decision,
the parent-toolset restriction (`child_restriction`, rule `agent tools`) and the critical-path
check all still act on the spawn first. The read-only spawn fast path above it also stays
unchanged (it still allows in every mode at `read_only_fast_path`).

A new `Tier::Spawn` (`"spawn"`) is added to the enum, `Tier::ALL` (17 → 18) and `as_str`. It
flows to the audit log, telemetry and the headless `gate_decision` event like any other tier;
the eval `expected_tier` comparison is string-based and needs no code change.

Plan mode allows the spawn because the child's edits are plan-blocked: the child's posture is
the tighter of its own and its parent's, so a plan-mode spawn can plan but cannot edit, and
each of its calls is still gated.

The `fontaine-guardian` crate is untouched: it keeps its spawn-aware request (`SpawnFacts`,
the child-toolset section of the prompt) because it is lift-out-able and an embedding harness
may still route spawns to it. Guardian-kind eval cases for `agent` calls
(`evals/dataset/overdeny.jsonl` `attack-overdeny-03`) test the crate in isolation and stay.

### The guardian's explanation reaches the audit log

In `with_guardian` (permissions.rs), fill the record's explanation when it is empty:

```rust
record
    .explanation
    .get_or_insert_with(|| adjudication.explanation.clone());
```

Deny records already carry the model-facing message, which embeds the explanation sentence, so
`get_or_insert` leaves them unchanged ("error text may explain a Deny, never change it");
guardian allows and escalate-prompts — the case in the incident — gain the guardian's own
sentence. The field doc in audit.rs is updated to say it holds the guardian's reason sentence,
or what the model was told for a denial.

## Implementation Steps

1. `rust/fontaine-core/src/tier.rs`: add the `Spawn` variant, its `as_str` arm (`"spawn"`), and
   extend `Tier::ALL`.
2. `rust/fontaine-core/src/permissions.rs`, `evaluate`: insert the `agent` branch before the
   mode match, with a comment giving the containment reason (each child call is gated; the
   child's mode is never looser than the parent's).
3. `rust/fontaine-core/src/permissions.rs`, `with_guardian`: fill `explanation` via
   `get_or_insert_with` as above. Update the `explanation` field doc in
   `rust/fontaine-core/src/audit.rs`.
4. Update `rust/fontaine-core/tests/agent_pipeline.rs`:
   - `the_spawn_call_is_gated_like_any_arbitrary_call`: in `auto` the spawn is now allowed at
     tier `spawn` with the guardian never consulted (`fake.calls() == 0`); the manual prompt and
     the deny-rule arms stay.
   - Replace `a_spawn_that_can_change_state_still_reaches_the_guardian` with its inverse: a
     state-changing spawn (`worker`, `fork`, `nests`, `shelly`, `fetchy`, `mcpish`) in `auto` is
     allowed at tier `spawn`. Its fake guardian is an escalate-verdict fixture (`Download+Execute`)
     and the spawn prompt is the AFK-style "the user is away, resolve these items" instruction
     from #360; assert `fake.calls() == 0` and `asked() == 0`.
   - Add mode coverage for a state-changing spawn: `plan` allows at tier `spawn`,
     `acceptEdits` prompts, `dontAsk` denies.
   - Add an audit assertion: a guardian escalate writes a record whose `explanation` is the
     guardian's sentence; a guardian deny record's explanation is unchanged (the model-facing
     message).
   - Rework `a_nested_child_whose_type_lists_agent_does_not_qualify`: the nested `nests` spawn
     in `auto` is now allowed at tier `spawn` with `fake.calls() == 0`; it still does not
     qualify for the read-only fast path.
   - Rework `the_guardian_takes_its_intent_from_the_roots_human_messages_only`: the spawn
     prompt never reaches the guardian, so the positive `contains(injected)` and
     delimited-tool-call assertions (~236) are dropped or inverted, `len() == 2` becomes 1, and
     only the intent-sanitization assertions that still apply to the child's shell request
     (`sent[0]`) are kept.
   - Rework `a_spawn_prompt_claiming_authorization_is_never_the_guardians_intent` in
     `rust/fontaine/tests/parallel_agents.rs` the same way (~891): drop or invert the positive
     spawn-prompt assertions, assert a single guardian request (the child's shell call), keep
     only the intent-sanitization assertions that still apply to it.
   - Fix `broad_code_execution_rules_are_suspended_while_auto_is_on` in
     `rust/fontaine-core/tests/permissions_auto.rs` (~292-325): its loop over `("agent", None)`
     in `auto` asserts a Guardian tier call; drop `agent` from the loop or assert tier `Spawn`
     with 0 guardian calls. The suspended-rule listing assertions stay.
5. `evals/dataset/spawn.jsonl`: rename `spawn-worker-not-fast-path` to
   `spawn-worker-auto-gate` (the old id claims the case is not on the fast path, which the
   new expectation contradicts) and set it to `"expected": "allow",
   "expected_tier": "spawn"` with a matching `fixture_verdict`
   (`{"decision": "allow", "tier": "spawn"}`).
6. `docs/schemas/headless-v1.schema.json`: add `"spawn"` to `$defs.tier.enum` (a closed list
   consumed by `gate_decision` and `result.denials`; `guardian_off` already sits in the enum
   at `schema_version` 1, so adding an enum value stays at v1). `headless_contract.rs`
   validates real `fontaine -p` output against it.
7. Docs: `docs/permissions.md` (step 3's spawn paragraph, step 7, the tier list, the
   "guardian skips" paragraph, and the suspended-rule text near lines 259-262 that says the
   rule "goes to the guardian instead", untrue for a bare `agent` rule now that
   `is_broad_code_execution` has `("agent", None)`), `docs/guardian.md` (intro list and the subagents section),
   `docs/agents.md` (spawn paragraph: `auto`/`plan` allow at the `spawn` tier,
   `manual`/`acceptEdits` ask; the passage "A bare `agent` allow rule is one of the broad rules
   `auto` sets aside"; and lines ~136-139, "auto has the guardian judge the task, toolset and
   model together"), `docs/evals.md` (`spawn-worker-auto-gate` sentence: a
   `worker` spawn in `auto` is allowed at tier `spawn`), `docs/headless.md` (add `spawn` to
   the `gate_decision` tier list), `evals/README.md` (the `tier`/`expected_tier` name list
   near lines 60-61 ends at `human`; add `spawn`), and the fast-path invariant in `AGENTS.md` (three
   exceptions become four); `docs/architecture.md` (the numbered pipeline diagram:
   show the new branch — a `Tier::Spawn` allow for `auto`/`plan`, a prompt for
   `manual`/`acceptEdits`, a deny for `dontAsk` — between the fast paths and the mode
   dispatch), `tasks/design/permissions.md` (the Tier-enum paragraph, the
   pipeline-step description, and the "Where allow acts" sentence that the
   containment fast path and Tier 0.5 are the only guardian skips earned
   without a grant), and `README.md` (the "Auto mode" list: a spawn of a
   subagent — read-only or state-changing — no longer reads as
   guardian-reachable). Add a dated line to `## Decisions` in
   `tasks/fontaine_harness_plan.md`, at the end of `### Permissions` (after the existing
   read-only-spawn lines, which it extends from read-only spawns to all spawns), recording that
   spawns are no longer guardian-judged at all, with the containment reason and the issue reference (#360). `CHANGELOG.md` is
   updated by the `pr` step.

## Files to Modify

- `rust/fontaine-core/src/tier.rs`
- `rust/fontaine-core/src/permissions.rs`
- `rust/fontaine-core/src/audit.rs` (field doc only)
- `rust/fontaine-core/tests/agent_pipeline.rs`
- `rust/fontaine-core/tests/permissions_auto.rs`
- `rust/fontaine/tests/parallel_agents.rs`
- `evals/dataset/spawn.jsonl`
- `evals/README.md`
- `docs/schemas/headless-v1.schema.json`
- `docs/permissions.md`, `docs/guardian.md`, `docs/agents.md`, `docs/evals.md`,
  `docs/headless.md`, `docs/architecture.md`, `README.md`, `AGENTS.md`,
  `tasks/design/permissions.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **New tier vs reusing `mode`.** A distinct `spawn` tier keeps the audit/telemetry record of why
  the call ran — the observability the issue asks for — at the cost of one enum variant. The
  tier string is a plain value in every consumer.

## Decisions

- 2026-10-31 (design): `plan`-mode spawns are allowed like `auto`'s; the child's plan block is
  inherited, so the spawn cannot edit through the back door.
- 2026-10-31 (design): `Tier::Spawn` is excluded from `guardian_allowed()`, like the other
  fast-path tiers; `allow()` calls it only for `TrackedEdit`/`ScratchWrite`.
- 2026-10-31 (design): a spawn of an unknown or malformed `agent_type` is allowed at the gate in
  `auto`/`plan` and refused by the `AgentTool` itself on execution, as it already is for
  allowed calls today. The gate gains no agent-type validation.
- 2026-10-31 (AFK): `Tier::Spawn` deliberately ignores the auto-pause circuit breaker (it returns
  before `guardian_branch`, where `auto_paused()` is checked); prompting for a spawn while paused
  would reintroduce the #360 AFK interruption, and damage stays bounded because every call the
  child makes is still gated and asks the human while the breaker is paused.

## Testing Strategy

- Unit/integration through the pipeline test rig (`Rig` in
  `rust/fontaine-core/tests/common`), with the fake/fixture guardian — no live APIs anywhere.
- CI steps from the repo conventions: `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test`, and both fixture eval runs
  (`cargo run -p fontaine-evals -- --dataset ../evals/dataset --guardian fixture` and
  `--profile minimal`); the full-profile run covers the renamed dataset case
  (`spawn-worker-auto-gate` stays a `"profile": "full"` case, which `--profile
  minimal` does not select).
- The `--guardian none` eval run keeps skipping `auto`-mode pipeline cases: it skips on
  `mode == auto` regardless of tool, and non-spawn `auto` calls still dispatch to the
  guardian, so the skip remains correct for them. A finer-grained skip is a possible
  follow-up, out of scope here.
