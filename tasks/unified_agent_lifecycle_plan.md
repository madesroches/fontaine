# Unified Agent Lifecycle Plan

Issue: [#435](https://github.com/madesroches/fontaine/issues/435), including the maintainer's comment
asking for a comparison of a unified agent model with the narrower "idle for all" fix.

## Overview

Today a subagent is one of several kinds. A plain subagent is gone for other agents once its turn
ends. A teammate goes idle and can be woken. `main` is the lead. Each kind has its own rules, and
several rules exist only because of the split: only `main` starts a teammate, `deliver` tells
`NotRunning` apart from `Idle`, retirement is tied to the board, and `/clear` and `/rewind` are
refused while a teammate is idle. A real session lost about 10 minutes of review work for this
reason. The root spawned a plain reviewer that ended its turn to wait for verdicts. The root's
`send_message` then failed with `agent not running`, and the root had to respawn the reviewer as a
teammate.

This plan replaces the kinds with one lifecycle for every subagent:
**running → idle → (woken → running) … → final**. Any message wakes an idle agent with its
conversation intact. A final end (killed, budget spent, or retired) cannot be undone. The
`teammate` argument goes away. The board, the idle hand-off and the `teammate_idle` /
`task_completed` hooks become team features. They apply to an agent that can work the board, not
to a separate kind. This matches Claude Code 2.1.318, whose SendMessage description says: "names
keep working after an agent completes (a send resumes it from its transcript)".

## Current State

**Node state** (`rust/fontaine-agents/src/tree.rs`)
- `Node` has `teammate: bool` and `idle: bool` (`tree.rs:212-215`).
- `finish` sets `idle = teammate && status == Done` (`tree.rs:752`) and returns it. `conclude`
  releases board tasks unless the node went idle (`spawn.rs:118-122`).
- `resume` clears `idle` and checks the kill, the session budget and `max_parallel`
  (`tree.rs:809-846`). `resume_blocker` / `wake_blocker` / `continue_blocker` say why a resume is
  refused (`tree.rs:766-801`).
- `deliver` (`tree.rs:1097-1135`): when the target is not running, it returns
  `DeliverError::Idle(id)` if the target is idle and reachable, and `DeliverError::NotRunning`
  otherwise.
- `kill_outcome` ends an idle teammate as `Killed::Idle` (`tree.rs:858-899`). `retire_idle` ends
  one as `done` (`tree.rs:903-916`). `idle_ids`, `any_idle`, `is_teammate`,
  `any_teammate_running` and `set_teammate` (`tree.rs:399-427`, `918-927`) serve the teammate split.
- `clear_ended` drops every ended node (`tree.rs:956-975`). An ended node's tasks are not released.

**Already true for every subagent.** A finished child keeps its `ChildHandle`, and with it its
conversation. Its per-agent log persists on disk (`logs/agent-<n>.jsonl`). The human can resume it
by typing into its view (`spawn.rs::resume_child`, the #321 path). An ended node's memory footprint
is therefore already paid for every spawn. Only *agent-sent* wakes are gated on `teammate`.

**Spawn and watch** (`rust/fontaine-agents/src/spawn.rs`)
- `AgentArgs.teammate`. The call refuses it when teams are off or when the caller is not `main`
  (`spawn.rs:561-573`). The schema adds the argument only with teams on (`spawn.rs:521-533`).
- `watch_report` (`spawn.rs:156-200`): for a teammate whose turn ended `Done`, it first runs
  `continue_teammate` (an inbox message, a `teammate_idle` hook block, or a claimable board task,
  capped by `MAX_IDLE_RESUMES`). It then queues the report: to the parent's inbox if the parent is
  running, else to root (`post_to_root`). Then come `conclude`, `resume_missed`, and for teammates
  `wake_for_inbox` and `retire_finished_team`.
- `retire_finished_team` (`spawn.rs:295-309`) retires idle teammates once the board has tasks, all
  of them completed, and no teammate is running. This is the 2026-11-02 (user) Decision.
- `SendMessageTool::call` maps `NotRunning` to the bare `"agent not running"` (`spawn.rs:775`).

**Core and surfaces**
- `/clear` (`fontaine-core/src/agent.rs:868-876`) and rewind (`agent/rewind.rs:91-93`) refuse with
  `ClearError::IdleTeammates` / `RewindError::IdleTeammates` while any agent is idle.
- The AFK stop kills running and idle agents by id (`agent/afk.rs:94`).
- The agent list keeps `running || idle || viewed` rows. A `done` row leaves at once
  (`fontaine-tui/src/agent_list.rs:69`). The panel allows a kill on `running || idle`
  (`agents_panel.rs:251`). Export shows `idle` (`fontaine-core/src/export.rs:92`), and so does
  `job_output` (`tree.rs:1259`).
- Board tools are given to the built-in `worker` (both tools) and `explore` (`board_show` only)
  whenever teams are on, teammate or not (`registry.rs:229-243`). A worker's own `worker` child
  inherits them as a subset.
- Spend: `debit` charges the agent's own budget, cumulatively across all its runs, and the session
  tally (`tree.rs:1002-1015`). Nothing new is needed for a woken agent.

## Design

### The comparison the issue asks for

| | Narrower fix: idle for all, `teammate` kept | Unified model (chosen) |
|---|---|---|
| Wake a finished plain agent | yes | yes |
| `teammate` argument | kept, means "joins the board loop" | removed |
| "only `main` starts a teammate" refusal | kept | gone; team features apply to `main`'s children by definition |
| `NotRunning` vs `Idle` in `deliver` | kept, with `Idle` for every agent | kept (reachability needs it), with no kind check |
| Retirement | still board-coupled for teammates; plain idle agents would need a second rule | one rule for all: final ends cascade, `/clear` and rewind retire |
| `/clear` / `/rewind` refusal while idle | becomes "refused almost always", because every finished agent is idle | removed: they retire idle agents |
| Node flags | `teammate`, `idle` | `retired` (idle is derived) |
| Claude Code parity | partial | matches SendMessage's "a send resumes it from its transcript" |

The narrower fix keeps every rule the split created and adds another, because plain idle agents
need a retirement rule the board one cannot provide. Worse, the `/clear` and `/rewind` refusals
would fire after nearly any subagent use, since nearly every agent would be idle. The unified model
removes those rules instead of adding to them. **Pick: the unified model.**

### Lifecycle

```
            spawn
              │
              ▼
   ┌──────► running ───────────── kill / budget spent ──────► final (killed / budget exhausted)
   │          │
   │  turn ends Done or Failed
   │          ▼
   └── wake ─ idle ──────── kill ──────► final (killed)
              │
              └── retire (parent's final end, /clear, conversation rewind) ──► final (done)
```

- **Idle is derived, not a flag.** An agent is idle when it is not running, its status is `Done` or
  `Failed`, and it is neither retired nor exhausted. That is exactly when `resume_blocker` allows a
  resume, so the human's resume and an agent's wake share one predicate. A `Failed` agent is
  wakeable because the human can already resume one (`resume_blocker` allows it). It keeps showing
  `failed` rather than `idle`, so its end stays visible.
- **`Node`** loses `teammate` and `idle` and gains `retired: bool`. `fn is_idle(&Node)` holds the
  derivation, and `AgentRow.idle`, `job_output` and export read it.
- **`finish`** no longer returns "left idle" for teammates only. It returns
  `node.status == Done && !retired && !killed`, which is the only end that keeps board tasks. A
  `Failed` end still releases them, as today.
- **Final ends:** killed, budget exhausted, retired. A retired agent shows `done`, so `done` now
  always means "final". `resume` refuses a retired node with a new `Refusal::Retired`.
  `resume_blocker` returns "it was retired (…)" with the reason that retired it.

### Retirement

There is no timeout and no cap on idle agents. An idle agent spends nothing and holds no
`max_parallel` slot. Its conversation is already kept today for every ended child. Claude Code keeps
every completed agent resumable for the session. An agent retires only when something makes it
meaningless to resume:

1. **Cascade on a final end.** When an agent is killed, runs out of budget, or retires, every idle
   descendant retires with it, and their tasks are released. Today's kill already stops the running
   descendants. This makes "a kill is final" cover the whole subtree. A parent that merely goes idle
   leaves its idle children alone: a later wake of the parent may need them. That is the reviewer
   scenario, where a woken reviewer messages its verifiers again.
2. **`/clear`.** The `IdleTeammates` refusal goes away. `clear_ended` already drops every ended
   node, idle ones included. It now returns the dropped ids, so `clear_conversation` releases their
   board tasks first. This extends the existing 2026-11-02 decision that pruning ends the
   resumability of pre-clear subagents.
3. **Conversation rewind.** The `IdleTeammates` refusal goes away. A rewind that includes the
   conversation retires every idle agent and releases its tasks. The refusal existed because a woken
   agent would be judged against intent the rewind dropped. Retiring keeps that property without
   refusing, and it also closes the same gap for today's human resume of a plain `done` child.
   Rows stay listed with status `done`, since rewind does not prune the list.
4. **Session end.** Unchanged: the shutdown flag refuses every wake.

`retire_finished_team` and its board-coupled rule are deleted, which supersedes the 2026-11-02 user
Decision. That rule existed to keep a finished team off the agent list. The list rule below now
handles that without making agents unreachable.

`AgentTree` API: `retire_idle(id)` becomes `retire(id) -> Vec<String>`, which retires the node and
its idle descendants under one lock and returns every id it retired so the caller can release
their tasks. `retire_all_idle() -> Vec<String>` serves rewind. A `spawn::retire` wrapper releases
the tasks, as `kill_agent` does for a kill. `kill_outcome`'s `Killed::Idle` arm calls the same
cascade.

### Waking

- **`deliver`** keeps its shape. For a target that is not running, it returns
  `DeliverError::Idle(id)` when `is_idle(target) && reachable`, with no kind check. Otherwise it
  returns `NotRunning`, which now carries the reason (`NotRunning(Option<String>)`, from
  `resume_blocker`) when the sender may reach the target. An unreachable sender still learns
  nothing, as today.
- **`SendMessageTool`** wakes on `Idle` through the existing `wake_idle`. On `NotRunning(Some(why))`
  it errors with `"<name> cannot be woken: <why>; start a new agent with the context it needs"`.
  This is the error-text bullet of the issue, now covering only the final ends.
- **A report to an idle parent wakes it.** In `watch_report`, a parent that is running gets
  `notify_report` as today. A parent that is idle gets the report queued and is woken through the
  same `wake_for_inbox` path. A parent that is final, or root's absence, falls back to
  `post_to_root` as today. This only fires when something woke the child, such as the human typing
  into it or a sibling's message. A subagent still joins its own running children before it goes
  idle (2026-10-30 #326), so a parent never goes idle with children running.
- **`wake_for_inbox`** runs for every agent that goes idle, not only teammates. It closes the race
  where a message arrives as the turn ends.
- The wake text keeps going through `resume_with_data`, so it never becomes guardian intent
  (2026-11-02 Decision, unchanged).

### Team features, layered on

The `teammate` argument, `set_teammate`, `is_teammate` and `any_teammate_running` are removed, along
with the "only the main conversation can start a teammate" refusal and the "has no `board_update`"
result note. **A team member is a direct child of `main` whose toolset holds `board_update`.** With
teams on, that is any `worker` (or custom type naming the tool) that `main` spawns.
`continue_teammate`, renamed `continue_team_member`, runs only for a team member whose turn ended
`Done`:

- The inbox, `teammate_idle` hook and claimable-task hand-off logic is unchanged, and so is the
  `MAX_IDLE_RESUMES` cap.
- A non-member skips straight to idle. An inbox message that arrived as it finished still wakes it,
  through `wake_for_inbox`.

The `task_completed` hook keys on board operations and is untouched. Limiting membership to
`main`'s children keeps Claude Code's "no nested teams" rule as a definition, not a refusal. A
nested worker can still use the board tools it inherits during its own runs, as today, but it is
never handed tasks.

`[agents] teams = false` keeps its meaning for the board: no board tools, no hand-off, no team
hooks. Idling and waking no longer depend on it.

### Text the model and the human read

- The idle report line changes from the teammate-only wording (`spawn.rs:179`) to one line for every
  agent: `Agent <name> (<id>, <type>) finished its turn and is idle; send_message wakes it with its
  conversation intact.` For `Failed` and other ends the existing `finished: <status>` line stays.
- In the `agent` description, "returns a report" gains: "When its turn ends it goes idle and keeps
  its conversation: `send_message` to its name wakes it for follow-up work, so prefer messaging a
  finished agent over starting a new one for the same task." The `teammate` schema entry is
  removed.
- In the `send_message` description, "A teammate that is idle is woken by it" becomes "An agent
  whose turn has ended is woken by it with its conversation intact, unless it was killed, ran out
  of budget or was retired".

### Surfaces

- **Agent list** (`agent_list.rs:69`): `running || viewed`, plus idle rows that own an in-progress
  board task. Without that condition every finished agent would stay listed. An idle row without
  tasks leaves at once, as a `done` row does today. The `/agents` panel keeps listing every row with
  `idle` / `done` / … and the kill key on idle rows. `AgentRow` gains `owns_task: bool`, filled by
  `ui/mod.rs` from the board.
- **Status bar, export, `job_output`**: they read `AgentRow.idle` / the derived state, and do not
  change.
- **AFK stop**: still kills `running_ids() ∪ idle_ids()`. `idle_ids` now covers every idle agent.

### Open points from the issue, settled

- **Memory**: no new cost. Every ended child already keeps its handle and on-disk log.
- **Spend**: a woken agent debits its own cumulative budget and the session tally (`debit`), as a
  teammate does. Fontaine has no per-subtree budget to inherit.
- **One-shot reliance**: `job_output` treats idle as ended (`AgentState::Ended { status: "idle" }`),
  so a parent waiting on a child still returns when the child's turn ends. Headless `settle` waits on
  `any_running` only, so idle agents never hold a run open. Both behave as for teammates today.
- **Nesting**: idle children of subagents are handled by the cascade rule above. Nothing needs an
  "only main" refusal.

## Implementation Steps

1. **Tree state** (`fontaine-agents/src/tree.rs`)
   - Replace `Node.teammate` / `Node.idle` with `retired`. Add `fn is_idle(&Node)`. Remove
     `set_teammate`, `is_teammate` and `any_teammate_running`. `is_idle`, `any_idle` and `idle_ids`
     use the derivation.
   - `finish`: return "kept its tasks" (`Done && !killed && !retired`).
   - `resume`: refuse `retired` (`Refusal::Retired(String)`). `resume_blocker`: add the retired
     reason. `spawn::describe` gains the arm.
   - `deliver`: drop the kind check. `DeliverError::NotRunning(Option<String>)` carries the reason
     for reachable senders.
   - `retire(id)` with cascade and `retire_all_idle()`, both returning the retired ids.
     `kill_outcome` cascades over idle descendants. `clear_ended` returns the dropped idle ids.
   - `rows()` / `AgentState` use `is_idle`. Delete `retire_idle`.
2. **Spawn and messaging** (`fontaine-agents/src/spawn.rs`)
   - Remove `AgentArgs.teammate`, its schema entry, its refusals and the board note.
   - `watch_report`: membership test (`parent == ROOT_AGENT && tools_of(id) holds board_update`)
     replaces `is_teammate`. Use the new idle report line. A report to an idle parent queues and
     wakes it. `wake_for_inbox` runs for every idle end. Delete `retire_finished_team`.
   - Rename `continue_teammate` to `continue_team_member`.
   - `kill_agent`: release the tasks of every id the cascade ended. Add `pub fn retire` and
     `retire_all_idle` wrappers that release tasks.
   - `SendMessageTool`: new error text and description. Update the `agent` description.
3. **Core** (`fontaine-core`)
   - `agent.rs::clear_conversation`: drop the `any_idle` refusal and release the tasks of the ids
     `clear_ended` returns.
   - `agent/rewind.rs`: drop the refusal. On a conversation rewind, call `spawn::retire_all_idle`.
   - `session.rs` / `rewind.rs`: delete the `IdleTeammates` variants.
4. **TUI** (`fontaine-tui`)
   - `AgentRow.owns_task` (set in `fontaine-tui/src/ui/mod.rs` from the board).
   - `agent_list.rs` listing rule.
   - Test renames in `agent_list.rs`, `agents_panel.rs` and `screen.rs`.
5. **Docs** (see Documentation).
6. **Plan Decisions** in `tasks/fontaine_harness_plan.md`, `### Agents` (end of subsection):
   - 2026-11-05 (supersedes the 2026-11-02 "teammate is an ordinary subagent with an idle state …
     only `main` starts a teammate" line): every subagent whose turn ends `done` or `failed` goes
     idle and keeps its conversation, and any reachable agent's message, a report, or the human
     wakes it. There is no `teammate` argument. Team features apply to `main`'s children that hold
     `board_update`. Claude Code parity: its SendMessage resumes a completed agent from its
     transcript (#435).
   - 2026-11-05 (supersedes the 2026-11-02 (user) board-retirement line): an idle agent retires only
     when its ancestor ends for good, on `/clear`, or on a conversation rewind. There is no
     board-coupled retirement, timeout or cap. The agent list hides idle rows that own no task (#435).
   - 2026-11-05: `/clear` and a conversation rewind retire idle agents instead of refusing (#435).

## Files to Modify

- `rust/fontaine-agents/src/tree.rs`
- `rust/fontaine-agents/src/spawn.rs`
- `rust/fontaine-core/src/agent.rs`, `rust/fontaine-core/src/agent/rewind.rs`,
  `rust/fontaine-core/src/session.rs`
- `rust/fontaine-tui/src/agent_list.rs`, `rust/fontaine-tui/src/ui/mod.rs`,
  `rust/fontaine-agents/src/tree.rs` (`AgentRow`), `rust/fontaine-tui/src/agents_panel.rs`,
  `rust/fontaine-tui/src/screen.rs` (tests)
- `rust/fontaine-core/tests/teams.rs`, `rust/fontaine-core/tests/agent_resume.rs`
- `docs/agents.md`, `docs/hooks.md`, `docs/tui.md`, `docs/config.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Narrower fix (idle for all, `teammate` kept).** Rejected; see the comparison table. It keeps
  every split-driven rule and makes the `/clear` and `/rewind` refusals fire nearly always.
- **Keep `teammate: true` as an opt-in for the board loop only.** It would avoid handing board
  tasks to a `worker` that `main` started for something else. Rejected: the board has tasks only
  when the lead put them there, a worker with `board_update` exists to take them, the hand-off is
  capped, and the flag would bring back the "spawn it as X" rule the model has to learn by failing.
- **Idle timeout or cap.** Rejected. Idle costs no spend and no slot, its conversation is already
  kept today, and Claude Code has neither. A timeout would bring back the failure that #435 reports,
  just later.
- **Only `Done` is idle (`Failed` final).** Rejected: the human can already resume a failed child,
  and two predicates for "can this be resumed" are what this plan removes.
- **Report to an idle parent goes to root (today's fallback).** Rejected: it would skip the agent
  that asked for the work, and "a message wakes an idle agent" would have an exception.
- **Rewind prunes like `/clear`.** Rejected: rewind keeps the list today, and retiring is enough
  for the intent property.

## Documentation

- `docs/agents.md`:
  - The `agent` arguments table: remove `teammate`.
  - "Messages between agents": the wake rule replaces "a teammate that is idle is the one
    exception".
  - A new short "Agent lifecycle" section: running / idle / final, the retirement triggers, and
    `Failed` being wakeable.
  - "Agent teams": membership as a direct child of `main` holding `board_update`; delete "Only
    `main` can start a teammate", "Idle instead of done", and the board-retirement and
    `/clear`/`/rewind` refusal text.
  - The "Watching and stopping" paragraph: `idle` vs `done`.
- `docs/hooks.md`: `teammate_idle` fires for a team member going idle (new definition).
- `docs/tui.md`: the agent-list rule for idle rows.
- `docs/config.md`: `[agents] teams` turns the board features on and off. It no longer controls
  idling.
- `CHANGELOG.md`: "Any finished subagent can be woken by `send_message`; the `teammate` argument
  is removed."

## Testing Strategy

Unit tests (`tree.rs`, `spawn.rs` test modules), against constructed trees:
- `is_idle` derivation: `Done` and `Failed` are idle. Killed, exhausted and retired are not.
  Running is not.
- `deliver` to a finished plain agent returns `Idle` for a reachable sender and `NotRunning(None)`
  for an unreachable one. A killed or retired target returns `NotRunning(Some(reason))`.
- `retire` cascades over idle descendants only, returns their ids, and leaves running nodes and an
  idle parent's children alone when the parent merely idles. `kill_outcome` on an idle node with
  idle children retires the children.
- `resume` refuses a retired node with `Refusal::Retired`.
- `finish` keeps tasks only for a `Done` end. `clear_ended` returns the pruned idle ids.
- `AgentArgs` with `teammate` is rejected as an unknown field. The schema has no `teammate`.
- `agent_list::rows`: an idle row is listed only with `owns_task` or when viewed.

Integration tests through the core with `rig_core::test_utils` scripted models
(`fontaine-core/tests/agent_resume.rs`, `teams.rs`):
- **Regression for the bug seen in the session (#435):** root spawns a plain `worker`, which ends
  its turn. Root's `send_message` returns `queued`, the worker takes a further turn whose request
  contains its earlier history and the sender-marked message, and the message is not recorded as a
  user message.
- A child woken by the human while its subagent parent is idle: the child's report wakes the parent
  with the report as its prompt.
- `/clear` with an idle agent owning an in-progress task succeeds, prunes it and releases the task.
  A conversation rewind retires the idle agent (status `done`), and a later `send_message` to it
  errors with the retired reason. This replaces `clear_and_rewind_are_refused_while_a_teammate_is_idle`.
- The hand-off note goes to `main`'s idle `worker` when a task is claimable, but not to a `worker`
  spawned by a subagent. The existing cap test keeps passing without `teammate: true`.
- With `teams = false`, a finished worker still idles and is woken by a message, and has no board
  tools.
- Existing teams tests are rewritten to spawn without `teammate`. Their assertions on idle, wake
  and non-recorded intent stay.

## Open Questions

- Agents from an earlier process of a `--resume`d session stay unaddressable, though their logs
  persist. Claude Code resumes from the transcript. Out of scope here; worth a follow-up issue if
  the maintainer wants it.
