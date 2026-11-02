# Monitor Without a Default Timeout Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/363

## Overview

The `monitor` tool stops every watch after 300 s unless the model passes `timeout_secs`. That
default doesn't fit what the tool is for. CI on this repo takes about 20 minutes, so a monitor
started at push time dies long before any check finishes and reports "reached its timeout", which
looks like a failure of the watched command. This change removes the default. A monitor started
without `timeout_secs` runs until its command ends, `job_cancel` stops it, it hits the
notification cap, or the session ends. An explicit `timeout_secs` keeps its current meaning and
its 3,600 s ceiling.

## Current State

- `rust/fontaine-tools/src/jobs/tools.rs:25-26`: `DEFAULT_MONITOR_SECS = 300` and
  `MAX_MONITOR_SECS = 3600`.
- `MonitorTool::call` (`tools.rs:454-458`) resolves
  `args.timeout_secs.unwrap_or(DEFAULT_MONITOR_SECS).clamp(1, MAX_MONITOR_SECS)` into a
  `Duration`, passes it in `MonitorSpec { description, timeout }`, and its start message says
  "it ends when the command ends or after {N}s" (`tools.rs:475-480`).
- `MonitorTool::description` (`tools.rs:412-421`) advertises "default 300, at most 3600", and the
  `timeout_secs` schema property is described as "Seconds before the monitor is stopped."
- `rust/fontaine-tools/src/jobs.rs:703-706`: `MonitorSpec { description: String, timeout:
  Duration }`. `Jobs::start` (`jobs.rs:786-794`) already turns it into
  `Control.deadline: Option<Instant>` through `checked_add`, and the job thread
  (`jobs.rs:1209`) kills with `Kill::MonitorTimeout` only when that deadline is `Some` and has
  passed. A plain background job already runs with `deadline: None`, so an unbounded job is an
  existing state of the job machinery and needs no new code path there.
- Bounds that stay in place without a timeout: at most `MAX_RUNNING` (8) jobs and monitors run at
  once, a monitor is terminated after `MONITOR_NOTIFICATIONS` (1,000) notifications
  (`jobs.rs:56`), pending lines are capped (`MONITOR_PENDING`), and session end calls
  `terminate_all` (TUI exit `fontaine-tui/src/lib.rs:388`, `/clear` via `end_jobs`, headless run
  end `headless.rs:928-929` and its failure paths), which stops every running job, monitors
  included; `Drop for Jobs` (`jobs.rs:1098`) is only a backstop. Headless runs never wait for
  jobs (they count only subagents), so a still-running monitor at the end of a `-p` run is
  stopped by `terminate_all` and nothing changes there.
- Docs: `docs/tools.md:23` (tools table: arguments) and `docs/tools.md:345-363` (`## Monitor`:
  "`timeout_secs` (default 300, at most 3,600)", "It ends at its timeout or when its command
  ends").
- Claude Code's `Monitor` has `timeout_ms` with a 5-minute default and a 30-minute effective cap,
  and tells the model to re-arm on expiry. So this change is a deviation and gets a Decisions line.

## Design

- `MonitorSpec.timeout` becomes `Option<Duration>`; `None` means no deadline.
  `Jobs::start` computes
  `spec.monitor.as_ref().and_then(|m| m.timeout).and_then(|t| Instant::now().checked_add(t))`.
  Nothing else in `jobs.rs` changes: the deadline check already handles `None`.
- `MonitorTool::call`: `args.timeout_secs.map(|s| Duration::from_secs(s.clamp(1, MAX_MONITOR_SECS)))`.
  Delete `DEFAULT_MONITOR_SECS`.
- Start message: with a timeout, it stays "it ends when the command ends or after {N}s". Without
  one, it says "it ends when the command ends; it has no timeout". The rest of the message is
  unchanged.
- Tool description: replace "until it ends or `timeout_secs` (default 300, at most 3600) passes"
  with "until it ends, or until `timeout_secs` (at most 3600) passes if you set one". Also add a
  short clause so an unbounded command (`tail -f`) is cancelled with `job_cancel` once the model
  is done with it, because nothing stops it otherwise. The schema's `timeout_secs` description
  becomes "Seconds before the monitor is stopped. Omit to run until the command ends."
- `MonitorArgs` and the JSON schema keep `timeout_secs` optional, `minimum: 1`,
  `maximum: MAX_MONITOR_SECS`. The cap on explicit values stays, which keeps the change to what
  the issue asks: an explicit timeout is a bound the model chose, and the existing clamp
  semantics for it are unchanged.

## Implementation Steps

1. `rust/fontaine-tools/src/jobs.rs`: change `MonitorSpec.timeout` to `Option<Duration>` and
   update the deadline computation in `Jobs::start`.
2. `rust/fontaine-tools/src/jobs/tools.rs`: remove `DEFAULT_MONITOR_SECS`, map the optional
   timeout, branch the start message, and update the tool and schema descriptions.
3. `rust/fontaine-tools/src/jobs/tests.rs`: the `monitor` helper takes `Option<Duration>`.
   Existing callers pass `Some(..)`.
4. Tests (see Testing Strategy).
5. `docs/tools.md`: tools-table row and `## Monitor` section.
6. `tasks/fontaine_harness_plan.md`: add a dated line at the end of `### Decisions` → `### Tools`
   recording the deviation from Claude Code.
7. `CHANGELOG.md` is updated by the PR step.

## Files to Modify

- `rust/fontaine-tools/src/jobs.rs`
- `rust/fontaine-tools/src/jobs/tools.rs`
- `rust/fontaine-tools/src/jobs/tests.rs`
- `rust/fontaine-tools/tests/jobs.rs`
- `docs/tools.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **No default vs. a default equal to the 3,600 s cap** (the issue offers both). A one-hour
  default still cuts a slow CI run or a long build short with the same confusing "reached its
  timeout". It would also leave a number the model has to reason about for no safety gain, because
  the notification cap, the eight-job limit and session-end teardown already bound a runaway
  monitor. No default is what the issue's title and its "runs until its command ends" describe.
- **Keep Claude Code's 5-minute default and re-arm pattern.** Rejected: re-arming costs a turn
  per expiry, leaves gaps in coverage, and is the behavior that misled the user in the issue.
- **Lift the 3,600 s cap on explicit values.** Not needed: anyone who wants longer omits the
  argument.

## Decisions

- 2026-11-01: no default monitor timeout; an omitted `timeout_secs` means no deadline.
- 2026-11-01: the 3,600 s cap stays on explicit `timeout_secs` values.
- 2026-11-01: deviates from Claude Code's 5-minute default and re-arm pattern (recorded in the master plan's Tools decisions).

## Documentation

- `docs/tools.md`: the tools-table `monitor` row (note that `timeout_secs` is optional with no
  default) and the `## Monitor` section. That section should say `timeout_secs` is optional, at
  most 3,600, with no default; that without it a monitor ends when its command ends,
  `job_cancel` stops it, or the session ends; and that a command that never ends should be
  cancelled when it's no longer needed.

## Testing Strategy

Unit and registry tests, no live services:

- `jobs/tests.rs`: start `monitor(&jobs, "sleep 30", None)` and assert the job's
  `control.deadline.is_none()` (read as in `tests.rs:661-675`; the only path to
  `Kill::MonitorTimeout` is a `Some` deadline). Check that a `Some(..)` monitor gets a deadline,
  then cancel.
- `tests/jobs.rs` (through the registry): a `monitor` call without `timeout_secs` returns a start
  message that says it has no timeout and contains no "after …s". A call with
  `timeout_secs: 5` still says "after 5s", and `timeout_secs: 99999` is clamped (there is no runtime
  schema validation) and the start message says "after 3600s".
- The existing explicit-timeout test keeps passing with `Some(..)`, so `Kill::MonitorTimeout` is
  still covered.
- The tool description no longer contains "default 300". Assert this in the registry test so the
  model-facing text can't drift back.
