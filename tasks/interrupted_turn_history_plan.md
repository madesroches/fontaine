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
  (line 841). Then `drop(run)` (line 907) drops rig's drive stream mid-flight. A `Finished`
  outcome that coincides with an AFK stop is relabelled `Stopped` (lines 909-917); that run did
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

/// The run's prompt, and how many messages `log` held when the run started (after any compaction).
pub struct Fallback { pub prompt: Message, pub log_len_before: usize }

/// Appends what an unsettled run committed to `log`, then a marker, then empties the journal.
/// `fallback` is used when the journal holds nothing and the run left nothing in `log` (the run
/// was dropped before its first model call, or every journal write failed).
pub(crate) async fn fold_unsettled<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    cause: Unsettled,
    fallback: Option<Fallback>,
) -> Result<bool, SessionError>   // whether anything was folded
```

Steps:

1. Load `journal_log(log)`. A missing journal reads as empty, as `load_async` already does.
2. Messages = the journal's messages. If empty, use `fallback.prompt`, but only if `log`
   still holds `fallback.log_len_before` messages (counts, not equality, as
   `InflightHook::settle` does). A failed retry attempt in `drive()` has already appended the
   prompt through `RecoveryHook` and cut the journal back, so an Esc during the backoff or
   before the next attempt's first model call finds an empty journal and a longer log; the
   fallback would double the prompt there. In that case (empty journal, `log` past
   `fallback.log_len_before`) append the marker alone, since the failed attempt is already in
   `log`. With an empty journal and no fallback, do nothing and return `false`.
3. `append_messages(store, log, messages + [marker])`. The marker is a
   `Message::user(text)`:
   - `User`: `[Request interrupted by user]`, the exact text Claude Code uses.
   - `AfkStop`: `[Harness note, not the user's words: the turn was stopped because the AFK
     temperature ceiling was reached.]`
   - `SessionEnded`: `[Harness note, not the user's words: the session ended before this turn
     finished.]`
   The harness-note wording follows `RETRY_NOTE` in `turn.rs`, and the master plan's Decision
   that a scheduled turn is a harness note, never a `UserMessage`: neither cause is the user's
   doing.
4. Only after the append succeeds, empty the journal: `replace_log_async` to zero bytes,
   the same cut `InflightHook::cut_back(0)` makes. Factor `cut_back`'s body into a free
   function `cut_journal(store, log, keep)` that both use. If the append fails, the journal
   stays and the fold before the next root run (see §3) folds it, so nothing is lost.
   The append and the cut are not atomic: if the append lands and the cut fails, the journal
   is still full at the next fold. So before step 3, when the journal is non-empty, record
   `log`'s message count and the journal's byte length in a sidecar `<log>.inflight.fold`
   (replaced, one entry); if that write fails, return the error without appending. Clear the
   sidecar after the cut. A fold that finds a sidecar whose journal length matches the
   journal and whose count plus the journal's message count plus one equals `log`'s count
   knows the append landed: it only cuts the journal and clears the sidecar. In every other
   case (no sidecar, a stale one, a count that does not fit) it folds as usual, so a
   mismatch can at worst double a turn, never drop one. Comparing contents instead could
   match an earlier identical folded turn and drop the new one; a doubled turn would repeat
   tool-call ids and make the next request invalid.

**Pairing.** A consecutive user message (the marker
after a tool-result message, then the next prompt) is already a shape the conversation carries
today after a retry (`RETRY_NOTE`). A test pins that the next request is accepted by the mock
providers (see Testing Strategy).

**Interaction with the hook at the next run.** `InflightHook::start` records `carried_bytes`
from the journal at run start and cuts back to it at settle, so a journal carried into a run
would outlive it and be folded after the newer turns rig saved, out of order. The surfaces
therefore fold any leftover journal before each root run starts (§3), so a run normally starts
from an empty journal.

### 3. Call the fold where a root run is dropped

A runtime method wraps the fold, warns through the approver on failure (`chain(&e)`, as
`RecoveryHook` does), and is the single call site for surfaces:

```rust
impl AgentRuntime {
    pub async fn keep_unsettled_turn(&self, cause: Unsettled, fallback: Option<Fallback>);
}
```

- **TUI** (`turn.rs` `turn()`). Before the run future is built, keep the prompt message the
  run will send: `user_message(&prompt, &attachments, &modalities)`, and `main`'s message
  count. Inside the run future, right after `compact_if_needed` returns, take `main`'s count
  again into a slot (an `Arc<AtomicUsize>` or `Cell`) the post-drop fold reads, because a
  compaction that saves shrinks `main`; an interrupt before the slot is set (during
  compaction) uses the count taken before. The prompt and that count make the `Fallback`.
  Expose `user_message` as
  `pub` from `fontaine_core::turn`, or add a small `pub fn prompt_message(...)` wrapper. After
  `drop(run)`, and before `finish_turn`, call `keep_unsettled_turn` when the loop broke with
  `Interrupted` (`Unsettled::User`) or with the signal branch's `Stopped`
  (`Unsettled::AfkStop`). Do **not** call it for a `Finished` relabelled `Stopped` (the run
  settled and was saved), or for `InputClosed` (the session is ending, and the journal is
  folded at the next resume). To tell the two `Stopped` cases apart, record whether the loop
  broke out of the signal branch before the relabel.
- **Headless** (`headless.rs` ~line 593). When the AFK stop branch wins (`None`) and the
  turn had not finished, call `keep_unsettled_turn(Unsettled::AfkStop, Some(fallback))` before
  `handle_afk_stop`. The prompt text is cloned and `main`'s message count recorded before
  `run_turn` takes it (headless sends no attachments).
- **Before each root run** (in the TUI, before `runtime.begin_turn` in the submission paths,
  through one helper they all call, so the folded journal sits before the rewind checkpoint;
  the harness-turn paths that record no checkpoint call the same helper before `turn()`; in
  headless, before `run_turn`). Call
  `keep_unsettled_turn(Unsettled::SessionEnded, None)`, with no fallback, so a journal left by
  a failed fold is folded before a later turn can run and be saved after it.
- **Session start** (`record_session_start`). For a `Continued` origin, call
  `keep_unsettled_turn(Unsettled::SessionEnded, None)`. A journal left by a killed process
  (headless Ctrl-C, a crash, a closed terminal) is folded before the first turn. A fresh
  session has no journal, and the call is a no-op there anyway. A fork copies the logs
  directory, so a leftover journal is folded into the copy too, which is correct: the copy
  continues the same conversation.

The TUI awaits the fold before `finish_turn`, so the next turn sees the folded history.

### Behavior

| Run ends by | Today | After |
|---|---|---|
| Answer / failure | Saved by rig / `RecoveryHook` | Unchanged; journal empties at settle |
| Esc / Ctrl-C in the TUI | Turn lost | Prompt + finished tool rounds + `[Request interrupted by user]` |
| Esc before the first model call (during auto-compaction) | Turn lost | Prompt + marker |
| AFK ceiling (TUI or headless) | Turn lost | Same as interrupt, AFK-stop note as marker |
| Process killed mid-turn, then `--resume` | Turn lost | Folded at session start, session-ended note as marker |
| Headless `--max-budget-usd` stop (`Flow::Stop` → `TurnOutcome::Stopped`, run dropped unsettled) | Turn lost | Journal kept; folded with the session-ended note before the next run (at `--resume`) |

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
   `record_session_start` for `Continued` origins. Surfaces name the enum as
   `fontaine_core::turn_recovery::Unsettled`.
4. **`fontaine-core/src/turn.rs`**: make the prompt message buildable by surfaces (`pub` on
   `user_message`, or a thin `pub fn`).
5. **`fontaine-tui/src/ui/turn.rs` `turn()`**: build and keep the prompt message and the
   post-compaction message count slot, track whether
   the run was dropped unsettled, and call `keep_unsettled_turn` after `drop(run)`. Fold any
   leftover journal before `begin_turn` in the submission paths (one shared helper).
6. **`fontaine/src/headless.rs`**: fold any leftover journal before `run_turn`; fold on the
   AFK-stop branch.
7. **Tests** (see Testing Strategy).
8. **Docs**: see `## Documentation`.
9. **`tasks/fontaine_harness_plan.md`**: a dated line at the end of the `### Sessions`
   subsection of `## Decisions` recording the one deviation from Claude Code: partial streamed
   text and an in-flight tool batch are not kept.

## Files to Modify

- `rust/fontaine-core/src/turn_recovery.rs`
- `rust/fontaine-core/src/agent/build.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-core/src/turn.rs`
- `rust/fontaine-tui/src/ui/turn.rs`
- `rust/fontaine/src/headless.rs`
- `rust/fontaine-core/tests/turn_recovery.rs`, `rust/fontaine-tui/tests/tui.rs`
- `docs/tui.md`, `docs/architecture.md`, `docs/headless.md`, `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Decisions

- 2026-11-06: Journal + fold instead of a cooperative cancel through rig's `cancel_error` path, because that would make Esc wait for the next hook boundary and cannot cover a killed process.
- 2026-11-06: The surface that drops the run calls the fold, not `run_turn`, because a drop guard cannot run async I/O.
- 2026-11-06: The surface passes its sent prompt as the fold's fallback instead of journaling in `on_run_start`, because a completion-call hook can patch the prompt and risk a duplicated or stale one.
- 2026-11-06: No synthetic tool results, because the journal never strands a tool call; keeping an in-flight batch's results is a follow-up.
- 2026-11-06: Accepted: when `RecoveryHook`'s append fails for a failed attempt and a retry in the same `run_turn` succeeds, the carried journal outlives the run and may be folded out of order later; this needs two store failures.
- AFK: Guard the fold with `main`'s message count and the journal's byte length recorded in a `<log>.inflight.fold` sidecar before the append, and only cut when they show the append landed, folding normally on any mismatch — a doubled turn repeats tool-call ids and breaks the next request, and a content check could match an earlier identical turn and lose the new one.
- AFK: Accept the `SessionEnded` marker for the rare in-process pre-run fold after a failed fold, no new marker variant — it only arises after a store failure and the wording still tells the model the turn did not finish.

## Documentation

- `docs/tui.md`: the Esc/Ctrl-C row. The interrupted turn is kept in the model's conversation
  up to its last finished tool round, followed by an interruption marker, and the next prompt
  continues from there. Text the model was still streaming is not kept.
- `docs/architecture.md`: session layout. `logs/main.inflight.jsonl` journals the main run in
  progress, as `agent-<n>.inflight.jsonl` does for subagents.
- `docs/tui.md`, queued messages (~lines 420-422): a message delivered with a tool round that
  finished before the interrupt is kept; one delivered into the tool batch still running at
  the interrupt is lost and must be sent again.
- `docs/headless.md` (~line 120) and the `--resume` passage of `docs/tui.md`: resuming after a
  process that died mid-turn keeps that turn, marked as unfinished.
- `CHANGELOG.md`: `Fixed`. An interrupted turn stays in the model's conversation.

## Testing Strategy

The wild bug (#457: the model said "I don't have the earlier context") is pinned by the first TUI
integration test.

- **Unit, `turn_recovery.rs`** (MemoryStore):
  - Fold appends the journal's messages and the marker in order and empties the journal.
  - An empty journal with a fallback appends prompt + marker. An empty journal with no
    fallback is a no-op that returns `false`.
  - A non-empty journal ignores the fallback, so the prompt is not doubled.
  - An empty journal with a fallback whose `log_len_before` is below `main`'s count (a failed
    retry attempt already appended the prompt, Esc during the backoff) appends the marker
    alone, so the prompt is not doubled and the interruption is still marked.
  - Each `Unsettled` cause produces its marker text.
  - An append failure (a MemoryStore wrapper that fails appends and replaces to the `main`
    log only) returns the error and leaves the journal unchanged.
  - A fold whose append landed but whose cut failed (sidecar count + journal + marker) is
    cut without a second append at the next fold; a sidecar whose count equals `main`'s, or
    whose journal length does not match, folds normally.
- **Core integration, `fontaine-core/tests/turn_recovery.rs`** (scripted models): run a root
  turn with a tool call whose second request is held, drop the `run_turn` future, call
  `keep_unsettled_turn(User, ..)`, and assert the stored conversation holds the prompt, the
  tool call, its result and the marker, and that `main.inflight` is empty. Then run a second
  turn and assert its request's history carries all of it. A second test: a session reopened
  with a leftover journal (written by dropping a run without folding) folds it at
  `record_session_start`, with the session-ended marker. A journal left by a failed fold is
  folded before the next turn's request, so that request's history holds the old turn before
  the new prompt.
- **TUI integration, `fontaine-tui/tests/tui.rs`**. This is the issue's regression test. Start
  a turn that makes a tool call (`sleep` shell, as the queued-message test does), wait for the
  tool card, press Esc, wait for "interrupted", send a follow-up, and assert that the
  follow-up's request contains the first prompt, the tool call, the interruption marker and the
  follow-up. Plus one test that interrupts before any model call completes and asserts the
  first prompt and marker reach the next request.
- **Headless.** In `rust/fontaine/tests/afk.rs`, extend
  `the_ceiling_stops_the_tree_and_a_stopped_session_refuses_to_run_again` (or add a sibling)
  to assert that `main` holds the prompt, the tool round and the AFK-stop marker after the
  stop.
