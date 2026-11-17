# Retry Budget Counts Only Waits Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/678

## Overview

`turn::drive` charges the whole run's wall time to the retry budget instead of the time spent
waiting between attempts, so any run longer than `[retry] max_total_delay_secs` (120 s by default)
settles its first retryable model failure at once — which is every real session. Retrying is thus
dead machinery for the failure it was built for. The fix is to accumulate the waits `drive` has
actually slept and pass that to the policy, which is what the setting is documented to bound.

## Current State

- `rust/fontaine-sdk/src/core/turn.rs:636` — `drive` captures `let started = tokio::time::Instant::now()`
  once, on entry. `drive` is entered once per prompt (`run_turn` → `drive`, and the child runner for
  subagents), so `started` spans every model call and tool call of the whole agentic run.
- `rust/fontaine-sdk/src/core/turn.rs:690-694` — the retry decision passes `started.elapsed()` as
  the policy's third argument.
- `rust/fontaine-sdk/src/core/retry.rs:81-96` — `RetryPolicy::next_delay` returns `None` (settle
  now) when `elapsed.saturating_add(delay) > self.max_total_delay`.
- `started` has no other use in `drive`: the only `elapsed` read is that one. The `elapsed_ms` in
  the model-request telemetry is `ModelStep::started` (`turn.rs:323-380`), a different per-request
  field that must stay.
- The policy's own arithmetic is right, and so is the classification: rig's `is_retryable` treats a
  connect timeout as retryable and `CausedClient` keeps that variant, so the failing call was
  meant to be retried.
- `docs/configuration.md:1203` already describes `max_total_delay_secs` as "the longest the retries
  of one turn **wait** in total", and `tasks/completed/provider_retry_backoff_plan.md` specifies the
  same; only the implementation disagrees.
- The retry notice (`<agent>: model call failed (<status, class>); retry n of N in Xs`,
  `turn.rs:730-740`) is already emitted per attempt.

## Design

Two lines of state in `drive`, and one rename in the policy — no new config, no new type, no change
to classification, the floor, the guardian or the pipeline.

```
let mut waited = std::time::Duration::ZERO;   // replaces `started`
loop {
    ...
    let wait = ... policy.next_delay(
        attempt,
        error_retry_after(&error, SystemTime::now()),
        waited,                                // was: started.elapsed()
        retry::jitter(),
    );
    ...
    tokio::time::sleep(wait).await;
    waited += wait;
    attempt += 1;
}
```

Because only the sleeps are added, everything else a turn spends time on — model latency, tool
runs, approval waits, usage-window pauses, the failed attempt's own duration — is free of the
budget. That is exactly the documented meaning.

`RetryPolicy::next_delay`'s third parameter is renamed `elapsed` → `waited` (and its doc comment), so the pure policy states the quantity it
wants and a future caller cannot repeat this bug. The `delay > max_delay` arm is untouched: a
`Retry-After` above `max_delay` still settles the failure at once.

`RetryPolicy::immediate` stays correct by construction: its `max_total_delay` is `ZERO` and so is
every wait it returns, so an accumulating budget never grows off zero.

## Implementation Steps

1. `rust/fontaine-sdk/src/core/retry.rs`: rename `next_delay`'s `elapsed` parameter to `waited` and
   reword its doc comment ("the waits taken so far", not "elapsed"). Update the call in
   `the_total_delay_cap_settles_the_failure` for the new name and add
   `the_budget_accumulates_across_attempts`: a policy with `base_delay = max_delay = 40s` and
   `max_total_delay = 100s` yields `Some(40s)` at `waited = 0`, `Some(40s)` at `waited = 40s`, and
   `None` at `waited = 80s`.
2. `rust/fontaine-sdk/src/core/turn.rs`: drop `let started = tokio::time::Instant::now()` from
   `drive`, introduce `let mut waited = std::time::Duration::ZERO`, pass `waited` to `next_delay`,
   and add the sleep's duration to it after `tokio::time::sleep(wait).await`. Leave `ModelStep`'s
   `started` alone.
3. `rust/fontaine-sdk/tests/turn_recovery.rs`: add a `Slow` transport wrapper next to the existing
   `Holding` one — same inner `MockRuntime`, but sleeping `delay` inside
   `Opening::new(async move { sleep.await; inner.send(request, exchange).await })` for the first
   request only. Add a `build_slow` helper beside `build_warning` that wires the
   wrapper into a `Models` with the `Warnings` approver, and the two tests below.

## Files to Modify

- `rust/fontaine-sdk/src/core/retry.rs` — parameter rename, doc comment, one new unit test.
- `rust/fontaine-sdk/src/core/turn.rs` — `drive`'s budget source.
- `rust/fontaine-sdk/tests/turn_recovery.rs` — `Slow` transport, `build_slow`, two tests.
- `CHANGELOG.md` — the fix, under `### Fixed`.

## Decisions

- Fixed inside `drive` and the policy's parameter name; no new `[retry]` key and no new
  whole-turn wall-clock setting (the conservative option: nothing new to configure).
- Accumulate the waits rather than reset the budget per attempt; keep `max_total_delay`; add no
  budget-exhausted warning line; do not charge hung attempts to the budget.

## Documentation

- `docs/architecture.md:190-195` already describes "a cap on the total wait" and stays accurate.
- `CHANGELOG.md` — one `### Fixed` entry naming the bug: a retryable failure late in a turn was
  never retried.

## Testing Strategy

Unit (`retry.rs`): the accumulating-budget arithmetic above, plus the existing exponential, cap,
jitter, `Retry-After` and quota-budget cases under the renamed parameter.

Integration (`turn_recovery.rs`, paused clock, real `run_turn` over the scripted mock transport):

- `a_long_turn_still_retries_a_transient_failure` — pins this bug. Script
  `[error_turn(503), answer_turn("done")]` and have `Slow` sleep `max_total_delay + 10 s` before
  the first request, so the turn has run far longer than the budget when the 503 arrives. Assert
  `TurnOutcome::Answer`, `request_count() == 2`, and a warning line containing `retry 1 of 4`.
  Before the fix the outcome is `Failed` with one request; after it, the retry happens and is
  announced.
- `the_total_wait_budget_still_settles_the_retries` — the bound must still bite. A user file with
  `[retry] max_total_delay_secs = 10`, turns `[throttle_with_retry_after("7"),
  throttle_with_retry_after("7"), answer_turn]`: the first wait is taken (7 s of 10 spent), the
  second would pass the total, so the failure settles with `request_count() == 2` and one warning
  line. This is what a future "budget removed entirely" regression trips.

No live-network, database or container step: every check is a unit or integration test over the
scripted mock transport, which is the tier that reproduces the bug deterministically.

## Open Questions

None: the issue states the cause, the expected behavior and the test, and the code agrees with it.
