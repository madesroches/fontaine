# Session Activity Snapshot Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/579

## Overview

An external consumer (an agent scheduler) cannot tell "the model's turn ended" from "the instance
is idle": `stop` fires at the turn boundary while a 20-minute background test may still run. This
adds one authoritative **session-activity snapshot**, derived from the runtime's own turn, agent,
job, notification and human-wait state, and exposes it twice from the same value: a new
observational `activity_changed` hook that carries the complete snapshot plus the transition
reason, and an `activity` object in the custom status-line JSON. Existing `stop`, `subagent_stop`
and `teammate_idle` semantics, the TUI's `app.running` flag, and headless job termination are
untouched.

## Current State

The issue's file paths predate the SDK consolidation; the code now lives under
`rust/fontaine-sdk/src/`.

- **Hooks.** `HookEvent` (`core/config.rs:721`, `ALL` at `:764`, `config_key`, `wire_name`,
  `matcher_kind`, `handlers`, `project_scope`) lists 18 events. The runtime fires lifecycle events
  through `LifecycleHooks` (`core/user_hooks.rs`); `notify` (`:1836`) is the precedent for a
  fire-and-forget observational event (spawned on the tokio runtime, never awaited by the caller,
  `discards_system_message` at `:596`). `core/agent/hooks.rs` is where the runtime decides when each
  one fires. `stop` is fired from `AgentRuntime::turn_ended`, called after `run_turn` returns
  (`fontaine-tui/src/ui/turn.rs:1592`, `run.rs:486`).
- **Jobs.** `Jobs` (`tools/jobs.rs:793`) holds the registry behind one mutex
  (`Shared.inner`) and a `watch::Sender<u64>` (`changed`) bumped by `wake()`. `snapshot()` (`:988`)
  lists every job; `running_background()` (`:997`) lists the shells and monitors that count
  (`Job::counts`), which covers both `run_in_background` jobs and foreground commands promoted
  after a timeout. `Shared::finish` (`:593`) records the end *and* posts the completion note to the
  sink while still holding the registry lock, and posts nothing for `Kill::Model`, `Shutdown` and
  `Cap`. `terminate_all` / `force_stop_all` end jobs at `/clear` and teardown.
- **Notifications.** `TreeJobs` (`core/agent/setup.rs:681`) turns a `JobNote` into
  `AgentTree::post_to_root` → the root node's `inbox`. `AgentTree` (`agents/tree.rs`) has
  `any_running`, `running_children`, `has_agent_notifications`, `has_subagent_mail`,
  `take_notifications` (the drain), `discard_job_notifications` (`/clear`), and two `Notify`
  handles (`changed()`, `display_changed()`). The surfaces drain the root inbox through
  `AgentRuntime::notification_prompt()` (`core/agent.rs:693`), which is `async` (the notes pass the
  result-screening pipeline), so a drained note is in flight, in neither the inbox nor a turn, for
  an `await`.
- **Human waits.** `SessionState::wait_on_human(agent)` (`core/gate.rs:1779`) holds a counted
  guard and pokes the agent tree; `is_waiting(agent)` reads it. A child's `WaitingOnPermission`
  status derives from it (`core/child_runner.rs:666`). Only `core/permissions.rs:2293` (guardian
  unavailable) and `:2874` (permission prompt) take it today; `ask_user` (`core/questions.rs`,
  `HumanQuestions::ask`) and MCP elicitation (`tools/mcp/elicitation.rs`, `core/agent/mcp.rs`) do not.
- **Turns.** `run_turn` (`core/turn.rs:264`) is the one entry for a root turn on every surface
  (TUI, headless, Claude-compat). It has no "a turn is running" state: the TUI keeps `app.running`
  and headless keeps its own loop. The end-of-turn work (`turn_ended`, goal evaluation,
  `stop`-hook continuation) runs *after* `run_turn` returns.
- **Status line.** `core/statusline/mod.rs` builds the JSON payload for the `[statusline]` command
  (`afk`, `guardian`, `recent_verdicts`, …); `StatuslineSnapshot` (`statusline/snapshot.rs`) is the
  Rune script's view and already carries `running_agents`, `background_shells`,
  `background_monitors`. `fontaine-tui/src/ui/mod.rs:199` fills the TUI's `app` mirror each frame
  and `fontaine-tui/src/remote/event.rs:141` carries those counts to `fontaine attach`.
- **Docs.** `docs/hooks.md` (event table `:20`, matcher table `:75`, payload table `:327`),
  `docs/configuration.md` (`:986` hook list), `docs/tui.md` (status-line JSON table `:257`),
  `docs/agents.md` (background jobs), and the master plan's `## Decisions`.

## Design

### The value: `SessionActivity`

New module `core/activity.rs` (core may depend on `tools` and `agents`; neither imports it back).

```
SessionActivity {
    session_id: String,
    seq: u64,                          // increments on every published change
    state: ActivityState,              // Active | Waiting | Idle
    turn_running: bool,                // a root turn, its end-of-turn hooks, or a claimed notification turn
    waiting_on_human: bool,            // the root is blocked on a prompt, question or elicitation, or a running subagent on a permission prompt
    running_agents: usize,             // subagents whose status is running (incl. waiting on permission)
    running_background_jobs: usize,    // background shells + monitors still running
    running_monitors: usize,           // the monitor subset of the above (the split the TUI already has)
    pending_notifications: usize,      // undelivered inbox entries for the root and for live subagents
    queued_prompts: usize,             // human messages queued behind the running turn (the root queue only)
}
```

One `SessionActivity::to_json()` produces the object both consumers receive, so there is a single
definition (field names snake_case, `state` as `"active" | "waiting" | "idle"`).

**Aggregate state, defined once:**

1. `waiting` when `waiting_on_human`: some agent cannot progress until a human answers. It is a
   state of its own, never folded into `active` or `idle`, because a scheduler must tell "busy" from
   "stuck on a person". It wins over `active` even while jobs run.
2. else `active` when any of `turn_running`, `running_agents`, `running_background_jobs`,
   `pending_notifications`, `queued_prompts` is non-zero.
3. else `idle`.

**Held deliveries are not pending.** `pending_notifications` counts only notes some surface will
deliver. Root notes are excluded while `unattended_blocked()` (AFK stopped or breaker-paused: no
notification turn starts, so e.g. a killed-job note would sit undrained forever), and held subagent
mail is excluded likewise. The held flag is a `held` source in `Sources` read from the same state
`unattended_blocked()` reads, and a change of it pokes the tracker. In hosts that do not deliver
notifications between host messages (headless `-p` long-lived, Claude-compat, SDK `Session`), root
notes posted after `settle` returns Done are excluded from `active` too, via a per-host
`delivers_notifications_between_messages` flag on the tracker (TUI `true`; those hosts `false`;
default `false`). No new notification-turn behavior is added; documented in `docs/hooks.md`. Those
notes stay in the inbox and are drained by the next host message as today.

Plain `-p` headless (`HumanChannel::Absent`) and AFK never block on a human (asks resolve to deny),
so they never report `waiting`; Claude-compat and SDK hosts with an approver do, while the host decides. Excluded on purpose: idle wakeable subagents (their turn ended; they are
woken by mail, and mail counts as pending), `/schedule` tasks and wake-ups (future work, already a
separate status-line segment), and a goal waiting between check-ins.

### Reading it: `ActivityProbe`

The snapshot is **derived, never event-sourced**: a pure `compute(&Sources) -> SessionActivity`
over the authoritative stores, so "natural exit, model cancel, panel kill, `/clear`, teardown" need
no per-path hook, since each already changes the registry or tree and the next compute sees it.
`Sources` are `&Jobs`, `&AgentTree`, `&SessionState` (human waits), the steering queue, and the new
turn counter below. `waiting_on_human` is sourced from a new `SessionState::any_waiting()` over the
`waiting` map (`core/gate.rs:1779-1795`), because `AgentTree` sees only children, never the root.

**Read order is the false-idle fix:** jobs, then `running_agents`, then the turn counter, then the
inbox, then the turn counter again; either counter read non-zero means `turn_running`.
- `Shared::finish` records a job's end and posts its notice under the registry lock, so jobs are
  read before the inbox: a reader sees the job running or sees it ended *and* its note queued.
- Subagent reports are queued before the node leaves running (`agents/spawn.rs:557`,
  `tree.finish`), so `running_agents` is read before the inbox.
- `TurnClaim` is taken before the drain, so the counter is read after the inbox.
- Goal-reminder (`core/agent/goal.rs:455` → `post_to_root`) and held-waiter job end
  (`tools/jobs.rs` `WaitGuard::drop`, ~`:1257`) post to the inbox while a `TurnScope` is held and
  drop it afterwards, so the counter is also read before the inbox.
- An owner reminder posted from `slot_freed` outside any turn (`agents/spawn.rs:410-451,520-544`;
  `release_tasks_of` at `:389`) can follow the node leaving running when the last subagent is
  killed (report skipped, `spawn.rs:559`). The gap is held inside `agents`, which never imports
  `core`: the tree counts the not-yet-posted reminder as pending until it is posted, under the
  same lock that takes the node out of running.

This ordering is documented at the compute site and pinned by a test per handoff.

### Turn accounting: `TurnScope` and `TurnClaim`

`SessionState` gains an `ActivityTracker` holding a counter (`turns`) and a `Notify`. Two RAII
tokens bump it; each owns an `Arc` of the tracker rather than borrowing, so it can be held across
`&mut AgentRuntime` calls:

- `TurnScope`: taken inside `run_turn` (so every surface is covered even if one forgets), and held
  by the surfaces around the whole turn loop (`Ui::turn`/`guarded_turn`, `run_one`/`continue_on_stop`),
  so pre-turn compaction, usage-limit waits, `turn_ended`, goal evaluation and `stop`
  continuation gaps are not idle. Counter
  based, so scopes nest. Dropped on interruption (the future is dropped at an await).
- `TurnClaim`: taken in the `AgentRuntime::notification_prompt()` wrapper only (never in
  `child_runner::notification_prompt`, which also serves subagents via `join_news`), **before** it
  drains the inbox and only when the inbox is non-empty (headless `wait_for_news` polls every 100 ms,
  so an empty poll must not touch the counter), and returned with the prompt (leak-proof RAII, released when the turn starts or the surface drops it
  without running one). This closes the drained-but-not-yet-a-turn window, i.e. "no false-idle gap
  between the final job ending and its notification turn". `run_turn` upgrades a claim by taking its
  own scope before the claim drops. Headless carries the claim in `News::Prompt` and holds it across
  `keep_unsettled_turn` into `run_one` (`run.rs:566,:605,:797-803`); the TUI keeps it as a local
  through `self.turn`.

- Retract → next-turn path: a `TurnScope` is taken BEFORE `steering.retract()` and held until the
  next turn's own scope exists, in the TUI `converse_loop` (`fontaine-tui/src/ui/turn.rs:~992`,
  covering `review_afk_exits` `:827`, `keep_leftover_turn` `:849/:883` and queued `Command`/`Shell`
  entries), `harness_turn_tail` (`turn.rs:~420`), the goal paths (`ui/goal.rs:~70/:89`) and
  `remote_idle` (`ui/remote.rs:~606`). `queued_prompts` also counts the Claude-compat router's own
  queued inputs (`claude_compat/router.rs:331-341`, `inbox.queue`, in the bin crate): the
  `InputSource` trait (`run.rs:125`) gains `fn queued_count(&self) -> usize { 0 }`, which the router
  implements; `run_session` (`run.rs:~135`) publishes it into the tracker and pokes the tracker
  whenever it changes (on receive and on dequeue).
- `wake_for_inbox` (`agents/spawn.rs:688-697`) is held inside `agents`, which never imports `core`
  (so no `TurnScope` there): `AgentTree` keeps an in-flight-notes counter that
  `pending_notifications()` includes. An RAII guard from the tree bumps it before
  `take_notifications` and drops it only after `wake_idle` (`resume_inner` marks the node running)
  or, on `AlreadyRunning`/`Transient`, after the requeue, so drained notes are never in neither the
  inbox, the counter nor a running agent. The counter is read with the inbox in `compute`'s inbox
  step.

### Publishing: `ActivityMonitor`

One task per `AgentRuntime`, spawned at runtime build through `Handle::try_current()` like the other
background spawns (`AgentRuntime::new` is synchronous and called from test helpers), owns change
detection and the hook. `AgentRuntime` keeps an abort-on-drop handle, and the monitor is stopped
by teardown (which quiesces it first), so it cannot fire after `session_end` or outlive a TUI session switch or
an SDK `Session` drop:

- Wakes on: a new public `Jobs::subscribe()` (wrapping the existing `watch`), `AgentTree::changed()`
  and `display_changed()` (`wait_on_human` already pokes the tree), the tracker's own `Notify`
  (turn counter, steering queue changes).
- It registers every wake source before each compute (enable the `Notified` futures from
  `AgentTree::changed()`/`display_changed()` and the tracker, mark the jobs watch seen), because
  `AgentTree::changed` uses `notify_waiters` and stores no permit (precedent:
  `fontaine-tui/src/ui/mod.rs:855-862`).
- On each wake it recomputes, compares to the last published value ignoring `seq`, and if different
  increments `seq`, publishes on a `tokio::sync::watch<SessionActivity>` (what status line and
  tests read via `runtime.activity()`), and queues a hook delivery. **Deduplicated** by that
  comparison; a burst that returns to the previous state publishes nothing.
- **Reason** is derived from the field diff by fixed priority (`waiting_changed`, `turn_started`,
  `turn_ended`, `agent_started`, `agent_ended`, `job_started`, `job_ended`, `notification_queued`,
  `notification_delivered`, `queue_changed`), plus two explicit overrides: `clear`
  (set before the first mutation of `clear_conversation`, released only after `end_jobs` and the
  discards, so `clear_ended`, `settle_retired`, `discard_root_reminders` and `end_jobs` all carry
  it; releasing it forces one recompute and publish even if nothing diffs, so the reason is never
  lost to dedup and a stale override cannot leak into the next transition). Rewind
  (`/rewind`) uses the same override under the reason `clear`. `shutdown` is computed by teardown
  itself (see the hook's Teardown). While an override is set, wakes are recomputed but the
  override reason replaces the diff reason. Reasons are a closed enum
  with stable wire strings.
- Hook delivery is **serial and latest-wins**: one in-flight hook run at a time, and while it runs
  only the newest pending snapshot is kept, so a slow hook never reorders or backs up, and the
  final state is always delivered. Intermediate states can be skipped; each delivered payload is
  complete, so consumers need no history. `previous_state` and `reason` are relative to the last
  *delivered* payload: a skipped snapshot's reason is dropped and the delivered payload carries the
  reason computed against the last delivered snapshot (a forced override reason such as `clear`
  survives a skip by being carried onto the next delivery); `seq` still shows the gap.
- A job that exits between two wakes is still seen: compute reads the registry, not the signal.

### The hook: `activity_changed`

New `HookEvent::ActivityChanged` (config key `activity_changed`, wire name `ActivityChanged`).

- Observational like `notification`: handlers `command` and `http` only (a `prompt` hook would
  force model spend), run through a `pub(crate) async fn LifecycleHooks::activity_changed` like
  `session_end` (`core/user_hooks.rs:1699`), awaited by the monitor (which does its own spawning,
  one in flight) and by teardown; it takes `notify`'s handler set and discarded output, decides
  nothing (output ignored, added to
  `discards_system_message`). It loads from project scope (`ProjectScope::Loads`): it cannot
  loosen anything and forces no model calls.
- Matcher: the new aggregate `state` (`active`, `waiting`, `idle`), so a scheduler can hook only
  `idle`.
- Payload: the common fields (`session_id`, `cwd`, …) plus `hook_event_name`, `reason`,
  `previous_state`, and `activity` (the shared object above, including `seq` and its own
  `session_id`).
- Teardown: `AgentRuntime::fire_session_end` (`core/agent/hooks.rs:44-46`, which every exit uses)
  first quiesces the monitor (stops its wake loop, awaits any in-flight delivery, takes its last
  published value and `seq`), then computes the current activity and awaits one `activity_changed`
  (reason `shutdown`, `seq` continuing the monitor's) before `session_end`. The `shutdown`
  transition is always delivered, never dropped by dedup as "no change", and is never concurrent
  with a monitor delivery. A fire-and-forget task would be dropped with the runtime, hence the await.
  `fire_session_end` takes a delivery cap for that awaited transition. `finish_final_shutdown`'s
  branch becomes `timeout(slice, wait_stopped)` → transition (bounded by a slice of the remaining
  deadline) → `session_end`, replacing the `tokio::join!` arm for `session_end`. The non-final
  callers (`fontaine-tui/src/lib.rs:727`, `headless.rs:291`, `claude_compat/mod.rs:362`,
  `session.rs:777`) pass a fixed 1.5 s cap, `session_end`'s own default timeout. Reorder the TUI
  session switch (`lib.rs:726-730`) to stop agents (`kill_all` + `settled`) and jobs before
  `fire_session_end`, and add `kill_all` + `settled` at the headless input-error exit
  (`run.rs:1118-1123`), so the final `shutdown` snapshot is not active or `running_agents > 0`.

### The status line

`activity` joins the status-line JSON payload as the same `to_json()` object, read from
`runtime.activity()` at render time, in `build_statusline_payload`
(`fontaine-tui/src/ui/mod.rs:399-451`), which already holds the runtime. The monitor publishes after
the draw that follows a turn end and `refresh_interval_secs` is off by default, so the TUI loop
redraws the status line on `changed()` of a receiver stored once as a `Ui` field (set from
`runtime.activity()` next to `afk_stops`, `fontaine-tui/src/lib.rs:~389/592` and the `ui/tests.rs`
setup), borrowed `&mut` in the `select!` and marked seen with `borrow_and_update` (precedent:
`afk_stops`, `quota_rx`), so a publish made while the loop is not parked is not dropped and the arm
does not spin. Rune accessor exposure is
not part of this change.

## Implementation Steps

1. **`core/activity.rs`**: `ActivityState`, `ActivityReason`, `SessionActivity` (+ `to_json`,
   aggregate fn), `ActivityTracker` (turn counter + `Notify`), `TurnScope`, `TurnClaim`, and the
   pure `compute`. Unit tests (below).
2. **Sources.** `Jobs::subscribe()`; `AgentTree::pending_notifications()` (root inbox plus the
   inboxes of running/idle subagents) and `SessionState::any_waiting()`; `SessionState::activity()`
   accessor; steering queue length source (the root queue only) plus a change poke on every
   `Steering` mutation (`core/steering.rs`: delivery drain, `retract`, `retract_typed`,
   `drain_remote`, `requeue`, `queue_remote`, `queue` (`:161`)). `Steering` takes an optional handle to the tracker's `Notify` (from `state.activity()` at
   `core/agent.rs:372`; `Default` leaves it unset for tests; the Claude-compat router shares the
   `Arc` via attach). Poke the tracker/tree from `take_notifications`, `discard_job_notifications`
   and `discard_root_reminders` (`agents/tree.rs`), and from job registration/promotion
   (`tools/jobs.rs` ~`:846-886`, `:956`), which notify nothing today. Take `state.wait_on_human(agent)`
   around `HumanQuestions::ask`, and around the inner `elicit`/`elicit_url` calls in `HookElicitor`
   (`core/agent/mcp.rs:520-620`, after hooks run) with `wait_on_human(ROOT_AGENT)`; both guards are
   taken only when a human can answer: a human channel is present and `!state.afk_on()` (the
   permission sites' `no_human()` at `core/permissions.rs:1685-1693` is private and unreachable from
   these types). So `HumanChannel` (`setup.human_channel`, `core/agent.rs:152`) is passed into
   `HumanQuestions` and `HookElicitor` where they are built (`core/questions.rs`,
   `core/agent/mcp.rs`, `core/agent/setup.rs`, `core/agent.rs`).
3. **Turn accounting wiring.** Take a `TurnScope` inside `run_turn`; hold an outer one around the
   whole turn loop in the TUI (`fontaine-tui/src/ui/turn.rs`, covering `turn_ended` at `:1592`) and
   headless (`run_one`/`continue_on_stop`, covering `turn_ended` at `run.rs:331` and `:486`). In
   headless, scope all of `handle_message` (`run.rs:837`) and the goal loop including
   `stop_before_call`/`compact_if_needed` (`run.rs:717,:749,:979`) and `goal_prompt`. In the TUI,
   scope prompt submission from the `user_prompt_submit` hook (`ui/turn.rs:~850,:884,:1618-1632`)
   through `Ui::turn`, including skill expansion (`turn.rs:610-634`) and the MCP prompt fetch, and
   wrap `scheduled_turn` from `scheduled_prompt`/`screened_text` on (the scheduler fire is already
   consumed at `ui/mod.rs:903-906`). Add
   scopes for TUI goal evaluation (`fontaine-tui/src/ui/goal.rs`), headless goal evaluation
   (`run.rs:984`), `/compact` and `/btw` (`ui/commands/context.rs`) and `!` shell lines
   (`ui/commands/workspace.rs`). Claude-compat runs through `run_session` and has no turn site. Hold the retract →
   next-turn scopes and the `wake_for_inbox` in-flight-notes guard (see Turn accounting), and count the
   Claude-compat router's queued inputs in `queued_prompts`. Add the headless input-error
   `kill_all` and move the TUI session-switch stop ahead of `fire_session_end`. Make
   the `AgentRuntime::notification_prompt` wrapper take/return a `TurnClaim`; update its callers, including
   `fontaine-sdk/tests/core_scanner.rs` and `tests/core_lsp.rs`.
4. **`ActivityMonitor`** spawned from runtime assembly (`core/agent/build.rs`/`setup.rs`) through
   `Handle::try_current()`, with an abort-on-drop handle on `AgentRuntime` and a `quiesce` that teardown
   calls, `AgentRuntime::activity()` (`watch::Receiver`), the `clear` reason override
   (`clear_conversation` and rewind, set before the first mutation, released after `end_jobs` and the discards with a forced recompute/publish) and the teardown transition in `fire_session_end` (`core/agent/hooks.rs`,
   `core/agent/shutdown.rs`). `begin_final_shutdown`'s `abort_all`/`force_stop_all` wake the monitor,
   so it is quiesced before the transition.
5. **Hook plumbing.** `HookEvent::ActivityChanged` in every `match` and `ALL` (now 19),
   `MatcherKind::Values(["active","waiting","idle"])`, `COMMAND_HTTP`, `ProjectScope::Loads`,
   `discards_system_message`; `LifecycleHooks::activity_changed`; the config catalog row
   `row!("hooks.activity_changed", …)` in `core/config_catalog.rs`, and the per-event project-scope rule in
   `core/config_scope.rs` (~`:215-232`) with its test-table entry (~`:600-650`); the hooks JSON schema
   is generated from `ALL`. Adding the event to `ALL` makes it registrable from plugins
   (`core/plugins.rs:1285`) and SDK `initialize.hooks` (`claude_compat/hooks.rs:71`); document that in
   `docs/plugins.md` and `docs/hooks.md`.
6. **Status line.** `activity` in the payload struct (`statusline/mod.rs`), read from
   `runtime.activity()` in `build_statusline_payload`; store the receiver as a `Ui` field set once
   from `runtime.activity()` next to `afk_stops` (`fontaine-tui/src/lib.rs:~389/592`, and in
   `ui/tests.rs` setup) and add a `changed()` arm on it, marked seen with `borrow_and_update`, to
   the TUI loop's `select!` (`fontaine-tui/src/ui/mod.rs:~946-1020`) calling
   `trigger_statusline(StatuslineTrigger::Draw)`.
7. **Docs and Decisions** (below).
8. `cargo fmt`, `clippy -D warnings`, `cargo test`, rustdoc.

## Files to Modify

- New: `rust/fontaine-sdk/src/core/activity.rs`; `core.rs` (module)
- `rust/fontaine-sdk/src/tools/jobs.rs` (`subscribe`, registration/promotion pokes), `agents/tree.rs`
  (queries, drain pokes)
- `rust/fontaine-sdk/src/core/questions.rs`, `core/agent/mcp.rs` (`wait_on_human`; `HumanChannel` plumbing via `core/agent/setup.rs`, `core/agent.rs`),
  `core/steering.rs` (change pokes, tracker `Notify` handle)
- `rust/fontaine-sdk/src/core/gate.rs` (tracker on `SessionState`), `core/turn.rs` (`TurnScope`),
  `core/agent.rs` (`notification_prompt`, `activity()`, `clear_conversation`),
  `core/agent/shutdown.rs`, `core/agent/build.rs` / `setup.rs` (monitor), `core/agent/hooks.rs`,
  `core/agent/goal.rs`, `session.rs`; `rust/fontaine-sdk/src/agents/spawn.rs`
- `rust/fontaine-sdk/src/core/config.rs`, `config_catalog.rs`, `config_scope.rs`, `user_hooks.rs`
  (event + method)
- `rust/fontaine-sdk/tests/core_scanner.rs`, `tests/core_lsp.rs` (`notification_prompt` callers)
- `rust/fontaine-sdk/src/core/statusline/mod.rs`, `rust/fontaine-sdk/src/run.rs`
- `rust/fontaine-tui/src/ui/{mod,turn,goal}.rs`, `ui/commands/{context,workspace}.rs`,
  `fontaine-tui/src/lib.rs`, `fontaine-tui/src/ui/remote.rs`
- `rust/fontaine/src/claude_compat/mod.rs` (final transition), `claude_compat/router.rs` (`queued_count`), `rust/fontaine/src/headless.rs`
- `docs/hooks.md`, `docs/configuration.md`, `docs/tui.md`, `docs/tools.md`, `docs/plugins.md`;
  `CHANGELOG.md`;
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **`TurnClaim` over a debounce/settle timer.** A timer makes idle detection probabilistic and
  tests time-dependent; an explicit claim is deterministic.

## Decisions

- 2026-11-21: new Fontaine-only `activity_changed` hook event, observational, `command`/`http` only.
- 2026-11-21: state names are `active`, `waiting`, `idle`.
- 2026-11-21: `waiting` takes precedence over `active`.
- 2026-11-21: excluded from activity: idle wakeable subagents, `/schedule` tasks, goal check-ins.
- 2026-11-21: `activity_changed` loads from project scope (cannot loosen anything, forces no model calls).
- 2026-11-21: hook delivery is serial and latest-wins; each payload is the complete snapshot.
- 2026-11-21: `activity_changed` is registrable from plugins and SDK `initialize.hooks`.
- 2026-11-21: `session_end` now fires after agents and jobs are stopped on the TUI session switch
  and the headless input-error exit (it fired before the stop at `fontaine-tui/src/lib.rs:726`), so
  the final snapshot is idle.
- 2026-11-21: the awaited `shutdown` transition of a non-final `fire_session_end` is capped at 1.5 s
  (`session_end`'s default timeout); final shutdown slices it from the remaining deadline.
- 2026-11-21: AFK: the subagent wake window is closed inside `agents` by an in-flight-notes counter on `AgentTree` (RAII around take → wake → requeue) included in `pending_notifications()`, not a core `TurnScope`, so `agents` never imports `core`.
- 2026-11-21: AFK: the `clear` override is set before the first mutation of `clear_conversation`, released after `end_jobs` and the discards with a forced recompute/publish even if nothing diffs; rewind uses reason `clear`.
- 2026-11-21: AFK: notes no surface will deliver are not `pending_notifications` (root and subagent notes while `unattended_blocked()`; root notes between host messages in headless/Claude-compat/SDK `Session` via a per-host `delivers_notifications_between_messages` flag, TUI true, others false); no new notification-turn behavior is added.
- 2026-11-21: AFK: under latest-wins delivery `previous_state` and `reason` are relative to the last delivered payload; a skipped snapshot's reason is dropped and `seq` shows the gap.
- 2026-11-21: AFK: Claude-compat queued inputs reach the compute through `InputSource::queued_count()` (default 0), published into the tracker by `run_session` and poked on change.
- 2026-11-21: AFK: turn-scope coverage is proven by mechanism tests (scope/claim nesting, claim upgrade, retract read-order) plus a debug-build `debug_assert` tripwire for turns, compaction and model requests run with no scope held, not per-site never-idle sampling.
- 2026-11-21: AFK: `Steering` (including `queue`) pokes the tracker through an optional `Notify` handle from `state.activity()`, unset under `Default`; the router shares it via attach.
- 2026-11-21: AFK: `HumanChannel` is passed into `HumanQuestions` and `HookElicitor`; their `wait_on_human` guards require a human channel and `!state.afk_on()`.

## Documentation

- `docs/hooks.md`: the `activity_changed` row in the event table, matcher values, payload fields
  (`reason`, `previous_state`, `activity`), delivery semantics (background, serial, latest-wins,
  teardown; a Claude-compat SDK-registered `activity_changed` misses the shutdown transition when the
  session ends by stdin EOF, `claude_compat/router.rs:194` returns `Disconnected`), the "Hooks a
  repository defines" project-scope table, the "SDK callback hooks" section, SDK/plugin registrability, what "active/waiting/idle" mean and what is excluded, and a scheduler example that
  hooks `idle`.
- `docs/tui.md`: the `activity.*` rows in the status-line JSON table.
- `docs/configuration.md`: add the event to the hook list at `:986` and to project-scope notes.
- `docs/tools.md` "## Background jobs": one line that `stop` is not idleness and points to the hook.
- `docs/plugins.md`: plugins can register `activity_changed`.
- Master plan `## Decisions` (Extensibility / TUI subsections): the lines in this plan's Decisions.
  `CHANGELOG.md` entry, noting the `HookEvent` variant addition (exhaustive stable enum) as breaking.

## Testing Strategy

All deterministic, no live APIs. Unit tests in `core/activity.rs` and integration tests in
`fontaine-sdk/tests/` (existing `tests/jobs.rs` fakes the sink and wait) with paused tokio time.

- Aggregate: every combination of the fields maps to the right state; `waiting` beats `active`;
  `to_json` shape is pinned (the shared object both consumers get).
- Explicit background start → active; after the answer and `stop`, still active while the job
  runs; idle only after it ends (the issue's headline case).
- Timeout-promoted foreground command → active the same way.
- Several jobs and agents → idle only after the last settles **and** the last notification is
  delivered and its turn finishes; the final-job-ends → notification-turn handoff never reports
  idle (read-order tests pinning each handoff: job end, held-waiter interruption, subagent
  report; plus a `TurnClaim` test that drains the inbox, awaits, then starts the turn, sampling at
  every step).
- Wake sources: a wake arriving between register and compute is not lost; `take_notifications`,
  `discard_job_notifications`, `discard_root_reminders`, job registration/promotion and every `Steering` mutation
  (including `queue`) each republish.
- Turn-scope mechanism (no cheap seam exists to sample a real site mid-flight, and a latest-wins
  hook cannot prove never-idle): `TurnScope`/`TurnClaim` nesting, claim upgrade into a scope in
  `run_turn`, the retract read-order (scope taken before `steering.retract()`), and a debug-build
  tripwire (`debug_assert`) that fires when `run_turn`, compaction or a model request runs with no
  scope held. Site tests are kept only where a real seam exists (headless `run_one`/`handle_message`,
  TUI turn loop). A `TurnClaim` is never taken by an empty inbox poll or by the subagent
  `join_news` path. The router's queued inputs count in `queued_prompts` (via
  `InputSource::queued_count`), and an `agents`-level test pins the `wake_for_inbox`
  take → wake → requeue handoff through the in-flight-notes counter (`pending_notifications() > 0`
  at every step, including the `Transient` requeue; never `running_agents == 0` with an empty inbox
  and a zero counter).
- Held deliveries and hosts: AFK stop that kills a background job → state `idle` once the job ends
  (its note is held, not pending); resuming AFK delivers it and the state is `active` until its
  turn ends; a root note posted between host messages in headless/Claude-compat/SDK `Session` leaves
  the state `idle`, and in the TUI keeps it `active` until delivered; `clear` and rewind carry reason
  `clear` even when nothing diffs; under a slow hook a skipped snapshot's reason is dropped and
  `previous_state` is the last delivered one.
- Shutdown/exit: the final transition is delivered once (continuing the monitor's `seq`, never
  deduped away, never concurrent with a monitor delivery, including a job or agent ending
  concurrently during teardown), inside `fire_session_end`, after job stop
  and before `session_end` at shutdown, headless, Claude-compat, embedded `Session` and TUI session
  switch; a slow `activity_changed` hook cannot starve `session_end`, and a job that never finishes does
  not either (the pre-transition `wait_stopped` is bounded by its own slice; non-final callers cap the transition
  at 1.5 s); the session-switch and
  headless input-error final snapshots show no running agents.
- Monitor lifetime: built without a tokio runtime it does not panic; it does not fire after
  `session_end`, and does not outlive a dropped runtime; a job or agent ending during teardown
  neither starts a second delivery nor is lost.
- Cancellation paths: model `job_cancel` (no note posted), panel `kill_by_user`, natural exit with
  completion consumed through `job_output`, `/clear` (`end_jobs` + discard), shutdown, each
  updating the snapshot with the right reason.
- Waiting: a permission prompt (root and subagent) sets `waiting`; an `ask_user` question and an
  MCP elicitation prompt each set it (a subagent `ask_user` case: a user-defined agent type
  inheriting `ask_user_question` can ask, `agents/registry.rs:257-291,340`, guard taken with the
  child's id; MCP elicitation goes through the session-wide `HookElicitor`, which receives no agent
  id, and is attributed to `ROOT_AGENT`); none sets it when
  no human can answer; answering restores the previous state; a cancelled prompt drops it (guard).
- Monitor: dedup (a burst returning to the same state publishes nothing), `seq` monotonic,
  latest-wins hook delivery with a slow hook, session_id carried.
- Hook: config parse of the new event, `command`/`http` accepted and `prompt` rejected, matcher on
  state, project-scope load, payload fields, system message discarded.
- Regression pins: `stop` still fires at the same point with the same payload (existing hook
  tests), subagent idle/wake tests unchanged, headless integration tests still prove running jobs
  are terminated at run end.
- Status line: payload includes `activity` identical to the hook object, and follows a late publish
  (the loop redraws on `activity().changed()`).

## Manual Verification

1. In a real tmux, run `fontaine` with a `[[hooks.activity_changed]]` command that appends its stdin
   to a file, and ask the model to start `sleep 60` in the background and answer. Expect `active`
   after the answer's `stop`, and `idle` about a minute later after the notification turn ends.
   Not automated: the real TUI event loop and the real process spawn together are not reachable by
   the deterministic fakes, and a stall would be obvious in the log.

## Open Questions

None the code cannot settle; the choices above are recorded in `## Decisions`.
