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
conversation intact, and a final end (killed, budget spent, or retired) cannot be undone. This
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
  one.
- **`Node`** loses `teammate` and `idle` and gains `retired: bool`. `fn is_idle(&Node)` holds the
  derivation. `AgentRow.idle`, `job_output` and export read it.
- **`finish`** returns whether the agent keeps its board tasks: `Done && !killed && !retired`. A
  `Failed` end still releases them, as today.
- **Final ends:** killed, budget exhausted, retired. A retired agent shows `done`, so `done` now
  always means "final". `resume` refuses a retired node with a new `Refusal::Retired(String)`, and
  `resume_blocker` returns "it was retired (<why>)".

### Retirement

There is no timeout and no cap on idle agents. An idle agent spends nothing and holds no
`max_parallel` slot, and its conversation is already kept today. Claude Code keeps every completed
agent resumable for the session. An agent retires only when resuming it no longer makes sense:

1. **Cascade on a final end.** When an agent is killed, runs out of budget, or retires, every idle
   descendant retires with it. Their tasks are released, and the boards they own are dropped (see
   below). A kill already stops the running descendants, so this makes "a kill is final" cover the
   whole subtree. A parent that merely goes idle leaves its idle children alone, because a later wake
   of the parent may need them. That is the reviewer scenario.
2. **`/clear`.** The `IdleTeammates` refusal goes away. `clear_ended` already drops every ended node,
   idle ones included. It now returns the dropped ids, so `clear_conversation` releases their tasks
   and drops their boards first. This extends the 2026-11-02 decision that pruning ends the
   resumability of pre-clear subagents.
3. **Conversation rewind.** The `IdleTeammates` refusal goes away. A rewind that includes the
   conversation retires every idle agent. The refusal existed because a woken agent would be judged
   against intent the rewind dropped. Retiring keeps that property without refusing, and it closes
   the same gap for today's human resume of a `done` child. Rows stay listed, as `done`.
4. **Session end.** Unchanged: the shutdown flag refuses every wake.

`retire_finished_team` and the board-coupled retirement are deleted. This supersedes the 2026-11-02
user Decision, whose aim (a finished team should not linger in the list) is now served by the agent
list rule below without making agents unreachable.

`AgentTree` API: `retire_idle(id)` becomes `retire(id) -> Vec<String>`, which retires the node and
its idle descendants under one lock and returns every id it retired. `retire_all_idle() ->
Vec<String>` serves rewind. A `spawn::retire` wrapper releases tasks and drops boards for those ids,
as `kill_agent` does for a kill. `kill_outcome`'s idle arm uses the same cascade.

### Waking

- **`deliver`** keeps its shape. For a target that is not running, it returns
  `DeliverError::Idle(id)` when `is_idle(target) && reachable`, with no kind check. Otherwise it
  returns `NotRunning(Option<String>)`, which carries the `resume_blocker` reason when the sender
  may reach the target. An unreachable sender still learns nothing.
- **`SendMessageTool`** wakes on `Idle` through the existing `wake_idle`. On `NotRunning(Some(why))`
  it errors with `"<name> cannot be woken: <why>; start a new agent with the context it needs"`.
- **A report to an idle parent wakes it.** In `watch_report`, a running parent gets `notify_report`
  as today. An idle parent gets the report queued and is woken through `wake_for_inbox`. A final
  parent falls back to `post_to_root` as today. This only fires when something woke the child: the
  human typing into it, or a sibling's message. A subagent still joins its own running children
  before it goes idle (2026-10-30 #326).
- **`wake_for_inbox`** runs for every agent that goes idle, closing the race where a message arrives
  as the turn ends.
- Wake text keeps going through `resume_with_data`, so it never becomes guardian intent (2026-11-02
  Decision, unchanged).
- **Nothing else wakes an agent.** The idle hand-off of claimable tasks is removed (see below).

### Boards per agent

`Teams { board, … }` becomes `Teams { boards: Boards, gate, streaks }`. `Boards` holds a map from
owner id to `Arc<TaskBoard>` and one session-wide id counter, so a task id (`t<n>`) is unique across
every board.

- **Ownership.** An agent's board is created the first time it runs `board_update create`. The
  creator is the owner. `owned_by`'s `ROOT_AGENT` exception becomes "the board's owner", so an owner
  can change, complete or release any task on its own board, as `main` can today.
- **Visibility.** An agent sees its own board and its parent's. It owns its own, and works its
  parent's by claiming and completing tasks. A sibling's or grandparent's board is out of reach,
  following `send_message`'s scoping. `main` has no parent, so it sees only its own board.
- **Routing, with no new argument.** `create` always goes to the caller's own board. `claim`,
  `complete` and `release` take a task id and act on whichever of the caller's own and parent's
  boards holds it. Any other id is `invalid_args` ("no task <id> on your board or your parent's").
  `board_show` lists both boards, each labelled with its owner's name.
- **Lifetime and persistence.** `main`'s board is persisted and restored exactly as the session's
  board is today (`fontaine-core/src/tasks.rs`). A subagent's board is in memory only, matching the
  rule that only the root's todo list is saved. It is dropped when its owner ends for good or is
  pruned by `/clear`. An idle owner keeps its board.
- **Releasing tasks.** `release_owned_by(agent)` runs on the agent's parent's board, the only one
  where it can own tasks. `/clear` and the cascade use the same helper.
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
  - the owner's turn end: the root's `after_turn`, or `watch_report` for a subagent before it is
    finished, which keeps it running for one more turn;
  - a slot freeing up or a task on its board becoming claimable while the owner is idle or between
    turns.
- **Delivery.** The note goes through the existing paths: `post_to_root` for `main`, the inbox for a
  running owner, and a `wake_idle` resume for an idle owner. It is not recorded as a user message.
- **Deduplication.** Each owner keeps the last `(board mutation count, free slots)` it was reminded
  at, and gets no new note until that pair changes. An owner that ignores a note is not nagged, and
  no loop sustains itself. This replaces `Teams::allow_idle_resume`'s role for the hand-off.
- **Who spawns.** Always the owner, through an ordinary `agent` call. Every spawn passes the floor and
  the `spawn` tier (asks in `manual` / `acceptEdits`). The owner picks the type, model and prompt.
  Board text is agent-written, untrusted data, never a spawn prompt on its own. The spawned agent
  claims its task with `board_update`.

### `teammate_idle` and `task_completed`

- `teammate_idle` becomes a quality gate on unfinished work. It fires when an agent goes idle (turn
  ended `Done`) while it owns an in-progress task on its parent's board. A block resumes the agent
  on the hook's reason, and the resume is capped by `MAX_IDLE_RESUMES` with no board change, as
  today. It never hands out new work. The hook keeps its Claude Code name. Its input names the agent
  as today.
- `task_completed` gates completion on every board, unchanged.

`[agents] teams = false` removes the board tools, boards, reminders and both team hooks. Idling and
waking do not depend on it.

### Text the model and the human read

- Idle report line, for every agent: `Agent <name> (<id>, <type>) finished its turn and is idle;
  send_message wakes it with its conversation intact.` Other ends keep `finished: <status>`.
- `agent` description, added after "returns a report": "When its turn ends it goes idle and keeps
  its conversation: `send_message` wakes it for follow-up on the same work. Start a new agent for
  new work." The `teammate` schema entry is removed.
- `send_message` description: "An agent whose turn has ended is woken by it with its conversation
  intact, unless it was killed, ran out of budget or was retired."
- `board_show` / `board_update` descriptions: "your board" (tasks you created) and "your parent's
  board" (tasks you can claim), and the routing rule.

### Surfaces

- **Agent list** (`agent_list.rs:69`): `running || viewed`, plus idle rows that own an in-progress
  task. An idle row without one leaves at once, as a `done` row does today. `AgentRow` gains
  `owns_task: bool`, filled in `fontaine-tui/src/ui/mod.rs`.
- **`/agents` panel:** every row with `idle` / `done` / … and the kill key on idle rows, as today.
  It lists each non-empty board under its owner's name instead of the single team board
  (`ui/mod.rs:368` builds `BoardRow`s per owner).
- **Status bar, export, `job_output`:** read the derived idle state; no other change.
- **AFK stop:** still kills `running_ids() ∪ idle_ids()`, and `idle_ids` now covers every idle agent.

### Agents across `--resume`

An agent is a conversation plus a few facts about it, not a process. Its log (`logs/agent-<n>.jsonl`)
persists, and its id and name are already stable across `--resume` (2026-11-21 #300). A resumed
session therefore brings back every agent that had not ended for good, as idle and wakeable, as
Claude Code resumes an agent from its transcript.

- **What the ledger must hold.** The `spawned` record gains `model` (the resolved `ModelSpec` text).
  Retiring appends `finished` with status `retired`. That covers the cascade, `/clear` and rewind,
  so a resumed session knows which agents are final without replaying those rules. `/clear` already
  records `ContextCleared`, and agents spawned before it stay gone on rebuild.
- **Rebuild at startup** (`agent/setup.rs`, next to `seed_names`). The ledger view's `agents` are
  walked in spawn order. Each agent whose last `finished` status is `done` or `failed` becomes an
  idle node with no handle:
  - Its type comes from the current registry.
  - Its tools are re-derived as the type's toolset cut to its rebuilt parent's, as at spawn.
  - Its mode is `own_mode` against the parent's posture, so it is never looser than the parent, and
    the session itself resumes in the default mode (P6).
  - Its depth comes from the parent chain.

  An agent whose parent is not rebuilt (final, or gone after a clear) is not rebuilt either. That is
  the cascade rule applied at load. Killed, budget-exhausted, retired and still-running agents (the
  previous process's `kill_all` ended those as `killed`) are not rebuilt.
- **Lazy handle.** A rebuilt node gets its `ChildHandle` on its first wake, not at startup, so a
  resume opens no model clients for agents nobody talks to. `ChildRunner` gains
  `revive(RevivalRequest) -> Result<Arc<dyn ChildHandle>, String>`. The request is a `SpawnRequest`
  without a prompt. `CoreChildRunner` builds the rig agent over the existing per-agent log without
  launching a run. `wake_idle`'s "it has no conversation to resume" branch calls `revive` first,
  then resumes as usual. A missing log, an unknown type or an unavailable model refuses the wake
  with that reason.
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
  instead of resetting it. `TaskBoard::restore` takes the rebuilt ids. A subagent's board is not
  persisted, so a revived subagent owner starts with an empty board.
- **Surfaces.** Rebuilt agents show in the `/agents` panel as `idle`, and `send_message`,
  `job_output` and typing into their view reach them by name or id.

### Open points from the issue, settled

- **Memory:** no new cost. Every ended child already keeps its handle and on-disk log.
- **Spend:** a woken agent debits its own cumulative budget and the session tally (`debit`), as a
  teammate does. There is no per-subtree budget.
- **One-shot reliance:** `job_output` treats idle as ended (`AgentState::Ended { status: "idle" }`),
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
   - Add `retire(id)` (cascade) and `retire_all_idle()`, both returning ids. `kill_outcome` cascades
     over idle descendants, and `clear_ended` returns the pruned ids. Delete `retire_idle`.
   - `rows()` / `AgentState` use `is_idle`. Add `AgentRow.owns_task`, or leave it to the UI (step 5).
2. **Boards** (`fontaine-agents/src/tasks.rs`, `tools.rs`)
   - Add `Boards` (owner → `TaskBoard`, shared id counter, a store only for `ROOT_AGENT`'s board).
     `TaskBoard` takes its ids from the shared counter. `restore` seeds the counter from `main`'s
     snapshot.
   - `owned_by`: the board owner replaces `ROOT_AGENT`.
   - Add `Boards::own(agent)`, `Boards::find(agent, parent, task_id)`, `Boards::drop_owner(agent)`
     and `Boards::release_owned_by(agent, parent)`.
   - Add a reminder check, `Boards::reminder(owner, free_slots) -> Option<String>`, with the per-owner
     dedup pair.
   - `board_show` / `board_update`: the routing and the new descriptions.
   - Remove the hand-off from `Teams` (`allow_idle_resume` stays only for the hook).
3. **Spawn and messaging** (`fontaine-agents/src/spawn.rs`)
   - Remove `AgentArgs.teammate`, its schema entry, its refusals and the board note.
   - `watch_report`:
     - For any `Done` end, first resume on queued inbox mail, then on a `teammate_idle` block when the
       agent owns an in-progress parent-board task, then on the owner reminder.
     - Use the new idle report line. A report to an idle parent queues and wakes it, and
       `wake_for_inbox` runs for every idle end.
     - Delete `retire_finished_team` and the claimable-task branch.
   - A slot freeing up or a task becoming claimable runs the reminder for idle owners. It hooks into
     `conclude` and the `board_update` tool.
   - `kill_agent`: release tasks and drop boards for every id the cascade ended. Add `pub fn retire`
     and `retire_all_idle` wrappers that do the same.
   - `SendMessageTool`: the new error text and description. Update the `agent` description.
4. **Core** (`fontaine-core`)
   - `agent.rs::clear_conversation`: drop the `any_idle` refusal, and release tasks and drop boards
     for the ids `clear_ended` returns.
   - `agent/rewind.rs`: drop the refusal. On a conversation rewind, call `spawn::retire_all_idle`.
   - `session.rs` / `rewind.rs`: delete the `IdleTeammates` variants.
   - `agent/setup.rs`: build `Boards` around the restored root board.
   - The root's `after_turn` runs the owner reminder for `main`.
5. **TUI** (`fontaine-tui`)
   - `AgentRow.owns_task`, and the `agent_list.rs` listing rule.
   - Per-owner boards in `ui/mod.rs` and `agents_panel.rs`.
   - Test updates in `agent_list.rs`, `agents_panel.rs` and `screen.rs`.
6. **Agents across `--resume`**
   - Ledger: `Record::Agent` gains an optional `model`, set on `spawned`; `AgentEvent::Spawned`
     carries it from the `agent` tool. Retiring appends `Finished { status: "retired" }` from
     `spawn::retire` / `retire_all_idle`. `AgentRecord` gains `model` (`fontaine-core/src/ledger.rs`,
     `child_runner.rs::LedgerWindow`).
   - `AgentTree::rebuild(records, types, limits)`: adds idle, handle-less nodes for revivable
     agents, re-deriving their tools, mode and depth (`tree.rs`). Called from `agent/setup.rs`.
   - `ChildRunner::revive` (`runner.rs`) and `CoreChildRunner::revive` (`child_runner.rs`), sharing
     the agent construction with `spawn`. `wake_idle` and the human resume path (`message_agent`)
     revive a handle-less node before resuming.
   - `TaskBoard::restore` keeps in-progress tasks whose owner was rebuilt.
7. **Docs** (see Documentation).
8. **Plan Decisions** in `tasks/fontaine_harness_plan.md`, at the end of `### Agents`:
   - 2026-11-05 (supersedes the 2026-11-02 line "one team per session … only `main` starts a
     teammate … a teammate is an ordinary subagent with an idle state"): every subagent whose turn
     ends `done` or `failed` goes idle and keeps its conversation. A reachable agent's message, a
     report, or the human wakes it, and there is no `teammate` argument. This is Claude Code parity:
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

## Files to Modify

- `rust/fontaine-agents/src/tree.rs`, `spawn.rs`, `tasks.rs`, `tools.rs`, `runner.rs`, `root_ledger.rs`
- `rust/fontaine-core/src/ledger.rs`, `child_runner.rs` (`revive`, `LedgerWindow`)
- `rust/fontaine-core/src/agent.rs`, `agent/rewind.rs`, `agent/setup.rs`, `session.rs`, the root
  `after_turn` path, `tasks.rs` (store unchanged; doc comment)
- `rust/fontaine-tui/src/agent_list.rs`, `agents_panel.rs`, `ui/mod.rs`, `screen.rs` (tests)
- `rust/fontaine-core/tests/teams.rs`, `rust/fontaine-core/tests/agent_resume.rs`,
  `rust/fontaine-agents/tests/seam.rs`, `rust/fontaine/tests/parallel_agents.rs` (wherever they
  pass `teammate`)
- `docs/agents.md`, `docs/hooks.md`, `docs/tui.md`, `docs/configuration.md`, `docs/architecture.md`, `CHANGELOG.md`
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
- **Every board visible to every agent.** Rejected: it leaks tasks across the tree, unlike
  `send_message`'s scoping, and makes routing by id ambiguous to reason about.
- **Persist every board.** Rejected: agent ids do not survive a resume, and a child's board is
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

## Documentation

- `docs/agents.md`:
  - The `agent` arguments table: remove `teammate`.
  - "Messages between agents": the wake rule replaces "a teammate that is idle is the one
    exception".
  - A new "Agent lifecycle" section: running / idle / final, the retirement triggers, `Failed`
    being wakeable, and agents coming back idle after `--resume` (model, tools and mode, spend
    starting over).
  - "Agent teams" becomes "Task boards": per-agent boards, visibility, routing, the owner reminder,
    fresh agents for new work, persistence of `main`'s board only. Remove "Only `main` can start a
    teammate", "Idle instead of done", "The idle hand-off", and the board-retirement and
    `/clear`/`/rewind` refusal text.
  - The "Watching and stopping" paragraph: `idle` vs `done`, and boards per owner in the panel.
- `docs/hooks.md`: `teammate_idle` fires for an agent going idle with an in-progress task on its
  parent's board. `task_completed` applies on every board.
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
  leaves its idle children alone. `kill_outcome` on an idle node retires its idle children.
- `resume` refuses a retired node with `Refusal::Retired`. `finish` keeps tasks only for a `Done`
  end. `clear_ended` returns the pruned idle ids.
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
    rebuilt.
  - It re-derives tools as the type's cut to the parent's, and a mode never looser than the parent's
    posture.
  - It skips agents spawned before a `ContextCleared`.
- A handle-less idle node: a wake calls `revive` once, then resumes. `revive` failing (missing log,
  unknown type) refuses the wake with that reason and releases its tasks.
- `TaskBoard::restore` keeps an in-progress task owned by a rebuilt id and resets the others.
- Integration (`fontaine-core/tests/agent_resume.rs`):
  - Run a session where root spawns a worker that ends `done`, then reopen the session from its
    store as `--resume` does.
  - Root's `send_message` to the worker's name returns `queued`, and the worker's next request
    holds its earlier history from the per-agent log.
  - The guardian intent for its calls is the replayed root intent.
