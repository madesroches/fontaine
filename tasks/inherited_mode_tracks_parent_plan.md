# Inherited Subagent Mode Tracks the Parent Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/790

## Overview

A subagent whose type says `mode: inherit` (the default) keeps the mode its parent had when it
was spawned as a ceiling, so it follows the parent down but never back up. Loosening the session
from `acceptEdits` to `auto` leaves a running child asking for every non-edit call. This plan makes
the tree store each child's *ceiling* — what its type's setting allows, independent of the parent's
mode at spawn — so `inherit` tracks the parent's current mode in both directions, while an
explicit `manual` or `dontAsk` stays capped. The invariant "a child's effective mode is never
looser than its parent's" is unchanged: the effective mode is still `min(ceiling, parent now)`.

## Current State

- `rust/fontaine-sdk/src/agents/mode.rs`: `own_mode(setting, parent)` resolves the type's
  `ModeSetting` against the parent's posture *at spawn*: `Inherit → parent.mode`,
  `Manual → Manual.min(parent.mode)`, `DontAsk → DontAsk`. `Posture::child(own, parent)` returns
  `own.min(parent.mode)`.
- `rust/fontaine-sdk/src/agents/tree.rs`: `Node::own_mode` stores that snapshot.
  `AgentTree::posture` (~930) walks the parent chain and folds `Posture::child` from the root's
  live mode down, on every call. `reserve` / `reserve_named` (~980–1050) take the `own_mode`;
  `revival_request` (~1301) hands `node.own_mode` to the runner as `RevivalRequest::mode`;
  `rebuild` (~1326) recomputes it with `own_mode(agent_type.mode, self.parent_posture(parent))`.
- `rust/fontaine-sdk/src/agents/spawn.rs` (~1138): computes `mode = own_mode(...)`, passes it to
  `reserve_named` and as `SpawnRequest::mode`.
- `SpawnRequest::mode` / `RevivalRequest::mode` (`agents/runner.rs`) are informational: the
  production runner (`core/child_runner.rs`) never reads them; only the test runner in
  `tests/seam.rs` records them. Every permission decision reads `tree.posture(agent)`
  (`core/permissions.rs` `posture_of`, `core/user_hooks.rs` ~850 and ~1168).
- Result: `own = AcceptEdits` (the parent was in `acceptEdits` at spawn) and parent raised to
  `Auto` gives `min(AcceptEdits, Auto) = AcceptEdits`. Same for an explicit `manual` spawned
  under a `dontAsk` parent: it stays `dontAsk` after the parent loosens.
- Tests `a_child_is_never_looser_than_its_parent` (mode.rs) and
  `posture_follows_the_parent_on_every_call` (tree.rs) only go Auto → Manual → Auto, so they
  never raise the parent above the child's spawn mode.
- `docs/agents.md` "Modes: never looser than the parent" says a child's mode is the tighter of
  its *own mode* and the parent's mode at that moment, without saying the own mode is a spawn
  snapshot.

## Design

Replace the snapshot with a ceiling that depends only on the type's setting.

`agents/mode.rs`:

```rust
impl ModeSetting {
    /// The loosest mode a child of this type may ever run in; its effective mode is this
    /// capped by its parent's current posture.
    pub fn ceiling(self) -> Ladder {
        match self {
            ModeSetting::Inherit => Ladder::Auto,
            ModeSetting::Manual => Ladder::Manual,
            ModeSetting::DontAsk => Ladder::DontAsk,
        }
    }
}
```

`Ladder::Auto` is the top of the ladder, so `Posture::child(Auto, parent) == parent`: an
`inherit` child equals its parent's live posture. `Posture::child` is unchanged. `own_mode` is
removed; the one value it still served (the mode the child starts with, reported to the runner)
is `Posture::child(setting.ceiling(), parent_posture).mode`.

`agents/tree.rs`:

- Rename `Node::own_mode` to `ceiling` and the `own_mode` parameter of `reserve` /
  `reserve_named` to `ceiling` (still a `Ladder`). Existing test callers that pass `Ladder::Auto`
  already mean "no cap of its own", and `Ladder::Manual` an explicit `manual` type, so they keep
  their meaning.
- `posture` pushes `node.ceiling` instead of `node.own_mode`; the fold is unchanged. Update its
  doc comment to say an `inherit` child follows its parent both ways and an explicit mode is a
  ceiling.
- `rebuild` stores `agent_type.mode.ceiling()` (no parent posture needed).
- `revival_request` sets `RevivalRequest::mode` to `self.posture(id).mode`, computed before
  taking the tree lock (`posture` takes the lock itself) — the same "starts with" meaning as
  `SpawnRequest::mode`.

`agents/spawn.rs`: pass `agent_type.mode.ceiling()` to `reserve_named`, and
`SpawnRequest::mode = Posture::child(ceiling, self.tree.parent_posture(&caller)).mode`, so the
runner and `tests/seam.rs` see the same values as today.

`agents/runner.rs`: reword the `SpawnRequest::mode` doc: the mode the child starts with; the tree
re-derives it from the type's ceiling and the parent's posture on every call.

The root node's placeholder `own_mode: Ladder::Manual` (tree.rs ~679) becomes `ceiling:
Ladder::Manual`; `posture` never reads the root's node value, so this is cosmetic.

## Implementation Steps

1. `agents/mode.rs`: add `ModeSetting::ceiling`, remove `own_mode`, update the module doc if
   needed. Rewrite the two unit tests over `ceiling` + `Posture::child`, adding the raise case:
   `child(Inherit.ceiling(), AcceptEdits) == AcceptEdits`, then `child(…, Auto) == Auto`;
   `child(Manual.ceiling(), DontAsk) == DontAsk`, then `child(…, Auto) == Manual`;
   `DontAsk` stays `DontAsk` under any parent.
2. `agents/tree.rs`: rename the field and parameters, update `posture` / `rebuild` /
   `revival_request` as above, fix the imports, and reword the `rebuild` doc comment (~1308): the
   node keeps its type's ceiling and its posture is derived from the parent's on every call.
3. `agents/spawn.rs`: use `ceiling()` and compute the start mode as above.
4. `agents/runner.rs`: doc comment on `SpawnRequest::mode`.
5. Tests (see Testing Strategy).
6. Docs and the plan's Decisions line (see Documentation).

## Files to Modify

- `rust/fontaine-sdk/src/agents/mode.rs`
- `rust/fontaine-sdk/src/agents/tree.rs`
- `rust/fontaine-sdk/src/agents/spawn.rs`
- `rust/fontaine-sdk/src/agents/runner.rs`
- `rust/fontaine-sdk/tests/seam.rs`
- `docs/agents.md`
- `tasks/fontaine_harness_plan.md` (Decisions → Agents)

## Trade-offs

- **Store the `ModeSetting` on the node** (the issue's first suggestion) vs. **store a `Ladder`
  ceiling**: the ceiling keeps `Posture::child` and the `reserve` signature's type unchanged, so
  the many test callers passing a `Ladder` keep compiling with the same meaning, and the tree
  stays free of the type-setting enum. Equivalent behavior.
- **Explicit `manual` as a ceiling vs. as a spawn snapshot** (`Manual.min(parent at spawn)`): the
  issue only requires `inherit` to track. Keeping the spawn-time min for `manual` would preserve
  the same one-way bug for a `manual` type spawned under a `dontAsk` parent. The ceiling form
  gives a `manual` child `manual` once the parent is at or above it, which is what its type asks
  for, and is never looser than the parent.
- **Leave `SpawnRequest::mode` meaning the start mode** vs. changing it to the ceiling: no
  production code reads it, and keeping it as the start mode leaves the runner seam and its
  tests unchanged.

## Decisions

## Documentation

- `docs/agents.md`, "Modes: never looser than the parent": say an `inherit` subagent follows the
  parent's current mode both ways (loosening the session to `auto` loosens a running `inherit`
  subagent's next call too), and an explicit `manual` or `dontAsk` is a ceiling: the tighter of
  it and the parent's current mode. Keep the existing `auto → manual` example.
- `tasks/fontaine_harness_plan.md`, `### Agents` subsection of `## Decisions`, appended at its
  end: `- 2026-11-25: (#790) A subagent's mode is the tighter of its type's ceiling (inherit = no
  cap, manual, dontAsk) and its parent's current posture, recomputed on every call; inherit no
  longer snapshots the parent's mode at spawn. Reason: loosening the session must reach running
  inherit children (restores the Phase 5 design intent that an inherited mode is re-evaluated
  against the parent's current mode on every call), and the child is still never looser than its
  parent.`
- `CHANGELOG.md` is handled by the PR step.

## Testing Strategy

All unit/integration tests, no live services.

- `agents/mode.rs` unit tests as in step 1 (ceiling values; raise and lower through
  `Posture::child`).
- `agents/tree.rs`: extend `posture_follows_the_parent_on_every_call` (or add a sibling test)
  with a root cell starting at `AcceptEdits`, a child reserved with `ModeSetting::Inherit.ceiling()`,
  then raising the root to `Auto` → child `Auto`; and a grandchild under it following the same
  raise (the chain fold). Add a `Manual` ceiling child: root `DontAsk` → `DontAsk`, root `Auto` →
  `Manual`.
- `tests/seam.rs` — regression test pinning the reported bug end to end through the `agent`
  tool: a rig built with `rig_with(..., Ladder::AcceptEdits)`; spawn `worker`
  (`inherit`); assert `spawned()[0].mode == AcceptEdits`; raise the root with `rig.tree.set_root_mode(|| Ladder::Auto)`; assert
  `tree.posture(child).mode == Auto`. Named after the bug (raising the session reaches a running
  inherit subagent). Existing seam assertions on `spawned()[..].mode`
  (`Auto`, `DontAsk`, `Manual`) must keep passing unchanged.

## Open Questions
