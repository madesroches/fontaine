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

`RetryPolicy::next_delay`'s third parameter is renamed `elapsed` → `waited` (its doc comment and
the unit tests' `ZERO`/`nearly` arguments move with it), so the pure policy states the quantity it
wants and a future caller cannot repeat this bug. The `delay > max_delay` arm is untouched: a
`Retry-After` above `max_delay` still settles the failure at once.

`RetryPolicy::immediate` stays correct by construction: its `max_total_delay` is `ZERO` and so is
every wait it returns, so an accumulating budget never grows off zero.

No whole-turn wall-clock cap is added. The issue only floats one ("if a cap on the whole turn's
wall time is wanted as well, give it its own setting"), and one already exists where unattended
runs need it: `[goal] max_duration_secs` pauses a goal stretch on wall time. Overloading
`max_total_delay_secs` with both meanings is the bug being fixed.

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
   `Opening::new(async move { sleep.await; inner.send(request, exchange).await })` for the requests
   at or after a chosen index. Add a `build_slow` helper beside `build_warning` that wires the
   wrapper into a `Models` with the `Warnings` approver, and the three tests below.
4. `rust/fontaine-sdk/tests/turn_recovery.rs`: keep the existing retry cases green as they are —
   `a_retryable_failure_is_retried_and_the_task_continues`, `retries_run_out_and_the_failure_is_reported_once`
   (5 attempts), `a_quota_failure_gets_fewer_retries_and_still_falls_through` (3 attempts),
   `a_retry_after_is_waited_out_and_each_retry_is_announced`,
   `a_retry_after_beyond_the_cap_settles_the_failure_at_once`, `a_retry_never_replays_a_tool_call`.
5. `docs/configuration.md:1203`: make the `max_total_delay_secs` meaning explicit about being the
   running total of the waits, so the sentence that was already right cannot drift again.

## Files to Modify

- `rust/fontaine-sdk/src/core/retry.rs` — parameter rename, doc comment, one new unit test.
- `rust/fontaine-sdk/src/core/turn.rs` — `drive`'s budget source.
- `rust/fontaine-sdk/tests/turn_recovery.rs` — `Slow` transport, `build_slow`, three tests.
- `docs/configuration.md` — one clarifying clause on `max_total_delay_secs`.
- `CHANGELOG.md` — the fix, under `### Fixed`.

## Trade-offs

- **Accumulate the waits (chosen)** vs. **reset `started` per attempt**: resetting would bound each
  attempt's budget separately, so N attempts could wait up to `N × max_total_delay` — not the
  "longest the retries of one turn wait in total" the setting promises. Accumulating also keeps the
  policy pure.
- **Accumulate (chosen)** vs. **drop `max_total_delay` and let `max_retries` bound it**: a single
  `Retry-After: 300` would then stall a turn for minutes. The bound is worth keeping; only its
  input was wrong.
- **No `AFK`-style "budget exhausted" warning line**: the per-attempt notices already show which
  retries happened before a settle, and the failure text carries the last error. A new line would
  be a user-visible message the issue does not ask for.
- **A hung attempt is not charged (chosen)** vs. **charging attempts too**: charging attempts is
  what made a 60 s hang cost a short run its second retry. Retry spacing is what a "total delay"
  names; a turn that hangs unbounded is the transport's timeout to bound, not the backoff's.

## Decisions

- Fixed inside `drive` and the policy's parameter name; no new `[retry]` key and no new
  whole-turn wall-clock setting (the conservative option: nothing new to configure).

## Documentation

- `docs/configuration.md` — the `max_total_delay_secs` row gains the "running total of the waits"
  clause. `docs/architecture.md:190-195` already describes "a cap on the total wait" and stays
  accurate.
- `CHANGELOG.md` — one `### Fixed` entry naming the bug: a retryable failure late in a turn was
  never retried.

## Testing Strategy

Unit (`retry.rs`): the accumulating-budget arithmetic above, plus the existing exponential, cap,
jitter, `Retry-After` and quota-budget cases under the renamed parameter.

Integration (`turn_recovery.rs`, paused clock, real `run_turn` over the scripted mock transport):

- `a_long_turn_still_retries_a_transient_failure` — pins this bug. Script
  `[shell_turn("echo x"), error_turn(503), answer_turn("done")]` and have `Slow` sleep
  `max_total_delay + 10 s` before the first request, so the turn has been working far longer than
  the budget when the 503 arrives. Assert `TurnOutcome::Answer`, `request_count() == 3`, and a
  warning line containing `retry 1 of 4`. Before the fix the outcome is `Failed` with one request;
  after it, the retry happens and is announced.
- `a_slow_failing_attempt_does_not_spend_the_retry_budget` — the issue's second clause: `Slow`
  sleeps past the budget before the *failing* request itself (`[error_turn(503), answer_turn]`),
  which under the old code charged the attempt's own duration and settled at once. Assert
  `Answer` and `request_count() == 2`.
- `the_total_wait_budget_still_settles_the_retries` — the bound must still bite. A user file with
  `[retry] max_total_delay_secs = 10`, turns `[throttle_with_retry_after("7"),
  throttle_with_retry_after("7"), answer_turn]`: the first wait is taken (7 s of 10 spent), the
  second would pass the total, so the failure settles with `request_count() == 2` and one warning
  line. This is what a future "budget removed entirely" regression trips.

No live-network, database or container step: every check is a unit or integration test over the
scripted mock transport, which is the tier that reproduces the bug deterministically.

## Open Questions

None: the issue states the cause, the expected behavior and the test, and the code agrees with it.
