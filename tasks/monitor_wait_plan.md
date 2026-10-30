# Monitor Wait Primitive Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/304

## Overview

`job_output(id, wait_secs)` refuses monitor ids, so a model waiting on a monitor spins on
immediate errors or unrelated polling commands. This plan makes `job_output` a bounded, wait-only
primitive for monitors: it blocks until the monitor posts a notification, ends, or the wait
expires, and reports status without ever carrying output bytes — output and the final notice stay
on the notification path, delivered exactly once.

## Current State

- `Jobs::output` (`rust/fontaine-tools/src/jobs.rs:898`) returns `Err(JobError::Monitor)` before
  any wait when the job has a `monitor` (`jobs.rs:909`); the wording is in `JobError`'s Display
  (`jobs.rs:89`). The behavior is locked in by
  `a_monitor_is_refused_by_job_output_but_listed` (`jobs/tests.rs:386`) and by the integration
  test `a_monitor_delivers_its_lines_as_notifications` (`rust/fontaine-tools/tests/jobs.rs:227`).
- A monitor's lines accumulate in `Monitor.pending` on each `Shared::append` (`jobs.rs:445`);
  `Shared::monitor_tick` (`jobs.rs:464`) batches them into a `JobNote` posted to the `JobSink`
  (half-second batch, one note per second, ≤200 lines, flood-kill at 1,000 notes). `monitor_tick`
  advances `job.cursor` past sent lines (`jobs.rs:497`) so `job_cancel`'s read does not repeat
  them; it does **not** call `Shared::wake`.
- `Shared::finish` (`jobs.rs:511`) flushes the buffer into `monitor.pending` and, for a background
  job or monitor with no waiter, posts the end note and marks the job `reported` with the cursor
  at the buffer end. With waiters present it instead sets `job.held = true`
  (`jobs.rs:544`); `WaitGuard::drop` (`jobs.rs:1071`) decrements `waiters` and, when the last
  waiter leaves without taking the news, posts the held end note exactly once. This is the
  exactly-once machinery the wait will reuse.
- Job notes reach the model through core's `TreeJobs` sink (`fontaine-core/src/agent/setup.rs:356`),
  which queues them in the `AgentTree`; `NotificationStage`
  (`fontaine-core/src/child_runner.rs:50`) appends queued notes to the next tool result the agent
  reads — possibly the wait call's own result.
- The `job_output` tool (`rust/fontaine-tools/src/jobs/tools.rs:119`) describes itself as the
  bounded-wait mechanism ("never poll with `sleep`") but exempts monitors; `docs/tools.md`
  documents the refusal in the `## Monitor` section ("`job_output` on a monitor is refused",
  `docs/tools.md:229`) and the `job_output` bullet in `## Background shell jobs`
  (`docs/tools.md:187`).
- The `changed` watch (`jobs.rs:426`) is the existing wake channel: appends set a `dirty` flag the
  job thread flushes at most every `WAKE` (50 ms, `jobs.rs:49`), and `finish` calls `wake()`.
- Issue #301 reports the same missing primitive for background subagents (`agent-*` ids, wait in
  `fontaine-agents/src/tree.rs`). It is out of scope here; the wait/notify split designed below is
  the pattern that plan should reuse.

## Design

### Semantics

`job_output` on a monitor id never returns output. It waits up to `wait_secs` (unchanged cap of
60 s; `wait_secs` omitted or 0 returns the status immediately — no error, so the observed
spin-on-error stops even for zero waits) and returns one of:

- **New notification posted.** A monitor note went out since the call started: the result says the
  monitor has new output and that its lines arrive as notifications. The note itself is already in
  the notification queue and is appended to this result or a subsequent one by
  `NotificationStage` — the model does not need to poll again to receive the lines.
- **Ended.** The monitor ended: the result says how (`EndInfo::describe`) and after how long, and
  that its final output arrives as a notification. The end note is never taken by the wait; the
  existing `held`/`WaitGuard` machinery delivers it exactly once — including when the user kills
  the monitor from the agents panel mid-wait, where `finish`'s `Kill::User` arm must hold the end
  note instead of posting its short user-kill variant (below).
- **Timeout.** The wait expired while the monitor runs: a successful "still running, Ns old"
  status, never an error.

### Registry changes (`rust/fontaine-tools/src/jobs.rs`)

- Delete the `JobError::Monitor` variant and its Display arm and the refusal branch in
  `Jobs::output`.
- Split the wait: `Jobs::output` keeps the read path for plain jobs unchanged, and routes monitor
  jobs to a new wait-only loop alongside it. The monitor loop:
  - snapshots `monitor.posted` (the existing flood counter, `jobs.rs:338`) as its watermark and
    registers a `WaitGuard` so `finish` holds the end note while it is parked;
  - on each `changed` wake (and once at entry), returns when `job.ended.is_some()`,
    `monitor.posted > watermark`, or the deadline passes; otherwise re-arms the watch;
  - never touches `cursor`, `reported`, or `held`, and never builds a `Chunk` — output state is
    the notification path's alone. `job_cancel`'s cursor accounting is unaffected.
- `Shared::monitor_tick` calls `Shared::wake()` after posting a note, so a wait parked on the
  watch wakes at the post rather than at the job thread's next periodic flush (the `dirty`/`WAKE`
  path can be up to a second stale when a batch posts on the interval timer alone).
- Return type: `Jobs::output` currently yields `Read`. Introduce a small sum so the tool layer can
  render the two shapes without flag fields on `Read`, e.g.
  `pub enum JobOutput { Read(Read), MonitorWait { id: String, age: Duration, end: Option<EndInfo>, note_posted: bool, already_reported: bool } }`,
  with `Jobs::output` returning `Result<JobOutput, JobError>`. `already_reported` covers a wait on
  an already-ended, already-notified monitor: the status says its final output was already
  delivered.
- `Shared::finish`'s `Kill::User` arm gains a wait interaction it never needed (the refusal meant
  no monitor waiter could exist): when the killed job is a monitor with a parked waiter
  (`waiters > 0`), it sets `held = true` instead of posting the short user-kill note, so the
  leaving `WaitGuard` posts the standard end note — which says "killed by the user" through
  `EndInfo::describe` and carries the tail — exactly once. With no waiter, a monitor instead
  posts that same standard end note and is marked `reported = true` with the cursor at the
  buffer end: the end note carries the tail, and a later `job_cancel` finds the cursor at the
  buffer end and returns only the end marker, no output. Plain background jobs keep today's
  short note, `reported = false` and all.
- `MonitorWait::already_reported` is `ended.is_some() && reported` — without the read path's
  `user_killed` exclusion, because a wait never carries the tail: once the end note went out,
  the model has been told all it will get.
- `WaitGuard::drop` is unchanged: last waiter leaving with the end note still held posts it. This
  is what keeps the final output and end notification exactly-once when the wait observes the end
  and returns — the guard drops after the result is built, the note posts, and `NotificationStage`
  delivers it inline or on a later result, never twice and never not at all.

### Tool changes (`rust/fontaine-tools/src/jobs/tools.rs`)

- `JobOutputTool::call` matches on `JobOutput`: the `Read` arm renders through `render_read` as
  today; the `MonitorWait` arm renders the status lines above (`format_age` is reused and
  `state_line` is refactored to take `id`/`age`/`end` directly, so the `Read` render and the
  `MonitorWait` arm share it; the "how it ended" wording comes from `EndInfo::describe` /
  `how_it_ended`).
- `JobOutputTool::description` replaces the refusal sentence with the wait semantics: with an id,
  a monitor is waited on (bounded by `wait_secs`) and reports new-output/ended/still-running
  status; its lines always arrive as notifications. The parameter schema is unchanged.
- Risk class, `root_only` gate and `MAX_WAIT_SECS` are unchanged — the tool stays read-only and
  main-agent-only.

## Implementation Steps

1. **`jobs.rs` — return type and removal.** Add `JobOutput` (or the chosen sum type); delete
   `JobError::Monitor` and its Display arm; change `Jobs::output` to route monitor jobs to the
   wait-only loop and plain jobs to the existing read loop; arm a `WaitGuard` in the monitor loop;
   return `MonitorWait` results from it; update the doc comment on `Jobs::output`
   (`jobs.rs:895`), which today describes only the read path.
2. **`jobs.rs` — wake on post.** Call `Shared::wake()` at the end of `Shared::monitor_tick` when
   a note was posted.
3. **`tools.rs` — render and describe.** Match `JobOutput` in `JobOutputTool::call`; render the
   three monitor statuses; update `JobOutputTool::description`.
4. **Unit tests (`jobs/tests.rs`).** Replace `a_monitor_is_refused_by_job_output_but_listed` and
   add the monitor-wait cases (below).
5. **Integration tests (`rust/fontaine-tools/tests/jobs.rs`).** Update
   `a_monitor_delivers_its_lines_as_notifications` (the refusal assertion becomes a wait
   assertion) and add a tool-level wait case.
6. **Docs.** Update the `job_output` bullet in `docs/tools.md` `## Background shell jobs` and the
   refusal sentence in `## Monitor`; add a `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-tools/src/jobs.rs`
- `rust/fontaine-tools/src/jobs/tests.rs`
- `rust/fontaine-tools/src/jobs/tools.rs`
- `rust/fontaine-tools/tests/jobs.rs`
- `docs/tools.md`
- `CHANGELOG.md`

## Trade-offs

- **`job_output` wait-only vs a dedicated `monitor_wait` tool.** Chosen: extend `job_output`. The
  observed failure is models naturally calling `job_output` on monitor ids; the tool already
  advertises itself as the bounded-wait mechanism, so a new tool name reintroduces the discovery
  problem and grows the tool surface. The agent counterpart (#301) likely wants a dedicated tool
  because no existing tool fits `agent-*` ids; monitors do not have that excuse.
- **Wake on posted notification vs on raw append.** Chosen: posted. Waking on appends returns
  before the lines are deliverable, which recreates the spin one level up. The cost is at most the
  batch/interval delay (~1 s) before the wait notices output — within the issue's "promptly".
- **Reuse `waiters`/`held`/`WaitGuard` vs monitor-specific hold state.** Chosen: reuse. The
  existing machinery already implements "hold the end note while a waiter is parked, post it when
  the last waiter leaves"; a parallel mechanism would double the exactly-once surface.
- **Sum return type vs a flag on `Read`.** Chosen: sum type. `Read` means "output the model has
  not seen"; a wait-only result never carries output, and folding both into one struct invites a
  caller to render `chunk` for a monitor.

## Decisions

- A zero-wait `job_output` on a monitor returns status instead of an error (settled during
  authoring; removes the spin even for models that call it without `wait_secs`).
- A user-killed monitor with no waiter parked posts the standard end note (carrying the tail)
  instead of the short user-kill variant and is marked reported: monitors lose their read path,
  and a wait-only result must never promise a notification that never arrives.

## Documentation

- `docs/tools.md`: the `job_output` bullet gains the monitor wait behavior; the `## Monitor`
  section's refusal sentence is replaced with the wait semantics and a pointer to the notification
  path; the read-only classification sentence (`docs/tools.md:71`, "returns only what that job
  printed") gains the monitor-wait status exception; check the tools table row for `job_output`
  still reads correctly.
- `CHANGELOG.md`: entry under the unreleased section.
- Tool description (`JobOutputTool::description`) ships the same wording to the model.

## Testing Strategy

All behavior is reachable with constructed inputs against the in-process registry — no live
services, so everything is unit/integration tier.

Unit tests (`jobs/tests.rs`, fake `JobSink`, real `sh` commands as the existing tests use):

- **Quiet monitor, then delayed output**: `sleep 1; echo ping; sleep 30` — a `wait_secs: 60` wait
  returns with `note_posted` set in ~1 s (assert elapsed well under the cap), the result carries
  no output bytes, and the sink holds exactly one note containing `ping`.
- **Completion while waiting**: a monitor whose command exits — the wait returns promptly with
  `end` set, and the end note (with the final lines) is posted exactly once; a second wait reports
  ended + already-reported, still with no output bytes.
- **Timeout while running**: a quiet `sleep 30` monitor with `wait_secs: 1` returns success with
  `note_posted == false`, no end; the later end note is still delivered exactly once (the held
  machinery was not falsely triggered by a timed-out wait).
- **Cancelled/interrupted wait**: a wait future dropped mid-wait while the monitor runs leaves no
  spurious note and decrements `waiters`; a wait future dropped after the monitor ended (note
  held) posts the end note exactly once.
- **Zero wait**: `wait_secs: 0` on a running monitor returns still-running status immediately
  with no error; the monitor is still listed.
- **Listing unchanged**: `snapshot` still shows the monitor while the wait runs (replaces the
  refusal half of the old test).
- **User kill while waiting**: `kill_by_user` during a parked wait posts exactly one end note
  carrying the tail (via the `Kill::User` hold above), and the wait reports ended with
  `already_reported` false; a second wait then reports ended + already-reported.
- **User kill with no waiter, then a wait**: `kill_by_user` on a printing monitor with nothing
  parked posts one standard end note carrying the tail and marks the job reported; a later
  zero-wait reports ended + already-reported and promises nothing further.

Integration tests (`tests/jobs.rs`, through the tool registry): the existing monitor notification
test's refusal assertion becomes "a wait returns status and the lines still arrive as
notifications"; a tool-level case asserts the rendered text of each status shape and that
`job_output` on an unknown id still errors with `there is no job`.

Existing tests that must change: `a_monitor_is_refused_by_job_output_but_listed` (replaced by the
wait tests above) and the refusal assertion in `a_monitor_delivers_its_lines_as_notifications`.

## Open Questions

None — the issue specifies the semantics and acceptance criteria; #301 (agent waits) is tracked
separately and should follow this plan's wait/notify split.
