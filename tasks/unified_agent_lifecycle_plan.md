# Unified Agent Lifecycle Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/435

Issue: [#435](https://github.com/madesroches/fontaine/issues/435), including the maintainer's comment
asking for a comparison of a unified agent model with the narrower "idle for all" fix.

## Overview

Today a subagent is one of several kinds. A plain subagent is gone for other agents once its turn
ends. A teammate goes idle and can be woken. `main` is the lead of the session's one team. Each kind
has its own rules, and several rules exist only because of the split: only `main` starts a
teammate, `deliver` tells `NotRunning` apart from `Idle`, retirement is tied to the board, and
`/clear` and `/rewind` are refused while a teammate is idle. A real session lost about 10 minutes of
review work for this reason. The root spawned a plain reviewer that ended its turn to wait for
verdicts. The root's `send_message` then failed with `agent not running`, and the root had to
respawn the reviewer as a teammate.

This plan makes every subagent the same thing, with one lifecycle:
**running → idle → (woken → running) … → final**. Any message wakes an idle agent with its
conversation intact (so do a report and the owner's board reminder), and a final end (killed, budget spent, or retired) cannot be undone. This
matches Claude Code 2.1.318, whose SendMessage description says: "names keep working after an agent
completes (a send resumes it from its transcript)".

Teams stop being a kind of agent. The `teammate` argument goes away, and so does the automatic
hand-off of board tasks to idle agents. Waking is for follow-up on an agent's own work, and new work
goes to a fresh agent with a clean context. The board becomes a feature of whichever agent
decomposes work: **any agent can own a board**, its children work it, and when tasks are claimable
and agent slots are free, the owner is reminded to spawn fresh agents for them.

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
- `kill_outcome` ends an idle teammate as `Killed::Idle` (`tree.rs:858-899`). `retire_idle` ends one
  as `done` (`tree.rs:903-916`). `is_teammate`, `any_teammate_running` and `set_teammate`
  (`tree.rs:399-427`) serve the split.
- `clear_ended` drops every ended node (`tree.rs:956-975`). An ended node's tasks are not released.

**Already true for every subagent.** A finished child keeps its `ChildHandle`, and with it its
conversation. Its per-agent log persists on disk (`logs/agent-<n>.jsonl`). The human can resume it
by typing into its view (`spawn.rs::resume_child`, the #321 path). The memory for an ended node is
therefore already paid for every spawn. Only *agent-sent* wakes are gated on `teammate`.

**Spawn and watch** (`rust/fontaine-agents/src/spawn.rs`)
- `AgentArgs.teammate`. The call refuses it when teams are off or when the caller is not `main`
  (`spawn.rs:561-573`).
- `watch_report` (`spawn.rs:156-200`): for a teammate whose turn ended `Done`, it first runs
  `continue_teammate` (`spawn.rs:206-283`), which resumes on an inbox message, a `teammate_idle`
  hook block, or a claimable board task (the idle hand-off), capped by
  `tasks::MAX_IDLE_RESUMES`. It then queues the report: to the parent's inbox if the parent is
  running, else to root (`post_to_root`). Then come `conclude`, `resume_missed`, and for teammates
  `wake_for_inbox` and `retire_finished_team` (`spawn.rs:295-309`, the 2026-11-02 (user) Decision).
- `SendMessageTool::call` maps `NotRunning` to the bare `"agent not running"` (`spawn.rs:775`).

**Board** (`rust/fontaine-agents/src/tasks.rs`, `tools.rs`; `fontaine-core/src/tasks.rs`)
- One `TaskBoard` per session inside `Teams { board, gate, streaks }`, installed on the tree by
  `fontaine-core/src/agent/setup.rs:534-553`. It is persisted beside `todos.json` (`TaskStore`) and
  restored without owners. Ids are `t<n>` from the board's counter.
- `owned_by` lets the task's owner or `ROOT_AGENT` (the lead) change an in-progress task
  (`tasks.rs:296-312`). `release_owned_by` frees an ended agent's tasks.
- `board_show` / `board_update` (`tools.rs`) act on that one board. The built-in `worker` gets both
  tools and `explore` gets `board_show` whenever teams are on (`registry.rs:229-243`).
- `Teams::allow_idle_resume` counts the hand-off and hook resumes per agent and resets when the
  board changes.

**Core and surfaces**
- `/clear` (`fontaine-core/src/agent.rs:868-876`) and rewind (`agent/rewind.rs:91-93`) refuse with
  `IdleTeammates` while any agent is idle.
- The AFK stop kills running and idle agents by id (`agent/afk.rs:94`).
- The agent list keeps `running || idle || viewed` rows. A `done` row leaves at once
  (`fontaine-tui/src/agent_list.rs:69`). The `/agents` panel shows every row and "the team's task
  board" (`fontaine-tui/src/ui/mod.rs:368`, `agents_panel.rs`).
- Spend: `debit` charges the agent's own budget, cumulatively across all its runs, and the session
  tally (`tree.rs:1002-1015`).

## Design

### The comparison the issue asks for

| | Narrower fix: idle for all, `teammate` kept | Unified model (chosen) |
|---|---|---|
| Wake a finished plain agent | yes | yes |
| `teammate` argument | kept | removed |
| "only `main` starts a teammate" | kept | gone: any agent can own a board and spawn its workers |
| `NotRunning` vs `Idle` in `deliver` | kept, with `Idle` for every agent | kept (reachability needs it), with no kind check |
| Retirement | board-coupled for teammates, plus a second rule for plain agents | one rule: final ends cascade, `/clear` and rewind retire |
| `/clear` / `/rewind` while idle | refused almost always, since every finished agent is idle | allowed: they retire idle agents |
| New board work | handed to idle teammates, mixing contexts | goes to fresh agents the board's owner spawns |
| Node flags | `teammate`, `idle` | `retired` (idle is derived) |
| Claude Code parity | partial | matches SendMessage's "a send resumes it from its transcript" |

The narrower fix keeps every rule the split created and adds a retirement rule for plain idle agents.
Worse, the `/clear` and `/rewind` refusals would fire after nearly any subagent use. The unified model
removes those rules instead. **Pick: the unified model.**

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
              └── retire (ancestor's final end, /clear, conversation rewind) ──► final (done)
```

- **Idle is derived, not a flag.** An agent is idle when it is not running, its status is `Done` or
  `Failed`, and it is neither retired nor exhausted. That is exactly when `resume_blocker` allows a
  resume, so the human's resume and an agent's wake share one predicate. A `Failed` agent keeps
  showing `failed`, so its end stays visible, but it can be woken, as the human can already resume
  one. `agent_state` and export print "idle" only for a `Done` idle node, as `row_label` does; a
  `Failed` node prints `failed`.
- **`Node`** loses `teammate` and `idle` and gains `retired: Option<String>`, the cause (an ancestor's final end, `/clear` or a rewind). `fn is_idle(&Node)` holds the
  derivation. `AgentRow.idle`, `job_output` and export read it.
- **`finish`** returns whether the agent keeps its board tasks: `Done && !killed && retired.is_none()`. A
  `Failed` end still releases them, as today.
- **Final ends:** killed, budget exhausted, retired. A retired agent shows `done`, so `done` now
  always means "final"; export maps a ledger agent's `retired` end to `done` (`export.rs`). `resume` refuses a retired node with a new `Refusal::Retired(String)`, and
  `resume_blocker` reads `retired` and returns "it was retired (<why>)".

### Retirement

There is no timeout and no cap on idle agents. An agent retires only when resuming it no longer makes sense:

1. **Cascade on a final end.** When an agent is killed, runs out of budget, or retires, every idle
   descendant retires with it. Their tasks are released, and the boards they own are dropped (see
   below). This makes "a kill is final" cover the
   whole subtree. A parent that merely goes idle leaves its idle children alone, because a later wake
   of the parent may need them. That is the reviewer scenario. A `Failed` end is idle, so it leaves
   its idle descendants alone; `finish` still kills its running descendants, as today, since the
   parent is no longer running to join them. The cascade walks the whole subtree at every depth,
   killing running descendants and retiring idle ones, including a running grandchild under an
   idle child, and including in `kill_outcome`'s idle arm (today only its running arm collects
   descendants, and only through running nodes). It runs on every path that makes a node final,
   including descendants that `finish` or the running arm of `kill_outcome` marks `Killed`, and a
   budget-exhausted end through `conclude`. `finish` and `conclude` return the ids their cascade
   retired, and `conclude` does what `spawn::retire` does for them: release tasks, drop boards and
   append `finished: retired`.
2. **`/clear`.** The `IdleTeammates` refusal goes away. `clear_ended` already drops every ended node,
   idle ones included. It now returns the dropped ids, split into the idle (`Done`) ones and the
   rest, so `clear_conversation` releases all their tasks and drops their boards first, and appends
   `finished: retired` only for the idle ones (a killed or failed agent keeps its own end). This is the existing `clear_ended` pruning (see `docs/tui.md`),
   which ends the resumability of pre-clear subagents.
3. **Conversation rewind.** The `IdleTeammates` refusal goes away. A rewind that includes the
   conversation retires every idle agent. The refusal existed because a woken agent would be judged
   against intent the rewind dropped. Retiring keeps that property without refusing, and it closes
   the same gap for today's human resume of a `done` child. Nodes are kept, not pruned, and show as `done` in the `/agents` panel.
4. **Session end.** Unchanged: the shutdown flag refuses every wake.

`retire_finished_team` and the board-coupled retirement are deleted. This supersedes the 2026-11-02
user Decision, whose aim (a finished team should not linger in the list) is now served by the agent
list rule below without making agents unreachable.

`AgentTree` API: `retire_idle(id)` becomes `retire(id, why) -> Vec<String>`, which retires the node and
its idle descendants under one lock and returns every id it retired. `retire_all_idle(why) ->
Vec<String>` serves rewind. A `spawn::retire` wrapper releases tasks and drops boards for those ids,
as `kill_agent` does for a kill. `kill_outcome`'s idle arm uses the same cascade.

### Waking

- **`deliver`** keeps its shape. For a target that is not running, it returns
  `DeliverError::Idle(id)` when `is_idle(target) && reachable`, with no kind check. Otherwise it
  returns `NotRunning(Option<String>)`, which carries the `resume_blocker` reason when the sender
  may reach the target. An unreachable sender still learns nothing.
- **`SendMessageTool`** wakes on `Idle` through the existing `wake_idle`. `wake_idle` (shared with
  `send_message`) releases tasks only on a final refusal; on a transient refusal (`Parallel`,
  `SessionBudget`) `send_message` returns its existing "cannot be woken" error and keeps the tasks.
  On `NotRunning(Some(why))` it errors with `"<name> cannot be woken: <why>; start a new agent with the context it needs"`.
- **A report to an idle parent wakes it.** In `watch_report`, a running parent gets `notify_report`
  as today. An idle parent gets the report queued and is woken through `wake_for_inbox`. A final
  parent falls back to `post_to_root` as today, and so does a final refusal of the report wake: the
  taken `Report` notes are forwarded to root with `post_to_root`, as `watch_report` does for a final parent. `AgentTree::push` (and so `notify_report`) also
  queues to an idle, wakeable node, so the report is in the parent's inbox before
  `wake_for_inbox(parent)` runs. The parent is woken only after the child's `conclude` has freed the
  child's slot. A transient refusal (`Refusal::Parallel`, `SessionBudget`) requeues every taken note,
  reports included, as the `AlreadyRunning` arm does, and does not release tasks; the wake is
  retried when the next slot frees, from the same slot-free hook the owner reminder uses. Only a
  final refusal releases the node's tasks. This only fires when something woke the child: the
  human typing into it, or a sibling's message. A subagent still joins its own running children
  before it goes idle (2026-10-30 #326).
- **`wake_for_inbox`** runs for every agent that goes idle, closing the race where a message arrives
  as the turn ends.
- Wake text keeps going through `resume_with_data`, so it never becomes guardian intent (2026-11-02
  Decision, unchanged).
- **Nothing else wakes an agent** except the owner reminder (below) for an idle owner. The idle hand-off of claimable tasks is removed (see below).

### Boards per agent

`Teams { board, … }` becomes `Teams { boards: Boards, gate, streaks }`. `Boards` holds a map from
owner id to `Arc<TaskBoard>` and one session-wide id counter, so a task id (`t<n>`) is unique across
every board.

- **Ownership.** An agent's board is created the first time it runs `board_update create`. The
  creator is the owner. `owned_by`'s `ROOT_AGENT` exception becomes "the board's owner", so an owner
  can change, complete or release any task on its own board, as `main` can today.
- **Visibility.** An agent sees its own board and its parent's. It owns its own, and works its
  parent's by claiming and completing tasks. A sibling's or grandparent's board is out of reach: an agent works only the
  board it was spawned to serve. `main` has no parent, so it sees only its own board.
- **Routing, with no new argument.** `create` always goes to the caller's own board. `claim`,
  `complete` and `release` take a task id and act on whichever of the caller's own and parent's
  boards holds it. Any other id is `invalid_args` ("no task <id> on your board or your parent's").
  `board_show` lists both boards, each labelled with its owner's name.
- **Lifetime and persistence.** `main`'s board is persisted and restored exactly as the session's
  board is today (`fontaine-core/src/tasks.rs`). A subagent's board is in memory only, matching the
  rule that only the root's todo list is saved. Because ids are shared, `main`'s snapshot is saved
  with the shared counter's `next` whenever any board mints a task, so ids minted on subagent boards
  are not re-minted after `--resume`. It is dropped when its owner ends for good or is
  pruned by `/clear`. An idle owner keeps its board.
- **Releasing tasks.** `release_owned_by(agent)` runs on both the agent's own board and its
  parent's board (`main` has no parent board). `/clear` and the cascade use the same helper.
- **Tools.** Teams on means the board tools are registered and `worker` / `explore` get them, as
  today. A `worker` spawned by a subagent inherits them as a subset, as today, and can now own a
  board of its own. Depth is bounded by `max_depth`.

### Working the board down: fresh agents, reminded owner

The idle hand-off (`continue_teammate`'s claimable-task branch, the "task is available" note) is
removed. A claimable task goes to a fresh agent with a clean context, and an idle agent is never
reused for unrelated work. When the owner judges that a task builds on what an idle agent already
knows, it says so with `send_message`.

The owner spawns, prompted by one harness note:

`[Harness note, not the user's words: 3 tasks on your board are claimable and 2 agent slots are free; start agents for them with agent, or do them yourself.]`

- **When the note is sent.** It goes to the owner of a board with claimable tasks (pending, unowned,
  unblocked) when `max_parallel` has free slots. The check runs at two points:
  - the owner's turn end: `AgentRuntime::goal_turn_ended` (`fontaine-core/src/agent/goal.rs`, the
    root turn-end hook shared by headless and the TUI) for `main`, run only after a real model turn
    (not a stop-before-call or a budget stop), or `watch_report` for a subagent before it is
    finished, which keeps it running for one more turn;
  - a slot freeing up or a task on its board becoming claimable while the owner is idle or between
    turns.

  The second check runs from the shared task-release helper that every path uses (`conclude`,
  `kill_agent` including its idle arm, `spawn::retire` / `retire_all_idle`, and `/clear`), and from
  `board_update`, so no release path skips it.
- **Delivery.** The note goes through the existing paths: `post_to_root` for `main`, the inbox for a
  running owner, and a `wake_idle` resume for an idle owner. It is not recorded as a user message.
- **Deduplication.** Each owner keeps the last `(board mutation count, free slots)` it was reminded
  at, and gets no new note until that pair changes. The free-slot count shown in the note is
  `max_parallel - running_children`, what `reserve_named` (`tree.rs:600`) allows. Only the dedup key
  is owner-adjusted, counting a running owner's own slot as free (`+1`), and an owner's own idle end
  does not trigger its own reminder, so the key is the same at its turn end and at its idle end.
  An owner that ignores a note is not nagged, and no loop sustains itself. This replaces `Teams::allow_idle_resume`'s role for the hand-off.
- **Who spawns.** Always the owner, through an ordinary `agent` call. Every spawn passes the floor and
  the `spawn` tier (asks in `manual` / `acceptEdits`). The owner picks the type, model and prompt.
  Board text is agent-written, untrusted data, never a spawn prompt on its own. The spawned agent
  claims its task with `board_update`.

### `teammate_idle` and `task_completed`

- `teammate_idle` becomes a quality gate on unfinished work. It fires when an agent goes idle (turn
  ended `Done`) while it owns an in-progress task on its parent's board. A block resumes the agent
  on the hook's reason, and the resume is capped by `MAX_IDLE_RESUMES` with no board change, as
  today. The resume streak resets on mutations of the parent's board, the one holding the agent's
  in-progress task. It never hands out new work. The hook keeps its Claude Code name. Its input names the agent
  as today.
- `task_completed` gates completion on every board, unchanged.

`[agents] teams = false` removes the board tools, boards, reminders and both team hooks. Idling and
waking do not depend on it.

### Text the model and the human read

- Idle report line, for every agent whose turn ended `Done`: `Agent <name> (<id>, <type>) finished
  its turn and is idle; send_message wakes it with its conversation intact.` Other ends, `Failed`
  included, keep `finished: <status>`.
- `agent` description, added after "returns a report": "When its turn ends it goes idle and keeps
  its conversation: `send_message` wakes it for follow-up on the same work. Start a new agent for
  new work." The `teammate` schema entry is removed.
- `send_message` description: "An agent whose turn has ended is woken by it with its conversation
  intact, unless it was killed, ran out of budget or was retired."
- `board_show` / `board_update` descriptions: "your board" (tasks you created) and "your parent's
  board" (tasks you can claim), and the routing rule.

### Surfaces

- **Agent list** (`agent_list.rs:69`): `running || viewed`, plus idle rows that own an in-progress
  task. An idle row without one leaves at once, as a `done` row does today. `ListRow`
  (`agent_list.rs:25-38`) gains `owns_task: bool`, filled in `fontaine-tui/src/ui/mod.rs` from the boards.
  The `ENDED_LISTED` retention arm for `Failed`, `Killed` and `BudgetExhausted` rows is unchanged,
  so a `Failed` idle row stays listed for that window.
- **`/agents` panel:** every row with `idle` / `done` / … and the kill key on idle rows, as today.
  It lists each non-empty board under its owner's name instead of the single team board
  (`ui/mod.rs:368` builds `BoardRow`s per owner).
- **Status bar, export, `job_output`:** read the derived idle state; no other change.
- **AFK stop:** `handle_afk_stop` (`fontaine-core/src/agent/afk.rs`) kills `running_ids()` and calls
  `spawn::retire_all_idle("afk stop")` for idle agents, so finished agents are recorded `retired`,
  not `killed`.

### Agents across `--resume`

An agent is a conversation plus a few facts about it, not a process. Its log (`logs/agent-<n>.jsonl`)
persists, and its id and name are already stable across `--resume` (2026-11-21 #300). A resumed
session therefore brings back every agent that had not ended for good, as idle and wakeable, as
Claude Code resumes an agent from its transcript.

- **What the ledger must hold.** The `spawned` record gains `model` (the concrete resolved reference that `tree.set_model` stores, not `$main`, so the agent revives on the exact model it used).
  Retiring appends `finished` with status `retired`. That covers the cascade, `/clear` and rewind,
  so a resumed session knows which agents are final without replaying those rules. `/clear` appends `retired`
  for each pruned node that was idle (`Done`), so pruned agents stay gone on rebuild. A pruned
  killed or failed node keeps its own `finished` end: a `retired` record would overwrite it and
  export would show it as `done`.
- **Rebuild at startup** (`agent/setup.rs`). It is split in two, because in `AgentRuntime::new`
  the registry, root posture (`session_state` / `set_root_mode`) and root tools (`wire_root_tree`,
  filled by `build_agent`) do not exist yet, and `TaskBoard::restore` (`setup.rs:546`, inside
  gates / `start_teams`) runs before `build_agent`. Before gates, the set of revivable ids is
  computed from the ledger and registry alone and passed to `TaskBoard::restore`.
  `AgentTree::rebuild` runs after `build_agent`, once root tools and posture exist. The ledger
  view's `agents` are walked in spawn order. Each agent whose last `finished` status is `done` or `failed` becomes an
  idle node with no handle:
  - Its type comes from the current registry.
  - Its tools are re-derived as the type's toolset cut to the parent's, without `agent` at
    `max_depth`, as at spawn.
  - Its mode is `own_mode` against the parent's posture, so it is never looser than the parent, and
    the session itself resumes in the default mode (P6).
  - Its depth comes from the parent chain.

  An agent whose type is no longer in the registry is skipped, and so are its descendants. An
  agent whose parent is not rebuilt (final) is not rebuilt either. That is
  the cascade rule applied at load. Killed, budget-exhausted, retired and still-running agents (the
  previous process's `kill_all` ended those as `killed`) are not rebuilt.
- **Lazy handle.** A rebuilt node gets its `ChildHandle` on its first wake, not at startup, so a
  resume opens no model clients for agents nobody talks to. `ChildRunner` gains
  `revive(RevivalRequest) -> BoxFuture<Result<Arc<dyn ChildHandle>, String>>`, async like `spawn`
  because `CoreChildRunner` awaits `model_for`. The request is a `SpawnRequest` without a prompt.
  `CoreChildRunner` builds the rig agent over the existing per-agent log without launching a run.
  `wake_idle` and `message_agent` stay sync: for a handle-less node they claim it with
  `tree.resume` as usual, then spawn a task that awaits `revive`, installs the handle and starts
  the run. A missing log or an unavailable model ends that run as `Failed`: the spawned task builds
  a `Failed` `ChildReport` with the reason, queues the same parent/root note `watch_report` builds,
  and calls `conclude` (through a `pub` wrapper if needed), which also releases the node's tasks.
  Steering mail queued during the pending revive is dropped, and the failure text names it.
  `rig_memory::load` returns an empty conversation for a missing file, so `CoreChildRunner::revive`
  checks that the agent's log exists and returns an error otherwise.
- **Model.** The agent revives on the recorded model. A record from before this change has none,
  and revives on its type's model. A type from a project agent file always runs on the session
  model, as at spawn.
- **Guardian intent.** The root's human messages and the agent-scoped ones (`Record::UserMessage {
  agent }`) are replayed from the ledger as today, so a revived agent is judged against the same
  intent it had.
- **Budgets.** The tree's spend tallies are in memory, so a revived agent's per-agent spend, like the
  session tally, starts over in the new process. The per-agent budgets are opt-in, and this matches
  the session budget.
- **Board.** `main`'s restored board keeps an in-progress task owned when its owner is rebuilt idle,
  instead of resetting it. `TaskBoard::restore` takes the revivable ids computed before gates. A subagent's board is not
  persisted, so a revived subagent owner starts with an empty board.
- **Surfaces.** Rebuilt agents show in the `/agents` panel as `idle`, and `send_message`,
  `job_output` and typing into their view reach them by name or id. A rebuilt agent's TUI view is
  filled only from live run events (`fontaine-tui/src/app.rs`, `view_of`), so it shows only turns
  from after the resume.

### Open points from the issue, settled

- **Spend:** There is no per-subtree budget.
- **One-shot reliance:** `job_output` treats a `Done` idle agent as ended (`AgentState::Ended { status: "idle" }`; a `Failed` one reports `failed`),
  so a parent waiting on a child returns when the child's turn ends. Headless `settle` waits on
  `any_running` only, so idle agents never hold a run open.
- **Nesting:** idle descendants follow the cascade rule. Boards nest through ownership, bounded by
  `max_depth` and `max_parallel`.

## Implementation Steps

1. **Tree state** (`fontaine-agents/src/tree.rs`)
   - Replace `Node.teammate` / `Node.idle` with `retired`, and add `fn is_idle(&Node)`. Remove
     `set_teammate`, `is_teammate` and `any_teammate_running`. `is_idle`, `any_idle` and `idle_ids`
     use the derivation.
   - `finish`: return "keeps its tasks". `resume`: `Refusal::Retired`. `resume_blocker`: the retired
     reason. `spawn::describe` gains the arm.
   - `deliver`: drop the kind check, and make it `DeliverError::NotRunning(Option<String>)`.
   - Add `retire(id, why)` (cascade) and `retire_all_idle(why)`, both returning ids. `kill_outcome` cascades
     over the whole subtree, in its idle arm too. `finish` also returns the ids its cascade
     retired. `clear_ended` returns the pruned ids, with the idle ones marked. Delete `retire_idle`.
   - `rows()` / `AgentState` use `is_idle`. `owns_task` is not on `AgentRow`; the TUI computes it (step 5).
2. **Boards** (`fontaine-agents/src/tasks.rs`, `tools.rs`)
   - Add `Boards` (owner → `TaskBoard`, shared id counter, a store only for `ROOT_AGENT`'s board).
     `TaskBoard` takes its ids from the shared counter. `restore` seeds the counter from `main`'s
     snapshot, and the snapshot is saved with the counter's `next` on any board's mint.
   - `owned_by`: the board owner replaces `ROOT_AGENT`.
   - Add `Boards::own(agent)`, `Boards::find(agent, parent, task_id)`, `Boards::drop_owner(agent)`
     and `Boards::release_owned_by(agent, parent)`.
   - Add a listing accessor, `Boards::all() -> Vec<(owner, Vec<Task>)>`, and
     `Boards::owns_in_progress(agent) -> bool`, which `ui/mod.rs` uses (it reads `teams.board.list()`
     today) for `BoardRow`s and `owns_task`.
   - Add a reminder check, `Boards::reminder(owner, free_slots) -> Option<String>`, with the per-owner
     dedup pair.
   - `board_show` / `board_update`: the routing and the new descriptions.
   - Remove the hand-off from `Teams` (`allow_idle_resume` stays only for the hook, and its streak
     resets on mutations of the parent's board).
3. **Spawn and messaging** (`fontaine-agents/src/spawn.rs`)
   - Remove `AgentArgs.teammate`, its schema entry, its refusals and the board note.
   - `watch_report`:
     - For any `Done` end, first resume on queued inbox mail, then on a `teammate_idle` block when the
       agent owns an in-progress parent-board task, then on the owner reminder.
     - Use the new idle report line. A report to an idle parent queues and wakes it, and
       `wake_for_inbox` runs for every idle end. `push` / `notify_report` queue to an idle node, the
       wake happens after `conclude` frees the child's slot, and a transient refusal requeues every
       taken note (reports included) without releasing tasks, retried from the slot-free hook. `wake_idle` releases tasks only on a final refusal.
     - Delete `retire_finished_team` and the claimable-task branch.
   - A slot freeing up or a task becoming claimable runs the reminder for idle owners. It hooks into
     the shared task-release helper (called by `conclude`, `kill_agent`, `spawn::retire` /
     `retire_all_idle` and `clear_conversation`) and the `board_update` tool.
   - `kill_agent`: release tasks and drop boards for every id the cascade ended. Add `pub fn retire`
     and `retire_all_idle` wrappers that do the same.
   - `conclude`: for the ids the cascade retired (from `finish` or a budget-exhausted end), release
     tasks, drop boards and append `finished: retired`, as `spawn::retire` does.
   - `SendMessageTool`: the new error text and description. Update the `agent` description.
   - Reword the "teammate" / "team" strings: `spawn.rs` (the `stays idle` warning at ~249, the
     resumed-in-a-row warning at ~256, the unread-message warning at ~337) and `tools.rs:17`
     ("teams are off in this session").
4. **Core** (`fontaine-core`)
   - `agent.rs::clear_conversation`: drop the `any_idle` refusal, and release tasks and drop boards
     for the ids `clear_ended` returns, and append `finished: retired` for its idle ones only.
   - `agent/rewind.rs`: drop the refusal. On a conversation rewind, call `spawn::retire_all_idle(why)`.
   - `session.rs` / `rewind.rs`: delete the `IdleTeammates` variants.
   - `agent/setup.rs`: build `Boards` around the restored root board.
   - `AgentRuntime::goal_turn_ended` (`agent/goal.rs`) runs the owner reminder for `main`, only after
     a real model turn.
5. **TUI** (`fontaine-tui`)
   - `ListRow.owns_task`, and the `agent_list.rs` listing rule.
   - Per-owner boards in `ui/mod.rs` and `agents_panel.rs`.
   - Test updates in `agent_list.rs`, `agents_panel.rs` and `screen.rs`.
6. **Agents across `--resume`**
   - Ledger: `Record::Agent` gains an optional `model`, set on `spawned` to the concrete resolved reference; `AgentEvent::Spawned`
     carries it from the `agent` tool. Retiring appends `Finished { status: "retired" }` from
     `spawn::retire` / `retire_all_idle`. `AgentRecord` gains `model` (`fontaine-core/src/ledger.rs`,
     `child_runner.rs::LedgerWindow`).
   - `AgentTree::rebuild(records, types, limits)`: adds idle, handle-less nodes for revivable
     agents, re-deriving their tools, mode and depth (`tree.rs`). Called from `agent/setup.rs` after
     `build_agent`, once root tools and posture exist; the revivable-id set is computed earlier from
     ledger and registry alone.
   - `ChildRunner::revive` (`runner.rs`) and `CoreChildRunner::revive` (`child_runner.rs`), sharing
     the agent construction with `spawn`; `revive` returns a `BoxFuture` like `spawn`. `wake_idle`
     and the human resume path (`message_agent`) stay sync and run revive-then-resume in a spawned
     task for a handle-less node.
   - `TaskBoard::restore` keeps in-progress tasks whose owner is in the revivable-id set. Update its callers
     (`fontaine-core/src/agent/setup.rs:546`, `fontaine-agents/src/tasks.rs:131,436,590`) and the test
     `a_saved_board_loads_back_without_owners` (`fontaine-core/src/tasks.rs:83-92`).
   - `message_agent` (`fontaine-core/src/agent/messaging.rs`, `handle_of` near line 139) revives a
     handle-less node. Fix the stale doc comments at `fontaine-agents/src/tasks.rs:134-135` and
     `fontaine-core/src/tasks.rs:1-2`, `fontaine-tools/src/jobs.rs:134` and
     `fontaine-core/src/user_hooks.rs:9-11`. Update the `teams` doc at `fontaine-core/src/config.rs:~1873`.
7. **Docs** (see Documentation).
8. **Plan Decisions** in `tasks/fontaine_harness_plan.md`, at the end of `### Agents`:
   - 2026-11-05 (supersedes the 2026-11-02 line "one team per session … only `main` starts a
     teammate … a teammate is an ordinary subagent with an idle state"): every subagent whose turn
     ends `done` or `failed` goes idle and keeps its conversation. Only the "one team / only `main` starts a
     teammate / teammate" clauses are superseded; teams on by default, `teams = false` turning off boards,
     the user-scope-only key, subagents not being independent sessions and idle holding no slot stay. A reachable agent's message, a
     report, the owner reminder or the human wakes it, and there is no `teammate` argument. This is Claude Code parity:
     its SendMessage resumes a completed agent from its transcript (#435).
   - 2026-11-05 (supersedes the 2026-11-02 (user) board-retirement line): an idle agent retires only
     when an ancestor ends for good, on `/clear`, or on a conversation rewind. There is no
     board-coupled retirement, timeout or cap, and the agent list hides idle rows that own no task
     (#435).
   - 2026-11-05: `/clear` and a conversation rewind retire idle agents instead of refusing (#435).
   - 2026-11-05 (user): Deviation from Claude Code, whose teammates claim their own next task. No
     idle agent is handed new board work. A claimable task goes to a fresh agent that the board's
     owner spawns after a deduplicated harness reminder. Reason: a fresh agent has a clean context,
     and reusing one is the owner's explicit choice through `send_message` (#435).
   - 2026-11-05 (user): Deviation from Claude Code's one team per session with a fixed lead. Any
     agent owns a board once it creates a task, and its children work it. An agent sees only its own
     board and its parent's. Only `main`'s board is persisted. Reason: any agent can decompose its
     work and spawn agents for it (#435).
   - 2026-11-05: `teammate_idle` fires only for an agent going idle with an in-progress task on its
     parent's board, as a gate on unfinished work (#435).
   - 2026-11-05 (user): A `--resume`d session rebuilds every agent that had not ended for good as
     idle, from the ledger and its per-agent log. Its handle is built lazily on the first wake, its
     tools and mode are re-derived (never looser than its parent's), and its per-agent spend starts
     over. Claude Code parity: an agent resumes from its transcript (#435).
   - 2026-11-05 (amends the 2026-11-02 wake line): the idle hand-off note is no longer a wake, and
     the cap of 3 covers only the `teammate_idle` resume. The owner reminder is deduplicated, not
     capped. A wake is still never recorded as a user message (#435).

## Files to Modify

- `rust/fontaine-agents/src/tree.rs`, `spawn.rs`, `tasks.rs`, `tools.rs`, `runner.rs`, `root_ledger.rs`
- `rust/fontaine-tools/src/jobs.rs`, `rust/fontaine-core/src/user_hooks.rs` (stale teammate comments)
- `rust/fontaine-core/src/ledger.rs`, `child_runner.rs` (`revive`, `LedgerWindow`)
- `rust/fontaine-core/src/agent.rs`, `agent/rewind.rs`, `agent/setup.rs`, `session.rs`, `agent/messaging.rs`, `agent/afk.rs`, `config.rs` (`teams` doc), `export.rs`, `agent/goal.rs`
  (`goal_turn_ended`), `tasks.rs` (store unchanged; doc comment)
- `rust/fontaine-tui/src/agent_list.rs`, `agents_panel.rs`, `ui/mod.rs`, `screen.rs` (tests)
- `rust/fontaine-core/tests/teams.rs`, `rust/fontaine-core/tests/agent_resume.rs`,
  `rust/fontaine-agents/tests/seam.rs`, `rust/fontaine/tests/parallel_agents.rs` (wherever they
  pass `teammate`)
- `docs/agents.md`, `docs/hooks.md`, `docs/tui.md`, `docs/configuration.md`, `docs/architecture.md`, `docs/afk.md`,
  `docs/permissions.md`, `docs/tools.md`, `docs/index.md`, `docs/telemetry.md`, `README.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Narrower fix (idle for all, `teammate` kept).** Rejected; see the comparison table.
- **Keep the idle hand-off for agents on the board** (by a `teammate` flag, or by "has claimed a
  task before"). Rejected. Each hand-off puts an unrelated task into an existing conversation, which
  pollutes its context and costs more per turn than a fresh agent. Reuse is worth it only when the
  work builds on what the agent knows, and that is the owner's judgement.
- **The harness spawns a `worker` per claimable task.** Rejected for now. It would need a synthetic
  spawn path through the gate, with no owner turn behind the call. It would also turn untrusted
  board text into a spawn prompt, and take the type, model and "do it myself" choice away from the
  owner. Worth a follow-up issue if the reminder proves too weak.
- **One session board with a fixed lead.** Rejected: a subagent that decomposes its work could not
  hand it out, which is the case the user raised.
- **Every board visible to every agent.** Rejected: an agent works only the board it was
  spawned to serve, and global visibility makes routing by id ambiguous to reason about.
- **Persist every board.** Rejected: a child's board is
  scratch state, like its todo list.
- **Idle timeout or cap.** Rejected. Idle costs no spend and no slot, its conversation is already
  kept, and Claude Code has neither. A timeout would bring back the #435 failure, just later.
- **Only `Done` is idle (`Failed` final).** Rejected: the human can already resume a failed child,
  and two predicates for "can this be resumed" are what this plan removes.
- **Report to an idle parent goes to root.** Rejected: it would skip the agent that asked for the
  work.
- **Rewind prunes like `/clear`.** Rejected: rewind keeps the list today, and retiring is enough for
  the intent property.
- **Revive agents eagerly at resume.** Rejected: it would open a model client per agent whether or
  not anyone wakes it. The lazy handle costs one `revive` seam.
- **Persist per-agent spend so budgets carry over a resume.** Rejected for now: the session tally
  does not carry over either, and the per-agent budgets are opt-in.
- **Persist subagent boards so a revived owner keeps its board.** Rejected: a child's board is
  scratch state, like its todo list. The owner can recreate tasks, and `main`'s board, the one the
  human sees, persists.

## Decisions

- 2026-11-05 (user): No idle agent is reused for new board work; claimable tasks go to fresh agents.
- 2026-11-05 (user): The owner of a board spawns its workers; the owner is any agent, not only `main`.
- 2026-11-05 (user): Agents survive `--resume`: an agent is its conversation, not a process.
- 2026-11-05: Ledgers written before this change (`/clear` wrote only `ContextCleared`, and `retire_idle` wrote no record) rebuild /clear-pruned and team-retired agents as idle and wakeable. Accepted: pre-release, only dev ledgers are affected, and the forward design appends `retired`.

## Documentation

- `docs/agents.md`:
  - The `agent` arguments table: remove `teammate`.
  - "Messages between agents": the wake rule replaces "a teammate that is idle is the one
    exception".
  - A new "Agent lifecycle" section: running / idle / final, the retirement triggers, `Failed`
    being wakeable, and agents coming back idle after `--resume` (model, tools and mode, spend
    starting over, and a view that shows only turns from after the resume).
  - "Agent teams" becomes "Task boards": per-agent boards, visibility, routing, the owner reminder,
    fresh agents for new work, persistence of `main`'s board only. Remove "Only `main` can start a
    teammate", "Idle instead of done", "The idle hand-off", and the board-retirement and
    `/clear`/`/rewind` refusal text.
  - The "Watching and stopping" paragraph: `idle` vs `done`, and boards per owner in the panel.
- `docs/afk.md` (The stop, step 3): idle subagents are retired, running ones killed.
- `docs/permissions.md` ("Agent teams", waking an idle teammate), `docs/tools.md` (the `agent` and
  board tool text), `docs/index.md`, `docs/telemetry.md` (the agent-team / task-board wording at line 155 and its `#the-task-board` link) and
  `docs/tui.md` (`/rewind` and `/clear` no longer refused while an agent is idle, plus the other
  teammate wording): update every passage that describes teammates, idle teammates or the
  refusals. `docs/configuration.md`: the "named teammates that go idle" text and `teammate` being
  refused when teams are off.
- Renaming "Agent teams" breaks the `#agent-teams` anchors in `docs/hooks.md`,
  `docs/configuration.md`, `docs/tools.md` and `docs/tui.md`; update every `#agent-teams` link. Also update `docs/hooks.md:161`'s
  `agents.md#the-idle-hand-off` link, which goes with the removed section, and the
  `#the-task-board` links (`docs/permissions.md:201`, `docs/tools.md:36`, `docs/telemetry.md:155`).
- `docs/hooks.md`: `teammate_idle` fires for an agent going idle with an in-progress task on its
  parent's board. `task_completed` applies on every board.
- `README.md:93`: "updates to the team task board" becomes task-board wording without "team".
- `docs/tui.md`: the agent-list rule for idle rows, and per-owner boards in the panel.
- `docs/configuration.md`: `[agents] teams` turns the boards and team hooks on and off. It no longer
  controls idling.
- `docs/architecture.md`: the ledger's agent records gain `model` on `spawned` and the `retired`
  end, and a resumed session rebuilds idle agents from them.
- `CHANGELOG.md`: "Any finished subagent can be woken by `send_message`. The `teammate` argument is
  removed, any agent can own a task board, and claimable tasks go to fresh agents."

## Testing Strategy

Unit tests (`tree.rs`, `tasks.rs`, `tools.rs`, `spawn.rs` test modules), against constructed trees
and boards:
- `is_idle` derivation: `Done` and `Failed` are idle. Killed, exhausted, retired and running are not.
- `deliver` to a finished plain agent returns `Idle` for a reachable sender and `NotRunning(None)`
  for an unreachable one. A killed or retired target returns `NotRunning(Some(reason))`.
- `retire` cascades over idle descendants only and returns their ids. A parent that merely idles
  leaves its idle children alone. `kill_outcome` on an idle node retires its idle children and kills a running grandchild under an
  idle child.
- `resume` refuses a retired node with `Refusal::Retired`. `finish` keeps tasks only for a `Done`
  end, and returns the ids its cascade retired. `clear_ended` returns every pruned id with the idle
  ones marked, and `/clear` appends `retired` only for those: a pruned killed agent still exports
  as killed.
- `Boards`:
  - Ids are unique across owners. `create` lands on the caller's board.
  - `claim` / `complete` / `release` route by id to the caller's own or parent's board, and refuse a
    sibling's or grandparent's task.
  - The board owner can complete or release any task on its own board, and a non-owner only its own
    claimed task.
  - `drop_owner` removes a board. `release_owned_by` frees an agent's tasks on its parent's board.
  - Only `main`'s board reaches the store.
- `Boards::reminder`: a note when claimable tasks and free slots exist; none when either is zero;
  none again until the `(mutations, free slots)` pair changes.
- `AgentArgs` with `teammate` is rejected as an unknown field, and the schema has no `teammate`.
- `agent_list::rows`: an idle row is listed only with `owns_task` or when viewed.

Integration tests through the core with `rig_core::test_utils` scripted models
(`fontaine-core/tests/agent_resume.rs`, `teams.rs`):
- **Regression for the bug seen in the session (#435):** root spawns a plain `worker`, which ends
  its turn. Root's `send_message` returns `queued`, and the worker takes a further turn whose
  request holds its earlier history and the sender-marked message. The message is not recorded as a
  user message.
- A child woken by the human while its subagent parent is idle: the child's report wakes the parent
  with the report as its prompt.
- `/clear` with an idle agent owning an in-progress task succeeds, prunes it and releases the task.
  After a conversation rewind, the idle agent shows `done`, and a later `send_message` to it errors
  with the retired reason. This replaces `clear_and_rewind_are_refused_while_a_teammate_is_idle`.
- Board work-down:
  - Root creates two tasks and ends its turn, and root gets one reminder note.
  - A `worker` that finishes its own task and goes idle is *not* handed the other task.
  - A subagent that creates tasks gets the reminder at its turn end, and its child claims from the
    subagent's board, not root's.
- `teammate_idle`: a blocking hook resumes an agent going idle with an in-progress task, capped as
  today. It does not fire for an agent with no task.
- With `teams = false`, a finished worker still idles and can be woken by a message, and has no
  board tools.
- `teams.rs` tests rewritten without `teammate`, keeping their wake and non-recorded-intent
  assertions.

Across `--resume` (unit tests on the ledger view and `AgentTree::rebuild`; one integration test):
- The ledger view's `AgentRecord` carries `model`, and `finished: retired` marks an agent final.
- `rebuild`:
  - It adds idle nodes for `done` and `failed` agents only, and skips a child whose parent was not
    rebuilt and an agent whose type is no longer in the registry (with its descendants).
  - It re-derives tools as the type's cut to the parent's, and a mode never looser than the parent's
    posture.
- A handle-less idle node: a wake calls `revive` once, then resumes. `revive` failing (missing log,
  unavailable model) ends the run `Failed` with that reason and releases its tasks. The missing-log
  case is tested against the real `CoreChildRunner`, not only a fake.
- `TaskBoard::restore` keeps an in-progress task owned by a rebuilt id and resets the others.
- Integration (`fontaine-core/tests/agent_resume.rs`):
  - Run a session where root spawns a worker that ends `done`, then reopen the session from its
    store as `--resume` does.
  - Root's `send_message` to the worker's name returns `queued`, and the worker's next request
    holds its earlier history from the per-agent log.
  - The guardian intent for its calls is the replayed root intent.
