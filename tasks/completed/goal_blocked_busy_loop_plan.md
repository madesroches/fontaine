# `/goal` Busy-Loop When Blocked Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/648

## Overview

A goal whose next step needs the user busy-loops: in session `16378e48` a guardian-denied
force-push left the model with nothing to do, yet nine *not met* verdicts started a new turn every
6–13 s for about 105 s. Three gaps let it happen. A read-only status poll or a `monitor` start
counts as progress, so the no-progress cap never fires. A running background shell or monitor job
does not defer evaluation, as a running subagent does. And the evaluator has no way to say "this
waits for the user", so *not met* always continues at once. This plan does three things. Background
jobs defer evaluation, with Claude Code's check-in cadence. Read-only calls and monitor starts stop
counting as progress. A *not met* verdict gets a `waiting_on_user` flag that pauses the goal.

## Current State

- `SessionGoal` (`rust/fontaine-core/src/goal.rs`) holds `ActiveGoal { pending, paused, made_call,
  calls_seen, stretch: Stretch { turns, idle_verdicts, spend_base }, .. }`.
  - `after_turn` (`goal.rs:543`) sets `made_call` when the ledger's root-agent action count grew
    (`root_actions`, `goal.rs:449`). It counts every action, whatever the tool.
  - `next_step(agents_running)` (`goal.rs:611`) returns `Pending` when `pending && !paused &&
    !agents_running`.
  - `not_met` (`goal.rs:679`) resets `idle_verdicts` on `made_call` and pauses at
    `NO_PROGRESS_LIMIT` (8), then checks `max_turns` and `max_cost_usd`.
  - `user_prompt` (`goal.rs:592`) clears the pause and starts a fresh stretch.
- `AgentRuntime::next_goal_step` (`rust/fontaine-core/src/agent/goal.rs:470`) passes only
  `state.agents().any_running()`. Background shells and monitors live in `Jobs`
  (`rust/fontaine-tools/src/jobs.rs:773`, `runtime.jobs()` at `agent.rs:1225`). A running
  background or monitor job is `Job::counts()` (`jobs.rs:441`), which is private. Its end and
  each monitor batch post a root notification through `TreeJobs` (`agent/setup.rs:676`).
- TUI: `Ui::run` (`rust/fontaine-tui/src/ui/mod.rs:760`) runs notification turns, then scheduled
  turns, then `goal_step` (`rust/fontaine-tui/src/ui/goal.rs:32`), then parks in a `select!` whose
  `tick` comes from `soonest_tick`.
- Headless: `handle_message` (`rust/fontaine/src/headless.rs:1356`) loops `settle` → 
  `next_goal_step` → `evaluate_goal` → goal turn. `settle` (`headless.rs:1308`) runs notification
  turns through `wait_for_news` (`headless.rs:1097`), which returns `Done` once no subagent
  runs. It never waits for jobs, which die with the run.
- Evaluator: `GoalVerdict { verdict: Judgment, reason }` (`goal.rs`), prompt
  `rust/fontaine-core/prompts/goal_evaluator.md`, which forbids `impossible` for work "blocked for
  now" and so leaves only `not_met`.
- `descriptor::find(name)` (`rust/fontaine-tools/src/descriptor.rs:367`) gives a built-in's
  `RiskClass`. `job_output` is `ReadOnly` and `monitor` is `Arbitrary`. `afk_report.rs:143` already
  maps an unknown name to `Arbitrary`.
- Plan Decisions (Agents): "2026-10-26 Deviation: a failed turn pauses the goal … and there are no
  idle check-ins for background work". The 2026-10-26 AFK line defines `made_call` as any root
  action.
- Claude Code ([goal](https://code.claude.com/docs/en/goal), "Background work defers evaluation"):
  - Evaluation is skipped while a subagent or a background shell runs.
  - A check-in is due once background work has kept the goal waiting 30 min. Later intervals
    double, up to 4× the first (30 min, then 1 h, then every 2 h).
  - A check-in is delivered at the end of a turn that finished with the work still running, or,
    interactive only, as an idle turn, at most 3 per goal between prompts. The third says idle
    check-ins are paused until the next prompt.
  - Work that stopped without reporting gets a "continue toward the goal" turn.
  - `CLAUDE_CODE_GOAL_CHECKIN_MINUTES` sets the interval; `0` turns check-ins off.

## Design

### 1. Background jobs defer evaluation, with check-ins

`fontaine-tools::jobs::Jobs` gains `pub fn running_background(&self) -> Vec<JobInfo>`, the jobs
for which `counts()` holds. `AgentRuntime` gains `fn background_work(&self) -> Vec<String>`: one
line per running subagent (`agent <id>`, from `running_ids()`) and per running job. A job's line is
`monitor <id>: <description>` or `shell <id>: <command>`, each cut to 200 characters. `JobInfo`
gains `description: Option<String>` (the monitor's, which `Monitor.description` holds privately),
filled in `snapshot` and `running_background`.

`ActiveGoal` gains `wait: Option<Wait>` and `turn_end: bool`, and `Stretch` gains
`idle_checkins: u32`. `after_turn` sets `turn_end` on every answered turn (check-in turns
included); every `take_step` on an active goal clears it, so only the first `take_step` after a
turn ends sees it:

```rust
struct Wait { since: Instant, checkins: u32 }   // since = deferral start, then last check-in
```

`next_step` becomes `take_step(&self, work: &[String], now: Instant, idle: bool, caps: &GoalCaps)
-> Option<Due>`, which may update the wait. `caps` carries the check-in interval and `max_turns`:

```rust
pub enum Due {
    Evaluate(Pending),
    CheckIn(CheckIn),          // { condition, work: Vec<String>, last_idle: bool }
    IdleExhausted,             // due, idle, but the idle cap is used up
    Paused(PauseReason),       // a due check-in hit max_turns: the goal is now paused
}
```

The rules, in order:

1. If the goal is paused or absent, the result is `None`.
2. With no work running:
   - If `pending`, the wait is cleared and the result is `Evaluate`.
   - If not `pending` but a wait exists and `idle` holds, the result is `Evaluate`. This is the
     fallback for a check-in that was taken but whose turn never ran (a hook or refresh refused
     it); job ends and subagent reports always post news, so no other state reaches it. The wait
     is cleared.
   - Otherwise the result is `None`.
3. With work running, a `pending` goal with no wait starts one at `now` (normally `after_turn`
   already did; see below). Without `pending` and without a wait, the result is `None`.
4. The check-in interval is `checkin × min(2^checkins, 4)`, from `[goal] checkin_minutes`. A
   check-in is due when `now ≥ since + interval`. `checkin_minutes = 0` means never due.
5. When a check-in is due:
   - With `turn_end` set (the first `take_step` since an answered turn, so `pending` holds), the
     result is a turn-end `CheckIn`, and `pending` is cleared.
   - Otherwise, an `idle` goal with `idle_checkins < IDLE_CHECKINS` (3) gets an idle `CheckIn`, and
     `idle_checkins` goes up by one. `last_idle` is set when it reaches 3. This holds even while
     a deferred `pending` stays set from the last turn: a check-in that falls due later in the
     same wait is idle and counts toward the cap.
   - An `idle` goal whose idle cap is used up gets `IdleExhausted`.
   - Otherwise the result is `None`.
   - Before either `CheckIn` is returned, if `stretch.turns >= max_turns`, `paused =
     TurnCap(max_turns)` and the result is `Paused(TurnCap)` instead; otherwise `stretch.turns`
     goes up by one.
   - On a `CheckIn`, `checkins` goes up by one and `since = now`.
6. Otherwise the result is `None`. A deferred `pending` stays set.

`after_turn` gains a `work_running: bool` and a `now: Instant` parameter: when it sets `pending` while
work runs, it starts the wait at once, so the wait exists before the first `take_step`. Headless's
`settle` blocks in `wait_for_news` before any `take_step`, and without this an untimed monitor would
never reach a check-in. `goal_turn_ended` passes `!background_work().is_empty()`.

`checkin_wait(&self, work_running: bool, now: Instant, respect_idle_cap: bool, caps: &GoalCaps) ->
Option<Duration>` returns the time until the next check-in falls due (zero when due). It returns
`None` when there is no wait, check-ins are off, or (with `respect_idle_cap`) the idle cap is used
up. The idle loops use it to wake.

`user_prompt` clears the wait along with the stretch. `set`/`clear`/`fresh` start with none.

`CheckIn` turns, like goal turns, count toward `stretch.turns` (incremented in `take_step`, rule
5). Reaching `max_turns` returns `Due::Paused(TurnCap)` instead of the check-in, so a check-in
can't escape the turn cap.
`AgentRuntime::checkin_prompt(&CheckIn) -> String` builds a harness note:

> [Harness note, not the user's words: a goal is active and background work is still running:
> <list>. Read its output; if it is progressing, end your turn and keep waiting; fix or stop
> anything that is stuck. Goal: <condition>]

When `last_idle` is set, the note adds "idle check-ins are paused until the user sends a message".
The list is the model's own commands and agent ids, not tool output, so it skips the result
pipeline. The condition goes through as written, as in `goal_prompt_text`.

`AgentRuntime::next_goal_step(idle: bool) -> Option<Due>` passes `background_work()`,
`Instant::now()` and `GoalCaps::from(&self.config.goal)`. On `Due::Paused(reason)` it appends the same ledger record
as `pause_goal` (`GoalEvent::Paused`, `reason: Some(reason.to_string())`) before returning it. It
also exposes
`goal_checkin_wait(respect_idle_cap)`. `GoalCaps` gains `checkin: Option<Duration>`, so `take_step`,
`checkin_wait` and `not_met` read one caps struct.

- **TUI** (`ui/goal.rs`, `ui/mod.rs`): `goal_step` calls `next_goal_step(true)`.
  - `Evaluate` behaves as today.
  - `CheckIn` shows `goal: checking on background work` and runs a goal turn with the check-in
    prompt. `goal_turn` (`ui/turn.rs`) takes the prompt `String` instead of a `Continue`: the
    evaluate path passes `runtime.goal_prompt(&next).await`, the check-in path
    `runtime.checkin_prompt(&checkin)`.
  - `IdleExhausted` does nothing.
  - `Paused(reason)` shows `reason.notice()`, as an evaluation's pause does.
  - `Ui::run` adds `runtime.goal_checkin_wait(true)` to `soonest_tick`, so an idle check-in wakes
    the parked loop.
- **Headless** (`headless.rs`): `wait_for_news` gains `goal: bool`. When set, it:
  - keeps waiting while `runtime.jobs()` has running background work, not only subagents, but only
    when check-ins are on (`checkin_minutes > 0`). With them off it returns `Done` once no
    subagent runs, as today, since nothing would ever end an untimed wait;
  - returns `Done` when `goal_checkin_wait(false)` is `Some(0)`. A wait started in `after_turn` makes
    this reachable before any `take_step`;
  - folds the check-in wait into its sleep (capped at its 100 ms poll as today).

  `settle` passes `involved` from `handle_message`. The goal loop calls `next_goal_step(true)`:
  - `CheckIn` runs a check-in turn and settles it.
  - `IdleExhausted` pauses the goal with `PauseReason::BackgroundWait` and breaks, so the run
    exits 1. With nobody to send the next prompt, the wait must end (see Decisions).
  - `Paused(_)` breaks, so the run exits 1 with the pause reported as for any other.
  - `None` breaks as today, except when an evaluation is still deferred (the goal is active,
    unpaused, `pending` and has a wait; this happens only with check-ins off, as jobs still run
    when `settle` returns). Then the goal pauses with `PauseReason::BackgroundWait` through
    `pause_goal` and breaks, so the run exits 1 rather than ending with an unevaluated goal.
    `AgentRuntime::goal_deferred() -> bool` exposes that state.

  A non-goal headless message keeps today's behavior: jobs are not waited for.

### 2. Read-only calls and monitor starts are not progress

`root_actions` becomes `progress_actions`. It counts root-agent ledger actions whose tool is not a
built-in `ReadOnly` (`descriptor::find(tool).is_some_and(|d| d.risk == RiskClass::ReadOnly)`) and
is not `monitor`. MCP and unknown tools still count: they find no descriptor, so they are treated
as acting. A turn that only reads, polls `job_output` or starts a monitor is idle for the cap. A
`shell` poll such as `gh pr view` still counts. The shell is `Arbitrary` and no regex reads its
arguments, so change 3 covers that case.

### 3. `waiting_on_user` pauses on *not met*

`GoalVerdict` gains `#[serde(default)] pub waiting_on_user: bool`. Its schemars doc says: "true
only with not_met, when the next step needs the user: an approval, a decision, an answer, or a
permission the session was denied". The flag is read only on `not_met`; with `met` or `impossible`
it is ignored. `not_met` checks it first: `Step::Pause(PauseReason::WaitingOnUser)` with the reason
kept as `last_reason`, no `stretch.turns` increment, and `idle_verdicts` updated as usual. Its
notice reads `Goal paused: it is waiting for you (<reason cut to 200 chars>). Send a message to
continue.`

The prompt `goal_evaluator.md` gains a paragraph:

> "Set `waiting_on_user` when the transcript shows the assistant cannot go on without the user,
> for example a denied action it needs permission for, or a question it asked and is waiting on.
> Do not set it for work that waits on a build, a CI run or another background task."

A transcript can only induce a pause, the safe direction. Headless's `goal` event keeps `verdict:
"not_met"`, and the `result.goal` reports the pause as for any other. No schema change.

### Config

`[goal] checkin_minutes`, 0–1440, default 30. `0` turns check-ins off, but deferral stays. It is
`user_or_cli` like the other `[goal]` keys (`config_scope.rs:175` covers the section).
`config::Goal` gains `checkin_minutes: u32`, and `GoalSection` gains `Option<u32>` with range
validation next to `max_turns`'s.

## Implementation Steps

1. `fontaine-tools/src/jobs.rs`: `Jobs::running_background()` and `JobInfo.description`, with a unit test that a foreground
   job is excluded and a monitor and a promoted job are included.
2. `fontaine-core/src/config.rs`: `checkin_minutes` in `GoalSection`/`Goal`, validation and
   default, and the `[goal]` merge (~`config.rs:3407`); a `goal.checkin_minutes` row in
   `fontaine-core/src/config_catalog.rs` next to the rows at `:580-584`; extend the
   `config_scope.rs:671` table test row if needed.
3. `fontaine-core/src/goal.rs`:
   - `Wait`, `turn_end`, `idle_checkins`, `Due` (with `Paused`), `CheckIn`, `take_step`, `checkin_wait`, `IDLE_CHECKINS`,
     `GoalCaps.checkin`, `after_turn(end, work_running, now)`;
   - a `deferred()` query (active, unpaused, `pending`, a wait) for `goal_deferred`;
   - `progress_actions`;
   - `GoalVerdict.waiting_on_user`;
   - `PauseReason::{WaitingOnUser(String), BackgroundWait}` with `Display`;
   - the check-in note text (`checkin_prompt_text`) beside `goal_prompt_text`;
   - the `PauseReason::NoProgress` `Display` ("{n} turns in a row made no tool call…") reworded to
     match the docs: reads and monitor starts don't count.
4. `fontaine-core/prompts/goal_evaluator.md`: the `waiting_on_user` paragraph.
5. `fontaine-core/src/agent/goal.rs`: `background_work`, `next_goal_step(idle)` (recording a
   `Due::Paused` in the ledger), `goal_checkin_wait`, `goal_deferred`, `checkin_prompt`; `evaluate_goal` fills `Record::Goal.reason` (now
   `None` for every `Step::Pause`) for a `WaitingOnUser` pause; headless's `BackgroundWait` goes
   through `pause_goal`, which already fills it; update the `reason` field's doc comment in
   `ledger.rs`, which limits it to pauses not from the goal's own rules.
6. `fontaine-tui/src/ui/goal.rs`, `ui/mod.rs`, `ui/turn.rs`: `Due` handling, `goal_turn` taking a prompt, the check-in turn and notice, and
   the tick.
7. `fontaine/src/headless.rs`: `wait_for_news(goal)` (jobs waited for only with check-ins on), the
   check-in turn, `IdleExhausted` and a deferred `None` → `BackgroundWait` pause, `Paused` → break.
8. Update the existing `next_step` call sites in `fontaine-core/tests/goal.rs` and its
   `runtime.next_goal_step()` callers (lines 973, 1067, 1120, 1126, 1277, 1280, 1324), and the
   `after_turn` callers for the new parameters. Also the struct literals that stop compiling:
   `GoalVerdict { verdict, reason }` at lines 88 and 783 (new `waiting_on_user`) and
   `GoalCaps { .. }` at 37 (const `CAPS`), 367 and 395 (new `checkin`).
9. Docs (below) and the Decisions lines in `tasks/fontaine_harness_plan.md` (Agents subsection,
   at its end).

## Files to Modify

- `rust/fontaine-tools/src/jobs.rs`
- `rust/fontaine-core/src/config.rs`, `rust/fontaine-core/src/config_catalog.rs`
- `rust/fontaine-core/src/goal.rs`
- `rust/fontaine-core/src/agent/goal.rs`
- `rust/fontaine-core/prompts/goal_evaluator.md`
- `rust/fontaine-core/tests/goal.rs`
- `rust/fontaine-tui/src/ui/goal.rs`, `rust/fontaine-tui/src/ui/mod.rs`,
  `rust/fontaine-tui/src/ui/turn.rs`
- `rust/fontaine/src/headless.rs`, `rust/fontaine/tests/goal_headless.rs`,
  `rust/fontaine/tests/background_jobs.rs`
- `docs/tui.md`, `docs/headless.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md`
- `CHANGELOG.md`

## Trade-offs

- **A flag, not a fourth verdict.** A `blocked` judgment would change the `goal` event's
  `verdict` enum in `headless-v1`. The flag keeps the schema, and the evaluator still says *not
  met*, which is true.
- **Repeated-reason detection** (issue's fix 2, second option) needs a fuzzy similarity threshold.
  That is nondeterministic in practice and hard to test. The flag covers the same case explicitly.

## Decisions

(Deviation lines to add at the end of the plan's Agents subsection, dated at implementation.)

- Supersedes the 2026-10-26 "no idle check-ins for background work" clause: a running background
  shell or monitor job defers goal evaluation like a running subagent. Check-ins follow Claude
  Code's cadence (30 min, doubling to 4×, at most 3 idle check-ins between prompts, the third
  saying so). The interval is `[goal] checkin_minutes` (0 turns check-ins off). The
  `CLAUDE_CODE_GOAL_CHECKIN_MINUTES` variable is not read, as with `[tui] prompt_suggestions`.
- Deviation: headless delivers check-ins while it waits on background work, not only at turn ends
  as `claude -p` does. Once its 3 idle check-ins are used and another would fall due, the goal
  pauses (`BackgroundWait`) and the run exits 1. Reason: a headless run has no user to resume it,
  and an untimed monitor would hang it.
- Deviation (narrows the 2026-10-26 AFK `made_call` line's "counts actions only"): for the
  no-progress cap, a root action counts as progress unless its tool is a built-in `ReadOnly` or
  `monitor`. Claude Code counts any tool use. Reason: a model polling or starting a watch while
  blocked must still hit the cap (#648).
- Deviation: the evaluator's verdict carries `waiting_on_user`; on *not met* it pauses the goal
  (`Goal paused: it is waiting for you (…)`) instead of starting a turn. Claude Code has no such
  verdict. Reason: a goal blocked on the user must stop at once, and the next prompt resumes it.
- AFK: a turn-end check-in is only the first `take_step` after an answered turn (a `turn_end` flag
  `after_turn` sets and `take_step` clears); a check-in falling due later in the same wait is idle
  and counts toward the 3-check-in cap even while `pending` stays set. Reason: otherwise every
  check-in turn's answer made the next one turn-end, so the idle cap, `IdleExhausted` and the
  headless `BackgroundWait` exit never fired.
- AFK: with `checkin_minutes = 0`, a headless goal run does not wait for background jobs (as
  today), and a still-deferred evaluation pauses the goal with `BackgroundWait` and exits 1 instead
  of ending unevaluated. Reason: most conservative choice; no unbounded wait and no busy loop.
- AFK: a due check-in at `max_turns` returns a new `Due::Paused(TurnCap)` (set in `take_step`,
  which otherwise increments `stretch.turns` for the check-in); `next_goal_step` records it like
  `pause_goal` and both surfaces show it. Reason: the plan stated the cap but named no mechanism
  for it (not already resolved in the text).

## Documentation

- `docs/tui.md` "Goals":
  - a **Background work** paragraph on deferral, check-ins, the idle cap and `checkin_minutes`;
  - the existing line that only subagents make the goal wait (`tui.md:1265`) extended to background
    jobs, or folded into that paragraph;
  - the no-progress bullet reworded ("8 verdicts in a row after turns that made no call other
    than reads and monitor starts");
  - a new pause bullet for the evaluator finding it waits for you.
- `docs/headless.md`:
  - "Goals": headless waits for background jobs while a goal runs, delivers check-ins while
    waiting, and pauses after the third. With `checkin_minutes = 0` it does not wait for jobs, and a
    goal still waiting on them pauses (background wait) with exit 1. Both new pause reasons (waiting for you; background wait)
    join the pause list at `headless.md:114-115`.
  - `headless.md:164` "Background jobs are not waited for" gets an exception while a goal is
    active.
- `docs/configuration.md` `[goal]`: the `checkin_minutes` row and the reworded no-progress note.
- `CHANGELOG.md`: an `[Unreleased]` entry for the goal change (#648), under the goal/agents
  subject of its change type.

## Testing Strategy

Unit tests (`rust/fontaine-core/tests/goal.rs`, `SessionGoal` with an injected `now`):

- `take_step` with work running defers a pending evaluation, then evaluates once the work is gone.
- With work running, a check-in falls due at 30 min, then at +60 min, then every +120 min
  (`checkin_minutes = 30`). With `checkin_minutes = 0`, none ever does.
- A turn-end check-in needs the first `take_step` after an answered turn. A check-in that falls
  due on a later `take_step` in the same wait is idle, even with `pending` still set, so after an
  answered turn whose wait keeps running, idle check-ins stop after 3, the third with `last_idle`,
  then `IdleExhausted` (regression: a check-in turn's own answer must not make every later
  check-in a turn-end one). `user_prompt` resets both counts and the wait.
- A wait whose work stopped with nothing pending evaluates when idle.
- A due check-in at `max_turns` returns `Due::Paused(TurnCap)` and sets the pause; below it, a
  check-in increments `stretch.turns`. Through the runtime, the pause is recorded in the ledger with
  its reason.
- `checkin_wait` counts down, is zero when due, and is `None` when off or the cap is used up.
- **Regression for #648:** a stretch of turns whose only root calls are `job_output`, `read` and
  `monitor` hits `NO_PROGRESS_LIMIT` and pauses. A `shell` or `edit` call still resets the count.
- **Regression for #648:** `not_met` with `waiting_on_user` pauses with `WaitingOnUser`, takes
  no turn and is recorded. With `met` the flag is ignored.
- Evaluator: a mock answer with and without `waiting_on_user` parses (default `false`).
- Runtime (in `rust/fontaine-core/tests/goal.rs`, following the job-start pattern of
  `rust/fontaine-core/tests/scanner.rs:277-295`; `#[cfg(unix)]`, as the Unix-only tests at
  `goal.rs:524` are, since the pattern runs `sh -c "sleep 30"`): a monitor job started with `Jobs`
  (`monitor: Some(MonitorSpec { .. })` in `Start`) on a long-sleeping process makes
  `next_goal_step(true)` return `None` after an answered turn, and a check-in prompt lists it as
  `monitor <id>: …`. This uses a real child process and pins the #648 path where a monitor didn't
  defer.
- `Jobs::running_background` (step 1) and config range validation for `checkin_minutes`.

Integration (`rust/fontaine/tests/goal_headless.rs`, deterministic models through `fontaine -p`):

- A goal whose evaluator answers `not_met` with `waiting_on_user` exits 1. The test runs with
  `--output-format text` (the stderr line prints only there) and uses a variant of the `judged`
  helper (`goal_headless.rs:29-35`) that adds `waiting_on_user: true`. It prints `fontaine:
  goal paused: it is waiting for you …`, and no goal turn ran after the set turn (two model
  requests: the set turn and the evaluator).
- In `rust/fontaine/tests/background_jobs.rs` (Linux-only, with its existing routed-endpoint
  infrastructure): a goal run with a background shell job still running waits for its end note
  before evaluating. With `checkin_minutes = 0`, the same run does not wait: the goal pauses with
  `BackgroundWait` and the run exits 1. The job must outlive the answer, so the test holds it past the
  `NotificationStage` drain edge (`background_jobs.rs:44-50`).

## Open Questions

None.
