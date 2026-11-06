# Interrupted Turn History Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/457

## Overview

Pressing Esc (or Ctrl-C) during a turn drops the whole turn from the model's conversation: the
prompt, every finished tool round, all of it. The transcript still shows it, but the next turn
starts from the conversation as it stood before. Claude Code keeps an interrupted turn and marks
it with `[Request interrupted by user]`, so the next prompt continues the same task. This plan
journals the root run's committed messages as it goes (the mechanism subagents already use), and
folds that journal into the conversation log with an interruption marker whenever a root run
ends without settling: a TUI interrupt, an AFK ceiling stop, or a process that died mid-turn and
is resumed later.

## Current State

- **The drop.** `fontaine-tui/src/ui/turn.rs` `turn()` (lines ~762-925) builds the run future
  (`compact_if_needed` then `run_turn`) and selects over it. `Action::Interrupt | Action::Quit`
  break with `TurnEnd::Interrupted` (line 856) and the AFK signal with `TurnEnd::Stopped`
  (line 841). Then `drop(run)` (line 900) drops rig's drive stream mid-flight. A `Finished`
  outcome that coincides with an AFK stop is relabelled `Stopped` (lines 908-916); that run did
  settle.
- **Where rig saves.** `rig-agent-0.45.0/src/agent/engine.rs` `drive_agent` saves the
  conversation only in the `AgentRunStep::Done` branch (`conversation.extend(run.messages());
  store.save(...)`, ~line 423). Failed exits go through `fail!`, which fires `on_run_settled`
  with `SettledOutcome::Error` and the run's messages. A dropped stream does neither.
- **Recovery hooks.** `fontaine-core/src/turn_recovery.rs`:
  - `RecoveryHook` appends a run's committed messages to its log on `SettledOutcome::Error`.
    It is registered on the root (`agent/build.rs` `finish_builder`, line ~307) and on
    subagents.
  - `InflightHook` journals committed messages to `<id>.inflight` at every
    `on_completion_call`: the messages past the input history, plus the call's prompt. After
    call 1 the journal holds the user prompt. After call N it also holds each finished
    assistant turn and its tool-result message. An assistant turn whose tools are still
    running is not journaled until the next call, so **the journal never holds an unpaired
    tool call**. On settle it cuts the journal back to what it held at run start: on success,
    or on an error the `RecoveryHook` append landed. It is registered only on subagents
    (`child_runner.rs` ~line 616). Subagents can't be interrupted in a way that strands the
    journal, because `KillHook` settles them through rig's cancel path.
  - `append_messages(store, log, messages)` is the shared load-extend-save helper.
- **Headless.** `fontaine/src/headless.rs` (~line 593) selects the AFK stop signal over
  `run_turn` and drops the turn the same way. Headless installs no SIGINT handler, so Ctrl-C
  kills the process, and any `--resume` starts from the pre-turn conversation.
- **Session start.** `AgentRuntime::record_session_start` (`fontaine-core/src/agent.rs`
  ~line 711) knows whether the session is `SessionOrigin::Continued` (resume, continue,
  fork/branch).
- **Docs** state the current behavior: `docs/tui.md` line 228, "The interrupted turn is
  discarded, earlier turns are kept".
- **Tests.** `fontaine-tui/tests/tui.rs` covers interrupts for prompts, queued messages and
  bang commands (`interrupting_a_turn_puts_the_queued_message_back_in_the_input`, line 930),
  but never checks the next request's history. `fontaine-core/tests/turn_recovery.rs` covers
  the failed-run path.

## Design

### 1. Journal the root run

Register `InflightHook::new(p.session.store(), CONVERSATION_LOG.to_owned(), p.approver.clone())`
in `finish_builder`, next to `RecoveryHook`. The journal is `main.inflight` (via the existing
`journal_log`). Nothing in the hook is subagent-specific. Its error-settle check loads the
agent's own log (`main`) and cuts back only once the recovery append landed. That works the same
for the root, so a settled root run (answer or failure) leaves the journal as it found it. The
cost is one extra append per model call on the root's log store, the same as a subagent pays.

Update `journal_log`'s doc comment to cover any agent, not only subagents.

### 2. Fold an unsettled run's journal into the conversation

A new core function in `turn_recovery.rs`:

```rust
/// Why a run ended without settling; picks the marker the model reads.
pub enum Unsettled { User, AfkStop, SessionEnded }

/// Appends what an unsettled run committed to `log`, then a marker, then empties the journal.
/// `fallback` is the run's prompt, used when the journal holds nothing (the run was dropped
/// before its first model call, or every journal write failed).
pub(crate) async fn fold_unsettled<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    cause: Unsettled,
    fallback: Option<Message>,
) -> Result<bool, SessionError>   // whether anything was folded
```

Steps:

1. Load `journal_log(log)`. A missing journal reads as empty, as `load_async` already does.
2. Messages = the journal's messages. If empty, use `fallback`. If both are empty, do nothing
   and return `false` (the run left no trace, for example a resume with no leftover journal).
3. `append_messages(store, log, messages + [marker])`. The marker is a
   `Message::user(text)`:
   - `User`: `[Request interrupted by user]`, the exact text Claude Code uses.
   - `AfkStop`: `[Harness note, not the user's words: the turn was stopped because the AFK
     temperature ceiling was reached.]`
   - `SessionEnded`: `[Harness note, not the user's words: the session ended before this turn
     finished.]`
   The harness-note wording follows `RETRY_NOTE` in `turn.rs`.
4. Only after the append succeeds, empty the journal: `replace_log_async` to zero bytes,
   the same cut `InflightHook::cut_back(0)` makes. Factor `cut_back`'s body into a free
   function `cut_journal(store, log, keep)` that both use. If the append fails, the journal
   stays and the next fold (next interrupt, or the next resume) folds it, so nothing is lost
   and nothing doubles.

**Pairing.** The journal never holds an unpaired tool call, as noted under Current State. The
last journaled message is always the user side of a round, either the prompt or a tool-result
message, so the folded history is valid for providers that require paired
tool_use/tool_result. No synthetic results are needed. A consecutive user message (the marker
after a tool-result message, then the next prompt) is already a shape the conversation carries
today after a retry (`RETRY_NOTE`). A test pins that the next request is accepted by the mock
providers (see Testing Strategy).

**Interaction with the hook at the next run.** `InflightHook::start` records `carried_bytes`
from the journal at run start. After a fold the journal is empty, so the next run starts at 0
as usual. If the fold could not empty the journal, the next run carries it and cuts back to it
at settle. The journal then still holds the unfolded turn for the next fold, which is the
intended behavior.

### 3. Call the fold where a root run is dropped

A runtime method wraps the fold, warns through the approver on failure (`chain(&e)`, as
`RecoveryHook` does), and is the single call site for surfaces:

```rust
impl AgentRuntime {
    pub async fn keep_unsettled_turn(&self, cause: Unsettled, prompt: Option<Message>);
}
```

- **TUI** (`turn.rs` `turn()`). Before the run future is built, keep the prompt message the
  run will send: `user_message(&prompt, &attachments, &modalities)`. Expose `user_message` as
  `pub` from `fontaine_core::turn`, or add a small `pub fn prompt_message(...)` wrapper. After
  `drop(run)`, and before `finish_turn`, call `keep_unsettled_turn` when the loop broke with
  `Interrupted` (`Unsettled::User`) or with the signal branch's `Stopped`
  (`Unsettled::AfkStop`). Do **not** call it for a `Finished` relabelled `Stopped` (the run
  settled and was saved), or for `InputClosed` (the session is ending, and the journal is
  folded at the next resume). To tell the two `Stopped` cases apart, record whether the loop
  broke out of the signal branch before the relabel.
- **Headless** (`headless.rs` ~line 593). When the AFK stop branch wins (`None`) and the
  turn had not finished, call `keep_unsettled_turn(Unsettled::AfkStop, Some(prompt))` before
  `handle_afk_stop`. The prompt text is cloned before `run_turn` takes it (headless sends no
  attachments).
- **Session start** (`record_session_start`). For a `Continued` origin, call
  `keep_unsettled_turn(Unsettled::SessionEnded, None)`. A journal left by a killed process
  (headless Ctrl-C, a crash, a closed terminal) is folded before the first turn. A fresh
  session has no journal, and the call is a no-op there anyway. A fork copies the logs
  directory, so a leftover journal is folded into the copy too, which is correct: the copy
  continues the same conversation.

The interrupt path in the TUI awaits one load and one save of the conversation log. That is the
same cost as a settled run's save, and it runs before the next prompt can be submitted, so the
next turn always sees the folded history.

### Behavior

| Run ends by | Today | After |
|---|---|---|
| Answer / failure | Saved by rig / `RecoveryHook` | Unchanged; journal empties at settle |
| Esc / Ctrl-C in the TUI | Turn lost | Prompt + finished tool rounds + `[Request interrupted by user]` |
| Esc before the first model call (during auto-compaction) | Turn lost | Prompt + marker |
| AFK ceiling (TUI or headless) | Turn lost | Same as interrupt, AFK-stop note as marker |
| Process killed mid-turn, then `--resume` | Turn lost | Folded at session start, session-ended note as marker |

What is still lost: assistant text streamed since the last model call committed, and the
results of a tool batch still running at the interrupt. The assistant turn that requested the
batch is not journaled until the next call, so the batch's finished results go with it. The
issue accepts this for a first cut.

## Implementation Steps

1. **`fontaine-core/src/turn_recovery.rs`**: add `Unsettled`, its marker texts,
   `fold_unsettled`, and the shared `cut_journal` helper (refactor `InflightHook::cut_back`
   onto it). Generalize the `journal_log` and `InflightHook` doc comments. Unit tests here.
2. **`fontaine-core/src/agent/build.rs` `finish_builder`**: register `InflightHook` on the
   root after `RecoveryHook`.
3. **`fontaine-core/src/agent.rs`**: add `AgentRuntime::keep_unsettled_turn` and call it from
   `record_session_start` for `Continued` origins. Re-export `Unsettled` where the surfaces
   can name it (for example `fontaine_core::turn_recovery::Unsettled`, which makes the module
   `pub` or re-exports the enum).
4. **`fontaine-core/src/turn.rs`**: make the prompt message buildable by surfaces (`pub` on
   `user_message`, or a thin `pub fn`).
5. **`fontaine-tui/src/ui/turn.rs` `turn()`**: build and keep the prompt message, track whether
   the run was dropped unsettled, and call `keep_unsettled_turn` after `drop(run)`.
6. **`fontaine/src/headless.rs`**: fold on the AFK-stop branch.
7. **Tests** (see Testing Strategy).
8. **Docs**: `docs/tui.md` line 228; `docs/architecture.md` session layout (`main.inflight.jsonl`
   next to `main.jsonl`); `docs/sessions.md` or wherever `--resume` is documented (a resumed
   session keeps a turn the previous process did not finish); `CHANGELOG.md` `Fixed`.
9. **`tasks/fontaine_harness_plan.md`**: a dated line at the end of the `### Sessions`
   subsection of `## Decisions` recording the one deviation from Claude Code: partial streamed
   text and an in-flight tool batch are not kept.

## Files to Modify

- `rust/fontaine-core/src/turn_recovery.rs`
- `rust/fontaine-core/src/agent/build.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-core/src/lib.rs` (visibility of `turn_recovery` / `Unsettled`, if needed)
- `rust/fontaine-tui/src/ui/turn.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-core/tests/turn_recovery.rs`, `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `docs/architecture.md`, the resume docs page, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Journal + fold vs. a cooperative cancel.** The issue's alternative is to stop the root
  run through rig's `cancel_error` path, as `KillHook` does for subagents, so it settles as
  `Error` and `RecoveryHook` saves it. rig exposes no cancel handle from outside the stream.
  The run only checks hook boundaries (before a model call, before each dispatch), so Esc
  during a long stream or a long tool would wait for the next boundary, or need the
  choke-point kill-select plus a grace window that subagent kills use. That trades Esc's
  immediacy for the save. The journal keeps Esc instant and reuses a hook already proven on
  subagents. It also covers a killed process, which no cooperative cancel can.
- **Fold at the surface vs. inside `run_turn`.** A drop guard cannot run async I/O, and the
  dropped future is the one that would own it. So the surface that drops the run calls the
  fold right after. One runtime method keeps that to one line per surface.
- **Fallback prompt vs. journaling at run start.** Journaling in `on_run_start` would cover
  the window before the first model call, but the prompt there can differ from what the first
  call sends if a completion-call hook patches it, which risks a duplicated or stale prompt.
  The surface already holds the exact message it sent, so it passes it as the fallback.
- **No synthetic tool results.** They are needed only when a tool call is stranded, and the
  journal never strands one. Adding them would mean journaling the in-flight assistant turn
  too (from `on_model_turn_finished`), which keeps the in-flight batch's finished results but
  adds a second writer and its ordering edge cases. That is left as a follow-up.

## Decisions

## Documentation

- `docs/tui.md`: the Esc/Ctrl-C row. The interrupted turn is kept in the model's conversation
  up to its last finished tool round, followed by an interruption marker, and the next prompt
  continues from there. Text the model was still streaming is not kept.
- `docs/architecture.md`: session layout. `logs/main.inflight.jsonl` journals the main run in
  progress, as `agent-<n>.inflight.jsonl` does for subagents.
- Resume docs: resuming after a process that died mid-turn keeps that turn, marked as
  unfinished.
- `CHANGELOG.md`: `Fixed`. An interrupted turn stays in the model's conversation.

## Testing Strategy

The wild bug (#457: the model said "I don't have the earlier context") is pinned by the first TUI
integration test.

- **Unit, `turn_recovery.rs`** (MemoryStore):
  - Fold appends the journal's messages and the marker in order and empties the journal.
  - An empty journal with a fallback appends prompt + marker. An empty journal with no
    fallback is a no-op that returns `false`.
  - A non-empty journal ignores the fallback, so the prompt is not doubled.
  - Each `Unsettled` cause produces its marker text.
  - An append failure (the `Broken` store) returns the error and leaves the journal.
  - A journal carried by a failed fold is folded whole by the next fold.
  - `InflightHook` with id `main`: a settled run (both outcomes) leaves an empty journal, as
    the subagent tests already show for `agent-1`, so no second fold doubles a settled turn.
- **Core integration, `fontaine-core/tests/turn_recovery.rs`** (scripted models): run a root
  turn with a tool call whose second request is held, drop the `run_turn` future, call
  `keep_unsettled_turn(User, ..)`, and assert the stored conversation holds the prompt, the
  tool call, its result and the marker, and that `main.inflight` is empty. Then run a second
  turn and assert its request's history carries all of it. A second test: a session reopened
  with a leftover journal (written by dropping a run without folding) folds it at
  `record_session_start`, with the session-ended marker.
- **TUI integration, `fontaine-tui/tests/tui.rs`**. This is the issue's regression test. Start
  a turn that makes a tool call (`sleep` shell, as the queued-message test does), wait for the
  tool card, press Esc, wait for "interrupted", send a follow-up, and assert that the
  follow-up's request contains the first prompt, the tool call, the interruption marker and the
  follow-up. Plus one test that interrupts before any model call completes and asserts the
  first prompt and marker reach the next request.
- **Headless.** A test for the AFK-stop fold, using an existing AFK-stop test harness in
  `fontaine/tests` if one drives the stop. Otherwise the unit and core coverage stands, since
  the call site is one line.

## Open Questions

- Should the AFK-stop marker and the session-ended marker be Claude Code's plain
  `[Request interrupted by user]` instead of harness notes? Claude Code has no AFK mode, and a
  killed process has no Claude Code analog. The plan uses harness notes because neither is
  the user's doing.
