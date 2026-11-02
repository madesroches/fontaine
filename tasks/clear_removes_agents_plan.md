# /clear Removes Old Agents from the Agent List Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/297

## Overview

`/clear` empties the model's context, the transcript, todos, scheduled tasks and the goal — but
the agent tree keeps every subagent ever spawned, so a subagent that failed, was killed or ran out
of budget within the last 30 seconds still appears in the Ctrl-T agent list after the clear. This
plan makes `/clear` prune the tree: a cleared session starts with an empty agent list, as the
issue expects.

## Current State

- `/clear` is handled by `Ui::clear` in `rust/fontaine-tui/src/ui/commands/context.rs`, which
  calls `AgentRuntime::clear_conversation` in `rust/fontaine-core/src/agent.rs:839`.
- `clear_conversation` refuses while any subagent runs or one has a report waiting
  (`ClearError::SubagentsRunning`, from `rust/fontaine-core/src/session.rs:138`), then clears the
  session conversation, background jobs, shell cwd, todos, scheduled tasks and the goal. It never
  touches the agent tree.
- The tree (`AgentTree` in `rust/fontaine-agents/src/tree.rs`) keeps every spawned node in
  `Inner.nodes`/`Inner.order` for the life of the process. There is no prune/clear method.
- Both agent surfaces read from the tree on every refresh
  (`rust/fontaine-tui/src/ui/mod.rs:70` → `AgentTree::rows()`): the compact agent list below the
  status bar (`rust/fontaine-tui/src/agent_list.rs`, where `ENDED_LISTED` keeps failed/killed/
  budget-exhausted agents listed for 30 s) and the full agents panel. After a clear, those rows are
  the stale ones the issue reports.
- The tree holds more than display rows, which a prune must not destroy:
  - `next` (AtomicU64, line 313) — the id counter, seeded at session load from the ledger "so ids
    never collide with persisted per-agent logs" (`rust/fontaine-core/src/agent/setup.rs:338`).
  - `names` (HashSet) — taken agent names, never reused (`generated_name`, tree.rs:267).
  - `session_spend` (tree.rs:215) — the session-wide spend tally the session budget cap is
    checked against (`debit`, tree.rs:819).
- Root state that legitimately survives a clear today: the session id, ledger, audit log, grants
  and permission mode (`docs/tui.md` Context section).

## Design

Add a prune to the tree and call it from `clear_conversation`, after the conversation clear has
succeeded. Keep every counter and tally; drop only the subagent nodes.

### `AgentTree::clear_ended` (new, `rust/fontaine-agents/src/tree.rs`)

Under one `Inner` lock:

- Retain only the root node (`ROOT_AGENT`) in `nodes`, plus any running nodes that are
  skipped per the next bullet; rebuild `order` from the retained nodes (root first, then the
  running nodes in their previous relative order) so every retained node stays iterable via
  `order` and visible to `kill_all` and the `/clear` refusal check.
- Remove only nodes whose status is not running. In practice none can be running here —
  `clear_conversation` refuses while any run, and `/clear` is not in `Command::runs_mid_turn`
  (`rust/fontaine-tui/src/commands.rs:116`), so no model call is in flight to reserve an agent
  mid-clear — but the method still skips running nodes rather than trusting the caller, so a
  racing spawn can never have its node yanked from under it.
- Leave `names`, `next` and `session_spend` untouched.
- Wake `changed` (as `kill_all` does at tree.rs:775) so tree watchers refresh.

### `clear_conversation` (`rust/fontaine-core/src/agent.rs:839`)

After `self.session.clear().await?` succeeds, call `self.state.agents().clear_ended()`. Keeping
the prune after the store clear means a store failure leaves everything as it was. The existing
running/notifications pre-check stays as the user-facing refusal.

### UI (`rust/fontaine-tui/src/ui/commands/context.rs`)

On success, also clear `app.agent_views` and switch back to the main view
(`self.app.view(None)`): the cached per-agent transcripts belong to the conversation that is
gone, like `pending_notices` already cleared there. A `/clear` queued mid-turn can run while
an agent view is on screen (`main_only` is enforced only for commands typed while viewing),
and leaving `viewing` set to a pruned id would route input to the dead agent and let
`shown_mut()` recreate an empty view for it. No other UI state needs it — the list rows and
the panel re-derive from the tree on the next refresh, and stale ids in
`agent_list.dismissed` can never match a row again.

### What is deliberately not touched

- Persisted per-agent logs on disk (`agent-<n>.jsonl`, `docs/architecture.md:186`) and
  `/export` — files survive the clear; only the live tree is pruned.
- `/cost` and `/usage` — they read the session-wide usage ledger
  (`runtime.state.usage_ledger()`), not the tree.
- Conversation `/rewind` — restoring an earlier conversation does not prune; the agents from
  that span are legitimately part of the session the rewind returns to.
- The session budget tally (`session_spend`) — resetting it would let `/clear` evade the
  session spend cap.

## Implementation Steps

1. `rust/fontaine-agents/src/tree.rs`: add `AgentTree::clear_ended` with unit tests (below).
2. `rust/fontaine-core/src/agent.rs`: call it in `clear_conversation` after the session clear.
3. `rust/fontaine-tui/src/ui/commands/context.rs`: clear `app.agent_views` and switch to the
   main view (`app.view(None)`) on success.
4. Docs: `docs/tui.md` Context section — note that past subagents leave the agent list and the
   agents panel (their persisted transcripts stay on disk for `/export`); `docs/tui.md` "The
   agent list and the agent view" — one clause that `/clear` empties the list; qualify the
   "one row per subagent, ended ones included" phrasing in `docs/tui.md`'s agents-panel section
   and in `docs/agents.md` (e.g. "until a `/clear`") so it stays true for agents that ended
   before a clear. In the same "The agent list and the agent view" section, qualify the
   typing-to-resume bullets ("you resumed agent-3") so resuming an ended agent holds only
   until a `/clear` prunes it.
5. `CHANGELOG.md`: add a `### Fixed` entry under `## [Unreleased]` describing that `/clear`
   prunes ended subagents from the agent list and agents panel.

## Files to Modify

- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-tui/src/ui/commands/context.rs`
- `CHANGELOG.md`
- `docs/tui.md`
- `docs/agents.md`

## Trade-offs

- **Prune the tree (chosen)** over filtering rows in the TUI by a clear timestamp: a UI filter
  would fix only the one surface, leave `report_of`/`status_of`-style queries answering for dead
  agents, and let the tree grow for the life of the process. Pruning at the source fixes the
  list, the panel and the query surface together.
- **Keep `names`, `next` and `session_spend`** over a full reset: id/name reuse could collide
  with persisted per-agent logs and the in-process `agent_views` cache, and resetting
  `session_spend` would make `/clear` an escape hatch from the session budget cap.
- **Prune after `session.clear()` succeeds** over before: a store failure then changes nothing,
  instead of leaving the agents gone while the conversation survived.

## Decisions

- 2026-11-02: Keep `names`, `next` and `session_spend` across the prune; only subagent nodes go.
- 2026-11-02: Conversation `/rewind` does not prune the tree; the issue covers `/clear` only.
- 2026-11-02: Pruning deliberately ends resumability of pre-clear subagents: ended agents no
  longer appear in the agents panel, cannot be resumed by typing in their view, and are gone
  from `AgentTree::resume`/`resume_child`; their `logs/agent-<n>.jsonl` transcripts stay on
  disk for `/export`.

## Documentation

`docs/tui.md` (Context section, "The agent list and the agent view" section and the agents-panel
section) and `docs/agents.md` per Implementation Step 4.

## Testing Strategy

- Unit tests in `rust/fontaine-agents/src/tree.rs`: after `clear_ended`, `rows()` is empty and
  the root remains; a node reserved after the clear gets a fresh id (the `next` counter was not
  reset) and no reused name; `session_spend` is retained.
- Unit test for the skip-running branch of `clear_ended` in `rust/fontaine-agents/src/tree.rs`:
  with a running child reserved (as in `a_finished_agent_frees_its_place`), `clear_ended`
  removes the ended nodes but leaves the running node present, running, and still in `order`,
  so a racing spawn never has its node yanked from under it.
- Integration test in `rust/fontaine-core` driving `AgentRuntime`: run a child subagent to an
  ended state, `clear_conversation()`, then assert `rows()` is empty. The existing refusal
  coverage (`rust/fontaine-core/tests/scanner.rs:365`, clear refused while a report waits to
  be delivered) keeps passing unchanged.
- Regression test at the UI tier in `rust/fontaine-tui/tests/agents.rs`: start an agent, end it
  unsuccessfully (so the 30 s `ENDED_LISTED` window would keep it listed), run `/clear`, and
  assert the agent list shows no rows. This is the bug from the issue, which actually happened
  in the wild, so it is pinned at the tier that reproduces the reported behavior rather than
  left to a manual check.

## Open Questions

None.
