# Queued Skill Invocations Plan

Issue: madesroches/fontaine#292.

## Overview

A skill invocation (`/name args`) typed while a turn is running is currently refused outright
("a turn is running; wait for it or press Esc to interrupt"). It should queue exactly like a
plain message and run — expanded as the skill it is — once the current turn ends. Built-in
commands queue too, as Claude Code documents, and replay through the same dispatch they would
get at an idle prompt; a small set of purely-UI commands (panels, reports, theme) runs
immediately mid-turn, Fontaine's analogue of Claude Code's run-immediately carve-outs. Only
unknown slash names and `!` shell commands stay refused mid-turn.

## Current State

- `App::enter` (`rust/fontaine-tui/src/app.rs:1004-1018`): while `self.running`, any input that
  `parse_command` recognizes — built-in or `Command::Other` (a would-be skill) — is refused with
  a notice and the text stays in the input. Plain text becomes `Action::Queue(String)`.
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
pub enum Queued {
    Message(String),
    Skill { raw: String, name: String, argument: String },
    /// A built-in command exactly as typed (`/compact fix tests`); re-parsed and dispatched
    /// when it replays.
    Command(String),
}

impl Queued {
    /// The text as typed: the message, or the slash command line. Used for the queue display,
    /// the input restore and the history.
    pub fn raw(&self) -> &str;
}
```

- `Steering::queue(entry: Queued)`; the queue holds `Vec<Queued>`.
- `Steering::queued()` keeps its `Vec<String>` signature (raws), so the display
  (`app.queued`, `view.rs queued_lines`, "queued: /review 42") is unchanged.
- `Steering::retract()` returns `Vec<Queued>`.
- `SteeringHook::steer` delivers only the **leading run of `Message` entries** and stops at the
  first `Skill` or `Command` entry, which stays queued with everything behind it. Neither can be
  delivered mid-turn: a skill's expansion is async, can run `!` commands that need approval, and
  must become a prompt of its own, and a command's text steered into a tool result would read as
  model input it never was — none of which a sync tool-result hook can do. The barrier also
  keeps the order the user typed in: a message queued after a skill or command is sent after its
  turn, not steered into the turn ahead of it.

### TUI: queue slash input (`rust/fontaine-tui/src/app.rs`, `commands.rs`)

- `Action::Queue(String)` becomes `Action::Queue(Queued)` (imported from
  `fontaine_core::steering`); a new `Action::Command(Command)` carries a command that runs
  immediately.
- `Command::runs_mid_turn()` (`commands.rs`, beside the `BUILTINS` table) marks the immediate
  set: the commands whose handler is synchronous and touches only UI state — `help`, `status`,
  `theme`, `todos`, `agents`, `denied`, `permissions`, `hooks`, `skills`, `plugins`, `mcp`,
  `copy`, `expand`, `config`. Everything else queues: it mutates the session or conversation
  (`clear`, `compact`, `resume`, `rewind`, `model`, the guardian toggles, ...), starts a turn
  (`plan`, `goal`, `loop`, `init`), awaits the runtime (`context`), or can block on an external
  program (`memory <n>` opens an editor, `export` writes a file). The rule, not the list, is the
  contract: a command joins the set only if running it cannot touch the turn, the conversation,
  the ledger, or a blocking wait. (`/config` qualifies today: its changes already defer through
  `pending_config` when a turn is running.)
- In `enter`'s `self.running` branch:

  ```rust
  match parse_command(&text) {
      Some(command) if command.runs_mid_turn() => Action::Command(command),
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
  `Action::CycleMode` today: a small dispatch calls the same handler methods `submit`'s arms
  call. The catch-all ignore lists in the main loop and the provider dialog gain the variant.

### TUI: typed end-of-turn replay (`rust/fontaine-tui/src/ui/turn.rs`)

- The two `Action::Queue(entry)` arms (`turn.rs:338`, `turn.rs:184`) pass the entry through:
  `self.remember(entry.raw()); runtime.steering.queue(entry);`
- `end_of_turn` takes `Vec<Queued>` and groups it:

  ```rust
  pub(crate) enum Submission {
      /// Consecutive queued messages joined with a blank line, as today.
      Message(String),
      Skill { raw: String, name: String, argument: String },
      /// A built-in command as typed; re-parsed and run through `submit`'s dispatch.
      Command(String),
  }

  pub(crate) enum EndOfTurn {
      Submit(Vec<Submission>),
      RestoreToInput(String), // raws joined, as today
      Nothing,
  }
  ```

  Grouping walks the retracted entries in order: consecutive `Message` entries join into one
  `Submission::Message` with `\n\n` (today's behavior); each `Skill` or `Command` entry is a
  submission of its own.

- `converse` gains a `pending: VecDeque<Submission>`: the initial `(text, expanded)` seeds it;
  each turn's `EndOfTurn::Submit(subs)` extends it; the loop pops one submission at a time. A
  `Submission::Skill` is expanded (see below) and run as `converse(raw, Some(prompt))` is today.
  On `RestoreToInput`, the raws of the submissions still pending are joined onto the restored
  text, so an interrupted replay loses nothing. Esc is live while a replayed skill expands
  (expansion serves keys), and that interrupt is treated like an interrupted turn: the
  interrupted skill's raw and the pending raws go back to the input and the replay stops, as
  undelivered queued text does today — expansion runs before any turn starts, so no
  `RestoreToInput` covers it and the pending entries would otherwise vanish with `converse`'s
  local queue. A failed expansion shows the idle path's error notice and does not stop the
  replay: the submissions behind it run on, each queued entry independent of the one that
  failed. Text typed while a replayed expansion runs is dropped, exactly as at an idle
  prompt today. The same interrupt rule covers a replayed command that serves keys (a replayed
  `/compact`): Esc stops the replay, and the command's raw and the pending raws go back to the
  input. Note the pitfall: `converse` → skill-run → `converse` as separate async fns is async
  recursion; keep the dispatch inside `converse`'s own loop, with expansion factored into a
  helper that does not call `converse`.

- Shared skill runner: extract the body of `submit`'s `Command::Other` arm (`ui/mod.rs:392-423`)
  — known check, `Record::UserMessage` of the raw text before expansion, `expand_serving`, error
  and interrupt notices — into a helper used by both idle `submit` and queue replay, so a
  replayed skill is ledgered, expanded and error-reported exactly as if typed at an idle prompt.
  (`expand_serving` already serves approvals while the skill's `!` commands run.)
- Shared command dispatcher: extract `submit`'s `match parse_command` into a helper used by both
  `submit` and the replay loop, so a replayed `/rename foo` or `/compact` does exactly what it
  would at an idle prompt. Two constraints fall out of running it inside `converse`'s loop:
  `launch_dir` (which `submit` takes and `converse` does not) is threaded in, and an arm that
  starts a turn — `/plan <task>`, `/goal <condition>`, `/loop`, `/init`, the skill arm — pushes
  its prompt onto `pending` instead of calling `converse` (the same async-recursion rule as
  skill expansion). `Break` (`/exit`, `/resume`, `/branch`, ...) propagates out of the loop.
- `scheduled_turn`, `goal_turn` and `goal_step` route their `EndOfTurn::Submit` through the same
  pending-aware path, so a skill queued during a scheduled or goal turn runs after it, ahead of
  the next scheduled or goal turn (in `goal_step`, the queued input already preempts the goal
  turn — that order is kept).
- `/compact` (`ui/commands/context.rs:77`) and the goal-evaluation interrupt path
  (`ui/goal.rs:93`) retract and restore to the input: join `raw()`s. A skill queued during a
  compaction comes back as `/name args` in the input, like messages do — a compaction has no turn
  to receive a prompt.

### What does not change

- `!` shell commands mid-turn and unknown slash names: refused as today, and nothing queues
  while a `!` run holds the terminal.
- Mid-turn steering of plain messages, ledger timing (a queued entry becomes guardian intent
  when the model receives it — for a skill, when its turn starts), the queue display, and
  Up-takes-back editing (`edit_queued` restores raws, which re-parse on submit).
- Headless `fontaine -p`: no mid-turn input exists there.

## Implementation Steps

1. `rust/fontaine-core/src/steering.rs`: add `Queued`, retype `queue`/`retract`, make
   `SteeringHook::steer` deliver only the leading message run. Update the existing unit tests
   (they queue strings) and add: a skill or command entry blocks mid-turn delivery of itself and
   later entries; messages before it are still delivered, in order; `queued()` returns raws;
   `retract` returns typed entries.
2. `rust/fontaine-tui/src/commands.rs`: `Command::runs_mid_turn`. Unit test: each immediate-set
   command returns true, a queued one (`/compact`, `/model`) and `Command::Other` return false.
3. `rust/fontaine-tui/src/app.rs`: `Action::Queue(Queued)`, `Action::Command`; the `enter`
   change above. Rewrite `a_message_during_a_turn_is_queued_and_a_command_is_refused` as
   `..._and_an_unknown_command_is_refused`: a skill invocation (add a skill row to `app.menu`
   in the test) queues as `Queued::Skill` with name, argument and raw intact and the input
   cleared; a built-in (`/rename later`) queues as `Queued::Command` with its raw; an
   immediate-set command (`/todos`) yields `Action::Command`; an unknown `/nope` is still
   refused with the notice and keeps its text.
4. `rust/fontaine-tui/src/ui/turn.rs`: typed `end_of_turn`/`Submission`, the two queue arms,
   `converse`'s pending loop with the command dispatch (`launch_dir` threaded in), the
   extracted skill-expansion helper, and the `Action::Command` arm in the turn loop and
   `serve_while`. Update `rust/fontaine-tui/src/ui/tests.rs`: message grouping, skill and
   command entries as separate submissions, interleaving order, restore joins raws (skill and
   command raws included).
5. `rust/fontaine-tui/src/ui/mod.rs`: extract `submit`'s dispatch into the shared helper;
   `submit`'s `Command::Other` arm calls the shared skill runner; `edit_queued` maps entries to
   `raw()` before joining. `scheduled_turn`/`goal_turn` in `turn.rs` and `goal_step` in
   `ui/goal.rs` replay submissions.
6. `rust/fontaine-tui/src/ui/commands/context.rs` and `ui/goal.rs`: restore retracted raws after
   a compaction or an interrupted goal evaluation.
7. Integration tests (below).
8. Docs (below).

## Files to Modify

- `rust/fontaine-core/src/steering.rs` — `Queued`, typed queue, hook prefix delivery.
- `rust/fontaine-tui/src/commands.rs` — `Command::runs_mid_turn`.
- `rust/fontaine-tui/src/app.rs` — `Action::Queue(Queued)`, `Action::Command`, `enter` running
  branch, tests.
- `rust/fontaine-tui/src/ui/turn.rs` — typed `end_of_turn`, `converse` replay loop, skill
  helper, mid-turn command dispatch.
- `rust/fontaine-tui/src/ui/mod.rs` — `submit`'s dispatch extracted and shared with replay;
  `edit_queued` restores raws.
- `rust/fontaine-tui/src/ui/goal.rs` — typed retract in `goal_step`.
- `rust/fontaine-tui/src/ui/commands/context.rs` — restore raws after `/compact`.
- `rust/fontaine-tui/src/ui/tests.rs` — `end_of_turn` grouping tests.
- `rust/fontaine-tui/tests/extensions.rs` — end-to-end queued-skill tests.
- `docs/tui.md`, `docs/checkpoints.md` — behavior documentation.

## Trade-offs

- **Skill waits for turn end vs. mid-turn delivery.** Claude Code queues commands and runs them
  after the turn (per its docs), and expansion cannot run inside the sync steering hook anyway.
  Delivering the raw `/name args` as steering text instead would send the model the invocation
  without the skill's body — the failure the issue calls out.
- **Barrier vs. reorder.** A queued skill blocks mid-turn delivery of messages queued after it.
  The alternative (deliver later messages, hold the skill) sends the model a later message before
  an earlier instruction; the barrier preserves typed order at the cost of those messages waiting
  for turn end.
- **Known-skill check at queue time (against `app.menu`)** vs. queueing every `Command::Other`
  and erroring at replay. Checking at queue time keeps the immediate refusal for typos, and
  keeps the replay path free of a dead-letter case; a name a built-in answers to is the
  built-in, queued or immediate, exactly as at an idle prompt.
- **Shared skill runner and command dispatcher** vs. duplicating the expand/ledger/error and
  dispatch logic in the replay path: extraction keeps one definition of "run a user-invoked
  skill" and one of "run a built-in command" (open/closed: the replay path adds callers, not
  copies).
- **Immediate set vs. queueing every built-in.** Claude Code runs a carve-out list mid-turn
  (`/status`, `/permissions`, dialogs) and queues the rest. Fontaine's rule — synchronous, and
  touches only UI state — picks the set from the code rather than copying the list, so a new
  command is classified by what it does, not by remembering to update a table.

## Decisions

- Semantics follow the issue and Claude Code's documented behavior: a command sent during a
  response is queued and runs after the turn finishes. Fontaine extends this to its built-in
  commands, which queue as typed and replay through the same dispatch as at an idle prompt; the
  synchronous UI/report commands run immediately mid-turn, Fontaine's analogue of Claude Code's
  run-immediately carve-outs (membership differs from Claude Code's list — chosen by the rule,
  and `/add-dir` queues rather than getting Claude Code's mid-turn confirm). Unknown slash
  names and `!` shell commands stay refused mid-turn.
- A queued skill invocation becomes a checkpoint prompt when it runs after the turn, as
  `docs/checkpoints.md` already frames queued messages ("a message queued during a turn once it
  is sent as the next turn").

## Documentation

- `docs/tui.md`, "Messages during a turn": replace the "`/` commands are not queued" bullet —
  a slash command queues like a message and runs when the turn ends, in the order typed: a skill
  as its expanded prompt, a built-in exactly as if submitted at that moment. Neither is steered
  into the running turn, and messages queued behind one wait with it. The commands that act on
  the UI alone (`/status`, `/theme`, the panels, ...) run immediately, turn or no turn.
- `docs/checkpoints.md`, "What is tracked": a queued skill or prompt-sending command (`/plan
  <task>`, `/goal <condition>`, `/init`, `/loop`) is a prompt when it runs after the turn (one
  clause; the current text already covers queued messages and idle skills/commands).

## Testing Strategy

Unit tests (all constructed inputs; no live APIs):

- `steering.rs`: prefix delivery stops at a skill entry and preserves order; skill entries reach
  `retract` typed; `queued()` returns raws; delivered messages still enter the ledger in order
  (existing test, updated).
- `app.rs`: skill invocation queues with kind/name/argument/raw during a turn; a built-in
  queues as `Queued::Command` with its raw; an immediate-set command runs instead of queueing;
  unknown slash commands are still refused; nothing queues during a `!` run (existing test).
- `commands.rs`: `runs_mid_turn` classifies the immediate set and excludes queued commands and
  `Command::Other`.
- `ui/tests.rs`: `end_of_turn` groups consecutive messages, splits skills and commands into
  their own submissions, keeps interleaved order, and restores raws (including `/name args` and
  `/rename later`) after an interrupt.

Integration tests in `rust/fontaine-tui/tests/extensions.rs` (scripted-model `World` harness,
as `a_skill_runs_as_a_command_with_its_arguments` and the `tui.rs` queue tests use — a
`tool_turn("c1", "shell", sleep)` keeps the turn open while input is sent):

- A skill typed during a turn shows `queued: /review 42`, and once the turn answers, the skill
  runs as the next turn: the *second* model request carries the expanded body
  ("Review pull request 42 and report."), never the literal `/review 42` as message text; the
  ledger records `"/text":"/review 42"` once. This is the wiring unit tests cannot reach: queue →
  retract → expansion → turn across the real runtime.
- A message queued after a skill is not steered into the running turn (barrier): the running
  turn's tool result carries neither, and both replay in order afterwards.
- Interrupting the turn with a skill queued puts `/review 42` back in the input; no expansion
  request is made (nothing is sent unseen).
- A built-in queued mid-turn runs after the turn: `/rename later` typed during a turn shows
  `queued: /rename later`, and the rename notice and effect land after the answer, in order
  behind a queued message. An immediate-set command (`/todos`) opens its panel while the turn
  still runs, and the turn's tool result is untouched.

## Open Questions

None.
