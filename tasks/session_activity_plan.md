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
    waiting_on_human: bool,            // the root or any running subagent is blocked on a human answer
    running_agents: usize,             // subagents whose status is running (incl. waiting on permission)
    running_background_jobs: usize,    // background shells + monitors still running
    running_monitors: usize,           // the monitor subset of the above (the split the TUI already has)
    pending_notifications: usize,      // undelivered inbox entries for the root and for live subagents
    queued_prompts: usize,             // human messages queued behind the running turn
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

Plain `-p` headless (`HumanChannel::Absent`) and AFK never block on a human (asks resolve to deny),
so they never report `waiting`; Claude-compat and SDK hosts with an approver do, while the host decides. Excluded on purpose: idle wakeable subagents (their turn ended; they are
woken by mail, and mail counts as pending), `/schedule` tasks and wake-ups (future work, already a
separate status-line segment), and a goal waiting between check-ins.

### Reading it: `ActivityProbe`

The snapshot is **derived, never event-sourced**: a pure `compute(&Sources) -> SessionActivity`
over the authoritative stores, so "natural exit, model cancel, panel kill, `/clear`, teardown" need
no per-path hook, since each already changes the registry or tree and the next compute sees it.
`Sources` are `&Jobs`, `&AgentTree`, `&SessionState` (human waits), the steering queue, and the new
turn counter below.

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

This ordering is documented at the compute site and pinned by a test per handoff.

### Turn accounting: `TurnScope` and `TurnClaim`

`SessionState` gains an `ActivityTracker` holding a counter (`turns`) and a `Notify`. Two RAII
tokens bump it:

- `TurnScope`: taken inside `run_turn` (so every surface is covered even if one forgets), and held
  by the surfaces around the whole turn loop (`Ui::turn`/`guarded_turn`, `run_one`/`continue_on_stop`),
  so pre-turn compaction, usage-limit waits, `turn_ended`, goal evaluation and `stop`
  continuation gaps are not idle. Counter
  based, so scopes nest. Dropped on interruption (the future is dropped at an await).
- `TurnClaim`: taken by `AgentRuntime::notification_prompt()` **before** it drains the inbox and
  returned with the prompt (leak-proof RAII, released when the turn starts or the surface drops it
  without running one). This closes the drained-but-not-yet-a-turn window, i.e. "no false-idle gap
  between the final job ending and its notification turn". `run_turn` upgrades a claim by taking its
  own scope before the claim drops.

`app.running` is not read or written by any of this; interruption, queueing, scheduling, spinner
and turn timing are unchanged.

### Publishing: `ActivityMonitor`

One task per `AgentRuntime`, spawned at runtime build, owns change detection and the hook:

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
  `notification_delivered`, `queue_changed`), plus two explicit overrides the runtime sets: `clear`
  (`clear_conversation`, after `end_jobs`) and `shutdown` (teardown). Reasons are a closed enum
  with stable wire strings.
- Hook delivery is **serial and latest-wins**: one in-flight hook run at a time, and while it runs
  only the newest pending snapshot is kept, so a slow hook never reorders or backs up, and the
  final state is always delivered. Intermediate states can be skipped; each delivered payload is
  complete, so consumers need no history.
- A job that exits between two wakes is still seen: compute reads the registry, not the signal.

### The hook: `activity_changed`

New `HookEvent::ActivityChanged` (config key `activity_changed`, wire name `ActivityChanged`).

- Observational like `notification`: handlers `command` and `http` only (a `prompt` hook would
  force model spend), run in the background through a `LifecycleHooks::activity_changed` method
  modelled on `notify`, never block, decide nothing (output ignored, added to
  `discards_system_message`). It loads from project scope (`ProjectScope::Loads`): it cannot
  loosen anything and forces no model calls.
- Matcher: the new aggregate `state` (`active`, `waiting`, `idle`), so a scheduler can hook only
  `idle`.
- Payload: the common fields (`session_id`, `cwd`, …) plus `hook_event_name`, `reason`,
  `previous_state`, and `activity` (the shared object above, including `seq` and its own
  `session_id`).
- Fired from the monitor, not from a turn path, so `stop`, `subagent_stop` and `teammate_idle`
  ordering and timing are untouched.
- Teardown: the shutdown path (`core/agent/shutdown.rs`) terminates jobs, recomputes, and awaits
  one bounded `activity_changed` (reason `shutdown`) before `session_end`, because a fire-and-forget
  task would be dropped with the runtime. The bound is the hook's own timeout. `finish_final_shutdown`
  (`core/agent/shutdown.rs:44-52`) runs `session_end` in a `tokio::join!` beside `jobs.wait_stopped`;
  restructure it to await the job stop, recompute, deliver the shutdown transition, then `session_end`.
  Every other exit does the same final-transition step: headless (`terminate_all` at
  `run.rs:1121/1205/1240`, then `fire_session_end("other")` at `fontaine/src/headless.rs:291`),
  Claude-compat (`claude_compat/mod.rs:362`), the embedded `Session` (`session.rs:777`) and the TUI
  session switch (`fontaine-tui/src/lib.rs:727`, `session_end("resume")`).

### The status line

`activity` joins the status-line JSON payload as the same `to_json()` object, read from
`runtime.activity()` at render time, in `build_statusline_payload`
(`fontaine-tui/src/ui/mod.rs:399-451`), which already holds the runtime. Rune accessor exposure is
not part of this change.

## Implementation Steps

1. **`core/activity.rs`**: `ActivityState`, `ActivityReason`, `SessionActivity` (+ `to_json`,
   aggregate fn), `ActivityTracker` (turn counter + `Notify`), `TurnScope`, `TurnClaim`, and the
   pure `compute`. Unit tests (below).
2. **Sources.** `Jobs::subscribe()`; `AgentTree::pending_notifications()` (root inbox plus the
   inboxes of running/idle subagents) and a "any child on permission" query beside `running_ids`;
   `SessionState::activity()` accessor; steering queue length source plus a change poke on
   enqueue/dequeue. Poke the tracker/tree from `take_notifications`, `discard_job_notifications`
   and `discard_root_reminders` (`agents/tree.rs`), and from job registration/promotion
   (`tools/jobs.rs` ~`:846-886`, `:956`), which notify nothing today. Take `state.wait_on_human(agent)`
   around `HumanQuestions::ask` and the MCP elicitation prompt.
3. **Turn accounting wiring.** Take a `TurnScope` inside `run_turn`; hold an outer one around the
   whole turn loop in the TUI (`fontaine-tui/src/ui/turn.rs`, covering `turn_ended` at `:1592`) and
   headless (`run_one`/`continue_on_stop`, covering `turn_ended` at `run.rs:331` and `:486`); add
   scopes for TUI goal evaluation (`fontaine-tui/src/ui/goal.rs`), headless goal evaluation
   (`run.rs:984`), `/compact` and `/btw` (`ui/commands/context.rs`) and `!` shell lines
   (`ui/commands/workspace.rs`). Claude-compat runs through `run_session` and has no turn site. Make
   `notification_prompt` take/return a `TurnClaim`; update its callers, including
   `fontaine-sdk/tests/core_scanner.rs` and `tests/core_lsp.rs`.
4. **`ActivityMonitor`** spawned from runtime assembly (`core/agent/build.rs`/`setup.rs`),
   `AgentRuntime::activity()` (`watch::Receiver`), and the shutdown/clear reason overrides
   (`core/agent/shutdown.rs`, `clear_conversation`).
5. **Hook plumbing.** `HookEvent::ActivityChanged` in every `match` and `ALL` (now 19),
   `MatcherKind::Values(["active","waiting","idle"])`, `COMMAND_HTTP`, `ProjectScope::Loads`,
   `discards_system_message`; `LifecycleHooks::activity_changed`; the config catalog row
   `row!("hooks.activity_changed", …)` in `core/config_catalog.rs` and any hooks JSON/schema surface
   listing events. Adding the event to `ALL` makes it registrable from plugins
   (`core/plugins.rs:1285`) and SDK `initialize.hooks` (`claude_compat/hooks.rs:71`); document that in
   `docs/plugins.md` and `docs/hooks.md`.
6. **Status line.** `activity` in the payload struct (`statusline/mod.rs`), read from
   `runtime.activity()` in `build_statusline_payload`.
7. **Docs and Decisions** (below).
8. `cargo fmt`, `clippy -D warnings`, `cargo test`, rustdoc.

## Files to Modify

- New: `rust/fontaine-sdk/src/core/activity.rs`; `core.rs` (module)
- `rust/fontaine-sdk/src/tools/jobs.rs` (`subscribe`, registration/promotion pokes), `agents/tree.rs`
  (queries, drain pokes)
- `rust/fontaine-sdk/src/core/questions.rs`, `tools/mcp/elicitation.rs`, `core/agent/mcp.rs`
  (`wait_on_human`)
- `rust/fontaine-sdk/src/core/gate.rs` (tracker on `SessionState`), `core/turn.rs` (`TurnScope`),
  `core/agent.rs` (`notification_prompt`, `activity()`, `clear_conversation`),
  `core/agent/shutdown.rs`, `core/agent/build.rs` / `setup.rs` (monitor), `core/agent/hooks.rs`,
  `core/agent/goal.rs`, `session.rs`
- `rust/fontaine-sdk/src/core/config.rs`, `config_catalog.rs`, `user_hooks.rs` (event + method)
- `rust/fontaine-sdk/tests/core_scanner.rs`, `tests/core_lsp.rs` (`notification_prompt` callers)
- `rust/fontaine-sdk/src/core/statusline/mod.rs`, `run.rs`
- `rust/fontaine-tui/src/ui/{mod,turn,goal}.rs`, `ui/commands/{context,workspace}.rs`,
  `fontaine-tui/src/lib.rs`
- `rust/fontaine/src/claude_compat/mod.rs` (final transition), `rust/fontaine/src/headless.rs`
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

## Documentation

- `docs/hooks.md`: the `activity_changed` row in the event table, matcher values, payload fields
  (`reason`, `previous_state`, `activity`), delivery semantics (background, serial, latest-wins,
  teardown), SDK/plugin registrability, what "active/waiting/idle" mean and what is excluded, and a scheduler example that
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
  `discard_job_notifications`, `discard_root_reminders`, and job registration/promotion each
  republish.
- Per-site turn scopes: each of the TUI turn loop, headless `run_one`/`continue_on_stop`, TUI and
  headless goal evaluation, `/compact`, `/btw` and `!` shell lines holds a scope across the whole
  site (sampled never idle mid-site).
- Shutdown/exit: the final transition is delivered after job stop and before `session_end` at
  shutdown, headless, Claude-compat, embedded `Session` and TUI session switch.
- Cancellation paths: model `job_cancel` (no note posted), panel `kill_by_user`, natural exit with
  completion consumed through `job_output`, `/clear` (`end_jobs` + discard), shutdown, each
  updating the snapshot with the right reason.
- Waiting: a permission prompt, an `ask_user` question and an MCP elicitation prompt (root and
  subagent) each set `waiting`; answering restores the previous
  state; a cancelled prompt drops it (guard).
- Monitor: dedup (a burst returning to the same state publishes nothing), `seq` monotonic,
  latest-wins hook delivery with a slow hook, session_id carried.
- Hook: config parse of the new event, `command`/`http` accepted and `prompt` rejected, matcher on
  state, project-scope load, payload fields, system message discarded.
- Regression pins: `stop` still fires at the same point with the same payload (existing hook
  tests), subagent idle/wake tests unchanged, headless integration tests still prove running jobs
  are terminated at run end.
- Status line: payload includes `activity` identical to the hook object.

## Manual Verification

1. In a real tmux, run `fontaine` with a `[[hooks.activity_changed]]` command that appends its stdin
   to a file, and ask the model to start `sleep 60` in the background and answer. Expect `active`
   after the answer's `stop`, and `idle` about a minute later after the notification turn ends.
   Not automated: the real TUI event loop and the real process spawn together are not reachable by
   the deterministic fakes, and a stall would be obvious in the log.

## Open Questions

None the code cannot settle; the choices above are recorded in `## Decisions`.
