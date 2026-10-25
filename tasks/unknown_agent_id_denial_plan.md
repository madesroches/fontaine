# Unknown Agent Id Denial Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/184

## Overview

The permission pipeline picks a call's mode from the agent id it carries, and an id the agent tree
has no node for is judged as the root (`posture_of`) with no toolset restriction
(`child_restriction`). "A child is never looser than its parent" therefore holds only because no
running child's node is ever released. Make the pipeline fail closed: only `ROOT_AGENT` gets the
root's posture; an id whose lineage does not resolve to the root through live nodes is denied,
in every mode.

## Current State

- `PermissionPipeline::posture_of` (`rust/fontaine-core/src/permissions.rs:491`) returns the session
  mode when `agent == ROOT_AGENT || !tree.is_child(agent)`. `AgentTree::is_child`
  (`rust/fontaine-agents/src/tree.rs:408`) is false for the root *and* for any id without a node.
- `AgentTree::posture` (`tree.rs:285`) walks parent links and stops at the first missing node, so
  a grandchild whose parent node was released also computes a posture looser than it should.
- `SessionState::child_restriction` (`rust/fontaine-core/src/gate.rs:517`) returns `None` when no
  entry exists and the parent walk ends (`parent_of` is `None` for a missing node).
- `adjudicate` (`permissions.rs:1054`) calls `posture_of` before running hooks; `evaluate`
  (`permissions.rs:608`) runs hook deny, deny rules, `child_restriction`, plan block, then the floor.
- `user_hooks.rs:413` (post-tool hook mode label) also uses `is_child`; it only labels an event for
  a call already decided, so it is unchanged.
- `fontaine-tools/src/guarded.rs:126` already refuses a call that carries no agent id.
- The root has a node in the tree (`tree.rs:196`) with no parent; `release` is only reached from
  failed or dropped spawns (`spawn.rs:81`, `:248`).

## Design

**Tree**: add `AgentTree::lineage_intact(&self, id) -> bool`: true for `ROOT_AGENT`; otherwise true
iff every node on the parent chain up to the root exists (bounded at 64 hops like `posture`, a
corrupt link counts as broken). This covers an unreserved id, a released id, and a descendant whose
ancestor was released.

**Pipeline**: `posture_of` returns `Option<(PermissionMode, bool)>`, `None` when the lineage is
not intact (root short-circuits before any tree lookup). `adjudicate` handles `None` before running
hooks: it builds the call context with the strictest mode (`dontAsk`) for the audit record and
denies through the existing `self.deny` path with `Tier::DenyRule`, rule `agent registry`, reason
"the calling agent is no longer registered". `Tier::DenyRule` keeps the denial in the floor's
audit/telemetry tier set, so it is recorded like any other floor denial, and no new tier is added.
Hooks and the guardian are never consulted, so no mode, hook or verdict can reach past it. The
other `posture_of` caller (`permissions.rs:1678`, a telemetry attribute) takes the mode with a
`dontAsk` fallback.

**Restriction**: `SessionState::child_restriction` returns `Some("the calling agent is no longer
registered")` for a non-root id whose lineage is not intact, as defense in depth for any caller
that reaches it without going through `adjudicate`.

No config surface, no regex on arguments.

## Implementation Steps

1. `fontaine-agents/src/tree.rs`: add `lineage_intact` with a unit test beside the existing
   posture tests.
2. `fontaine-core/src/permissions.rs`: change `posture_of` to return an `Option`; handle `None` in
   `adjudicate` as above; adapt the telemetry caller.
3. `fontaine-core/src/gate.rs`: broken-lineage branch in `child_restriction`.
4. Tests (see Testing Strategy).
5. Docs and CHANGELOG entry.

## Files to Modify

- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/permissions.rs`
- `rust/fontaine-core/src/gate.rs`
- `rust/fontaine-core/tests/agent_pipeline.rs`
- `docs/permissions.md` (and `docs/agents.md` if it states the child-mode invariant)
- `CHANGELOG.md`

## Trade-offs

- **Deny before hooks vs. inside `evaluate`**: running hooks for a caller we cannot identify gains
  nothing and would hand its input to user commands; denying first is simpler.
- **`lineage_intact` vs. plain `contains_key`**: the latter misses a grandchild under a released
  parent, which `AgentTree::posture` would also loosen.
- **Reusing `Tier::DenyRule`** over a new tier: avoids touching `Tier::ALL`, the telemetry tier
  set and docs tables.

## Documentation

`docs/permissions.md`: note that a call from an agent the session no longer knows is denied in
every mode. `CHANGELOG.md`: one entry.

## Testing Strategy

All unit/integration, no live APIs. In `tests/agent_pipeline.rs`, using `Rig::check_as`:

- A never-reserved id, a reserved-then-released id, and a grandchild whose parent was released are
  each denied, under every mode (`Manual`, `Auto`, `DontAsk`, `Plan` as supported by the rig),
  including an `auto` root with an allow-verdict fake guardian (guardian call count stays 0).
- The audit record has `Decision::Deny`, `Tier::DenyRule`, rule `agent registry`.
- Root calls and existing child-posture tests are unaffected (existing tests stay green).
- `child_restriction` directly: unknown non-root id yields a restriction, root and a live child
  do not.
- `tree.rs`: `lineage_intact` for root, live child, grandchild, released child, released parent,
  never-reserved id.

## Open Questions

None.
