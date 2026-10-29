# Queued Skill Invocations Plan

Issue: madesroches/fontaine#292.

## Overview

A skill invocation (`/name args`) typed while a turn is running is currently refused outright
("a turn is running; wait for it or press Esc to interrupt"). It should queue exactly like a
plain message and execute — expanded as the skill it is — once the current turn ends. Built-in
commands and `!` shell commands queue too, as Claude Code documents, and execute through the
same dispatch they would get at an idle prompt; a small set of purely-UI commands (panels,
reports, theme) runs immediately mid-turn, Fontaine's analogue of Claude Code's run-immediately
carve-outs. Only unknown slash names stay refused mid-turn. The lifecycle is the same for every
input: entered, queued (or not), executed.

## Current State

- `App::enter` (`rust/fontaine-tui/src/app.rs:1004-1018`): while `self.running`, any input that
  `parse_command` recognizes — built-in or `Command::Other` (a would-be skill) — is refused with
  a notice and the text stays in the input; the earlier `!` branch is refused the same way ("run
  shell commands between turns"). Plain text becomes `Action::Queue(String)`.
- Queue plumbing: `Action::Queue(text)` in the turn loop (`rust/fontaine-tui/src/ui/turn.rs:338`)
  and in `serve_while` (`turn.rs:184`) pushes the bare text onto `runtime.steering`
  (`rust/fontaine-core/src/steering.rs`), which stores `Vec<String>`.
- `SteeringHook` (`steering.rs:66-86`) appends queued strings to the root agent's next tool
  result and records each as `Record::UserMessage` in the ledger at delivery time.
- End of turn: `end_of_turn` (`turn.rs:39-49`) takes `steering.retract()` (undelivered strings)
  and returns `EndOfTurn::Submit(joined)` after an answer, `EndOfTurn::RestoreToInput(joined)`
  after an interrupt/failure. `converse` (`turn.rs:225-262`), `scheduled_turn`, `goal_turn` and
  `goal_step` (`rust/fontaine-tui/src/ui/goal.rs:98-102`) then send the joined text as the next
  turn **as a plain message** — `converse` never re-parses it as a command, so a queued
  `/review 42` would reach the model as literal text. This is the gap the issue names: the queue
  loses that the entry was a skill invocation.
- Skill dispatch at idle: `Ui::submit`'s `Command::Other` arm
  (`rust/fontaine-tui/src/ui/mod.rs:392-423`) checks the name against `runtime.slash_skills()`,
  records `Record::UserMessage` with the raw text, expands via `expand_serving`
  (`turn.rs:197-217`, which serves prompts while `runtime.expand_skill` runs, since a skill's
  `!` commands can ask for approval) and runs `converse(raw, Some(expansion))`.
- `App.menu` already holds one row per user-invocable skill (`rust/fontaine-tui/src/lib.rs:315`),
  excluding skills hidden by a built-in — exactly the set `submit` would run as a skill.
- Claude Code parity: its docs state "If you send a command while Claude is responding, Claude
  Code queues it and runs it after the current turn finishes." Queued-and-run-at-turn-end is the
  documented behavior, not interrupt-and-run.

## Design

### Core: typed queue entries (`rust/fontaine-core/src/steering.rs`)

```rust
/// One queued input: a plain message, a skill invocation as typed, or a built-in command
/// as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Queued {
    Message(String),
    Skill { raw: String, name: String, argument: String },
    /// A built-in command exactly as typed (`/compact fix tests`); re-parsed and dispatched
    /// when it executes.
    Command(String),
    /// A `!` shell command as typed, prefix included (`!cargo test`); executed as a shell
    /// command when the turn ends.
    Shell(String),
}

impl Queued {
    /// The text as typed: the message, the slash command line or the `!` line. Used for the
    /// queue display, the input restore and the history.
    pub fn raw(&self) -> &str;
}
```

- `Steering::queue(entry: Queued)`; the queue holds `Vec<Queued>`.
- `Steering::queued()` keeps its `Vec<String>` signature (raws), so the display
  (`app.queued`, `view.rs queued_lines`, "queued: /review 42") is unchanged.
- `Steering::retract()` returns `Vec<Queued>`.
- `Steering::set_delivery_hold(bool)` holds replay delivery without hiding or consuming entries:
  while held, `SteeringHook::steer` leaves the whole queue untouched and appends no ledger records;
  `queued()` and `retract()` behave normally. Releasing the hold restores ordinary steering.
- `SteeringHook::steer` delivers only the **leading run of `Message` entries** and stops at the
  first non-`Message` entry (`Skill`, `Command`, `Shell`), which stays queued with everything
  behind it. None of them can be delivered mid-turn: a skill's expansion is async, can run `!`
  commands that need approval, and must become a prompt of its own; a command or shell line
  steered into a tool result would read as model input it never was. The barrier also keeps the
  order the user typed in: a message queued after one of them is sent after it executes, not steered
  into the turn ahead of it.

### TUI: queue slash input (`rust/fontaine-tui/src/app.rs`, `commands.rs`)

- `Action::Queue(String)` becomes `Action::Queue(Queued)` (imported from
  `fontaine_core::steering`); a new `Action::Command { command: Command, raw: String }` carries
  a command that runs immediately and its trimmed text for history.
- `Command::runs_mid_turn()` (`commands.rs`, beside the `BUILTINS` table) marks the immediate
  set: the commands whose handler is synchronous and touches only UI state — `help`, `status`,
  `theme`, `todos`, `agents`, `denied`, `permissions`, `hooks`, `skills`, `plugins`, `mcp`,
  `expand`, `config`. Everything else queues: it mutates the session or conversation (`clear`,
  `compact`, `resume`, `rewind`, `model`, the guardian toggles, ...), starts a turn (`plan`,
  `goal`, `loop`, `init`), awaits the runtime (`context`), or can block (`copy` can wait for
  `tmux`, `memory <n>` opens an editor, `export` writes a file). The rule, not the list, is the
  contract: a command joins the set only if running it cannot touch the turn, the conversation,
  the ledger, or a blocking wait. (`/config` qualifies today: its changes already defer through
  `pending_config` when a turn is running.)
- The `!` branch in `enter` queues instead of refusing when a turn runs (the bare-`!` early
  return is unchanged):

  ```rust
  if let Some(rest) = text.strip_prefix('!') {
      let command = rest.trim();
      if command.is_empty() {
          return Action::None;
      }
      self.input.take();
      if self.running {
          return Action::Queue(Queued::Shell(text.trim().to_string()));
      }
      return Action::Shell(command.to_string());
  }
  ```

- In `enter`'s `self.running` branch:

  ```rust
  match parse_command(&text) {
      Some(command) if command.runs_mid_turn() => {
          self.input.take();
          Action::Command { command, raw: text.trim().to_string() }
      }
      Some(Command::Other { name, argument })
          if self.menu.iter().any(|entry| entry.name == name) =>
      {
          self.input.take();
          Action::Queue(Queued::Skill { raw: text.trim().to_string(), name, argument })
      }
      Some(Command::Other { .. }) => { /* existing refusal notice, input kept */ }
      Some(_) => { self.input.take(); Action::Queue(Queued::Command(text.trim().to_string())) }
      None => { self.input.take(); Action::Queue(Queued::Message(trimmed)) }
  }
  ```

  The menu check is the known-skill test: a bare unknown `/name` keeps the current refusal.
  `App` needs no new state — `menu` is already the user-invocable skill list plus built-ins.
- `Action::Command` is served by both input loops (the turn loop and `serve_while`), like
  `Action::CycleMode` today: each calls `self.remember(&raw)` before a small dispatch calls the
  same handler methods `submit`'s arms call. The catch-all ignore lists in the main loop and the
  provider dialog gain the variant.

### TUI: typed end-of-turn execution (`rust/fontaine-tui/src/ui/turn.rs`)

- The two `Action::Queue(entry)` arms (`turn.rs:338`, `turn.rs:184`) pass the entry through:
  `self.remember(entry.raw()); runtime.steering.queue(entry);`
- `end_of_turn` takes `Vec<Queued>` and groups it:

  ```rust
  #[derive(Debug, PartialEq, Eq)]
  pub(crate) enum Submission {
      /// Consecutive queued messages joined with a blank line, as today.
      Message(String),
      Skill { raw: String, name: String, argument: String },
      /// A built-in command as typed; re-parsed and executed through `submit`'s dispatch.
      Command(String),
      /// A `!` shell command as typed; run through the idle `Action::Shell` path.
      Shell(String),
      /// A turn ready to run: the text the existing idle path displays and checkpoints (not
      /// necessarily the raw command), plus its expanded prompt. Its user-message record has
      /// already been appended, as today. Only the dispatcher and seeding callers create one;
      /// it never waits behind another submission (see the front-push invariant below).
      Expanded { text: String, prompt: String },
  }

  pub(crate) enum EndOfTurn {
      Submit(Vec<Submission>),
      RestoreToInput(String), // raws joined, as today
      Nothing,
  }
  ```

  Grouping walks the retracted entries in order: consecutive `Message` entries join into one
  `Submission::Message` with `\n\n` (today's behavior); each `Skill`, `Command` or `Shell`
  entry is a submission of its own.

- `converse`'s signature becomes `converse(runtime, pending: VecDeque<Submission>)`: the queue
  is a seeded parameter, and the loop just pops one submission at a time — there is no
  first-iteration special case. Idle `submit` seeds it with one entry (`Message(text)` for a
  plain message, `Expanded { text: raw, prompt }` for a skill, whose expansion happens in
  `submit` ahead of the call as today); `scheduled_turn`, `goal_turn`, `goal_step` and
  `notification_turn` seed it from their `EndOfTurn::Submit(subs)`. Each turn's own
  `EndOfTurn::Submit(subs)` back-extends `pending`.
  **Replay-hold invariant:** when the loop starts a submission with later submissions still
  pending, it sets steering's delivery hold; it releases the hold when the final pending
  submission begins and on every exit from the execution loop. Thus a message typed during one
  replay turn cannot steer ahead of older submissions waiting in `pending`: it retracts and
  back-extends in typed order instead. Ordinary mid-turn steering resumes for the final pending
  submission because no older queued input remains.
  Nothing sits in `pending` already expanded except `Expanded`, and a `Submission` that needs
  preparation is prepared at pop time, against live state: a
  `Submission::Skill` is expanded (see below) and run as the seeded `Expanded` is — as
  `converse(raw, Some(prompt))` is today.
  Esc during a *turn* of the execution loop behaves exactly as an idle turn-interrupt:
  `RestoreToInput`, with the raws of the submissions still pending joined onto the restored
  text, so the loop loses nothing. Esc is live while a queued skill expands (expansion serves
  keys and the shared runner marks it running), and that interrupt aborts the expansion: the
  interrupt notice shows and the skill's raw is spent (it is in history and the ledger). A
  failed expansion likewise shows the idle path's error notice and does not stop execution: the
  submissions behind it execute on, each queued entry independent of the one that failed.
  Ordinary text typed while a queued expansion executes is dropped, exactly as at an idle prompt
  today. A `Submission::Shell`
  strips the prefix and executes through the same `shell()` the idle `Action::Shell` path
  uses; while it runs, `shell_running` holds and nothing queues, exactly as an idle `!` run,
  and its output reaches the next submission's turn as it reaches the next message at idle.
  Note the pitfall:
  `converse` → skill-run → `converse` as separate async fns is async recursion; keep the
  dispatch inside `converse`'s own loop, with expansion factored into a helper that does not
  call `converse`.

- Shared skill runner: extract the body of `submit`'s `Command::Other` arm (`ui/mod.rs:392-423`)
  — known check, `Record::UserMessage` of the raw text before expansion, `expand_serving`, error
  and interrupt notices — into a helper used by both idle `submit` and the execution loop, so a
  queued skill is ledgered, expanded and error-reported exactly as if typed at an idle prompt.
  The runner sets `app.running` before expansion and clears it on success, error, interrupt and
  input close, so Esc produces `Action::Interrupt` on both idle and queued expansion paths.
  (`expand_serving` already serves approvals while the skill's `!` commands run.)
- Shared command dispatcher: extract `submit`'s `match parse_command` into a helper used by both
  `submit` and the execution loop, so a queued `/rename foo` or `/compact` does exactly what it
  would at an idle prompt. Two constraints fall out of running it inside `converse`'s loop:
  `launch_dir` (which `submit` takes and `converse` does not) is threaded in — through
  `notification_turn`, `scheduled_turn`, `goal_turn` and `goal_step` too, which reach the
  execution loop without it today and whose queued `/add-dir` or `/export` needs it — and an arm
  that starts a turn hands a submission back to the loop instead of calling `converse` (the same
  async-recursion rule as skill expansion). `/plan <task>` returns `Message(description)`;
  `/goal <condition>`, `/loop`, `/init` and the skill arm return `Expanded`. The dispatcher owns
  no queue: it processes exactly the one `Command` it was popped for, and the loop front-pushes
  the returned turn submission. **Front-push invariant: a dispatcher-returned `Message` or
  `Expanded` runs next, ahead of everything already pending** — nothing executes between a
  command's preparation and its turn, so the prompt always reflects the state its own dispatch
  left, and never goes stale behind later entries.
  Ledger, transcript and checkpoint behavior stay per command, exactly as at idle today. `/plan
  <task>`'s `Message(description)` takes the ordinary recording path. Every `Expanded` keeps the
  `text` its command passes to `converse` today and has already appended the same user-message
  record as today: raw text for skills, `/goal` and `/loop`, and the enriched intent for `/init`.
  In particular, a skill's raw remains recorded **before** expansion so the guardian judging its
  expansion-time `!` commands has that intent. A self-paced `/loop` handoff also retains the
  existing post-first-iteration `end_of_iteration` call when its execution returns `Continue`,
  arming the fallback or ending the loop as today; `Break` skips it.
- A queued command that opens a picker or dialog (`/model` or `/guardian-model` without an
  argument, bare `/resume`, `/rewind`, `/login`) suspends replay until that UI closes or is
  cancelled; later submissions never run underneath it. Dispatch reports `AwaitingModal`, and
  the execution loop serves the picker/dialog with the same handlers as the idle loop, applying
  model/provider choices directly rather than through `pending_pick`/`pending_provider`, awaiting
  rewind actions, and only then popping the next submission. A resume selection follows the
  switch guard below.
- `Break` from quitting propagates out of the loop. A queued `/resume <argument>` or `/branch`
  with later submissions is refused before any switch side effects, with a notice, and replay
  continues in the current session so no typed input disappears; the same refusal applies when a
  bare `/resume` selection would switch with submissions still pending. As the last submission,
  a successful switch propagates `Break` as today.
- `notification_turn`, `scheduled_turn`, `goal_turn` and `goal_step` route their
  `EndOfTurn::Submit` through the same pending-aware path, so a skill queued during one of these
  turns runs when it ends, ahead of the next scheduled or goal turn (in `goal_step`, the queued
  input already preempts the goal turn — that order is kept). `notification_turn` returns
  `ControlFlow` like `scheduled_turn` (its caller in `run` today only checks `InputClosed`):
  without this, an entry queued while a background agent's report is read — never steerable,
  never retracted there — would sit in the queue until some later turn ends.
- `/compact` (`ui/commands/context.rs:77`) and the goal-evaluation interrupt path
  (`ui/goal.rs:93`) retract and restore to the input: join `raw()`s. A skill queued during a
  compaction comes back as `/name args` in the input, like messages do — a compaction has no turn
  to receive a prompt.

### What does not change

- Unknown slash names mid-turn: refused as today. Nothing queues while a `!` run holds the
  terminal — including one running from the queue; no agent run consumes the queue then.
- Mid-turn steering of plain messages when no older replay submissions are pending, per-command
  ledger text and execution-time recording (a skill records its raw immediately before expansion,
  as at idle), the queue display, and Up-takes-back editing (`edit_queued` restores raws, which
  re-parse on submit).
- Headless `fontaine -p`: no mid-turn input exists there.

## Implementation Steps

1. `rust/fontaine-core/src/steering.rs`: add `Queued` and the delivery hold, retype
   `queue`/`retract`, make `SteeringHook::steer` deliver only the leading message run and deliver
   nothing while held. Update the existing unit tests (they queue strings) and add: a skill or
   command entry blocks mid-turn delivery of itself and later entries; messages before it are
   still delivered, in order; a hold leaves even a message-only queue and the ledger untouched,
   and releasing it restores delivery; `queued()` returns raws; `retract` returns typed entries.
2. `rust/fontaine-tui/src/commands.rs`: `Command::runs_mid_turn`. Unit test: each immediate-set
   command returns true; queued ones (`/compact`, `/model`, `/copy`) and `Command::Other` return
   false.
3. `rust/fontaine-tui/src/app.rs`: `Action::Queue(Queued)`, `Action::Command`; the `enter`
   changes above. Rewrite `a_message_during_a_turn_is_queued_and_a_command_is_refused` as
   `..._and_an_unknown_command_is_refused`: a skill invocation (add a skill row to `app.menu`
   in the test) queues as `Queued::Skill` with name, argument and raw intact and the input
   cleared; a built-in (`/rename later`) queues as `Queued::Command` with its raw; a `!echo hi`
   queues as `Queued::Shell` with its raw (and a bare `!` is ignored); an immediate-set command
   (`/todos`) yields `Action::Command` with its raw and clears the input; an unknown `/nope` is
   still refused with the notice and keeps its text. Delete the now-redundant
   `enter_on_a_command_while_a_turn_runs_is_refused_as_before`, whose `/clear` refusal expectation
   contradicts the new behavior.
4. `rust/fontaine-tui/src/ui/turn.rs`: typed `end_of_turn`/`Submission` (with `Expanded`), the
   two queue arms, `converse`'s seeded-`pending` execution loop with command dispatch
   (`launch_dir` threaded in, turn-starting arms returning `Message` or `Expanded` to front-push),
   the replay delivery-hold lifecycle, modal suspension, the session-switch guard and queued
   shell execution, the extracted skill-expansion helper (including its running-state bracket),
   and the remembering `Action::Command` arm in the turn loop and `serve_while`. Update
   `rust/fontaine-tui/src/ui/tests.rs`: message grouping, skill, command and shell entries as
   separate submissions, interleaving order, restore joins raws (skill, command and `!` raws
   included), and a dispatcher-returned `Message` or `Expanded` front-pushes ahead of what was
   already pending.
5. `rust/fontaine-tui/src/ui/mod.rs`: extract `submit`'s dispatch into the shared helper;
   `submit`'s `Command::Other` arm calls the shared skill runner and seeds `converse` with the
   resulting `Expanded`; `edit_queued` maps entries to `raw()` before joining. Adapt the command
   handlers in `ui/schedule.rs`, `ui/providers.rs` and
   `ui/commands/{memory,model,rewind,session}.rs` to the handoff/modal/switch outcomes without
   changing their idle behavior. `notification_turn`, `scheduled_turn`/`goal_turn` in `turn.rs`
   and `goal_step` in `ui/goal.rs` seed `converse`'s `pending` from their `EndOfTurn::Submit`
   (`launch_dir` threaded through each).
6. `rust/fontaine-tui/src/ui/commands/context.rs` and `ui/goal.rs`: restore retracted raws after
   a compaction or an interrupted goal evaluation.
7. Integration tests (below).
8. Docs and changelog (below).

## Files to Modify

- `rust/fontaine-core/src/steering.rs` — `Queued`, typed queue, hook prefix delivery and the
  replay delivery hold.
- `rust/fontaine-tui/src/commands.rs` — `Command::runs_mid_turn`.
- `rust/fontaine-tui/src/app.rs` — `Action::Queue(Queued)`, `Action::Command`, `enter` running
  branch, tests.
- `rust/fontaine-tui/src/ui/turn.rs` — typed `end_of_turn`, `converse` execution loop, skill
  helper, mid-turn command dispatch.
- `rust/fontaine-tui/src/ui/mod.rs` — `submit`'s dispatch extracted and shared with the
  execution loop; `edit_queued` restores raws.
- `rust/fontaine-tui/src/ui/goal.rs` — typed retract in `goal_step`.
- `rust/fontaine-tui/src/ui/schedule.rs` — `/loop` turn handoff and first-iteration completion.
- `rust/fontaine-tui/src/ui/commands/context.rs` — restore raws after `/compact`.
- `rust/fontaine-tui/src/ui/providers.rs` — provider-dialog outcomes and action handling.
- `rust/fontaine-tui/src/ui/commands/{memory,model,rewind,session}.rs` — command handoffs, modal
  outcomes and switch guards.
- `rust/fontaine-tui/src/ui/tests.rs` — `end_of_turn` grouping tests.
- `rust/fontaine-tui/tests/extensions.rs` — end-to-end queued-input tests.
- `rust/fontaine-tui/tests/tui.rs` — notification-turn replay coverage.
- `docs/tui.md`, `docs/checkpoints.md` — behavior documentation.
- `CHANGELOG.md` — Unreleased entry for queued skills, built-ins and `!` commands.

## Decisions

- Semantics follow the issue and Claude Code's documented behavior: a command sent during a
  response is queued and runs after the turn finishes. Fontaine extends this to its built-in
  commands, which queue as typed and execute through the same dispatch as at an idle prompt; the
  synchronous UI/report commands run immediately mid-turn, Fontaine's analogue of Claude Code's
  run-immediately carve-outs (membership differs from Claude Code's list — chosen by the rule,
  and `/add-dir` queues rather than getting Claude Code's mid-turn confirm). `!` shell commands
  queue and execute after the turn in typed order, one at a time, as Claude Code documents; the
  standing decision that nothing queues *while* a `!` run holds the terminal is untouched.
  Unknown slash names stay refused mid-turn.
- A queued skill invocation becomes a checkpoint prompt when it executes after the turn, as
  `docs/checkpoints.md` already frames queued messages ("a message queued during a turn once it
  is sent as the next turn").
- 2026-10-29: raws queue, expansions never do. What sits in steering and in `pending` across
  any state change is the as-typed entry; preparation (skill expansion, command dispatch with
  its side effects, prompt computation) happens at pop time against live state, so a queued
  `/model` ahead of a queued `/plan` takes effect before `/plan`'s prompt is computed. The
  dispatcher-returned `Message` or `Expanded` is a hand-off to the loop, not a queued artifact:
  it front-pushes and runs next, so nothing executes between a command's preparation and its
  turn. Deferred expander objects (a closure/trait queued at type time) were rejected:
  the raw string plus the shared dispatcher already is the deferred expander, and only raws
  survive display, Up-takes-back editing and Esc-restore. `converse`'s `pending` is a seeded
  parameter so idle submit and the scheduled/goal/notification callers all enter through the
  one uniform pop loop.
- 2026-10-29: strict typed order spans replay as well as a single steering queue: starting a
  submission with later submissions pending holds steering delivery until the final pending
  submission begins, so newer input cannot overtake the replay backlog.
- 2026-10-29: a queued command that opens a picker or dialog suspends replay until that UI closes
  or is cancelled, applying the choice before the next entry. `/resume <argument>` and `/branch`
  (including a switch chosen in the bare `/resume` picker) are refused while later submissions
  remain, rather than silently dropping typed input on a session switch; replay then continues
  in the current session.
- 2026-10-29: Esc while a queued `/compact` or `!` shell run (or a queued skill expansion)
  executes interrupts only that command/run/expansion, exactly as at an idle prompt; execution
  continues with the next submission and nothing is restored to the input. Esc
  stops the loop only when it interrupts a turn (`RestoreToInput`). Verified against Claude
  Code 2.1.312: its Esc maps to a turn-scoped abort of the current dispatch only, an interrupt
  never removes user-queued entries from its queue, and queued entries reach the input only
  through the Up-arrow take-back.

## Documentation

- `docs/tui.md`, "Messages during a turn": replace the "`/` commands are not queued" bullet —
  a slash command queues like a message and runs when the turn ends, in the order typed: a skill
  as its expanded prompt, a built-in exactly as if submitted at that moment. Neither is steered
  into the running turn, and messages queued behind one wait with it. `!` shell commands queue
  the same way and execute after the turn, one at a time and in order; while one executes,
  nothing else
  queues. The commands that act on the UI alone (`/status`, `/theme`, the panels, ...) run
  immediately, turn or no turn. A queued command that opens a picker or dialog holds later
  entries until it closes; `/resume <argument>` and `/branch` are refused while later entries
  remain, so a session switch cannot silently discard them.
- `docs/tui.md`, "Shell commands": replace "It runs only between turns. While a turn runs,
  Enter on a `!` input says so and keeps the text." — a `!` input typed mid-turn queues and
  runs after the turn, in the order typed; the "while a command runs nothing else can be sent
  or queued" sentence stays.
- `docs/checkpoints.md`, "What is tracked": a queued skill or prompt-sending command (`/plan
  <task>`, `/goal <condition>`, `/init`, `/loop`) is a prompt when it executes after the turn (one
  clause; the current text already covers queued messages and idle skills/commands).
- `CHANGELOG.md`: an Unreleased entry covering queued skills, built-in commands and `!` shell
  commands, including the immediate UI-command carve-out.

## Testing Strategy

Unit tests (all constructed inputs; no live APIs):

- `steering.rs`: prefix delivery stops at a skill entry and preserves order; a delivery hold
  leaves a message-only queue and its ledger untouched until released; skill entries reach
  `retract` typed; `queued()` returns raws; delivered messages still enter the ledger in order
  (existing test, updated).
- `app.rs`: skill invocation queues with kind/name/argument/raw during a turn; a built-in
  queues as `Queued::Command` with its raw; a `!` command queues as `Queued::Shell` with its
  raw; an immediate-set command carries its raw, clears the input and runs instead of queueing;
  unknown slash commands are still refused; nothing queues during a `!` run (existing test).
- `commands.rs`: `runs_mid_turn` classifies the immediate set and excludes queued commands,
  including `/copy`, and `Command::Other`.
- `ui/tests.rs`: `end_of_turn` groups consecutive messages, splits skills, commands and shell
  lines into their own submissions, keeps interleaved order, restores raws (including `/name
  args` and `!echo hi`) after an interrupt, and front-pushes either kind of dispatcher handoff.

Integration tests use the scripted-model `World` harness, as
`a_skill_runs_as_a_command_with_its_arguments` and the `tui.rs` queue tests do (a
`tool_turn("c1", "shell", sleep)` keeps the turn open while input is sent). Unless noted, these
cases go in `rust/fontaine-tui/tests/extensions.rs`:

- A skill typed during a turn shows `queued: /review 42`, and once the turn answers, the skill
  runs as the next turn. With the script `[tool_turn(...), text_turn(current answer),
  text_turn(skill answer)]`, the *third* model request (`requests()[2]`) carries the expanded body
  ("Review pull request 42 and report."), never the literal `/review 42` as message text; the
  ledger records `"text":"/review 42"` once. This is the wiring unit tests cannot reach: queue →
  retract → expansion → turn across the real runtime.
- A message queued after a skill is not steered into the running turn (barrier): the running
  turn's tool result carries neither, and both execute in order afterwards.
- With skill `S` and message `M` retracted into pending replay, a newer message `N` typed during
  `S` is held out of `S`'s tool results while `M` remains pending; `S`, `M` and `N` then run in
  typed order. Ordinary steering resumes when the final pending submission begins.
- Interrupting the turn with a skill queued puts `/review 42` back in the input; no expansion
  request is made (nothing is sent unseen).
- A built-in queued mid-turn runs after the turn: `/rename later` typed during a turn shows
  `queued: /rename later`, and the rename notice and effect land after the answer, in order
  behind a queued message. An immediate-set command (`/status`) shows its report while the turn
  still runs, clears its input, and leaves the turn's tool result untouched.
- A queued `/plan do x` exercises the command → dispatcher → `Message` handoff: the third model
  request sends `do x` with plan mode on, and the ledger records `"text":"do x"` once, preserving
  the idle command's text rather than inventing a second raw-command record.
- Notification replay: extend `a_pick_made_during_a_notification_turn_applies_when_the_turn_ends`
  in `tests/tui.rs` with a message queued during its delayed notification turn and one extra
  routed response; the message becomes the next request after the notification answer.
- Modal replay and switch safety: a queued `/rewind` followed by a message opens the picker after
  the current answer but makes no further model request until Esc closes it, then runs the
  message; a queued `/branch` followed by a message refuses the switch and runs the message in
  the current session instead of dropping it.
- A `!echo hi` typed mid-turn shows `queued: !echo hi`, is never steered into the running turn,
  and runs as a shell run after the answer, in typed order with the other queued entries; its
  output reaches the next message, as at idle.
- Esc while a queued `!sleep` executes kills it as at idle, and the next queued entry still
  executes afterwards (the shell's raw is spent, not restored). Esc during a queued skill
  expansion aborts only that expansion and continues with the next submission. Esc while a
  queued `/compact` executes aborts the compaction and execution continues with the next
  submission.

## Open Questions

None.
