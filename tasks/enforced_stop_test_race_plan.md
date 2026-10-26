# Cascade Test Guardian-Call Race Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/209

## Overview

`an_enforced_stop_drops_the_guardian` failed once in CI on `main` (`left: 0, right: 1` at
`cascade.rs:134`). The test asserts the guardian endpoint has already received its request when
`adjudicate` returns, but nothing orders the two: the screen answers instantly, so on a loaded
runner `adjudicate` can return before the concurrently polled guardian request has reached the mock
server. This plan makes the assertion wait for the request to arrive, in both tests that share the
pattern.

## Current State

`adjudicate` (`fontaine-guardian/src/cascade.rs:33`) pins the guardian future and the screen
future and `select!`s them. The guardian future is polled first, but its HTTP request still has to
be built, connected and written before the mock server records it. A screen with zero delay can
finish first; the enforced `Stop` / `Allow` arm then drops the guardian future, cancelling the
request. The doc comment on `Composed::dropped_input_tokens` already says the request "has usually
been sent", not "has been sent".

Two tests assert the opposite as a hard fact, with `endpoint.calls().await` read once, immediately
after `adjudicate` returns:

- `an_enforced_stop_drops_the_guardian` (`tests/cascade.rs:134`), the observed failure.
- `an_enforced_allow_drops_the_guardian` (`tests/cascade.rs:158`), identical setup (zero-delay
  screen, 3 s guardian) and the same unguarded assertion.

`Endpoint::calls` (`tests/common/mod.rs:131`) is a point-in-time count of the mock server's
received requests. The other `calls()` assertions in `tests/guardian.rs` run after the guardian
call has been awaited to completion, so they are not racy; `guardian.rs:555` asserts `0` for an
estimate that sends nothing, which is also not a race.

## Design

Keep the production code as is (see Decisions); only the tests' assumption changes.

Add one helper to `tests/common/mod.rs`:

```rust
impl Endpoint {
    /// Waits until at least `n` requests have arrived, for the cases where the request is sent
    /// by a future that raced something else; returns the count seen when it stops waiting.
    pub async fn wait_for_calls(&self, n: usize, within: Duration) -> usize
}
```

It polls `calls()` every few milliseconds until the count reaches `n` or `within` elapses, and
returns the last count so the caller's `assert_eq!` still prints a useful `left`/`right` on a
real failure. It does not sleep a fixed time first, so the common case stays instant.

Both tests replace `assert_eq!(endpoint.calls().await, 1)` with
`assert_eq!(endpoint.wait_for_calls(1, Duration::from_secs(1)).await, 1)`.

The deadline sits well under the 3 s guardian delay, and the request is recorded on arrival, not on
response, so a mock that is still sleeping does not delay the count. The wait runs after the
`started.elapsed() < 2s` check, so it cannot mask a regression where the guardian is awaited.

`assert_eq!(.., 1)` stays an equality, not `>= 1`: a retry or a second request would be a real
behavior change the test should keep catching (the guardian's retries only follow a failed
response, and this reply is a slow success).

## Implementation Steps

1. `fontaine-guardian/tests/common/mod.rs`: add `Endpoint::wait_for_calls`.
2. `fontaine-guardian/tests/cascade.rs`: use it at lines 134 and 158; update the comment at 133 to
   say the request is awaited because it races the screen.
3. Run `cargo test -p fontaine-guardian --test cascade` repeatedly, and under CPU load (see
   Testing Strategy), to confirm.

## Files to Modify

- `rust/fontaine-guardian/tests/common/mod.rs`
- `rust/fontaine-guardian/tests/cascade.rs`

## Trade-offs

- **Nonzero screen delay (e.g. 50 ms)** instead of polling: smaller diff, but it only shifts the
  race; a loaded runner can still stall the guardian task past any fixed delay, and it slows the
  tests.
- **Drop the call-count assertion**: removes the check that the guardian was in fact asked in
  parallel with the screen (the point of the cascade's overlap), so the cascade's "billed but
  dropped" behavior would go unpinned.
- **Make production guarantee the send before returning**: would delay every enforced screen
  decision on network I/O, defeating the stage's purpose.

## Decisions

- 2026-10-26: `dropped_input_tokens` and its doc ("usually been sent") stay unchanged; only the
  tests' stronger assumption is corrected.

## Testing Strategy

The change is to test code, so verification is running it:

- `cargo test -p fontaine-guardian --test cascade` passes.
- Stress for the race: run the two tests many times with the CPU saturated, e.g.
  `cargo nextest run -p fontaine-guardian --test cascade -E 'test(/drops_the_guardian/)' --stress-count 200`
  (or a shell loop of `cargo test` if nextest's stress mode is unavailable) while a parallel
  `yes > /dev/null` per core runs. Before the change this should reproduce the `0 != 1` failure at
  some rate; after it, none. If the failure does not reproduce before the change, say so in the PR
  rather than claim a verified fix.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.

## Documentation

None: no user-visible behavior changes. Add a `CHANGELOG.md` line only if the project logs
test-only fixes (check the recent entries).

## Open Questions

None.
