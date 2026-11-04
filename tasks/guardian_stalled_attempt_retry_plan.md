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
  timeout is `min(remaining, max(remaining / attempts_left, ATTEMPT_FLOOR))`, with
  `ATTEMPT_FLOOR = 5 s`. With the defaults (30 s, 3 attempts) that is 10 s for the first attempt;
  the last attempt always gets all that is left, and a floor that exceeds `remaining` is clamped to it.
- An attempt that hits its timeout counts as a used attempt. If attempts remain and budget is left,
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
   timeout logic in `run`; extract the slice computation into a small pure function.
2. `guardian.rs`: pass `budget: self.settings.timeout`, remove the outer `timeout`, map `RunError`.
3. `scanner.rs`: same for `LlmScanner::run` (`budget: self.timeout`).
4. Tests (below), docs, CHANGELOG entry under `### Fixed`.

## Files to Modify

- `rust/fontaine-guardian/src/structured.rs`
- `rust/fontaine-guardian/src/guardian.rs`
- `rust/fontaine-guardian/src/scanner.rs`
- `rust/fontaine-guardian/tests/guardian.rs`, `rust/fontaine-guardian/tests/scanner.rs`
- `docs/guardian.md`, `docs/configuration.md`
- `CHANGELOG.md`

## Trade-offs

- Equal split of the remaining budget (as the issue suggests) over a fixed per-attempt timeout:
  it adapts to the configured `timeout` and to attempts already spent, with no new config key.
- Floor of 5 s: models answer in 1–2 s, but a slow local model or a short configured timeout should
  not be cut into slices too small to answer.

## Documentation

- `docs/guardian.md`: the retry-budget sentence (~l.250, "two retries, shared between malformed
  answers and provider errors") also says stalled attempts use that budget; the `Timeout` row and the scanner bound: a stalled attempt is dropped and
  retried within the same timeout; the timeout remains the ceiling.
- `docs/configuration.md`: the `timeout_secs` row and the scanner bound (~l.526, "`timeout_secs` bounds each scan") say the same in one clause.
- `CHANGELOG.md`: `### Fixed` entry (#310).

## Testing Strategy

All automated with the scripted endpoint (no live APIs). `Reply::Slow` is a wiremock delay on a real
socket, so a paused clock auto-advances past the retried attempt's slice before the real reply
arrives. The all-stall tests (every attempt hangs) use `start_paused = true`; the success-after-stall
tests run on the real clock with a configured timeout above 2x the floor (15 s guardian, 12 s
scanner) and a first reply that sleeps past the first slice, so each takes about one slice (~5 s) of
real time; keep them few. A stall in the wild
(`attack-data-exfiltration-02` at 30001 ms) motivated this, so it is pinned by a regression test.

- Unit tests for the slice function: first attempt with default budget gets budget/3; floor applies;
  floor clamps to remaining; last attempt gets the remainder; zero remaining returns `TimedOut`
  without another attempt.
- `tests/guardian.rs`: a reply that hangs and then a valid one yields the verdict within the budget
  and the endpoint saw 2 calls; a hang that is followed by two more hangs still gives `Timeout`
  with total elapsed not above the budget; a stall plus a malformed answer plus a good one stays
  within the 3-attempt cap; Malformed, then Slow, then a valid reply gives a verdict with 3 calls, and
  the third request still contains the "Your previous answer was rejected" section; usage billed before a stall is still tallied on `Timeout`; a stalled
  attempt after a rejected answer still resends (no extra attempt). Existing timeout tests stay green.
- `tests/scanner.rs`: hang then valid reply is a verdict; always-hang is `Failed`.

## Open Questions

None.
