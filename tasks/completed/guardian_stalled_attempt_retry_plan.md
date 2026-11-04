# Retry a Stalled Guardian Call Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/310

## Overview

A guardian (or LLM-scanner) model call that never answers burns the whole `[guardian] timeout`
(30 s default) and is denied as `Timeout` with no retry, because the retry budget in
`structured::run` only reacts to calls that return. In unattended `auto` that becomes a deny nobody
clears. Give each attempt its own slice of the overall budget inside `structured::run`, so a
stalled attempt is dropped and retried (counting as one of the existing retries) while the
overall timeout stays the hard ceiling and the fail-closed result is unchanged.

## Current State

- `rust/fontaine-guardian/src/structured.rs`: `run` loops `attempt_once` up to `1 + options.retries`
  times. It reacts only to returned errors; there is no time bound inside it.
- `rust/fontaine-guardian/src/guardian.rs` (~l.282): wraps `structured::run::<Verdict>` in
  `tokio::time::timeout(settings.timeout, ..)`; `Err(_)` becomes `FailClosed::Timeout` with the
  accumulated `spent.total()`. `EXTRACTOR_RETRIES = 2`.
- `rust/fontaine-guardian/src/scanner.rs` (~l.167): same shape for `LlmScanner::run` with
  `retries: 1`, mapping the elapsed timeout to `Scan::Failed`.
- `Tally` is shared across attempts and records usage as each response arrives, so dropping an
  in-flight attempt loses nothing already billed.
- Tests: `rust/fontaine-guardian/tests/guardian.rs` has a scripted `Endpoint` with `Reply::Slow`,
  and `a_guardian_that_does_not_answer_in_time_is_denied_as_a_timeout` (paused clock);
  `tests/scanner.rs` covers the scanner.
- Docs: `docs/guardian.md` (~l.246, 763-766) and `docs/configuration.md` (~l.518) describe the
  timeout as covering retries.

## Design

`structured::run` owns the time budget.

- `Options` gains `budget: Duration` (the overall ceiling; callers pass `settings.timeout` /
  `self.timeout`).
- `run` records `started = tokio::time::Instant::now()` (not std, so the per-attempt
  `tokio::time::timeout` and the paused test clock agree) and, before each attempt, computes
  `remaining = budget.saturating_sub(elapsed)` (when zero, return `TimedOut` without starting another attempt) and `attempts_left = 1 + retries - attempt`. The attempt's
  timeout is `min(remaining, max(remaining - (attempts_left - 1) * ATTEMPT_FLOOR, ATTEMPT_FLOOR))`: the first attempt keeps the budget minus a floor reserved for each later attempt, with
  `ATTEMPT_FLOOR = 5 s`. The slice function returns `Option<Duration>`, `None` meaning out of budget (zero remaining), and `run` returns `TimedOut` on `None`. With the defaults (30 s, 3 attempts) that is 20 s for the first attempt, then 5 s for each retry;
  the last attempt always gets all that is left, and a floor that exceeds `remaining` is clamped to it.
- An attempt that hits its timeout counts as a used attempt and logs a `tracing::warn!` carrying the attempt number and the slice that ran out. If attempts remain and budget is left,
  retry with the same message the stalled attempt carried (a stall adds no new note; the previous note is kept).
  Otherwise return the timeout.
- The error type changes from `StructuredOutputError` to a new `pub(crate) enum RunError { TimedOut,
  Failed(StructuredOutputError) }`.
  Budget exhaustion after earlier returned errors: if the final attempt stalls, the result is
  `TimedOut` (the budget ran out), matching today's behavior where the ceiling wins.
- `guardian.rs` and `scanner.rs` drop their outer `tokio::time::timeout` (the budget now lives in
  `run`) and map `RunError::TimedOut` to `FailClosed::Timeout` / the existing `Scan::Failed`
  message, and `Failed(e)` to the existing arms. The `Timeout` explanation text and `spent.total()`
  tally are unchanged.
- The `Settings::timeout` doc comment stays accurate and is unchanged.

## Implementation Steps

1. `structured.rs`: add `budget` to `Options`, `ATTEMPT_FLOOR`, `RunError`, and the per-attempt
   timeout logic in `run`; extract the slice computation into a small pure function. Update the `run` doc (a stall keeps the earlier rejection note; only a rejected answer earns a new one) and add a doc for the new `budget` field.
2. `guardian.rs`: pass `budget: self.settings.timeout`, remove the outer `timeout`, map `RunError`. Update the `EXTRACTOR_RETRIES` doc: retries also cover a stalled attempt.
3. `scanner.rs`: same for `LlmScanner::run` (`budget: self.timeout`).
4. Tests (below), docs, CHANGELOG entry under `### Fixed`.

## Files to Modify

- `rust/fontaine-guardian/src/structured.rs`
- `rust/fontaine-guardian/src/guardian.rs`
- `rust/fontaine-guardian/src/scanner.rs`
- `rust/fontaine-guardian/tests/guardian.rs`, `rust/fontaine-guardian/tests/scanner.rs`,
  `rust/fontaine-guardian/tests/common/mod.rs`
- `docs/guardian.md`, `docs/configuration.md`
- `CHANGELOG.md`

## Trade-offs

- Front-loaded slicing (first attempt gets the budget minus a floor per later attempt) over an
  equal split: an equal ~10 s slice would time out models that legitimately need 10–20 s per answer
  (kimi-for-coding p95 17.5 s, see `docs/evals.md`) on every attempt, a regression from today.
  Answers taking 20–30 s now need a larger `timeout_secs`. It adapts to the configured `timeout` with no new config key. Cost: a
  retry after a stall gets only the floor (5 s), enough for a fast hosted model, not for a slow one;
  a model that needs more than `timeout - 2 * floor` (20 s by default) per answer needs a larger `timeout_secs`.
- Floor of 5 s: a short configured timeout should not be cut into slices too small to answer.

## Documentation

- `docs/guardian.md`: the retry-budget sentence (~l.250, "two retries, shared between malformed
  answers and provider errors") also says stalled attempts use that budget; the `Timeout` row and the scanner bound: a stalled attempt is dropped and
  retried within the same timeout; the timeout remains the ceiling. Note that the first attempt may use all but 10 s of the timeout, so a slow model needs a `timeout_secs` above its worst answer time plus 10 s.
- `docs/configuration.md`: the `timeout_secs` row and the scanner bound (~l.526, "`timeout_secs` bounds each scan") say the same in one clause.
- `CHANGELOG.md`: `### Fixed` entry (#310).

## Testing Strategy

All automated, no live APIs. `Reply::Slow` is a wiremock delay on a real socket, so a paused clock
auto-advances past a retried attempt's slice before the real reply arrives; the stall behavior tests
therefore use a deterministic seam instead: a small delaying mock `Transport` (rig test-utils
`Transport` trait + `Opening::new`; pattern at `rust/fontaine-core/tests/goal.rs:796-810` `Silent`)
that either sleeps with `tokio::time::sleep` or never resolves, and otherwise delegates to the mock.
It lives in `rust/fontaine-guardian/tests/common/mod.rs`; the wrapper calls the inner `MockRuntime`
before it sleeps or hangs, so the stalled request is recorded (hence "endpoint saw 2 calls").
Hang-then-valid, all-stall, stall+malformed+valid (rejection note kept), usage-before-stall tallied
on `Timeout`, and scanner hang-then-valid are integration tests using this transport on a paused
clock (`Guardian::new` / `LlmScanner::new` take a public `DynModel`); `structured.rs` keeps only the
slice-function unit tests. Exactly one real-socket wiremock test stays
(`tests/guardian.rs`, real clock, a `Slow` first reply, 15 s timeout, about one slice (~5 s) of real
time) as the in-the-wild regression for `attack-data-exfiltration-02` (stalled at 30001 ms).

- Unit tests for the slice function: first attempt with default budget gets 20 s and later ones 5 s; a slow-but-working answer (e.g. 18 s) fits attempt 1; floor applies;
  floor clamps to remaining; last attempt gets the remainder; zero remaining returns `None`, and `run` returns `TimedOut`
  without another attempt.
- Stall tests on the delaying transport: a reply that hangs and then a valid one yields the verdict
  within the budget and the endpoint saw 2 calls; a hang that is followed by two more hangs still gives `Timeout`
  with total elapsed not above the budget; a stall plus a malformed answer plus a good one stays
  within the 3-attempt cap; Malformed, then Slow, then a valid reply gives a verdict with 3 calls, and
  the third request still contains the "Your previous answer was rejected" section; usage billed before a stall is still tallied on `Timeout`. Existing timeout tests stay green.
- Scanner, same transport: hang then valid reply is a verdict; always-hang is `Failed`.
- `tests/guardian.rs`: the single real-clock wiremock regression above.

## Decisions

- AFK: 2026-11-04: Front-load the slices (first attempt = remaining minus floor per later attempt, 20 s/5 s/5 s at defaults) instead of an equal split, so slow-but-working models still answer on attempt 1; answers needing over timeout minus 2 floors need a larger `timeout_secs`, documented.

## Open Questions

None.
