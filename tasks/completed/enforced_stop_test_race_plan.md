# Cascade Test Guardian-Call Race Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/209

## Overview

`an_enforced_stop_drops_the_guardian` failed once in CI on `main` (`left: 0, right: 1` at
`cascade.rs:134`). The test asserts the guardian endpoint has already received its request when
`adjudicate` returns, but nothing orders the two: the screen answers instantly, so on a loaded
runner `adjudicate` can return before the guardian request has reached the mock server. This plan
makes the screen server hold its answer until the guardian endpoint has received its request, in
both tests that share the pattern, so the race cannot happen.

## Current State

`adjudicate` (`fontaine-guardian/src/cascade.rs:33`) pins the guardian future and the screen
future and `select!`s them. The guardian future is polled first, but its HTTP request still has to
be built, connected and written before the mock server records it. A screen with zero delay can
finish first; the enforced `Stop` / `Allow` arm then drops the guardian future, cancelling the
request before it is ever sent. The doc comment on `Composed::dropped_input_tokens` already says
the request "has usually been sent", not "has been sent".

Two tests assert the opposite as a hard fact, with `endpoint.calls().await` read once, immediately
after `adjudicate` returns:

- `an_enforced_stop_drops_the_guardian`, the observed failure.
- `an_enforced_allow_drops_the_guardian`, identical setup (zero-delay screen, 3 s guardian) and
  the same unguarded assertion.

The other `calls()` assertions in `tests/guardian.rs` run after the guardian call has been awaited
to completion, so they are not racy.

## Design

Keep the production code as is (see Decisions); only the tests' setup changes. Polling the count
after `adjudicate` returns cannot fix this (see Trade-offs): a guardian future dropped before it
sent never sends, so the count stays 0. The fix must order the two events before the drop.

In `tests/common/mod.rs`, `Endpoint` exposes `arrivals()`, a clone of the `Arc<AtomicUsize>` that
its `Scripted` responder bumps for every request it answers.

In `tests/cascade.rs`, `screen_server` and the new `screen_server_after_guardian` share
`screen_server_with`, which mounts the three screen reads (stop, steering, allow) and takes a
closure building the responder for each read's score. `screen_server_after_guardian` takes the
guardian endpoint's `arrivals()`; each screen responder waits (bounded at 1 s, so a guardian that is
never asked cannot hang the test) until it is non-zero, then answers with no delay. Blocking the
responder is fine: the mock server runs on its own thread.

The two tests use it, so the enforced decision always finds the guardian request already sent
and drops a request in flight, which is the case the test means to pin. The count assertion goes
back to `assert_eq!(endpoint.calls().await, 1)`, with a comment saying why it is exact: wiremock
records a request before running its responder, so once the screen gate opens the request is
already counted. It stays an equality, not `>= 1`: a retry or a second request would be a real
behavior change the test should keep catching.

## Implementation Steps

1. `tests/common/mod.rs`: add `Endpoint::arrivals`.
2. `tests/cascade.rs`: factor `screen_server_with` out of `screen_server`; add
   `screen_server_after_guardian`; use it in both tests and restore the plain `calls()` assertion
   with the why-comment.
3. Run `cargo test -p fontaine-guardian --test cascade` repeatedly, and under CPU load (see
   Testing Strategy), to confirm.

## Files to Modify

- `rust/fontaine-guardian/tests/common/mod.rs`
- `rust/fontaine-guardian/tests/cascade.rs`

## Trade-offs

- **Poll for the count after `adjudicate` returns** (a `wait_for_calls` helper): the first design,
  rejected. When the enforced arm drops the guardian future before it sends, the request never
  arrives, so the count stays 0 however long the test waits. Stress runs still failed with it.
- **Nonzero screen delay (e.g. 50 ms)**: smaller diff, but it only shifts the race; a loaded
  runner can still stall the guardian task past any fixed delay, and it slows the tests.
- **Drop the call-count assertion**: removes the check that the guardian was in fact asked in
  parallel with the screen (the point of the cascade's overlap), so the cascade's "billed but
  dropped" behavior would go unpinned.
- **Make production guarantee the send before returning**: would delay every enforced screen
  decision on network I/O, defeating the stage's purpose.

## Decisions

- 2026-10-26: `dropped_input_tokens` and its doc ("usually been sent") stay unchanged; only the
  tests' stronger assumption is corrected.
- 2026-10-26: the polling-only design was replaced by gating the screen on the guardian's arrival
  because stress runs showed polling still failed (polling-only: 1 failure in 150 runs, twice;
  before any change: 2 in 150; gated: 0 in 400).

## Testing Strategy

The change is to test code, so verification is running it:

- `cargo test -p fontaine-guardian --test cascade` passes.
- Stress for the race: run the two tests many times with the CPU saturated, e.g.
  `cargo nextest run -p fontaine-guardian --test cascade -E 'test(/drops_the_guardian/)' --stress-count 200`
  (or a shell loop of `cargo test` if nextest's stress mode is unavailable) while a parallel
  `yes > /dev/null` per core runs. Before the change this reproduced the `0 != 1` failure at
  some rate; with the gate, none.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.

## Documentation

None: no user-visible behavior changes. Add a `CHANGELOG.md` line only if the project logs
test-only fixes (check the recent entries).

## Open Questions

None.
