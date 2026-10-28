# Failed Model Call Recovery Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/281

**Status**: implemented on branch `resilience`. Gates green: `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, and both `--guardian fixture` eval
runs. The live-terminal manual check below remains undone.

## Overview

A transient provider failure mid-stream (connection reset, body read error, 5xx, timeout) currently
ends the turn and leaves the in-flight user message and any completed tool calls out of the
conversation store, so the next model call starts as if the task never happened. This plan makes a
failed model call recoverable: the conversation is saved when a run fails, and retryable transport
errors are retried a bounded number of times inside the turn before the failure reaches the user.
After the fix, typing `retry` (or any next message) continues the same task, a failed turn is
neither dropped from nor half-written into the history the next call sends, and the failure is
reported once through the existing turn-failure path with the full error chain.

## Current State

- `run_turn` (`rust/fontaine-core/src/turn.rs:98`) and `drive` (`turn.rs:202`) drive one rig-agent
  run and stream its `SessionEvent`s to the surface. `finish_builder`
  (`rust/fontaine-core/src/agent/build.rs:245`) binds the root agent to the session store with
  `.memory(p.session.store()).conversation(CONVERSATION_LOG)`.
- rig-agent writes the conversation **only when a run finishes** (`AgentRunStep::Done`). Its memory
  module states this explicitly: "a run that fails or is cancelled saves nothing"
  (`rig-agent-0.45.0/src/agent/memory.rs`). The engine's failure path fires the `RunSettled` hook
  with the run's committed messages but does not save them (`rig-agent-0.45.0/src/agent/engine.rs`,
  `fail!` macro). `fork_context.rs` already works around this save-only-at-Done behavior for forked
  subagents (`ParentContextHook`), which records the latest model call because the stored
  conversation lacks mid-turn state.
- On a mid-stream error `drive` collapses the `PromptError` to
  `TurnOutcome::Failed(failure_text(...))` (`turn.rs:339-357`), the TUI shows it once in the
  transcript (`rust/fontaine-tui/src/ui/turn.rs` `settle_turn`), and the prompt returns. The
  conversation store still holds only the prior successful turns, so the next call does not see the
  interrupted request. `retry` is plain user text — there is no retry command — so today it reaches
  the model as a fresh task. The todo bar survives because todos are a separate store
  (`Session::todos`), which matches the report.
- rig already classifies retryable transport errors (`ProviderError::is_retryable`,
  `ErrorReport::is_retryable`; connection reset and body read errors are `Instance`/`StreamEnded`,
  5xx and 408/425/429 are retryable by status — `rig-core-0.45.0/src/error.rs`,
  `src/error/retry.rs`), but the agent engine does not retry transport errors itself. Claude Code
  retries them a bounded number of times; Fontaine does not.

## Design

### 1. Save the conversation when a run fails

The failed run's committed messages already reach Fontaine through the `RunSettled` observing hook
(`messages: Some(&[Arc<Message>])` on error as well as success). Persist them on error so the next
run loads the interrupted task.

- Add `Session::append_messages(&self, messages: &[Arc<Message>]) -> Result<(), rig_memory::SessionError>`
  to `session.rs` next to `store()` (`session.rs:441`). It mirrors the engine's `Done` step:
  `load_async(&self.store, CONVERSATION_LOG)`, `conversation.extend(messages.iter().cloned())`,
  `save_async`. The messages a failed model call commits are already paired (no unanswered tool
  calls), so they save as a canonical transcript.
- Add a `RecoveryHook` in a new `rust/fontaine-core/src/turn_recovery.rs`, modeled on
  `ParentContextHook` (`fork_context.rs`). It is generic over the store
  (`RecoveryHook<S: AsyncStore>`) and holds the store, its log name and the session's
  `Arc<dyn Approver>`. In `on_run_settled`, when the outcome is `SettledOutcome::Error(_)` and
  `messages` is `Some`, it appends them through a shared `append_messages` helper (load, extend,
  save). A failed save warns once through `Approver::warn` with the full `error_chain::chain`;
  the run's outcome is unchanged. `Session::append_messages` delegates to the same helper.
- Register the hook on the root agent in `finish_builder` (`agent/build.rs`) with
  `RecoveryHook::new(p.session.store(), CONVERSATION_LOG, p.approver.clone())`, and on every
  subagent in `CoreChildRunner::spawn` (`child_runner.rs`) with its own store and log `"main"`:
  subagent model calls get the same retry primitive, which relies on the failed attempt being
  in the subagent's store too.
- Saving on every error — including `MaxTurns` and non-retryable errors — is the point: the session
  stays intact however the turn ends. This is a behavior change only on the failure path; success
  still goes through rig's `Done` save, and the hook does nothing on success.

### 2. Retry retryable transport errors inside the turn

`drive` becomes a small retry loop around a single-attempt primitive, so every caller — the
root's `run_turn` and the subagent runner alike — gets the same recovery. Re-driving rig's own
run is a surface-level orchestration, not a custom agent loop; the continuation note plays the
role the user's `retry` would.

```
drive(prompt)
   └─ for attempt in 0..=MAX_RETRIES:
        drive_once(prompt or RETRY_NOTE)
        ├─ Answer / MaxTurns / Stopped ──▶ return
        └─ Failed(e) and retryable(e) and attempts left ──▶ backoff, retry
        └─ Failed(e) ──▶ TurnOutcome::Failed(failure_text(e))
```

- Add `fn retryable(error: &PromptError) -> bool` in `turn.rs`:
  `PromptError::Provider(e) => e.is_retryable()`,
  `PromptError::Report(report) => report.is_retryable()`, else `false`.
- Refactor `drive` so the single attempt returns the typed error instead of collapsing it. Introduce
  a private `enum DriveEnd { Outcome(TurnOutcome), Failed(PromptError) }` and move the old
  `drive` body into `drive_once`, which returns `DriveEnd`. The public `drive` is the retry
  loop; on a settled outcome or a final failure its result is exactly what the old `drive`
  returned, so caller semantics (`run_turn`, `child_runner.rs`) are unchanged apart from the
  added retries.
- The loop reuses the caller's span and feed; attempt 0 uses the caller's prompt, a retry uses
  a fixed harness note as the prompt:

  ```text
  [Harness note, not the user's words: the previous model call failed before it finished; continue the task.]
  ```

  Because design 1 saved the failed attempt's messages (the original prompt plus completed tool
  calls and results), the retry run loads them from the store and the note is its only new input —
  the same effect as the user typing `retry`. The note is model context, not a ledger `UserMessage`,
  so the guardian's intent view is untouched.
- Bounded retries and backoff live in `turn.rs` as constants: `MAX_RETRIES = 2` (three total
  attempts) with a short doubling backoff (250 ms, then 500 ms). Exhausted retries or a
  non-retryable error map to `TurnOutcome::Failed(failure_text(...))`, reported once by the existing
  surface path.
- The retry runs re-use the same span and feed; each attempt streams events as it would
  for a normal turn. A transient failure that a retry absorbs is not reported to the user.

### 3. Telemetry

`api_error` keeps emitting from the turn loop, once per failed attempt, with no attempt count. This
preserves the existing event shape and the earlier decision that attempt counts are not exported
(observability design, `api_error`). The wiremock regression below asserts the retry count through
the provider's request log, not through telemetry.

## Implementation Steps

1. **Conversation append.** Add `Session::append_messages` in `rust/fontaine-core/src/session.rs`,
   importing `rig_core::message::Message`/`Arc` as needed. Unit-test it in `session.rs` or the new
   recovery module with a `DirStore` in a temp dir: append on an empty log, append onto existing
   entries, and a torn log remains readable.
2. **Recovery hook.** Create `rust/fontaine-core/src/turn_recovery.rs` (`RecoveryHook<S>` plus
   the shared `append_messages` helper), register the module in `lib.rs`, add it in
   `finish_builder` (`agent/build.rs`) with the session store, and in `CoreChildRunner::spawn`
   (`child_runner.rs`) with the subagent's store.
3. **Turn retry.** In `rust/fontaine-core/src/turn.rs`:
   - add `retryable`, `MAX_RETRIES`, the backoff and the retry note constants;
   - introduce `DriveEnd` and `drive_once`, and re-express `drive` as the retry loop around it.
4. **Tests.** Add core unit tests (see Testing Strategy) and update the one affected existing test.
5. **Wiremock regression.** Extend `rust/fontaine/tests/common/mod.rs` with a mid-body-cut `Turn`
   variant and add the headless recovery test.
6. **Docs and changelog.** Update `docs/architecture.md`, `docs/telemetry.md` if the event wording
   changes, and `CHANGELOG.md`.

## Files to Modify

- `rust/fontaine-core/src/session.rs` — `Session::append_messages`
- `rust/fontaine-core/src/turn_recovery.rs` — new `RecoveryHook` and `append_messages` helper
- `rust/fontaine-core/src/lib.rs` — module registration
- `rust/fontaine-core/src/agent/build.rs` — register the hook on the root agent
- `rust/fontaine-core/src/child_runner.rs` — register the hook on every subagent
- `rust/fontaine-core/src/turn.rs` — `retryable`, `DriveEnd`/`drive_once`, retry loop in `drive`
- `rust/fontaine-core/tests/audit_telemetry.rs` — the `run_turn` 429 telemetry test (see Testing Strategy)
- `rust/fontaine-core/tests/turn_recovery.rs` — new unit tests
- `rust/fontaine/tests/common/mod.rs` — a mid-body-cut scripted reply
- `rust/fontaine/tests/headless_recovery.rs` — new wiremock regression
- `docs/architecture.md`, `docs/telemetry.md` (if needed), `CHANGELOG.md`

## Trade-offs

- **Save-on-error hook vs patching rig's engine.** rig's engine already exposes the failed run's
  messages through `RunSettled`; a hook keeps rig untouched and follows the existing
  `ParentContextHook` precedent for working around save-only-at-Done. Patching rig to save on
  failure would also work but means carrying a rig fork for a small, well-contained behavior.
- **Re-drive the whole turn with a continuation note vs re-issuing only the failed model call.**
  Re-issuing only the model call (rig's `AgentRun::next_step` on an `AwaitingModel` state) is the
  most faithful to Claude Code, but that state lives inside rig's engine and is not reachable from
  the public API. Re-driving the turn through `run_turn` is implementable entirely at the Fontaine
  layer: the failed attempt's committed history is saved first, so the retry continues instead of
  re-running completed tool calls, and the continuation note is the model-visible "retry".
- **Save on every error vs only retryable/transport errors.** `SettledOutcome::Error` carries only
  the rendered string, so the hook cannot cheaply classify the error. Saving on every error is also
  the desired end state — `MaxTurns` and non-retryable failures should not lose their work either.
- **Retry loop in `drive` vs in `run_turn` only.** Putting the loop in `drive` gives subagent
  model calls the same primitive with one implementation; a subagent's in-memory store is a
  store like any other, so the recovery hook works there unchanged. The subagent's failure
  still surfaces through its report and the agents panel once the retries are spent.

## Decisions

- 2026-10-28: Auto-retry re-drives the turn with a harness continuation note rather than the
  original prompt, so the original user message is not duplicated in the conversation. The note is
  model context only and is never recorded as a ledger `UserMessage`.
- 2026-10-28: The recovery hook saves on every error outcome, not only transport errors, because the
  `RunSettled` error payload is a rendered string and because preserving the conversation on
  `MaxTurns` and non-retryable failures is the same "session stays intact" invariant.
- 2026-10-28: `api_error` stays one event per failed attempt with no attempt field; retry counts are
  observed through provider request logs in tests, not telemetry.
- 2026-10-28: Subagent model calls get the same retry primitive (user request, widening the
  original root-only scope): the retry loop lives in `drive`, and `RecoveryHook` is generic over
  the store so it also serves each subagent's in-memory store.

## Out of scope

- Guardian and scanner calls: the guardian already has its own bounded retry budget; this plan
  concerns the main model call.
- Configurability of the retry count and backoff: fixed constants until there is evidence a session
  needs to tune them.

## Documentation

- `docs/architecture.md`: in "Runs and the turn runner", one sentence that a failed run's committed
  conversation is saved and retryable transport errors are retried before a turn fails.
- `docs/telemetry.md`: only if the `api_error` description is touched (no change is planned).
- `CHANGELOG.md`: failed model calls no longer lose the interrupted turn, and retryable transport
  errors are retried a bounded number of times.

## Testing Strategy

All tests stay offline; unit tests use `rig_core::test_utils` and the wiremock regression uses the
existing scripted endpoint helpers.

- **Unit — history preserved on failure (regression for the reported bug):** build a runtime whose
  main model returns a single non-retryable failure (e.g. a 400 `MockStreamEvent::Error`), run
  `run_turn`, assert `TurnOutcome::Failed`, then `load_async` the session store and assert the
  user's prompt is present. This pins the exact wild bug: the in-flight request must survive a
  failed turn.
- **Unit — retryable error is retried and the history survives:** script `[error(429), text(...)]`
  through `MockCompletionModel::from_stream_turns`, run `run_turn`, assert `TurnOutcome::Answer`,
  assert the model was asked twice (initial attempt, then one retry that succeeds), and assert the
  store contains the original prompt and the continuation note. rig canonicalizes the retry's
  request into one user message holding both texts, so assertions match on the rendered history
  rather than on message boundaries.
- **Unit — non-retryable error is not retried:** script a 400, assert `TurnOutcome::Failed` and one
  model call.
- **Unit — retries exhaust:** script a repeating 429, assert `TurnOutcome::Failed` with the error
  text and exactly `1 + MAX_RETRIES` model calls.
- **Unit — `retryable` classification:** `Provider` and `Report` variants with retryable vs
  non-retryable statuses.
- **Unit — `append_messages`:** appends to an empty and to an existing log (tested through the
  shared helper on a `MemoryStore`; `Session::append_messages` delegates to it).
- **Integration (wiremock, headless `fontaine -p`):** add a scripted reply that emits part of an SSE
  body and ends without `[DONE]` (an early stream end, which rig classifies as a retryable transport
  error — the closest offline stand-in for a connection reset). Script `[cut, text]`, run
  `fontaine -p --session-id <id> hi`, assert success and that the endpoint saw two requests. Then
  run `fontaine -p --resume <id> "retry"` and assert the resumed request's `messages` array still
  contains the first prompt. This tier reaches the real HTTP/SSE body behavior a mock completion
  model cannot reproduce, and pins the "next turn still sees the prior request" half of the bug.
- **Existing test updates:** `a_failed_main_model_call_emits_an_api_error_without_the_message`
  (`rust/fontaine-core/tests/audit_telemetry.rs`) uses `run_turn` with a retryable 429 and asserts a
  single `api_error`. Change its script to a non-retryable status (e.g. 400) so it still checks the
  event shape in one attempt; the retry-count coverage comes from the new unit test. Any
  `drive`-based test scripting a retryable error now sees the retries (one `api_error` per
  attempt) and is updated the same way.
- **Existing suites stay green:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` from `rust/`, including the choke-point test.

## Manual Verification

- In a real terminal UI, point a session at a local provider you can kill mid-reply (or use a
  wiremock-style proxy), submit a task, and after the failure type `retry`. Confirm the model
  continues the original task rather than answering that it has no previous action, the transcript
  shows one final error only after the bounded retries, and the todo bar is unchanged. This is the
  end-to-end wiring and human-eyeball check that the offline tests approximate; it is not automated
  because a real provider reset mid-body is not reproducible deterministically in CI.

## Open Questions

None blocking implementation. One follow-up worth confirming separately: whether the retry
count and backoff should become configuration.
