# Agent Teams Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/40

## Overview

Claude Code's agent teams run several sessions as named teammates that share a task list, message
each other and are held to quality gates by hooks. Fontaine already has most of the pieces: named,
addressable subagents (`name`, `send_message`), resumable ended agents, a join-before-end rule, a
budget tree that debits the session root, and every call gated. Two things are missing: a **shared
task board** the team claims work from, and **teammates that stay alive** (idle, wakeable) between
pieces of work, with `task_completed` / `teammate_idle` hooks as quality gates. This plan adds
those on top of `fontaine-agents`, with no new agent loop and no new permission path: every task
operation and every message is an ordinary gated pipeline request.

## Current State

- `AgentTool` (`rust/fontaine-agents/src/spawn.rs:41`, args at `:33`) spawns a named subagent;
  `reserve_named` (`tree.rs:510`) enforces unique names, `max_parallel` and the depth cap.
- `SendMessageTool` (`spawn.rs:390`) delivers through `AgentTree::deliver` (`tree.rs:908`), which
  refuses a target that is not running (`DeliverError::NotRunning`). Messages reach parent,
  children, siblings and `main`; the text is queued, sender-marked and untrusted.
- An ended agent (`done`/`failed`) can be resumed by the human typing into its view
  (`resume_child`, `spawn.rs:133`; `AgentTree::resume`, `tree.rs:714`, which re-checks `max_parallel`;
  `resume_blocker`, `tree.rs:687`, which refuses killed or budget-exhausted agents).
- The existing wake path for a message is `message_agent` (`rust/fontaine-core/src/agent/messaging.rs`),
  which handles the is_running/drain race and treats `AlreadyRunning` as "queue".
- `watch_report` (`spawn.rs:102`) delivers a finished child's report to its parent as a notification
  and calls `conclude` (ledger `finished`, `tree.finish`, telemetry).
- `todo_write` (`rust/fontaine-tools/src/todo.rs`, store in `rust/fontaine-core/src/todos.rs`) is a
  per-agent private list; only the root's is saved (`todos.json`), because agent ids do not survive
  a resume.
- Tools are registered in `rust/fontaine-core/src/agent/build.rs:215` (`extension_tools`) and must
  have a row in `rust/fontaine-tools/src/descriptor.rs` (risk class, destructive, specifier) or the
  choke-point test (`rust/fontaine-core/tests/choke_point.rs`) fails.
- `spawn_changes_state` (`rust/fontaine-core/src/adjudication.rs:78`) decides the read-only spawn
  fast path from the child's toolset.
- Hooks (`docs/hooks.md`) have three events, all tool-centered: `pre_tool_use`, `post_tool_use`,
  `permission_denied`. There is no lifecycle event.
- `[agents]` keys are `user_trust` scope (`docs/configuration.md:113`): project scope cannot loosen them.

## Design

### Model

- **Team** = the session's agent tree rooted at `main`, which is the **lead**. One team per
  session, no `team` object to create or name; the lead is always `main`. A **teammate** is a
  subagent spawned with `teammate: true`.
- Teams are **off by default**: `[agents] teams = false` (user/CLI scope only, like the other
  `[agents]` loosening keys). When off, `teammate` is refused with a message naming the key and the
  task tools are not registered.
- Teammates are ordinary agent nodes: same toolset cut-down, mode never looser than the parent's,
  per-call gating, per-agent and session budgets (every teammate debits the session root), depth
  and `max_parallel` accounting. Nothing in this feature reaches past the floor.
- Only `main` may spawn a teammate (no nested teams, as in Claude Code); a subagent may still
  spawn plain subagents under the existing rules.

### Teammate lifecycle: running ⇄ idle

Today a run that ends is `done`. A teammate's finished run becomes **`idle`** instead:

- `ChildStatus` (`runner.rs`) gains no new terminal state; the node gets an `idle` flag in
  `tree.rs` set by `finish` for a teammate whose report status is `Done` and that was not killed.
  `failed`, `killed` and `budget_exhausted` stay final, exactly as for any agent.
- The idle decision is made in the async `watch_report` task, before `finish` and before the report
  is queued: for a teammate whose run ended `Done` and was not killed, the `teammate_idle` hook and
  the idle hand-off (below) run first. If either resumes the teammate, the same run record continues
  and no `Done` report is produced. Only when neither does is the node finished as idle, and then the
  report is delivered to the lead as the usual notification, worded `finished its turn` / marked
  idle, so the lead sees what was done.
- An idle teammate holds its name. Waking it goes through `resume_blocker` (killed/exhausted) and
  `AgentTree::resume` (cap and budgets), so a wake can be refused like any resume. `tree.resume`
  clears the `idle` flag.
- Wake on message: `AgentTree::deliver` is synchronous under the tree lock and has no runner or
  ledger, so it cannot resume. It returns a new `DeliverError::Idle` for an idle teammate target
  (checked after reachability, before `NotRunning`). `SendMessageTool::call`, after its reachability
  check, handles that result by waking the teammate through a trait seam implemented in
  `fontaine-core` (the agents crate cannot depend on core) that reuses `message_agent`'s path:
  `resume_blocker`, then `resume_child` with the message as the new prompt (sender-marked,
  untrusted, same rendering as a queued one); `AlreadyRunning` means "queue". The sender still gets
  `{"status":"queued"}`; a refused wake returns the refusal reason as the tool error. A wake after
  session shutdown is refused.
- Wake sources: a message, a human typing into its view (existing path), the **idle hand-off**
  below, or a `teammate_idle` hook block. The three non-human sources resume through a variant of
  `resume_child` / `ChildHandle::resume` (`child_runner.rs`) that delivers the sender-marked text
  without `record_human_prompt`; the existing human path keeps recording, so a peer message,
  hand-off note or hook reason is never merged into the teammate's guardian intent (`intent_for`).
- Session end: idle teammates hold nothing, so `kill_all` (which selects only running nodes) needs
  no change; a shutdown flag on the tree makes every wake refuse once session end begins. The
  headless wait (`wait_for_news` on `any_running()`) must not end between a teammate's `conclude`
  and a pending hand-off or hook resume: that decision is made before `finish`, so the node never
  stops counting as running, and the resume (when there is one) keeps the run open. Idle teammates
  themselves never keep a run open. `/clear` and rewind (`agent.rs` `clear_conversation`,
  `agent/rewind.rs`), which refuse while something runs or a report is pending, also refuse while any
  teammate is idle: it holds intent history a later wake needs.
- Ownership: a task owned by an agent that ends `failed`, `killed` or `budget_exhausted`, or whose
  wake is refused, is released (see Shared task board). An idle teammate's `InProgress` tasks stay
  owned by it; they are released only if it later ends non-`Done`.

### Shared task board

New module `rust/fontaine-agents/src/tasks.rs`: `TaskBoard`, an in-memory board behind one mutex
with a `TaskStore` trait seam (same inversion as `TodoStore`) for persistence implemented in
`fontaine-core`.

```
Task { id: "t1", title, description, status: Pending|InProgress|Completed,
       owner: Option<agent id>, blocked_by: Vec<task id>, created_by: agent id }
```

- Ids are `t<n>`, minted by the board, never reused. Caps: 100 tasks, title 200 chars,
  description 5 000 chars, 20 `blocked_by` entries; over a cap is an invalid-arguments error.
- **Claim is atomic**: under the board mutex, a task is claimed only if `Pending`, unowned and every
  `blocked_by` task is `Completed`; two simultaneous claims have exactly one winner.
- `blocked_by` must name existing tasks; cycles are impossible by construction (a task can only
  depend on already-existing ids, and dependencies are fixed at creation).
- Only the owner (or the lead) may update an `InProgress` task; completing needs the owner's id.
- `TaskBoard::release_owned_by(agent)` returns every task the agent owns to `Pending` with no owner.
  `conclude`/`watch_report` call it when an agent ends in any non-`Done` status, and the wake path
  calls it when a wake is refused; each released task emits `TaskReleased`.
- Task text is **agent-authored, untrusted data**: when `task_list` renders it to another agent it
  is delimited and marked with its creator's name and id, never as intent for the guardian. The
  guardian's intent source stays the root ledger's human messages.

### Tools

Two tools, split by risk class because the descriptor table gives one class per tool:

| Tool | Class | Actions |
|---|---|---|
| `task_list` | `ReadOnly`, `path_free`, `Destructive::Never`, `Specifier::None` (like `todo_write`) | list all tasks, or `id` for one (status, owner, deps, text) |
| `task_update` | `Arbitrary` | `create`, `claim`, `complete`, `release` (owner gives a task back to `Pending`) |

`task_update` is `Arbitrary` like `send_message` and `agent`: `manual`/`acceptEdits` ask, `auto`
and `plan` allow it at the same non-guardian tier, `deny`/`ask` rules apply first (`task_update`
has no specifier). This satisfies "every message and task claim is a gated pipeline request".
`task_list` reading the shared board is read-only, so it joins the read-only fast path, and a
`explore`-style type holding `task_list` still takes the read-only spawn fast path. `task_update`
is **not** added to `spawn_changes_state`'s read-only allowance, so a type holding it is a
state-changing spawn. `permission_rules.rs:610` maps `"send_message" | "ask_user_question"` to no
specifier; `task_update` is added to that arm.

Both are registered only when `[agents] teams` is on, for every agent in the tree (a teammate's
toolset is still its type's list cut down to its parent's, so a type must name `task_list` /
`task_update` or name no tools). `AgentTypes::builtin()` takes no config, so the registry
post-processes the built-in set when teams are on: `worker` gains both tools in its list and
`explore` gains `task_list` only. `fork` has `tools: None` and inherits `task_update` from its
parent automatically.

### Idle hand-off (self-claiming)

When a teammate's run ends `Done`, before it is finished as idle (after the `teammate_idle` hook, if
that did not resume it) the runtime asks the board for a claimable task (`Pending`, unowned,
unblocked). If there is one, it resumes the teammate through the non-recording `resume_child` variant
with the system note "Task <id> is available: use `task_list` and `task_update` to claim it." The note is
data-marked as harness text, not intent. The loop is bounded by budgets, `max_parallel` and the kill
switch, exactly like a subagent's join rounds (no separate round cap); a resume refusal leaves the
teammate idle and warns once through `Approver::warn`.

### Quality-gate hooks

Two new hook events in `rust/fontaine-core/src/user_hooks.rs`, configured like the existing three
(`[[hooks.task_completed]]`, `[[hooks.teammate_idle]]`; project hooks load after workspace trust and
can only tighten):

| Event | Fires | Hook can |
|---|---|---|
| `task_completed` | Before `task_update complete` takes effect | `block` with a `reason`: the task stays `InProgress` and the reason is the tool error the owner reads |
| `teammate_idle` | After a teammate's run ends `Done`, before `finish` and before the report is queued | `block` with a `reason`: the teammate is resumed through the non-recording `resume_child` variant with the reason as feedback instead of idling |

Input fields: the common ones (`session_id`, `cwd`, `hook_event_name`, `agent_id`), plus `task`
(the task object) or `teammate_name`. A hook can never **loosen**: it has no `allow` answer, it can
only add work. Answer shape is as for the existing hooks and Claude Code: exit code 2 with the
reason on stderr blocks. These events carry no `tool_name`, so they take no matcher. A hook that
errors or times out warns and does not block (these are quality gates, not security gates, and a
failed gate that blocked forever would wedge the team); `block` loops are bounded the same way as
the idle hand-off. Hooks run outside the board lock. `QualityGate` is a trait defined in
`fontaine-agents` and implemented in `fontaine-core` over `user_hooks.rs`.

### Persistence and resume

`tasks.json` beside `todos.json` in the session dir, atomic write per mutation, owner-only
(`owner_only`). On resume the board loads, but agent ids do not survive, so every `InProgress` task
is returned to `Pending` with no owner and teammates are not recreated. The lead re-spawns what it
needs. Ledger events: `AgentEvent` gains `TaskCreated`, `TaskClaimed`, `TaskCompleted`,
`TaskReleased` (ids and agent only, no text); idle and wake reuse `Finished` and `Resumed`.

### Observability and UI

- Telemetry: `task` events (`created`, `claimed`, `completed`, `released`) with `agent.id`,
  `agent.name`, `task.id`. Audit log needs nothing new:
  the gate decision of each `task_update`/`send_message` call is already recorded.
- Agents panel (`rust/fontaine-tui`): teammate rows show `idle` as a status distinct from `done`; a
  Tasks section under the agents lists each task with status, owner name and blocked-by. The
  consumers keying on `status.is_running()` (`agents_panel.rs`, `agent_list.rs`, `ui/mod.rs`, and
  `export.rs` in core) read the `idle` flag so an idle teammate is not shown or exported as done. The
  existing `/tasks` alias of `/agents` and Ctrl+T open the panel, where the Tasks section lives. No tmux split-pane display (Claude Code's display mode)
  is built.
- Headless: `gate_decision` events already carry `agent_id`/`agent_name`; no new surface.

## Implementation Steps

1. **Config**: add `teams` (bool, default false) to `[agents]` in
   `rust/fontaine-core/src/config.rs`, its row in `config_catalog.rs`, and the `AgentsConfig`
   consumer (`config_scope.rs` already covers `[agents]` as `user_trust`).
2. **Task board**: `rust/fontaine-agents/src/tasks.rs` with `TaskBoard`, `TaskStore`, caps and
   validation, atomic claim, dependency check; export in `lib.rs`. Unit tests (below).
3. **Tools**: `TaskListTool` and `TaskUpdateTool` in `rust/fontaine-agents/src/spawn.rs` (or a new
   `tools.rs` if `spawn.rs` grows past readable), using `caller_of`; ledger events and telemetry.
4. **Descriptor rows**: `task_list` (ReadOnly, never destructive, `Specifier::None`) and
   `task_update` (Arbitrary, destructive `Always` like `send_message`, `Specifier::None`) in
   `rust/fontaine-tools/src/descriptor.rs`; `task_update` joins the
   no-specifier arm at `rust/fontaine-core/src/permission_rules.rs:610`.
5. **Teammate flag and lifecycle**: `AgentArgs.teammate: Option<bool>` (`spawn.rs`), refused when
   teams are off or the caller is not `main`; `idle` flag in `tree.rs` `Node`, `finish`, `rows`
   (`AgentRow` gains `idle`), `DeliverError::Idle`, `tree.resume` clearing `idle`, a shutdown flag
   refusing wakes. `SendMessageTool::call` performs the wake through a core-implemented trait seam
   (`resume_blocker`, then `resume_child`, `AlreadyRunning` queues). Add the non-recording resume
   variant in `rust/fontaine-core/src/child_runner.rs` (and its `spawn.rs` wrapper) for peer, hand-off
   and hook-reason wakes. In the async `watch_report` task, before `finish` and before the report is
   queued, the `teammate_idle` hook and idle hand-off decide resume vs idle; `finish` then marks idle
   only if neither resumed, and the "idle" notice is sent only then. `/clear` and rewind also refuse
   while a teammate is idle. `conclude`/`watch_report` and a refused wake call `TaskBoard::release_owned_by` on any non-`Done` end.
6. **Wiring**: register both tools in `extension_tools` (`agent/build.rs`) when teams are on;
   `TaskStore` implementation in `fontaine-core` (`tasks.rs` next to `todos.rs`) with load on
   resume and warning through `Approver::warn`; built-in type toolsets in `registry.rs`.
7. **Hooks**: add the two events, their inputs and the `block` answer in
   `rust/fontaine-core/src/user_hooks.rs`, `HooksSection` in `config.rs` (it has
   `deny_unknown_fields`), `config_scope.rs` (tighten-only for project scope) and the plugin-hook
   mapping; call `task_completed` from `TaskUpdateTool` through the `QualityGate` trait seam
   (defined in `fontaine-agents`, implemented in core, injected like `RootLedger`) and
   `teammate_idle` in `watch_report` before `finish`.
8. **TUI**: idle status and the Tasks section in the agents panel (`rust/fontaine-tui`).
9. **Docs**: see Documentation.
10. **Tests**: see Testing Strategy.

## Files to Modify

- `rust/fontaine-agents/src/{tasks.rs (new), spawn.rs, tree.rs, registry.rs, root_ledger.rs, lib.rs}`
- `rust/fontaine-tools/src/descriptor.rs`
- `rust/fontaine-core/src/{agent/build.rs, tasks.rs (new), user_hooks.rs, permission_rules.rs, adjudication.rs, config.rs, config_catalog.rs, config_scope.rs, export.rs}`
- `rust/fontaine-core/src/agent/messaging.rs` (wake seam), `rust/fontaine-core/src/child_runner.rs` (non-recording resume), `rust/fontaine-core/src/agent.rs` (`clear_conversation`), `rust/fontaine-core/src/agent/rewind.rs`
- `rust/fontaine-tui/src/*` (agents panel, agent list, `ui/mod.rs`)
- `rust/fontaine-core/tests/choke_point.rs`, `rust/fontaine-agents/tests/seam.rs`, `rust/fontaine/tests/parallel_agents.rs`
- `docs/{agents,hooks,configuration,tools,tui,permissions,telemetry}.md`
- `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Reuse the agent tree vs a separate "team" runtime.** Claude Code runs teammates as independent
  sessions. Fontaine's subagents already have identity, names, budgets, messaging, resume and
  per-call gating; a second runtime would duplicate the permission-critical parts. Teammates are
  therefore subagents with an idle state.
- **Two task tools vs one `action` tool.** The descriptor table gives one risk class per tool and
  the read-only fast path depends on it; a single mixed tool would have to be `Arbitrary` and make
  every listing a gated, guardian-adjacent call.
- **Tasks in the session dir vs in-memory only.** In-memory would lose the board on resume; a file
  costs one small atomic write and lets the lead continue after a resume.
- **Idle teammates outside `max_parallel`.** Counting them would make a four-teammate team unable to
  run anything else; the cap is about concurrent model calls, which idle ones make none of.

## Decisions

- Teams are off by default behind `[agents] teams`, user/CLI scope only; a repository cannot turn them on.
- The lead is always `main`; one team per session; teammates cannot spawn teammates.
- `task_completed` / `teammate_idle` hooks can only `block`, never allow, and a hook failure warns and does not block; reason: quality gates add work, they must not wedge the team or loosen anything.
- The `task_completed` / `teammate_idle` events take no matcher; reason: they carry no tool name.
- Task text is untrusted agent-authored data, never guardian intent.
- No tmux/split-pane display and no per-teammate model override beyond what `agent` already has; both are Claude Code display/ergonomics features outside this issue.
- Deviation from Claude Code: teammates are Fontaine subagents (same process, same session tree), not independent sessions; reason: one permission pipeline, one budget tree.

## Documentation

- `docs/agents.md`: new "Agent teams" section (teammate flag, idle/wake, `send_message` to idle,
  task board and tools, idle hand-off, limits, resume behaviour); update the `send_message` text
  ("reaches only a running agent" gains the idle-teammate case).
- `docs/hooks.md`: the two new events, inputs, `block` answer, failure behaviour.
- `docs/configuration.md`: `[agents] teams`.
- `docs/tools.md`: `task_list`, `task_update`; `docs/permissions.md`: their class and tier;
  `docs/tui.md`: idle status and Tasks section; `docs/telemetry.md`: new events.
- `CHANGELOG.md` entry (added by the `pr` step).

## Testing Strategy

Unit and integration tests only, deterministic models, no live APIs.

- `tasks.rs` unit tests: create/caps/validation; claim only when pending, unowned and unblocked;
  **concurrent claim of one task has exactly one winner** (threads on one board); completing
  unblocks dependents; release; owner-only update; `blocked_by` naming a missing id is refused;
  restore from the store turns `InProgress` into `Pending`; ids never reused.
- `tree.rs` unit tests: a teammate's `Done` finish sets idle and a failed/killed/budget one does
  not; `deliver` to an idle teammate returns `DeliverError::Idle`; `tree.resume` clears `idle`;
  `deliver` to a plain finished agent still says `agent not running`.
- Wake (`SendMessageTool`, core seam): a message to an idle teammate resumes it; a wake refused by
  the cap, budget, kill or shutdown returns the reason and releases its tasks; `AlreadyRunning`
  queues. `release_owned_by` runs on a failed/killed/exhausted end and leaves an idle teammate's
  tasks owned.
- `seam.rs`: `teammate: true` refused when teams are off and when the caller is a subagent; both
  tools refuse a call with no agent id (`caller_of`); task text is rendered delimited and
  sender-marked.
- Non-recording resume: a peer message, hand-off note and `teammate_idle` hook reason wake a teammate
  without appending `Record::UserMessage` to the root ledger (guardian intent unchanged); a human
  prompt into its view still records. `/clear` and rewind refuse while a teammate is idle. The idle
  decision runs before `finish`: a hook block or hand-off keeps `any_running` true and queues no
  `Done` report.
- Hooks (`rust/fontaine-core/tests`): `task_completed` block keeps the task `InProgress` with the
  reason as the tool error; `teammate_idle` block resumes with feedback; a failing hook warns and
  does not block; a hook answer of `allow` is ignored.
- Permissions: `choke_point.rs` registers both tools and checks each has a risk class; `task_update`
  is asked in `manual`/`acceptEdits` and allowed at the non-guardian tier in `auto`, and a `deny`
  rule on it wins in every mode; `spawn_changes_state` treats a toolset with `task_list` as
  read-only and one with `task_update` as state-changing.
- `parallel_agents.rs` (headless, wiremock models): a lead spawns two teammates, they claim
  different tasks, one finishes and goes idle, a message wakes it, the idle hand-off claims the
  remaining task, and the run ends without waiting on idle teammates. Pins the silent-failure
  class: a double claim, and a budget refusal that must not leave a task owned by a dead agent.
- TUI: a `TestBackend` render test of the idle status and Tasks section (live layout checks stay
  in Manual Verification).

## Manual Verification

1. `fontaine` in a scratch repo with `[agents] teams = true`; ask the model to split a three-part
   job across two teammates with dependencies. Expected: Ctrl+T shows rows `idle` after their turn
   and the Tasks section updating. Not automated because the panel's look and feel needs eyes (see
   `tasks/design/testing.md`, "Reproducing terminal layout bugs", for a real tmux).

## Open Questions

- Should the idle hand-off be switchable off (`[agents] teams_auto_claim`)? Default proposal: no
  key, since budgets and the kill switch bound it; revisit only if live runs show waste.
