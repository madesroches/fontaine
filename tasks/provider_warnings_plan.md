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
  Threading an approver through `ResolveParams`/`Switcher` would touch all of them, and the
  client would still be built before the approver is known.
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

- `ConnectContext` gains `pub warn: Warn`; `client_for` sets `Warn::process()`. This is the
  issue's "warn hook through `ConnectContext`/`client_for`", with the late binding the startup
  order forces. The process-wide sink is already the accepted shape for the token warning (sources
  are shared process-wide; the latest session wins), so this generalizes it rather than adding a
  second mechanism.
- `token_source::set_warn_sink` and its `warn_sink()` move into `warn.rs`; `TokenSource` emits
  through `Warn::process()`. `agent/setup.rs:868` calls `providers::warn::set_sink`.
- claude-subscription: `Connected` keeps `ctx.warn`; `Selection`/`ClaudeTransport` carry it, and
  `transport.rs:478` becomes `warn.emit(..)` with the same text (no `tracing` structured field;
  the version stays in the message). `Scratch` holds a `Warn` clone so its `Drop` warning
  reaches the user. `Cli` is built in `connect`, so it receives `ctx.warn` there.
- `CacheWire` takes a `Warn` in `new` (its constructors in `openrouter.rs`/`anthropic.rs` pass
  `ctx.warn`).
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
4. Thread `Warn` through claude-subscription (`Connected`, `Selection`/`ClaudeTransport`, `Cli`,
   `Scratch`) and convert `transport.rs:478` and `cli.rs:832`.
5. Thread `Warn` into `CacheWire::new`; convert `cache_wire.rs:124`. Update its call sites and
   unit tests.
6. Tests (below); docs and CHANGELOG.

## Files to Modify

- `rust/fontaine-core/src/providers/{warn.rs (new),mod.rs,token_source.rs,cache_wire.rs,openrouter.rs,anthropic.rs}`
- `rust/fontaine-core/src/providers/claude_subscription/{mod.rs,transport.rs,cli.rs}`
- `rust/fontaine-core/src/{models.rs,agent/setup.rs}`
- `rust/fontaine-core/tests/network.rs`, `rust/fontaine-core/tests/claude_subscription.rs`
- `docs/` page covering the claude-subscription provider's cache self-check (find via grep),
  `CHANGELOG.md`

## Trade-offs

- **Approver through `ResolveParams`/`Switcher`**: explicit, but needs an approver before one
  exists and edits every `resolve` call site; rejected for the late-bound handle.
- **Per-client sink set at connect**: same startup-order problem; the process-wide sink is
  already what the token warning uses.
- **A `Warn` trait object on `AccountClient`**: widens every provider's surface for three
  claude-subscription/cache_wire call sites; `ConnectContext` is the existing per-provider input.

## Decisions

## Documentation

Update the docs page that mentions the cache self-check warning to say it is shown in the session
(not only logged). `CHANGELOG.md` entry. No plan-section citations in docs or comments.

## Testing Strategy

Unit tests, all against `Warn::to` so none touches the process-wide sink:

- `Warn::emit` calls the pinned sink; `Warn::process` follows a later `set_sink` (late binding)
  and falls back to `tracing` with none set (assert no panic; sink untouched).
- `CacheWire` with a non-JSON StaticPrefix body emits one warning through its `Warn`.
- claude-subscription: the self-check path (existing fake `claude_cli_double` test in
  `tests/claude_subscription.rs`) delivers the warning to a pinned sink; `Scratch` drop failure
  delivers one.
- `token_source` refresh-save failure still reaches the sink (existing test moved to the new
  module path).
- Integration: a session built with a recording `Approver` receives the provider warning via
  `Approver::warn` (the process-wide sink is installed by `prepare`; the test pins it by
  running in its own test binary, as `guardian_cache_notice.rs` does).

## Open Questions

None.
