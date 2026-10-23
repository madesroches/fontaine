# Turn Transport Cause and Error Handling Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/88

## Overview

When a model turn fails to connect, the error says the request failed but not why (refused,
reset, timed out, TLS, DNS). This plan makes every model request (turn, guardian, scanner,
listing) carry its transport cause, then fixes the general problems a review of the workspace's
error handling found: causes dropped before they reach the user, write failures that only go to
`tracing`, one latent fail-open path, and misleading wrapper messages. It also writes the error
handling strategy into `AGENTS.md`, so new code follows one convention.

## Current State

### The turn failure (#88)

- `fontaine-core/src/models.rs` `Client::build` builds each account's provider client with
  `config.client()`, rig's process-wide reqwest client.
- Turns stream. A transport failure surfaces from rig as `PromptError::Report(ErrorReport)`.
  rig builds that report with `ErrorReport::from(&ProviderError)` (`rig-core-0.45.0/src/error.rs:719`):
  `message` is the `ProviderError` Display and `source_chain` comes from `ProviderError::source`.
  `source` returns `None` for `Http` (`error.rs:407`), so the chain is empty.
- The cause is still there one layer down. `rig_http::http_client::Error::Instance` is a
  thiserror `#[from]` around the boxed `reqwest::Error`, whose `source()` chain reaches hyper and
  the `io::Error` ("Connection refused"). rig cuts the chain one level above it, at `ProviderError`.
- `turn.rs:256` turns the failure into `TurnOutcome::Failed(other.to_string())`. Headless
  (`fontaine/src/headless.rs:462, 667`), the TUI transcript (`fontaine-tui/src/lib.rs:930`) and
  subagent reports (`child_runner.rs:401`) all show that string.
- rig's `HttpMiddleware` (`rig-http-0.45.0/src/http_client/middleware.rs`) has no error hook.
  Provider configs take any HTTP client through `connect(impl HttpClientExt + 'static)`
  (`rig-core-0.45.0/src/client/macros.rs:32`), so a wrapping client is the only seam.
- The open PR #87 (`accounts-listing-error-chain`, not merged) adds two things in `models.rs`:
  a per-account `reqwest::Client` (`own_http_client`, using the `rig-reqwest` crate) and a
  `with_causes(&ProviderError)` walk used only by listing.

### Error handling in general

The workspace has no `anyhow` and no `thiserror`. It has 13 hand-written error enums and about
77 functions returning `Result<_, String>`. Panics are already rare: no `unwrap()` outside tests,
and the five `expect`s have messages. The guardian is fail-closed on every non-verdict path.
The problems are:

1. **Causes lost on the way to the user.**
   - The top-level exits (`fontaine/src/lib.rs`, about 19 `eprintln!("fontaine: {e}")`) print
     Display only.
   - Guardian and scanner fail-closed explanations (`fontaine-guardian/src/guardian.rs:223-237`,
     `scanner.rs:124-126`) drop the error, which goes to `tracing::warn!` and is invisible
     without `--debug`.
   - MCP connect (`fontaine-mcp/src/servers.rs:168-171, 223, 230`) flattens with `e.to_string()`.
   - `SessionError` holds an `io::Error` without implementing `source()` (`session.rs:52`).
   - `webfetch::describe` (`fontaine-tools/src/webfetch.rs:181`) is the only chain walk in the
     workspace.
2. **Misleading wrapper messages.**
   - `ConfigError::Io` always says "cannot read" (`config.rs:98`), but `fontaine auth login`
     (`credentials.rs:113`) and `trust.rs` writes use it too.
   - `SessionError::Io` always says "cannot create" (`session.rs:44`), but resume uses it to
     report a corrupt ledger.
3. **Write failures only in `tracing`.**
   - Audit log append (`permissions.rs:1337`).
   - Ledger appends (`gate.rs:576`, `child_runner.rs:99`, `steering.rs:80`). `agent.rs:1014`
     already warns through `Approver::warn`.
   - Compaction warnings use `eprintln!` from core (`session.rs:237-240`), which corrupts the
     TUI screen (`Approver::warn` docs, `permissions.rs:79`).
4. **Latent fail-open.** `fontaine-tools/src/guarded.rs:125` `agent_of` gates a call as the root
   agent when the `AgentId` slot is missing or doesn't decode. That would drop a child's own
   mode and restrictions. It can't happen today only because `turn.rs:149` always inserts the
   slot, and that insert's result is ignored with `let _ =`.
5. **Silent fallbacks.**
   - `validated_models` (`models.rs`) warns and returns an empty list if its compile-time TOML
     doesn't parse.
   - `parse_rule(..).ok()` at `child_runner.rs:144,152` and `extensions.rs:224` silently drops
     entries that were validated at load.

## Design

### 1. A transport client that keeps the cause in its message

Add `fontaine-core/src/transport.rs` with a `CausedClient(ReqwestClient)` that implements
`rig_core::http_client::HttpClientExt` by delegating to the inner client. On the way out it
rewrites any `Error::Instance(inner)` as `Error::Instance(Box::new(WithCauses(inner)))`.
`WithCauses`:

- Display: the inner error's message followed by each `source()` link whose text isn't already
  shown, joined with `: `. This is the chain helper below.
- `source()`: the inner error's `source()`. Nothing in rig downcasts an `Instance` box, and the
  variant stays `Instance`, so rig's retry classification (`transient_transport`) is unchanged.

Errors are rewritten in all three `HttpClientExt` methods: the `send` and `send_multipart`
futures, the `LazyBody` future each returns, and both the `send_streaming` future and every
chunk of its body stream (a reset mid-stream fails in a chunk, not in the send). The cause then
becomes part of the `ProviderError::Http` Display. That Display is the `ErrorReport.message`
rig relays, so the cause survives into `PromptError::Report`, guardian and scanner failures,
listing, and every surface that shows those strings. Nothing on rig's side changes.

`Client::build` constructs every account client with `config.connect(caused_client())`, where
`caused_client()` wraps a per-account `reqwest::Client::new()`. It uses the same per-account
client #87 introduces, for the same reason: no pooled connection outlives the runtime that made
it. The Decisions section covers how the two PRs merge.

```
reqwest error ─▶ CausedClient ─▶ Instance(WithCauses) ─▶ ProviderError::Http ─▶ ErrorReport.message
                 (adds ": client error (Connect): tcp connect error: Connection refused (os error 111)")
```

### 2. One chain helper

Add `fontaine-tools/src/error_chain.rs` with `pub fn chain(error: &(dyn Error + 'static)) -> String`:
the error's Display, then each `source()` link whose text isn't already in the string. This is
#87's dedupe rule, needed because wrappers such as `ConfigError` repeat their source.
`fontaine-tools` is the lowest crate that both `fontaine-core` and the tools depend on.
`fontaine-guardian` stays free of it: its errors already carry the cause in their Display, via
design 1.

- `webfetch::describe` becomes `chain(&error.without_url())`.
- `WithCauses` (design 1) uses it.
- The listing site in `models.rs` drops any special-case walk: `{e}` already carries the cause
  through the caused client.

### 3. Render the chain where errors reach the user

- `fontaine/src/lib.rs` exit paths: print `chain(&e)` instead of `{e}`.
- `turn.rs:256` keeps `other.to_string()`. With design 1 the string already names the cause,
  and an `ErrorReport` has no `source()` to walk.
- Guardian and scanner fail-closed explanations append the error:
  `"the guardian model could not be reached: {error}"`, and likewise for an unusable verdict
  and the scanner. The outcome is still `FailClosed` (Deny). Only the explanation text changes.
- MCP connect: replace `e.to_string()` with `chain(&e)` (`servers.rs:168-171, 223, 230`).
- Add `SessionError::source()`, so `chain` reaches the io error.

### 4. Messages that say what failed

- Split `ConfigError::Io` into `Read { path, source }` and `Write { path, source }`. Write
  call sites (`credentials.rs`, `trust.rs` record/approve) use `Write`. Exit codes are unchanged.
- `SessionError::Io` gains the operation: `Create` for new sessions, `Open` for resume. A
  corrupt ledger then reads "cannot open session <dir>: ...".

### 5. Write failures reach the human once

- The audit append (`permissions.rs:1337`) and the ledger appends (`gate.rs:576`,
  `child_runner.rs:99`, `steering.rs:80`) warn through `Approver::warn`, as `agent.rs:1014` does.
- To avoid a warning per decision on a full disk, the session warns once per sink (audit,
  ledger) using a `std::sync::Once`-style flag on `SessionState`. It keeps logging to `tracing`
  every time.
- Compaction's `warn` (`session.rs:237`) goes through the approver instead of `eprintln!`. Its
  callers (TUI and headless) already hold one.

### 6. Close the latent fail-open

- `guarded.rs` `agent_of` returns `Result<String, ToolExecutionError>`. It uses
  `context.require::<AgentId>()` (or `get` with `None` treated as an error), and a missing or
  undecodable id fails the call before it is gated. This keeps "a child's mode is never looser
  than its parent's" true without depending on wiring elsewhere.
- `turn.rs:149` handles the `insert` result. Failing to tag the context is a bug, so it
  `expect`s with a message.

### 7. No silent fallbacks for invariants

- `validated_models`: `expect("the shipped validated-models table parses")`, plus a unit test
  that parses it.
- `parse_rule(..).ok()` at the three sites: `expect("validated when the definition was loaded")`.

### 8. Error handling strategy in AGENTS.md

Add an `## Error handling` section to `AGENTS.md` (the user asked for it). It draws on the
micromegas conventions and the rule the user stated:

- Typed error enums (thiserror-style) only where a caller dispatches on the kind: exit codes,
  retry, fail-closed classification. Everywhere else, carry context as a chain (anyhow-style
  context, or a `source()`), not a flattened `String`.
- Keep the cause as `source()`. Never `format!("{e}")` an error into another error's message.
  Render the full chain once, where the error reaches a human, with `error_chain::chain`.
- `expect("why this cannot fail")` instead of `unwrap()` outside tests. Invariants panic loudly
  instead of falling back silently.
- Don't swallow errors. A failure the user would care about (audit, ledger, config, credentials)
  reaches them through `Approver::warn` at least once. `let _ =` is only for best-effort cleanup
  and sends to closed channels.
- Security paths stay fail-closed: the error text may explain a Deny, never change it.
- `fontaine-guardian` stays dependency-light. It doesn't take the helper crate.

## Implementation Steps

1. **Transport cause (#88).**
   - Add the `rig-reqwest` workspace dependency (`=0.45.0`) and make `reqwest` non-optional in
     `fontaine-core`.
   - Add `fontaine-tools/src/error_chain.rs` with unit tests: dedupe, a multi-level chain, an
     error with no source.
   - Add `fontaine-core/src/transport.rs` (`CausedClient`, `WithCauses`) and switch
     `Client::build` to it.
   - Rewrite `webfetch::describe` in terms of `chain`.
2. **Rendering.**
   - Top-level `chain` in `fontaine/src/lib.rs`.
   - Guardian and scanner explanations with the cause.
   - MCP connect with `chain`.
   - `SessionError::source()`.
3. **Messages.** `ConfigError::Read`/`Write` split, `SessionError` operation, call-site updates.
4. **Surfacing.** Warn-once audit and ledger failures through the approver; route compaction
   warnings through the approver.
5. **Invariants.**
   - `agent_of` fails closed.
   - `turn.rs:149` `expect`.
   - `validated_models` and `parse_rule` `expect`s.
6. **Docs.**
   - `AGENTS.md` `## Error handling`.
   - `docs/` page(s) that describe the guardian's fail-closed explanation, if they quote its text.
   - `CHANGELOG.md`.

## Files to Modify

- `rust/Cargo.toml`, `rust/fontaine-core/Cargo.toml`
- `rust/fontaine-tools/src/error_chain.rs` (new), `rust/fontaine-tools/src/lib.rs`, `rust/fontaine-tools/src/webfetch.rs`, `rust/fontaine-tools/src/guarded.rs`
- `rust/fontaine-core/src/transport.rs` (new), `models.rs`, `turn.rs`, `session.rs`, `config.rs`, `credentials.rs`, `trust.rs`, `permissions.rs`, `gate.rs`, `child_runner.rs`, `steering.rs`, `extensions.rs`, `lib.rs`
- `rust/fontaine-guardian/src/guardian.rs`, `scanner.rs`
- `rust/fontaine-mcp/src/servers.rs`
- `rust/fontaine/src/lib.rs`
- `AGENTS.md`, `docs/` (as needed), `CHANGELOG.md`

## Trade-offs

- **Wrapping client vs. waiting for a rig change.** An upstream fix to `ProviderError::source`
  is the clean answer, but rig is pinned exactly and the fix would also need `ErrorReport` to
  keep a source. The wrapper fixes every model call today without forking rig. If rig later
  keeps the chain, `CausedClient` can be deleted and nothing else changes.
- **Cause in Display vs. a side channel.** The issue also suggests recording the "last
  transport error" per client and attaching it to the turn failure. A shared slot races once
  the main agent, subagents and the guardian share an account's client. Putting the cause into
  the error's own Display needs no shared state.
- **In-house `chain` vs. adopting anyhow now.** Most existing errors are hand-written enums
  whose callers dispatch on `exit_code()`, and the 77 `String` errors work. Converting them all
  is churn with no user-visible gain. The strategy in AGENTS.md applies to new and touched code.

## Decisions

- 2026-10-22 AFK: this branch is based on `main`, not stacked on the open #87 branch. Stacking
  needs history-rewriting git commands, which this session's permission layer refused. #87's
  `with_causes` and listing change become redundant once every client is a `CausedClient`.
  Whichever of #87/#88 merges second resolves the `models.rs` and `Cargo.toml` overlap, which
  is the same per-account client and the same dependency line.
- 2026-10-22: the error-handling review is in scope (user request). Items from the review that
  are not in this plan are listed under Out of scope, so they aren't re-raised.

## Out of scope

- Converting the 77 `Result<_, String>` functions or the remaining enums to thiserror/anyhow.
  They change opportunistically under the new AGENTS.md rule.
- `guardian_default` / `candidates` turning a credentials load error into `None`
  (`models.rs:635, 662`), and `ChildContext::model_for`'s `$guardian_default` fallback
  (`child_runner.rs:171`). This is behavior Claude Code-parity work owns, with its own UX.
- The PromptGuard download (`scanner_model.rs:126`) and the OTLP exporter error texts.

## Documentation

- `AGENTS.md`: new `## Error handling` section (design 8).
- `docs/guardian.md`: update if it quotes the fail-closed explanation text.
- `CHANGELOG.md`: turn failures name their transport cause; error-message and warning
  improvements.

## Testing Strategy

- **Regression for #88 (integration, headless):** in `fontaine/tests/headless_smoke.rs`,
  `a_turn_that_cannot_connect_names_the_cause`. Point a `Sandbox` at a closed port with
  `models = [...]` so listing is skipped, run `fontaine -p hi`, and assert exit 1 and that
  stderr contains `tcp connect error`. This mirrors `a_listing_that_cannot_connect_names_the_cause`
  and uses hyper's platform-neutral text, not the OS message.
- **Unit:** `error_chain::chain` (dedupe, depth, no source). `WithCauses` Display/`source`.
  `CausedClient` rewrites `Instance` errors from `send` and from a failing stream chunk. Use a
  fake `HttpClientExt` returning canned errors; no network.
- **Unit:** `ConfigError::Write` wording at a write site. `SessionError` open-vs-create wording.
  `SessionError::source()`.
- **Unit:** guardian explanation contains the error text and the outcome is still
  `FailClosed`/Deny, using rig's deterministic test model returning an error.
- **Unit:** `agent_of` with no `AgentId` slot fails the call (fail-closed regression for the
  security invariant).
- **Unit:** audit/ledger write failure warns through a recording approver exactly once across
  two failures.
- **Unit:** the shipped validated-models table parses.
- Existing suites stay green, including the choke-point test and the fixture eval runs.

## Open Questions

None.
