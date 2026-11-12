# Prompt Session Quit and Shutdown Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/592

## Overview

Make `/exit`, `/quit`, and empty-prompt Ctrl-D terminal actions even while work is running. Quit
will latch before any further turn or continuation, cancel session-owned work immediately, and
share one two-second deadline across conversation recovery, hooks, subprocess and server cleanup,
terminal restoration, Tokio/runtime teardown, scratch cleanup, and telemetry. A watchdog will force
the final process exit with terminal restoration retried if any destructor or exporter remains
stuck. Ordinary Ctrl-C/Esc interruption, per-agent kill/report grace, session switching, and
headless-run shutdown keep their existing semantics.

## Current State

- `/exit` and `/quit` parse as `Command::Exit` (`rust/fontaine-tui/src/commands.rs:280`), but the
  command is absent from `runs_mid_turn` (`commands.rs:134-160`), so `App::queue_during_turn`
  queues it behind the turn and earlier submissions (`app.rs:2030-2067`). Empty-prompt Ctrl-D
  explicitly does nothing while `App::running` (`app.rs:1561-1569`). Ctrl-D with text is an editing
  key, and Ctrl-D in an MCP elicitation means decline; those are separate meanings to preserve.
- The two generic running-work input loops collapse `Action::Quit` into `TurnEnd::Interrupted`
  (`ui/turn.rs:397-451` and `:1189-1199`). `Interrupted` restores undelivered input and returns to
  the idle loop. A quit during stop-hook serving is likewise treated as an ordinary interruption,
  after which goal handling and queued replay can continue. Input EOF already has the desired
  terminal propagation through `TurnEnd::InputClosed`.
- The idle loop drains deferred actions and can start notification, scheduled, and goal turns
  before it polls input (`ui/mod.rs:668-728`). The root and generic select loops poll their work
  future before input, so already-queued quit input can lose a same-poll race.
- `drive` awaits `session_end`, then `AgentTree::kill_all` and `settled(KILL_SETTLE)`, then jobs and
  LSP shutdown sequentially (`fontaine-tui/src/lib.rs:591-603`). `KILL_SETTLE` is 35 seconds
  (`fontaine-tools/src/kills.rs:17-22`), jobs have their own `STOP_GRACE + 5 s` bound
  (`jobs.rs:1099-1112`), and each LSP server honors its configured graceful timeout
  (`lsp/manager.rs:509-530`). MCP connection draining can add another independent bound during
  plugin removal (`agent/mcp_supervisor.rs:34-37`).
- Subagent launch and report-watch tasks are detached with `tokio::spawn` and their `JoinHandle`
  is discarded (`fontaine-core/src/child_runner.rs:565-580`,
  `fontaine-agents/src/spawn.rs:488-526`). Dropping `AgentRuntime` therefore does not abort a wedged
  child or its watcher. Its kill path allows 30 seconds to reach a rig hook boundary and save a
  transcript; that is useful for an ordinary kill but cannot be the final-process-exit policy.
  `AgentTree::kill_all` also latches shutdown without closing every reservation/resume path, so a
  racing descendant must be refused atomically rather than merely killed after attachment.
- `Jobs` owns worker threads and joins them in `Drop` (`fontaine-tools/src/jobs.rs:1122-1136`).
  Managed SearXNG shutdown signals immediately but schedules its hard kill after a ten-second grace
  (`websearch.rs:180-231`). Status-line work is another detached task
  (`fontaine-tui/src/ui/mod.rs:274-296`). MCP watchers/reconnects need explicit retirement; shared
  SearXNG instances are deliberately not session-owned.
- `fontaine_tui::run` joins the terminal reader after `drive` (`lib.rs:826-827`), and its
  `TerminalGuard` restores the terminal only on return/drop. The binary then drops scratch state
  and the Tokio runtime before `Telemetry::finish` (`fontaine/src/lib.rs:470-559`). Telemetry has a
  separate two-second watchdog (`fontaine/src/telemetry/mod.rs:263-280`), so it neither shortens
  earlier cleanup nor shares a deadline with it.
- Root recovery can preserve an interrupted turn through `keep_unsettled_turn`, but `InputClosed`
  currently skips that fold (`ui/turn.rs:1245-1273`). Some pre-run conversation-length and journal
  writes happen before the root select begins (`ui/turn.rs:1045-1085`), so blocked persistence can
  also prevent quit input from being served at all.

## Design

### 1. Latch quit once, and make it terminal rather than an interrupt

Add an idempotent `Ui::request_quit` and a `quit_started: Option<Instant>` latch. Route every
`App::handle_input` call in `Ui` through one helper which invokes the latch for `Action::Quit`
(six call sites, the running-work and nested-modal loops; see Step 1). The idle-prompt dialogs under
`ui/commands/` (`afk.rs`, `skills.rs`, `guardian.rs`, `bug.rs`, `plugin.rs`, ...) also call
`App::handle_input` but are unchanged: they are opened from `dispatch_command`, where Quit already
exits through `Break`. The latch:

- records the first timestamp and never clears;
- closes the input receiver, discards pending deferred/session-switch state, and prevents a queued
  `/resume` or `/branch` from winning over exit;
- aborts the owned status-line task and starts final cancellation through the runtime immediately,
  before recovery or lifecycle hooks;
- arms the process watchdog through the production callback described below.

Special-case `Command::Exit` in `App::queue_during_turn`: return the existing immediate
`Action::Command { command: Exit, raw }` instead of `Action::Queue`, without adding exit to
`runs_mid_turn` (whose contract remains synchronous UI-only commands). `run_immediate_inner` gains a new
`Exit` arm which calls `request_quit` (it has none today), and the `serve_modal` and provider loops get
matching `Command::Exit` handling; idle `dispatch_command`
returns `Break`, and both mid-turn input loops stop on the latch. Empty-prompt Ctrl-D now returns
`Action::Quit` regardless of `running`. It keeps its text-editing behavior with nonempty input and
keeps the MCP form/URL meaning of decline, because the issue applies only where Ctrl-D means quit.
Ctrl-C/Esc remain ordinary interrupts and never set the latch.

Use `TurnEnd::InputClosed` as the outward terminal outcome, not `Interrupted`: it already stops
queued replay and produces `DriveOutcome::Exit`. In the root `attempt`, first unwind a quit through
the existing recovery path with `Unsettled::SessionEnded`, then normalize to `InputClosed` before
`turn()` can run stop/stop-failure hooks, a stop-hook continuation, goal handling, a retry, or
queued replay. Other `serve_while` callers can return `InputClosed` directly. Check the latch at
`Ui::run`, `converse`, modal serving, deferred draining, provider/model/session changes, and every
notification, scheduled, goal, AFK-review, stop-hook, and retry entry point. Give pending input
priority over work futures in the root and generic select loops, and service already-buffered
input before idle automatic work, so a quit already waiting cannot start another continuation.

Move root pre-run conversation-length reads and run-start journal writes into the future guarded
by the root input select. No persistence await may sit outside quit-aware polling. Recovery on the
quit path is bounded by the same shutdown deadline; ordinary interrupt recovery is unchanged.

### 2. One final-exit deadline and a cancellation-first coordinator

Use a named `FINAL_SHUTDOWN_BUDGET = Duration::from_secs(2)`, measured from the first quit request
(or from final input-channel closure when there was no explicit key/command). `DriveOptions` is a new type, passed as a new
`drive` parameter (`fontaine-tui/src/lib.rs:~403-411`), carrying the budget and an exit callback; production uses the constant, while tests can inject a short
budget without an environment variable or a new user-facing setting. The two seconds are fixed, not
configurable.

Split final process exit from session switching. `DriveOutcome::Switch` keeps the existing graceful
`resume` teardown, including `KILL_SETTLE`; headless paths are also untouched. Final
`DriveOutcome::Exit` uses a new `AgentRuntime` shutdown coordinator with two idempotent phases:

1. **Begin synchronously.** Latch the agent tree against wakes/spawns, abort nested agent tasks,
  request forced termination of every job/process group, retire MCP reconnect/watch activity, and
  stop new TUI work. This phase is called by `request_quit`, before any root recovery await.
2. **Finish concurrently.** Under one `tokio::time::timeout_at` using the original deadline, run
   `session_end("prompt_input_exit")`, best-effort root unsettled-turn recovery, job reaping, MCP
   connection closes, LSP termination, and owned-helper termination. No stage gets a fresh
   multi-second timeout and no stage waits for an earlier stage's long grace. Dropping the joined
   futures on timeout drops their process/transport guards; the process watchdog remains the hard
   backstop for blocking destructors.

| Work or resource | Final-exit behavior |
|---|---|
| Main model/provider/tool, compaction, usage wait, skill/MCP prompt, `!`, lifecycle hook | Drop the selected run future immediately. Its existing drop guards cancel prompts and kill owned process groups. No stop-hook or other continuation follows. |
| Nested agents, including one never reaching a rig hook boundary | Retain launch and report-watch `AbortHandle`s; add a final-only `ChildHandle::abort` and `AgentTree::abort_all`. Abort immediately without `settled(KILL_SETTLE)` and without fabricating killed reports. Make reservation and direct resume reject the tree's shutdown latch atomically; late attach still replays the kill. Ordinary `kill`, `KILL_GRACE`, transcript recovery, and headless settlement are unchanged. |
| Background jobs and monitors | Add a forced, deadline-bounded variant alongside the existing `Jobs::terminate_all` (left unchanged); it tells job workers to skip ordinary stop grace and terminate their groups, plus a wait bounded by the shared deadline. No job notifications are posted. Unix keeps group signals; Windows keeps its current direct-child kill. `Jobs::drop` remains protected by the process watchdog. |
| MCP servers | Retire every slot/watch/reconnect first, take and close all connections concurrently without the plugin-removal ten-second drain, and synchronously group-kill each stdio server's retained pid with `procgroup::terminate` on timeout/drop so descendants cannot survive on Unix (Windows keeps rmcp's direct-child kill). |
| LSP servers | Add a final `terminate_all` path which removes clients, terminates their process groups instead of honoring each configured graceful shutdown timeout, and reaps concurrently within the shared deadline. Keep `shutdown_all` for normal reset/reload paths. |
| SearXNG and managed helpers | Take and terminate session-owned children/process groups immediately rather than scheduling a later hard kill. Do not kill a configured shared SearXNG instance or other resource the session does not own. Provider CLI children continue to use their existing whole-tree drop guard. |
| TUI-owned tasks and input | Close input, abort status-line and notice-forwarder tasks, stop quota pollers with `Ui`, and stop the terminal reader. Any other fire-and-forget task touched by this work must be owned and aborted rather than left to runtime drop. |
| `session_end` and final persistence | Start only after cancellation has begun, concurrently with resource reaping. Both are best-effort within the remaining budget. Already-journaled child messages and settled reports survive; no final report is awaited. |

MCP stdio currently relies on rmcp's direct-child `kill_on_drop`, which does not cover descendants. Keep rmcp's `TokioChildProcess` and its graceful stdio close; on Unix set `process_group(0)` on the existing command, record `TokioChildProcess::id()` before `serve`, and at final shutdown call `procgroup::terminate` on that pid synchronously. `procgroup::terminate` is a no-op off Unix, so Windows keeps the direct-child `kill_on_drop` (matching the jobs row); whole-tree Windows MCP cleanup is out of scope. The headers helper is unchanged: it runs to completion during connect and cannot outlive quit.

`fire_session_end` is not called before `kill_all` on final exit. The `resume` and `clear` hook
reasons and their behavior are unchanged.

### 3. Carry the watchdog through the real process-exit path

Add an `ExitWatchdog` in `fontaine-tui/src/shutdown.rs`. Arming it spawns a standard thread which,
at the original deadline, calls the existing idempotent `restore_terminal` and then
`std::process::exit(0)` (interactive Exit maps to `Outcome::Done(0)`). Dropping/disarming the watchdog after successful final
cleanup prevents a later firing; tests will inject the restore/exit actions rather than exit the
test process.

Keep `DriveOutcome` unchanged for the existing `drive` test API. The production
`fontaine_tui::run` will return a small `RunOutcome { outcome: DriveOutcome, watchdog:
Option<ExitWatchdog> }`: its `DriveOptions` callback stores the watchdog outside `Ui`, which lets
`run` bound the reader join by the same deadline and then hand the still-armed watchdog to the
binary. `TerminalGuard` still performs normal restoration; the watchdog repeats only the portable
raw-mode/cursor/bracketed-paste restoration on the forced path, without trying to take a stuck
screen mutex.

In `fontaine/src/lib.rs`, the `Outcome::Done(code)` arm breaks out of the loop with `(code, watchdog)`,
and the watchdog is moved into a binding declared outside the loop and the `match export.runtime()`
scope, so it outlives session-span and scratch destruction and Tokio runtime drop. It is not held in
`Outcome` or disarmed by `Drop`. Only after runtime teardown call `Telemetry::finish`, then disarm
the watchdog explicitly. Change `Telemetry::finish` to accept an optional
absolute deadline: a final quit uses only the remaining time and needs no second watchdog; other
exits retain the current `FLUSH_CAP` behavior. OTLP's per-provider timeout argument is the remaining
duration, not a fresh two seconds, and the global watchdog still covers Micromegas drop and any
third-party shutdown which ignores its timeout.

The measured interval is therefore:

```text
quit input
  → latch + abort/terminate requests
  → recovery + session_end + concurrent resource cleanup
  → reader stop + final render + TerminalGuard
  → scratch and Tokio runtime drop
  → telemetry finish
  → watchdog disarm / normal return

At the same original deadline in every case: restore terminal again, then force process exit.
```

### 4. Preserve ordinary behavior deliberately

- Esc/Ctrl-C still interrupt and restore undelivered text; only quit takes the terminal
  `InputClosed` path.
- Ctrl-D with input text still deletes, and MCP elicitation Ctrl-D still declines.
- Per-agent kill keeps `KILL_GRACE` and graceful partial-report saving. The final-only abort path
  does not weaken it, and headless `KILL_SETTLE` call sites remain unchanged.
- `/resume` and `/branch` keep graceful switching and cannot be overridden by a late pending
  selection after quit.
- Scheduled prompts and goals are not deleted from disk; their in-memory continuations simply
  cannot start after the latch. Queued user input is discarded rather than restored or submitted.

## Implementation Steps

1. **Quit recognition and latch** — `fontaine-tui/src/app.rs`, `commands.rs`, `lib.rs`,
   `ui/mod.rs`, `ui/turn.rs`, `ui/mcp.rs`, and `ui/providers.rs`:
   - make both aliases immediate and empty-prompt Ctrl-D quit while running;
   - add the common `Ui` input wrapper, `request_quit`, a new `DriveOptions` type, and continuation
     guards. The six running-work and nested-modal `Ui` input loops call `request_quit` on `Action::Quit`: the idle loop
     (`ui/mod.rs:~745`), the two generic running-work loops (`ui/turn.rs:~413`, `:~1196`),
     `serve_modal` (`ui/turn.rs:~812`, today `Break`), the MCP approval prompt (`ui/mcp.rs:~90`,
     today returns false), and the provider sign-in loop (`ui/providers.rs:~228`, today grouped
     with `CancelSignIn`). In `providers.rs`, `Action::Quit` and `/exit` (an `Action::Command`
     the loop currently swallows) latch quit and return terminal `InputClosed` instead of
     cancelling sign-in;
   - propagate quit as `InputClosed`, with root recovery completed before that normalization;
   - prioritize pending input and move pre-run persistence inside the selectable root future;
   - own/abort the status-line `JoinHandle` and close input on quit.
2. **Final-only nested-agent abort** — `fontaine-agents/src/runner.rs`, `tree.rs`, `spawn.rs`,
   and `fontaine-core/src/child_runner.rs`:
   - store launch and report-watch `AbortHandle`s in the run/node state;
   - add a default `ChildHandle::abort` (falling back to `kill` for test/simple handles) and a core
     override which aborts the task after setting its kill flag;
   - add `AgentTree::abort_all`, preserving its existing shutdown/wake latch, and leave
     `kill_all`/`settled` unchanged for other callers;
   - make `reserve_named` and direct `resume` reject shutdown under the same lock as their state
     transition, retaining late-attach kill replay and adding no synthetic report or ledger record.
3. **Bounded subsystem shutdown** — add the runtime coordinator in
   `fontaine-core/src/agent/shutdown.rs` and the resource-specific final paths in
   `fontaine-tools/src/jobs.rs`, `lsp/client.rs`, `lsp/manager.rs`, `websearch.rs`,
   `mcp/servers.rs`, `procgroup.rs`, and `fontaine-core/src/agent/mcp_supervisor.rs`. Use the
   process-group helpers already present; do not add a dependency or change ordinary shutdown
   methods. MCP stdio servers gain a Unix process group and a final group kill; no custom transport.
4. **Final TUI teardown and watchdog** — add `fontaine-tui/src/shutdown.rs`; split final exit from
   switch teardown in `fontaine-tui/src/lib.rs`; run final stages concurrently against the one
   deadline; bound reader joining; return `RunOutcome` from production `run` while keeping
   `DriveOutcome` stable for `drive` callers. Update the `drive` call sites in
   `rust/fontaine-tui/src/lib.rs` (production, ~797) and `rust/fontaine-tui/tests/common/mod.rs`
   (~465, ~505) for the new parameter.
5. **Binary and telemetry propagation** — `fontaine/src/lib.rs`, `telemetry/mod.rs`, and
   `telemetry/otlp.rs`: carry the watchdog through runtime/scratch teardown, pass the absolute
   deadline into telemetry, and disarm only after telemetry finishes. Do not call `process::exit`
   early enough to skip normal restoration or a flush which can finish within budget.
6. **Tests and documentation** — add `fontaine-tui/tests/quit.rs` and focused unit/integration
   coverage below; update `docs/tui.md`, `docs/hooks.md`, `docs/agents.md`, `docs/architecture.md`,
   `docs/telemetry.md`, `docs/configuration.md`, `tasks/design/sessions.md`, `tasks/design/observability.md`, `CHANGELOG.md`, and the
   master plan's dated Decisions as described below.

## Files to Modify

- `rust/fontaine-tui/src/app.rs`, `commands.rs`, `lib.rs`, `shutdown.rs` (new),
  `ui/mod.rs`, `ui/turn.rs`, `ui/mcp.rs`, `ui/providers.rs`, `ui/tests.rs`
- `rust/fontaine-tui/tests/quit.rs` (new), `tests/common/mod.rs`, and focused existing TUI tests
- `rust/fontaine-agents/src/runner.rs`, `tree.rs`, `spawn.rs`, and agent test doubles implementing
  `ChildHandle` if the default abort is insufficient
- `rust/fontaine-core/src/child_runner.rs`, `agent.rs`, `agent/shutdown.rs` (new),
  `agent/mcp_supervisor.rs`
- `rust/fontaine-tools/src/jobs.rs`, `procgroup.rs`, `lsp/client.rs`, `lsp/manager.rs`,
  `websearch.rs`, `mcp/servers.rs` (and a focused stdio-transport helper if that keeps the change small)
- `rust/fontaine/src/lib.rs`, `telemetry/mod.rs`, `telemetry/otlp.rs`
- `docs/tui.md`, `docs/hooks.md`, `docs/agents.md`, `docs/architecture.md`,
  `docs/telemetry.md`, `docs/configuration.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md` (TUI, Agents, Tools, Extensibility, and Observability Decisions),
  `tasks/design/sessions.md`, `tasks/design/observability.md`

## Documentation

- `docs/configuration.md`: the MCP `shutdown_timeout_ms` row notes that the shared final-exit budget
  overrides it on quit.
- `docs/tui.md`: both aliases and empty-prompt Ctrl-D quit during
  work; queued work and automatic continuations are discarded; the two-second whole-exit bound;
  editing/elicitation and ordinary interrupt distinctions; update the mid-turn command and
  keybinding descriptions.
- `docs/hooks.md`: final-exit `session_end` starts after cancellation and shares the remaining
  global budget, so a hook may be cut off earlier than its own timeout.
- `docs/agents.md` and `docs/architecture.md`: final-only nested-task abort and best-effort
  persistence versus ordinary graceful kill; session-owned process/server cleanup.
- `docs/telemetry.md`: final quit uses the remaining whole-exit budget rather than a fresh
  two-second flush; other exits keep the existing cap.
- `CHANGELOG.md`: `Fixed` entry for prompt, bounded quit and process cleanup.
- `tasks/fontaine_harness_plan.md`:
  - TUI: dated line recording immediate `/exit`, `/quit`, empty-prompt Ctrl-D and the two-second
    final-exit policy; it supersedes the 2026-10-29 statement that all built-ins except `/loop`
    queue mid-turn for these terminal commands.
  - Agents: dated line stating that final process exit aborts nested tasks without
    `KILL_SETTLE`, superseding the 2026-10-31 exit-wait choice only for final interactive exit;
    ordinary kills, headless, and session switches retain graceful transcript semantics.
  - Extensibility: dated line recording cancellation-before-`session_end` on final exit and MCP
    stdio's adoption of Fontaine's whole-tree guard.
  - Observability: dated line recording that final quit telemetry shares the global remainder and
    the independent watchdog restores the terminal before forcing exit.
- `tasks/design/sessions.md` and `tasks/design/observability.md`: describe the final-exit recovery
  and telemetry guarantees as they now behave, including journal-on-resume after a forced exit.

## Testing Strategy

All tests are offline and deterministic except for short wall-clock bounds; no live provider,
collector, or external MCP/LSP service is used.

- **App/command units**: both aliases are immediate; empty Ctrl-D quits idle and running; nonempty
  Ctrl-D editing, rebound quit keys, MCP decline, and Ctrl-C/Esc interruption are unchanged.
- **TUI latch/unwind units**: quit maps to terminal `InputClosed`, not replay; stop hooks cannot
  request another turn; deferred model/provider/resume changes, AFK review, notification,
  scheduled, and goal starts are all refused after the latch. Include already-buffered quit input
  and a pre-run persistence future which remains pending.
- **`fontaine-tui/tests/quit.rs`**: for `/exit`, `/quit`, and Ctrl-D, hold a routed model response,
  keep the input sender alive so EOF cannot make a broken implementation pass, and assert each path
  finishes within the injected budget plus scheduling tolerance. Queue a message/command behind
  quit and assert request counts and markers show no subsequent work. One drive-level test per
  trigger, and one test per scenario (nested agents, one wedged before its kill boundary, a blocked
  stop hook, idle quit) using a single trigger.
- **Watchdog**: at the injected unit level, with injected restore/exit actions, cover firing at the
  deadline (restore then exit) and disarm preventing a later firing.
- **Whole-process wall clock**: one binary-level test (pattern: `rust/fontaine/tests/exit_code.rs`,
  `CARGO_BIN_EXE_fontaine`) verifying the watchdog survives runtime drop, with actual process exit
  measured within the original budget.
- **Agent tests**: a nested task and its report watcher which never reach a rig hook boundary are
  aborted promptly; abort is idempotent; reservation and direct resume after the latch fail; a
  reservation already in flight is killed on attach. No synthetic report is written. Existing
  ordinary-kill transcript and `KILL_SETTLE` tests remain unchanged and green.
- **Resource tests**: use real short-lived child/grandchild scripts and existing fake MCP/LSP
  connectors to cover forced job/monitor group termination, unresponsive LSP shutdown, MCP close
  without drain/reconnect, an MCP wrapper child with a live grandchild, and owned SearXNG/helper
  termination within budget; verify a shared SearXNG is not killed. The MCP case must prove both
  descendants die, not only the direct server process. Existing user-shell and provider-CLI tests
  confirm their drop guards still terminate the whole tree.
- **Hooks/persistence/telemetry**: slow `session_end`, pending root recovery, and blocked exporter
  stand-ins all share the same deadline rather than summing timeouts. `session_end` fires once with
  `prompt_input_exit`; healthy recovery persists the partial root turn, while a wedged child report
  is never awaited.
- **Regression suite**: run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` from `rust/`; run the Windows GNU clippy compile check locally because process-group
  and Job Object code changes must compile there too.

## Manual Verification

Real terminal state cannot be established by `TestBackend`. In a detached real tmux, run a debug
Fontaine with a deliberately held local model route and a long-lived child job; send each of
`/exit`, `/quit`, and empty-prompt Ctrl-D during the turn. Measure key-to-shell time, verify the job
and its child are gone, and run `stty -a` plus a cursor-visible smoke command at the returned shell
to confirm canonical input and cursor restoration. This checks actual terminal modes and process
reaping rather than duplicating the renderer model.

## Open Questions

None.

## Decisions

- AFK: MCP stdio keeps rmcp's `TokioChildProcess` with `process_group(0)` and a final `procgroup::terminate` on its pid (Unix whole-tree; Windows stays direct-child); no `AsyncRwTransport`/`TreeGuard` rewrite and no headers-helper conversion.
