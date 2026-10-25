# Session-Scoped Scheduling Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/16

## Overview

Claude Code's `/loop` re-runs a prompt on an interval, or at a pace the model picks, and its
`CronCreate`/`CronList`/`CronDelete` tools schedule one-shot or recurring prompts that are restored
on resume ([scheduled-tasks](https://code.claude.com/docs/en/scheduled-tasks)). This plan adds the
same to the interactive session: a per-session scheduler that, when the conversation is idle, starts
a turn from a due job's prompt. That turn is flagged as scheduled, so it is **not a user message**:
it never enters the ledger's `user_messages`, so the guardian's task intent stays what the human
actually said. The tools are registered with descriptor rows like every other tool.

## Current State

- A harness-started turn already exists: `AgentRuntime::notification_prompt`
  (`fontaine-core/src/agent.rs:385`) builds a `[Harness note, not the user's words: ...]` prompt,
  and the TUI runs it with `Ui::notification_turn` (`fontaine-tui/src/ui/turn.rs:52`) via
  `turn(runtime, prompt, None)`, where `human: None` means no `Record::UserMessage` is appended
  (`turn.rs:164`). Headless does the same in `wait_for_news` (`fontaine/src/headless.rs:538`).
- Guardian intent comes only from `LedgerView::user_messages` (`fontaine-core/src/ledger.rs:214`),
  fed by `Record::UserMessage` (`ledger.rs:27`); `/clear` resets it with `ContextCleared`.
- Session-scoped, restorable state has a precedent in `SessionTodos` (`fontaine-core/src/todos.rs`):
  an `Arc` on `Session` (`session.rs:172, 208, 242`), written through atomically and owner-only to
  `<session dir>/todos.json`, loaded on `open`, with an unparseable file surfacing once through
  `take_warning` -> `Approver::warn` in `build_agent` (`agent/build.rs:78`). `Session::fork` copies
  the directory.
- Extension tools are `ExtensionTool::new(...)` entries in `register_tools`
  (`agent/build.rs:104`), each with one row in `fontaine-tools/src/descriptor.rs` (`TOOLS`); a tool
  without a row registers as `Arbitrary`, and the choke-point tests fail on it. `TodoTool`
  (`fontaine-tools/src/todo.rs`) is the pattern for a store seam implemented in core.
- Built-in slash commands are one table in `fontaine-tui/src/commands.rs` (`BUILTINS`, sorted by
  name, with a `Command` variant each).
- The workspace has no cron parser. `chrono` is already in `Cargo.lock`.
- `research/research-claude-code-parity.md:311` lists this feature as `missing | backlog`.

## Design

### Scheduler (`fontaine-core/src/schedule.rs`, new)

```text
struct Job {
    id: String,              // 8 lowercase hex chars
    cron: String,            // 5 fields, local time; for a self-paced wakeup the pinned minute
    prompt: String,
    recurring: bool,
    origin: Origin,          // User (typed /loop) | Model (cron_create)
    kind: Kind,              // Cron | Wakeup { loop_id, fallback }
    created_ms: i64,         // epoch ms
    next_ms: i64,
}
pub struct Scheduler { jobs: Mutex<Vec<Job>>, dir: PathBuf, warning: Mutex<Option<String>>, clock: Arc<dyn Clock> }
```

`Clock` is a one-method trait (`now_ms() -> i64`); production uses the wall clock, tests a manually
advanced fake. Every method that took `now` reads the clock instead.

- `Scheduler` is an `Arc` on `Session` beside `todos`, persisted to `<session dir>/schedule.json`
  (owner-only, `owner_only::write_atomic`, write inside the mutex, never across an await, as
  `SessionTodos` does). `Session::open` loads it; `fork` copies it with the directory.
- `Cron` is a hand-written parser/matcher for standard 5-field vixie syntax: `*`, values, `a-b`,
  `*/n`, `a-b/n`, lists; day-of-week `0`/`7` = Sunday; day-of-month and day-of-week both
  constrained = either matches; no `L W ? MON JAN`. `Cron::next_after<Tz: TimeZone>(t)` returns the
  next matching minute (searching at most 5 years; none = `Err`). Evaluated in the local timezone
  with `chrono::Local`; a local time that does not exist (DST gap) is skipped. Generic over the
  timezone so tests use `FixedOffset`.
- API: `create(job_spec, now) -> Result<Job, ScheduleError>`, `list()`, `delete(id) -> bool`,
  `due(now) -> Option<Fired>` (the earliest due job; recurring jobs get `next_ms` recomputed from
  `now`, so a job that fell due while the model was busy fires **once**, with no catch-up; a
  one-shot is removed), `next_wake() -> Option<i64>`, `take_warning()`.
- Lock policy: the `jobs` mutex feeds what runs, so it uses `fontaine_tools::lock::lock` (panics on
  poison); the advisory `warning` may use `lock_recovering`, with a one-line reason at the call site.
- Limits: 50 jobs per session; prompt non-empty and at most 2,000 characters; a recurring job
  expires 7 days after creation (it fires one final time, then is removed). A one-shot whose time
  has passed, or an expression that never matches, is rejected at creation.
- `ScheduleError` is a typed enum (`BadCron`, `NeverFires`, `TooMany`, `PromptTooLong`,
  `Persist { source }`): the tools dispatch on the kind to word the model-facing error, and
  `Persist` keeps the `io::Error` as `source()`.
- Restore (`Scheduler::load`): drop expired recurring jobs and one-shots whose time passed, drop all
  `Wakeup` jobs (a self-paced loop is not restored; the user runs `/loop` again), and recompute
  `next_ms` of what remains from now, so a restored recurring job never replays missed runs. Every
  drop is counted in a single `take_warning` line (`n scheduled tasks expired while the session was
  closed`). An unparseable file yields an empty scheduler plus a warning, like `todos.json`.

### Firing: a scheduled turn, not a user message

`AgentRuntime::scheduled_prompt(&Fired) -> String` (`async`: it awaits `state.results.run`) builds, in the style of `notification_prompt`:

```text
[Harness note, not the user's words: scheduled task <id> (<cron or "every ...">, set by <you|the
model>) is due. Its prompt follows. It is not a new request from the user and does not widen what
the user asked for earlier in this conversation.]

<prompt>
```

The prompt text of a model-created job is model-authored, so it passes through `state.results`
(the result pipeline, with the scanner) exactly as `notification_prompt` does; its `CallView` carries a distinct tool name (`scheduled`), not
`"agent"`. A user-created job's
prompt is the human's own typed text and is passed verbatim.

The TUI idle loop (`fontaine-tui/src/ui/mod.rs`, where the main prompt awaits input) gains one
`select!` arm: a sleep of `next_wake() - clock.now_ms()` (none = pending forever). `next_wake()` is
re-read on every loop pass and the `changed` arm already wakes the loop, so no `Notify` is needed.
A due job runs `Ui::scheduled_turn`, a sibling of `notification_turn`: `turn(runtime, prompt,
None)`, so no `UserMessage` is appended, plus a transcript notice (`scheduled: <id>`). Unlike
`notification_turn`, it then reuses the `converse` tail (`end_of_turn(&end, runtime.steering
.retract())`), so text the user queued during the scheduled turn is sent as the next turn or
restored to the input, and the draft is kept while it runs. A running TUI never swaps sessions, so
the arm needs no re-binding. Background-agent notifications are drained first. Because the arm sits in the
idle wait, a task that comes due mid-turn waits and fires once afterwards.

Ledger: a new `Record::Scheduled { event, id, origin, cron, recurring }` where `event` is
`created | fired | deleted | expired`, so the audit trail shows what ran and who scheduled it. The
prompt text is not stored (it is already in the conversation store or the typed `/loop` message).
`LedgerView::apply` ignores it, so `user_messages` and the guardian request are unchanged by
construction; a regression test pins that (see Testing).

### Tools (`fontaine-tools/src/schedule.rs`, new; store seam implemented in core)

All four are root-agent-only: the tool reads `context.require::<AgentId>()` (fail closed when absent,
unlike `SkillTool`'s default-to-root) and compares it with `ROOT_AGENT`; any other caller gets an
error result. Registered only when the session has a scheduler consumer
(`AgentSetup.scheduling`, set by the TUI; headless leaves it off, see Decisions).

| Tool | Arguments | Result |
|---|---|---|
| `cron_create` | `cron` (5-field), `prompt`, `recurring` (default true) | id, plain-language cadence, next run in local time |
| `cron_list` | none | one line per job: id, cadence, origin, next run, prompt (truncated) |
| `cron_delete` | `id` | confirmation, or an error naming the unknown id |
| `schedule_wakeup` | `delay_seconds` (60-3600), `reason`, or `stop: true` | confirms the pending wakeup |

`schedule_wakeup` carries **no prompt**: it re-arms the prompt of the self-paced loop the user
started with `/loop`, so the model can set the pace but never author new standing text. With no
active self-paced loop it returns an error.

Descriptor rows (`fontaine-tools/src/descriptor.rs`):

| name | risk | path_free | destructive | specifier | claude |
|---|---|---|---|---|---|
| `cron_create` | Arbitrary | yes | Always | None | `CronCreate` |
| `cron_list` | ReadOnly | yes | Never | None | `CronList` |
| `cron_delete` | ReadOnly | yes | Never | None | `CronDelete` |
| `schedule_wakeup` | ReadOnly | yes | Never | None | `ScheduleWakeup` |

`cron_create` is `Arbitrary` because it plants model-authored text that will later run as a turn
without the human in the loop: under auto mode the guardian judges it, under manual mode the user
is asked. The others only observe, cancel, or re-time text a human already supplied. None of them
touches a file, so containment has nothing to check (`path_free`). The deterministic floor still
runs first, so `deny`/`ask` rules naming any of these tools apply.

### `/loop` (TUI)

`Command::Loop(String)` plus a `BUILTINS` entry (sorted position). Forms:

- `/loop 5m <prompt>` (also a trailing `every 2 hours`): interval units `s m h d`; seconds round up
  to a minute; an interval that is not a clean cron step is rounded to the nearest that is (`7m` ->
  `*/7`-style if it divides the hour, else the nearest divisor; hours/days map to `0 */n`/`0 0 */n`),
  and the transcript states the cadence chosen. Creates a recurring `Origin::User` job **directly**
  (no model round trip) and runs the first iteration immediately as a turn recorded as the typed
  message (`converse`), so the human's `/loop ...` line is in `user_messages` and is the intent the
  later iterations inherit.
- `/loop <prompt>` (no interval): a self-paced loop. The first iteration runs now; the scheduled
  prompt for later iterations carries a short instruction to call `schedule_wakeup` at the end of
  each iteration (or `stop` when done). If an iteration ends with no pending wakeup, the scheduler
  arms one fallback wakeup 20 minutes out (`fallback: true`); an iteration that ends without
  rescheduling after a fallback ends the loop.
- `/loop list`: the jobs. `/loop stop [id]`: cancel one, or all when no id is given. `Esc` while the
  idle prompt shows a pending self-paced wakeup cancels it: `App` keeps a `wakeup_pending` flag
  (set from the scheduler each pass) and, after menu handling, Esc at idle returns
  `Action::CancelWakeup`.
- `/loop` with no prompt is a usage error (see Decisions).

The status bar shows `⏱ N` when jobs exist (`statusbar.rs`).

### Config

`[schedule] enabled = true` (user-scope only; a project config cannot set it, per the scope
table in `config_scope.rs`). When false the tools are not registered, `/loop` says scheduling is
off, and a restored `schedule.json` is left untouched on disk but never fires.

### System prompt

One short paragraph in `fontaine-core/prompts/system_prompt.md`: prefer `cron_create` only when
the user asks for something to happen later or repeatedly; scheduled prompts are harness notes and
not the user speaking; end a self-paced iteration with `schedule_wakeup`. The paragraph is included
only when the scheduling tools are registered (the prompt layers already vary on session state).

## Implementation Steps

1. `fontaine-core/src/schedule.rs`: `Cron` parser and `next_after`, `Job`, `Clock`, `Scheduler` (create,
   list, delete, due, next_wake, persistence, restore, expiry, warning). Add `chrono` to
   `rust/Cargo.toml` `[workspace.dependencies]` (`default-features = false`, `clock`) and use it as
   `chrono.workspace = true` in `fontaine-core/Cargo.toml`; `cargo deny check` must stay
   green. Unit tests.
2. `Session` holds `Arc<Scheduler>` (`session.rs` create/open/accessor); a test pins that `fork`
   copies `schedule.json`. `ledger.rs`: `Record::Scheduled`; `LedgerView::apply` ignores it.
3. `fontaine-tools/src/schedule.rs`: the four tools over a `ScheduleStore` trait (`create`, `list`,
   `delete`, `wake`), the root-only check, argument validation (`deny_unknown_fields`);
   `descriptor.rs` rows (`rows_are_pinned` hard-codes `[Pinned; 13]`: now 17); `names`/docs wiring
   falls out of the table. `fontaine-core/tests/choke_point.rs` hard-codes the extras set: its
   `extension_asking` helper registers the four tools and the expected set grows.
4. `fontaine-core`: implement `ScheduleStore` for `Scheduler` behind an approver-reporting adapter
   (as `ReportedTodos`); register the tools in `register_tools` (`agent/build.rs`, which reads `p.setup.scheduling` from
   `AgentSetup` via `Parts`) when `setup.scheduling && config.schedule.enabled`; drain `take_warning` beside the todos warning; `AgentRuntime::scheduled_prompt`.
5. Config: `[schedule]` struct following the three-place pattern in `config.rs`, a user-only row in
   `config_scope.rs`,
   a test that a project file setting it is refused.
6. TUI: `Command::Loop` and interval parsing in `commands.rs`; `scheduled_turn` and the idle-wait
   arm in `ui/mod.rs`/`ui/turn.rs`; `/loop list|stop`; `Esc` handling; status-bar indicator;
   `KEYS`/help text. `AgentSetup.scheduling` set by the TUI entry point.
7. System-prompt paragraph; docs (below); `research/research-claude-code-parity.md` row 311 to
   `done`; master plan `## Decisions` lines (see Decisions).
8. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and the Windows
   clippy target (`chrono` local time and paths are cross-platform).

## Files to Modify

- `rust/fontaine-core/src/{schedule.rs (new), lib.rs, session.rs, ledger.rs, agent.rs, agent/build.rs, config.rs, config_scope.rs}`, `rust/fontaine-core/prompts/system_prompt.md`, `rust/Cargo.toml`, `rust/fontaine-core/Cargo.toml`
- `rust/fontaine-tools/src/{schedule.rs (new), lib.rs, descriptor.rs}`
- `rust/fontaine-tui/src/{commands.rs, statusbar.rs, reports.rs, lib.rs, ui/mod.rs, ui/turn.rs, ui/commands/*}`
- `rust/fontaine/src/` (the TUI entry sets `scheduling`)
- tests: `rust/fontaine-core/tests/{choke_point.rs, docs.rs, ledger.rs, sessions.rs}`, `rust/fontaine-tui/tests/tui.rs`
- docs: `docs/architecture.md`, `tasks/design/sessions.md`, `docs/tools.md`, `docs/tui.md`, `docs/configuration.md`, `docs/permissions.md`, `docs/headless.md`
- `research/research-claude-code-parity.md`, `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Scheduled turn vs. synthetic user message**: injecting the prompt as a user message (what the
  issue's "user-turn-equivalent" could suggest) would feed the guardian's intent from model-authored
  text. A harness-note turn, as for background-agent reports, keeps intent untouched with no
  guardian change.
- **Own cron parser vs. a crate**: the syntax is five numeric fields; a parser is ~200 lines with
  exhaustive unit tests and adds no dependency to audit. `chrono` is the only new direct dependency.
- **`/loop` creates the job in the harness, not via the model**: it needs no model round trip,
  works on any provider, and makes the human's typed line the recorded origin. Claude Code routes it
  through a skill and the model.
- **`schedule_wakeup` without a prompt**: costs flexibility Claude Code has, but makes the tool
  read-only in effect and keeps a guardian call out of every loop iteration.

## Decisions

- Deviation from Claude Code: no fire-time jitter and no early firing of one-shots. Jitter exists
  to spread many sessions' load on one provider; a local single-session scheduler fires at the
  scheduled minute. Record in the master plan.
- Deviation: `schedule_wakeup` takes no `prompt`; it re-arms the loop's own prompt.
- Deviation: scheduling is interactive-only; headless `-p` exits after the answer and registers no
  scheduling tools, so a scheduled job could never fire there.
- Deferred: bare `/loop` (maintenance prompt, `loop.md`) and `/loop 20m /skill` expansion at fire
  time. A loop's prompt is plain text in this plan. File a follow-up issue when the PR opens.
- A model-created job's first user-visible trace is the transcript notice `scheduled: <id>` when it
  fires and the `Scheduled { created }` ledger record when it is created.

## Documentation

- `docs/tools.md`: four rows in the built-in table; the no-path sentence names them.
- `docs/tui.md`: a "Scheduled prompts" section (`/loop` forms, `/loop list|stop`, cadence rounding,
  firing only when idle, resume behavior, 50-job and 7-day limits, the status-bar indicator).
- `docs/configuration.md`: `[schedule] enabled`. `docs/permissions.md`: why `cron_create` is
  arbitrary and a scheduled turn is not user intent. `docs/headless.md`: not available.
- `docs/architecture.md`: the `scheduled` ledger record and `schedule.json` beside `todos.json`;
  `tasks/design/sessions.md`: `schedule.json` in the session directory layout.
- `CHANGELOG.md` is handled by the PR step.

## Testing Strategy

Unit (constructed inputs, fake `Clock`):
- `Cron`: every field form, ranges/steps/lists, Sunday as 0 and 7, the dom/dow OR rule, month/year
  rollover, Feb 29, rejected syntax (`L`, `MON`, 6 fields, out-of-range), never-matching expression,
  DST gap skipped (with a `chrono-tz`-free `FixedOffset` plus a hand-built gap check).
- `Scheduler`: create/list/delete, 50-job cap, prompt length, one-shot removal after firing, recurring
  re-arm from `now` (no catch-up after a long busy period), 7-day final fire then removal, persistence
  round trip, restore dropping expired/past/wakeup jobs with the warning text, unparseable file.
- Interval parsing for `/loop`: units, seconds rounding up, non-clean steps rounding, trailing
  `every N units`; `parse_command` tests; `the_table_is_sorted_by_name` stays green.
- Tools: argument validation, subagent call refused, `schedule_wakeup` with no active loop, error
  chain for `Persist`.
- Descriptor rows and the choke-point tests (`fontaine-tools` and `fontaine-core`) cover the four
  names; `tests/docs.rs` requires the docs rows.

Integration (fixture models, no live APIs; the TUI schedule tests drive the injected fake clock plus
the harness's real short `wait_until` sleeps, since tokio paused time cannot make a wall-clock job due):
- **Intent isolation** (silent permission failure, so automated): a scheduled turn runs, the ledger
  holds a `Scheduled { fired }` record and no `UserMessage` for it, and the guardian request built
  during that turn has `recent_user_messages` unchanged from before the fire.
- TUI: a due job starts a turn only when idle; a job due mid-turn fires once after it; `/loop 1m x`
  runs one immediate iteration then a scheduled one after a fake-clock advance; `/loop stop`; Esc
  cancels a pending wakeup; the self-paced fallback wakeup and loop end after an unrescheduled
  iteration.
- Config: `[schedule] enabled = false` registers no tools; project-scope setting refused.

## Open Questions

- Should `/clear` also cancel scheduled jobs? Default taken: no, jobs belong to the session, not
  the conversation context (Claude Code is silent).
