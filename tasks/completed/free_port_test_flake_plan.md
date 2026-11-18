# Fix flaky `free_ports_are_loopback_ports_that_can_be_bound` Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/700

## Overview

`core::managed_search::tests::free_ports_are_loopback_ports_that_can_be_bound` failed once in a
full parallel `cargo test` run. The test calls `free_port()`, then re-binds the returned port;
another process or test thread can take the port between the two steps. Make the test tolerate
that race without losing what it checks.

## Current State

- `free_port()` (`rust/fontaine-sdk/src/core/managed_search.rs`, ~line 120) binds `127.0.0.1:0`,
  reads the port and drops the listener.
- The test (~line 515) asserts `port > 0`, then `TcpListener::bind(("127.0.0.1", port)).unwrap()`.
  That `unwrap` is the flake: the sdk lib tests run in parallel and open many loopback sockets.
- Production use has the same choose-then-use window; it is inherent to `free_port()`'s contract
  ("nothing listens on now") and is not changed here.

## Design

Test-only change. Retry the whole `free_port()` + bind sequence a few times (fresh port each
attempt) and pass on the first success; panic with the last bind error if all attempts fail.
This keeps the check that the returned port is a real, bindable loopback port, and a genuinely
broken `free_port()` (e.g. always returning an occupied or invalid port) still fails every attempt.

No change to `free_port()` or any production code.

## Implementation Steps

1. In `rust/fontaine-sdk/src/core/managed_search.rs`, rewrite the test body: loop up to 5
   attempts of `free_port()` (assert `port > 0`) then bind; return on first `Ok`; after the loop
   panic with the last error. Add a one-line comment saying why it retries (port can be taken
   between the calls by parallel tests).
2. Run `cargo test -p fontaine-sdk free_ports_are_loopback` and the full sdk lib tests a few times.

## Files to Modify

- `rust/fontaine-sdk/src/core/managed_search.rs` (test module only)

## Trade-offs

- Only asserting `port > 0`: simplest, but drops the bindability check.
- Retrying (chosen): keeps the check; residual flake probability is negligible (needs 5
  consecutive collisions).
- Holding the port open to avoid the race: contradicts `free_port()`'s contract of returning an
  unbound port, so it cannot test it.

## Testing Strategy

The change is itself a test. Verify with `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, and `cargo test -p fontaine-sdk` run repeatedly. A deterministic reproduction of the
race is not possible, and the fix is not a behavior change, so no further test is added.

## Documentation

None; no user-visible change.
