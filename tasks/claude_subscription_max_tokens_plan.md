# claude-subscription: end a max_tokens response at message_stop Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/479

## Overview

When a response through the Claude CLI stops at `max_tokens`, the CLI runs its own output-limit
recovery in the same process: it appends a synthetic user message and calls the model again, up
to three times, and `--max-turns 1` does not stop it. The recovery's `user` frame reaches
`Reader::denial_echo`, which refuses it as native tool execution, so a truncated response becomes
a non-retryable protocol failure, and the extra inferences are billed to the plan for nothing.
The adapter should treat a `max_tokens` stop as terminal at `message_stop`: stop the process
tree there, release the held terminal delta, and hand rig the truncated response like any other
provider's.

## Current State

All in `rust/fontaine-core/src/providers/claude_subscription/`:

- `protocol.rs` `Reader::stream_event`: `message_delta` with a `stop_reason` stores it and holds
  the frame in `terminal`; `message_stop` sets `stopped` and returns `Event::Nothing`. The
  response completes only at `Reader::result` (`Event::Complete`), which validates the subtype,
  the denial/echo bookkeeping and the usage, and records `cache_usage`.
- `Reader::accept` routes `user` frames to `denial_echo`, which accepts only error
  `tool_result`s answering calls this response announced.
- `transport.rs` `stream`: on `Event::Complete` it stops the child, runs `context::finished`
  with `reader.cache_usage()`, then pushes `reader.release()` so rig ends its read only after
  validation. A `max_tokens` stop never gets there: the recovery frames arrive first.
- `context::finished` takes `usage: Option<CacheUsage>`; `None` drops the conversation's
  self-check record without flagging the replay broken, so a response ended early is safe there.
- `cli.rs` `FIXED_ENV` carries the switches that turn CLI behaviors off. None disables the
  output-limit recovery.

## Design

### Terminal at message_stop for `max_tokens`

In `Reader::stream_event`'s `message_stop` arm, after the existing checks (stop reason present,
no open blocks), when `stop_reason == "max_tokens"` return a new `Event::Cut` instead of
`Event::Nothing`. `stopped` is set as today.

Skipped for this stop: the `result` checks (subtype, tool-stop/denied-call contradiction, usage
cross-check) and `cache_usage`. They describe a completed run, and the CLI's result for a
recovered run would describe several inferences rather than this response.

### Transport

`transport.rs` `stream` handles `Event::Cut` like `Event::Complete` minus the result
dependent work: stop the child, call `context::finished(sent, None, now)` (drops the record,
no false "cache broken" warning), push `reader.release()` and end. To avoid duplicating the
stop/release tail, factor it into one local helper used by both arms.

The `Event` enum lives in `protocol.rs`; `Event::Complete` consumers outside `transport.rs`
(the `prepare` ack loop at `transport.rs:273`) treat `Cut` the same as the existing
unexpected-event arm.

### Recovery switch

`FIXED_ENV` stays unchanged unless the live probe below finds a switch. Investigation step: grep
the CLI bundle / `claude --help` / documented env for an output-limit recovery toggle
(`maxOutputTokensRecoveryCount` is internal), and run the live measurement from the issue
(`claude-haiku-5-5`, `max_tokens: 200`, thinking) to count how often a recovery `message_start`
is sent before the kill. Findings go in the PR description; if a switch exists, add it to
`FIXED_ENV` with a *why* comment and extend the existing env test.

## Implementation Steps

1. `protocol.rs`: add `Event::Cut`; return it from `message_stop` for `max_tokens`;
   document on the `Reader` that this stop ends the response without a result.
2. `transport.rs`: handle `Event::Cut` in `stream` and the `prepare` ack loop; share the
   stop-and-release tail with `Complete`.
3. Live probe for a CLI switch and for the pre-kill recovery rate; add the switch if found.
4. Tests (below).
5. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`
- `rust/fontaine-core/tests/claude_subscription.rs` (double-driven test)
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (only if a switch is found)
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **A new `Event` vs. reusing `Complete`**: `Complete` implies a validated result and carries
  `cache_usage`; a distinct event keeps the cache self-check from treating a missing result as
  evidence.

## Decisions

- 2026-11-08: a `max_tokens` stop ends the CLI at `message_stop` instead of letting its output-limit recovery run, because it bounds plan-usage cost and avoids discarded inferences.

## Documentation

`docs/claude-subscription.md`: note that a `max_tokens` stop ends the process at `message_stop`
(the response is returned truncated, the CLI's own recovery never runs), and any residual usage
cost measured live.

## Testing Strategy

Unit tests in `protocol/tests.rs` (constructed frames, no process):

- `max_tokens` stop then `message_stop` yields `Event::Cut`; the held terminal delta is
  then released unmodified; other stops still return `Nothing` until the result.
- A `max_tokens` stop with an open block returns `Err(ProviderError::Truncated)`, not an accepted
  response.
- A `max_tokens` stop whose tool_use block was closed with `content_block_stop` returns
  `Event::Cut`, and the call is forwarded with whatever partial input the stream carried.

Integration test with the double (`tests/claude_subscription.rs`): a script emitting a
`max_tokens` response, then a recovery `user` frame and a second message, yields the first
response truncated with no protocol error, and the test asserts the child stopped via the double's descendant pid not being alive (pins the
reported bug: the recovery `user` frame was refused as a native tool request).

## Manual Verification

1. With a logged-in CLI, request `max_tokens: 200` against `claude-haiku-5-5` with a prompt that
   spends the budget thinking; expect a truncated response, no error, and the process gone.
   Not automated: needs the live CLI and a subscription, and only measures the pre-kill leak.

## Open Questions

- Does the CLI expose a switch for the output-limit recovery? Settled by the live probe in
  step 3; the code change does not depend on it.
