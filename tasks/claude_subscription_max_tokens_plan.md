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
no open blocks), when `stop_reason == "max_tokens"` return a new `Event::Truncated` instead of
`Event::Nothing`. `stopped` is set as today. `Reader` records that the response ended without a
result (`result_seen` stays false) so a late `result` frame cannot be accepted: after the stop
nothing further is read, so this is only a guard for the unit-test path.

Skipped for this stop: the `result` checks (subtype, tool-stop/denied-call contradiction, usage
cross-check) and `cache_usage`. They describe a completed run, and the CLI's result for a
recovered run would describe several inferences rather than this response.

### Transport

`transport.rs` `stream` handles `Event::Truncated` like `Event::Complete` minus the result
dependent work: stop the child, call `context::finished(sent, None, now)` (drops the record,
no false "cache broken" warning), push `reader.release()` and end. To avoid duplicating the
stop/release tail, factor it into one local helper used by both arms.

The `Event` enum lives in `protocol.rs`; `Event::Complete` consumers outside `transport.rs`
(the `prepare` ack loop at `transport.rs:273`) treat `Truncated` the same as the existing
unexpected-event arm.

### Failure ordering

The max_tokens response is accepted only if every earlier validation passed (second
`message_start`, native tools, open blocks). A recovery that starts before the kill never
reaches the reader because the stream ends at `message_stop`; the process-tree stop is the
existing `CliChild::stop`.

### Recovery switch

`FIXED_ENV` stays unchanged unless the live probe below finds a switch. Investigation step: grep
the CLI bundle / `claude --help` / documented env for an output-limit recovery toggle
(`maxOutputTokensRecoveryCount` is internal), and run the live measurement from the issue
(`claude-haiku-5-5`, `max_tokens: 200`, thinking) to count how often a recovery `message_start`
is sent before the kill. Findings go in the PR description; if a switch exists, add it to
`FIXED_ENV` with a *why* comment and extend the existing env test.

## Implementation Steps

1. `protocol.rs`: add `Event::Truncated`; return it from `message_stop` for `max_tokens`;
   document on the `Reader` that this stop ends the response without a result.
2. `transport.rs`: handle `Event::Truncated` in `stream` and the `prepare` ack loop; share the
   stop-and-release tail with `Complete`.
3. Live probe for a CLI switch and for the pre-kill recovery rate; add the switch if found.
4. Tests (below).
5. Docs and changelog.

## Files to Modify

- `rust/fontaine-core/src/providers/claude_subscription/protocol.rs`
- `rust/fontaine-core/src/providers/claude_subscription/protocol/tests.rs`
- `rust/fontaine-core/src/providers/claude_subscription/transport.rs`
- `rust/fontaine-core/tests/claude_subscription.rs` (double-driven test)
- the `claude_cli_double` script support if it cannot emit a post-stop `user` frame and second message
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs` (only if a switch is found)
- `docs/claude-subscription.md`, `CHANGELOG.md`

## Trade-offs

- **Terminal at `message_stop` vs. tolerating the recovery frames**: tolerating them would let the
  CLI keep calling the model, spending plan usage on inferences Fontaine discards, and make the
  response depend on which of several inferences "wins". Ending at `message_stop` is the only
  option that bounds cost.
- **A new `Event` vs. reusing `Complete`**: `Complete` implies a validated result and carries
  `cache_usage`; a distinct event keeps the cache self-check from treating a missing result as
  evidence.

## Decisions

## Documentation

`docs/claude-subscription.md`: note that a `max_tokens` stop ends the process at `message_stop`
(the response is returned truncated, the CLI's own recovery never runs), and any residual usage
cost measured live.

## Testing Strategy

Unit tests in `protocol/tests.rs` (constructed frames, no process):

- `max_tokens` stop then `message_stop` yields `Event::Truncated`; the held terminal delta is
  then released unmodified; other stops still return `Nothing` until the result.
- A `user` recovery frame or second `message_start` after a `max_tokens` stop is never reached
  in the transport path, and a `max_tokens` stop with an open block is still `Truncated`
  error (`ProviderError::Truncated`), not an accepted response.
- A `max_tokens` response containing a tool-use block is accepted as truncated (the model was
  cut mid-call); rig surfaces the partial call as it does for other providers.

Integration test with the double (`tests/claude_subscription.rs`): a script emitting a
`max_tokens` response, then a recovery `user` frame and a second message, yields the first
response truncated with no protocol error, and the double observes the child stopped (pins the
reported bug: the recovery `user` frame was refused as a native tool request).

## Manual Verification

1. With a logged-in CLI, request `max_tokens: 200` against `claude-haiku-5-5` with a prompt that
   spends the budget thinking; expect a truncated response, no error, and the process gone.
   Not automated: needs the live CLI and a subscription, and only measures the pre-kill leak.

## Open Questions

- Does the CLI expose a switch for the output-limit recovery? Settled by the live probe in
  step 3; the code change does not depend on it.
