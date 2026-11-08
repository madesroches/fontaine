# Provider Warnings Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/505

## Overview

Providers have no route to `Approver::warn`. The claude-subscription cache self-check warning and
the provider's other warnings go only to `tracing::warn!`, which a TUI or headless user never
sees. Add a warn handle to `ConnectContext` and route every provider warning through it, so each
reaches the session's `Approver`.

## Current State

- `ConnectContext` (`fontaine-core/src/providers/mod.rs:299`) carries no approver or warn hook;
  `client_for` (`models.rs:423`) builds it.
- Clients are built before any `Approver` exists: `models::resolve` (`ResolveParams`) runs at
  startup from ~10 call sites (`fontaine/src/lib.rs`, `auto_mode.rs`, `doctor_cmd.rs`,
  `models_cmd.rs`, `fontaine-tui/src/setup.rs`, tests), and `Switcher` re-resolves mid-session.
- One provider warning already reaches the user: `providers/token_source.rs:119-127` holds a
  process-wide, latest-session-wins `warn_sink()` set by `set_warn_sink`, installed in
  `agent/setup.rs:868` (`prepare`) as `approver.warn`. It falls back to `tracing::warn!` when
  unset (`token_source.rs:281`).
- Provider warnings that only reach `tracing`:
  - `claude_subscription/transport.rs:478`: the cache self-check (the replayed context was not
    read back).
  - `claude_subscription/cli.rs:832`: `Scratch::drop` could not remove the temp directory.
  - `providers/cache_wire.rs:124`: the guardian request body is not JSON, so no cache marker.

## Design

A provider-warning handle that binds late: the client is built before the approver exists, and
the handle looks up the sink when a warning is emitted.

New module `providers/warn.rs`:

```rust
#[derive(Clone)]
pub struct Warn(Option<Arc<dyn Fn(&str) + Send + Sync>>);

impl Warn {
    /// Late-bound: reads the process-wide sink on every `emit`.
    pub fn process() -> Warn;
    /// Pinned to one sink, for tests and callers that own their destination.
    pub fn to(sink: impl Fn(&str) + Send + Sync + 'static) -> Warn;
    /// Sink if any, else `tracing::warn!`.
    pub fn emit(&self, message: &str);
}

pub fn set_sink(sink: impl Fn(&str) + Send + Sync + 'static); // moved from token_source
```

- `ConnectContext` gains `pub warn: Warn`; `client_for` sets `Warn::process()`.
- `token_source::set_warn_sink` and its `warn_sink()` move into `warn.rs`; `TokenSource` emits
  through `Warn::process()`. `agent/setup.rs:868` calls `providers::warn::set_sink`.
- claude-subscription: `Cli` holds the `Warn` (given `ctx.warn` in `connect`). `Running`
  (`transport.rs:400-407`) gains a `warn` field, filled from `self.cli` where it is built
  (`transport.rs:183`), so the free fn `stream` (`transport.rs:438`) can call `warn.emit(..)` at
  `transport.rs:478`. The version and error are structured `tracing` fields today, so the
  self-check message text must now include the CLI version. `Scratch::new` takes a clone from
  `Cli` (sites `transport.rs:203`, `cli.rs:439`, `cli.rs:556`) so its `Drop` warning reaches the
  user; that message must now include the error chain (`fontaine_tools::error_chain::chain`).
- `CacheWire` takes a `Warn` in `new`. Only `openrouter.rs:65` builds one, inside
  `Connected::model()` after connect: openrouter's `connect` stores `ctx.warn` on its `Connected`
  and `model()` passes `self.warn.clone()` to `CacheWire::new`.
- Warnings are advisory text only: no decision depends on them. The sink calls `Approver::warn`,
  whose default writes to stderr and which the TUI renders in the transcript.
- A warning emitted once per condition stays once (the self-check already fires once per CLI
  version, `mark_broken`); no new dedupe layer.

## Implementation Steps

1. Add `providers/warn.rs` (`Warn`, `set_sink`); export from `providers/mod.rs`.
2. Move the sink out of `token_source.rs`; emit through `Warn::process()`; update
   `agent/setup.rs:868`.
3. Add `warn: Warn` to `ConnectContext`; set it in `client_for` (`models.rs`) and in the
   `ConnectContext` literal in `fontaine-core/tests/network.rs`.
4. Thread `Warn` through claude-subscription (`Cli`, `Running`, `Scratch`) and convert
   `transport.rs:478` and `cli.rs:832`, adding the version and error chain to the message text.
5. Thread `Warn` into `CacheWire::new` (openrouter `Connected` stores `ctx.warn`); convert
   `cache_wire.rs:124`. Update its call sites and unit tests.
6. Tests (below); docs and CHANGELOG.

## Files to Modify

- `rust/fontaine-core/src/providers/{warn.rs (new),mod.rs,token_source.rs,cache_wire.rs,openrouter.rs}`
- `rust/fontaine-core/src/providers/claude_subscription/{mod.rs,transport.rs,cli.rs}`
- `rust/fontaine-core/src/{models.rs,agent/setup.rs}`
- `rust/fontaine-core/tests/network.rs`, `rust/fontaine-core/tests/claude_subscription.rs`
- `rust/fontaine-core/tests/common/` (receives the `claude_cli_double` helpers)
- `rust/fontaine-core/tests/provider_warn.rs` (new: sink-installing tests, own binary)
- `rust/fontaine-core/tests/provider_warn_claude_subscription.rs` (new: session-level self-check
  test, own binary)
- `docs/claude-subscription.md` (around lines 179-185), `CHANGELOG.md`

## Decisions

- 2026-11-08: Late-bound process-wide sink rather than an approver threaded through
  `ResolveParams`/`Switcher`: clients are built before the approver exists.
- 2026-11-08: Generalize the `token_source` sink rather than add a second one: the latest session
  wins.
- 2026-11-08: Warn handle as a `ConnectContext` field rather than a `Warn` on `AccountClient`.

## Documentation

Update `docs/claude-subscription.md` (lines ~179-185, which say the adapter "logs one warning
naming the version") to say the warning is shown in the session. `CHANGELOG.md` entry. No plan-section citations in docs or comments.

## Testing Strategy

Unit tests use `Warn::to` and never touch the process-wide sink. The sink is a never-unset
`OnceLock`, and `cargo test` runs unit tests as threads in one binary, so every test that calls
`set_sink` or relies on no sink being set lives in its own integration-test binary
(`tests/provider_warn.rs`).

- Unit: `Warn::emit` calls the pinned sink.
- Unit: `CacheWire` wrapping a fake inner wire whose `encode` returns a non-JSON `Body::Bytes` (rig's OpenAI Chat wire always emits JSON, so the `cache_wire.rs:124` branch is reachable only via a double) emits one warning through its `Warn`.
- Unit: `Scratch` drop failure delivers one warning to a `Warn::to` sink, with the error chain.
- `tests/provider_warn.rs`: one test function, since the steps share the process-wide sink and
  would race as parallel libtest threads. Fixed order: with no sink set, `Warn::process` falls back
  to `tracing` without panicking; then `set_sink` A, and `Warn::process` (created earlier) follows
  it (late binding); then `set_sink` B, and a `token_source` refresh-save failure (chmod pattern
  from `tests/token_source.rs`, step gated `#[cfg(unix)]`) reaches B.
- Integration (new binary `tests/provider_warn_claude_subscription.rs`): runs `prepare` through
  `tests/common/runtime.rs::build_with_approver` with `Scripted` (`tests/common/pipeline.rs`) as
  the approver, drives the claude-subscription self-check through `claude_cli_double`, and asserts
  `Scripted.warnings` holds the warning with the CLI version string in its text. The
  `claude_cli_double` helpers (`double()`, `Claude`, `three_steps`, `tagged_conversation`) move from
  `tests/claude_subscription.rs` into `tests/common/`.

## Open Questions

None.
