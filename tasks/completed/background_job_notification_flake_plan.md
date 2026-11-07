# Background Job Notification Test Flake Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/460

## Overview

Make `a_job_that_ended_before_the_answer_gets_a_notification_turn` reliable under workspace-wide
parallel test load by replacing the background command's fixed 300 ms lifetime with an explicit
handoff from the fake model endpoint. The job will wait until the second model request proves that
the shell result has been processed, then finish while that answer is held back. Production code
and behavior stay unchanged.

## Current State

- `rust/fontaine/tests/background_jobs.rs:43-95` scripts a shell command containing `sleep 0.3`
  and applies `Route::slow(Duration::from_millis(1000))` to every model answer. The 300 ms process
  can be scheduled late enough that the headless run finishes with two requests instead of the
  expected three.
- The ordering has two edges. `NotificationStage`
  (`rust/fontaine-core/src/child_runner.rs:113-130`) drains an already queued job note into the
  next tool result, so an instant job can deliver its note in request 2 rather than in a turn of
  its own. Conversely, `wait_for_news` (`rust/fontaine/src/headless.rs:676-710`) returns `Done`
  after an answer when no notification is queued and no subagent runs; it deliberately does not
  wait for background jobs.
- `Route` (`rust/fontaine/tests/common/mod.rs:354-385`) supports a fixed response delay but no
  request-arrival hook. `Routed::respond` (`common/mod.rs:430-466`) assigns the route's zero-based
  turn index before constructing the response, which is the point at which a test can release
  other work.
- The job registry tests already cover completion posting to a `JobSink`, and core tests cover
  rendering a queued job note through `notification_prompt`. This issue is the headless test's
  cross-process scheduling, not a demonstrated production defect.

## Design

Add an additive request-arrival hook to the shared fake routed endpoint:

- `Route` gains an optional `Arc<dyn Fn(usize) + Send + Sync>` and a builder such as
  `.on_serve(...)`. `Routed::respond` invokes it with the turn index immediately after matching
  the route and incrementing that route's counter, before applying the response delay. Existing
  routes without a hook behave exactly as before. Preserve `Route`'s `Clone + Debug` support with
  a small hook wrapper or manual `Debug` implementation that does not print the closure.
- Hooks are synchronous test callbacks and should return promptly. The API runs only in the test
  helper; it adds no production or configuration surface.

In the flaky test, construct the sandbox before the endpoint and choose a marker path in its
runtime directory:

- Replace `sleep 0.3; echo job-says-hi` with a loop that waits for that marker and then prints
  `job-says-hi`. The polling interval is only a wait mechanism; no assertion depends on a fixed
  job lifetime.
- The route's `on_serve` callback creates the marker only for turn index 1 (the second request).
  Request 2 cannot arrive before the shell tool result has passed through `NotificationStage`, so
  this release excludes the left-edge race in which the note rides that tool result.
- Keep the 1 s response delay. It starts after the release and covers job reaping and the
  synchronous `JobSink::post` before the answer reaches `wait_for_news`. The old fixed 300 ms
  process lifetime disappears; a stalled test runner now delays consumption of the same delayed
  response whose completion it could otherwise overtake.
- Keep every existing assertion, especially three requests, no completion note in either earlier
  request, and the final answer `the job said hi`. If the handoff does not happen, the headless
  run terminates its waiting job at exit and the request-count assertion fails rather than
  leaving a test process behind.

## Implementation Steps

1. `rust/fontaine/tests/common/mod.rs`: add the optional route callback and invoke it at request
   arrival with the zero-based turn index; leave routing, response bodies, counters, and delays
   unchanged for all other callers.
2. `rust/fontaine/tests/background_jobs.rs`: create the sandbox first, add the marker-gated shell
   command and second-request release hook, remove the 300 ms sleep, and update the comments to
   describe the two ordering edges.
3. Run the focused regression and shared-helper checks in Testing Strategy.
4. Let the PR step add a `CHANGELOG.md` reliability entry for the test fix.

## Files to Modify

- `rust/fontaine/tests/common/mod.rs`
- `rust/fontaine/tests/background_jobs.rs`
- `CHANGELOG.md` (PR finalization)

## Trade-offs

- **Release the job at request 2 vs. make it instantaneous.** The request-arrival handoff is
  necessary because an already queued note would be consumed by `NotificationStage` into the
  shell result. An instant command only moves the flake to that other ordering edge.
- **Keep the response margin vs. add a production notification-test seam.** A sink that mirrors
  queued notes to the endpoint could provide a strict cross-process happens-before edge, but it
  would change production setup solely for this test. The handoff removes the scheduler-sensitive
  300 ms job lifetime while the existing delay covers the short in-process completion path.
- **Gate the response on a process-written completion file.** Rejected: file creation by the
  command precedes process reaping and `JobSink::post`, so it adds blocking-helper complexity
  without proving that the notification was queued.
- **Retries or running only in a less-loaded configuration.** Rejected: both hide the regression
  and reduce the headless coverage that caught the problem.

## Decisions

None yet.

## Documentation

No product documentation changes: the patch touches only test infrastructure and a test. The PR's
changelog entry records the reliability fix.

## Testing Strategy

The original failure occurred at the headless integration tier, so the same test remains the
regression pin; no live API is involved.

- `cargo test -p fontaine --test background_jobs` from `rust/`: both tests pass, with the first
  still proving a real job end note produces a third request carrying the exit status, output,
  untrusted-data warning, and job-news header.
- `cargo test -p fontaine` from `rust/`: all users of the shared `Route` helper retain their
  behavior, including routed-agent, auto-headless, fork, and cassette-backed tests.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` from `rust/`.
- Repeat the focused test under CPU contention as a stress check. This catches cross-process
  scheduling failures that a single green run cannot clear; it supplements rather than replaces
  the automated regression above.

## Manual Verification

From `rust/`, with `stress-ng` available, run the focused test repeatedly while every CPU has a
worker:

```sh
stress-ng --cpu "$(nproc)" --timeout 180 &
load=$!
for run in $(seq 1 50); do
    cargo test -p fontaine --test background_jobs \
        a_job_that_ended_before_the_answer_gets_a_notification_turn -- --exact || break
done
wait "$load"
```

Expected: all 50 runs pass. This remains a manual stress check because CI cannot guarantee a
scheduler interleaving on demand; the deterministic request-to-job handoff and assertions are in
the ordinary automated test.

## Open Questions

None.
