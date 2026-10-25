# Typed Runtime Errors and Full Cause Chains Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/185

## Overview

`AGENTS.md` requires errors to carry context as a `source()` chain and to be rendered once, with
`fontaine_tools::error_chain::chain`, where they reach a human. `AgentRuntime`'s public API,
`accounts.rs`, `transport.rs`, the compaction warnings, the TUI slash-command handlers and several
loaders do the opposite: they flatten the cause into a `String` (or print `{e}`), so TLS, proxy and
DNS reasons vanish from user-visible failures and no caller can inspect what failed. This plan
gives the runtime one error type that keeps its cause, renders chains at the human edges, and
makes `HookError` and the scanner loaders keep their sources. No permission decision changes.

## Current State

- `fontaine-core/src/agent.rs`: `AgentRuntime::new`, `rebuild`, `set_output_style`,
  `expand_skill`, `approve_mcp_server`, `add_dir`, `switcher`, `guardian_for`,
  `switch_main_model`, `install_main`, `set_guardian_model`, `retry_guardian`, `use_guardian`,
  the account-listing method (`:1066`), `build_screen` (`:167`), `build_agent` and the helper at
  `:186` return `Result<_, String>`. `guardian_for` does `.map_err(|e| e.to_string())` and `persist`
  returns `Option<String>` built with `{e}`.
- `accounts.rs:178`–`:208` (`check_name`, `check_name_in`, `check_base_url`, `build`) and
  `transport.rs:125`/`:132` (`caused_client`, `reqwest_client`) return `Result<_, String>`.
  `reqwest_client` already renders the chain once, into the string, at the point of failure.
- Callers: `fontaine/src/lib.rs:642` (headless: `eprintln!("fontaine: {message}")`),
  `fontaine-tui/src/lib.rs:183` (`?` into a `String` error), the `fontaine-tui/src/ui/commands/*`
  slash handlers, `ui/mcp.rs:43`, `ui/turn.rs:112`, `provider_dialog.rs:370`, providers
  (`caused_client(ctx.network)?` in four provider modules, `device_flow.rs:137`,
  `opencode_go.rs:336` mapping into `FlowError::Invalid(String)`), `fontaine-evals/src/live.rs:168`
  (`build_screen`) and tests.
- Human-facing `{e}` sites that drop the cause: `session.rs:407`, `:421`, `:428`, `:437`, `:448`;
  `agent.rs:488`; `permissions.rs:1470`–`:1478` (the rule-save notice); TUI `lib.rs`/`ui/*`
  sites listed by `rg 'format!\("[^"]*\{e\}' rust/fontaine-tui/src`.
- `HookError { Failed(String), Malformed(String) }` (`permissions.rs:163`) is built in
  `user_hooks.rs:88`, `:91`, `:101`, `:165` from `io::Error` / `serde_json::Error` rendered with `{e}`.
- `fontaine-guardian/src/scanner.rs:204`–`:249` and `scanner/pg2.rs:95`–`:213` build `String`
  errors from `io`, `serde_json`, tokenizer and candle errors.
- `scanner_stage.rs:229` writes the PromptGuard download notice with `eprintln!`, deliberately
  before the TUI takes the terminal so a slow first start does not look hung.
- Neighbouring precedent: `AccountsError::{Io, CredentialKept}` (`accounts.rs:73`) already holds
  `source: Box<dyn Error + Send + Sync>`; `ModelError`, `ConfigError`, `FetchError` exist.

## Design

### `RuntimeError`

New module `fontaine-core/src/runtime_error.rs`, re-exported as `fontaine_core::RuntimeError`:

```rust
pub struct RuntimeError {
    context: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}
impl RuntimeError {
    pub fn new(context: impl Into<String>) -> Self;                 // no cause
    pub fn caused(context: impl Into<String>, source: impl Error + Send + Sync + 'static) -> Self;
}
// Display = context; Error::source = the cause.
```

A context-plus-`source()` struct, not an enum: nothing today dispatches on the failure kind (every
caller prints it), and the issue allows a struct "otherwise". A kind can be added when a caller
needs one. Display is only the context, so `chain(&e)` shows `context: cause: …` once.

`impl From<ConfigError>`, `From<ModelError>` and `From<AccountsError>` are not added blindly; sites
that today do `map_err(|e| e.to_string())` become `RuntimeError::caused("…", e)` with a context
that does not repeat the cause (`chain` also de-duplicates a source already contained in the text).

### Signature changes

- All `Result<_, String>` in `agent.rs` (list above) → `Result<_, RuntimeError>`; `persist`
  returns `Option<RuntimeError>`, and `Switched::persist_error` becomes `Option<RuntimeError>` and is
  rendered by the TUI with `chain`. If other public structs carry the same strings
  (`rg 'persist_error'`), convert them the same way.
- `accounts::{check_name, check_name_in, check_base_url, build}` → `Result<_, RuntimeError>`.
  `check_name_in` feeds `provider_dialog.rs:370`, which only displays text → `.err().map(|e| chain(&e))`
  (no source there, so identical text).
- `transport::{caused_client, reqwest_client}` → `Result<_, RuntimeError>` whose cause is the
  `tls::build_client` error (`without_url()` applied as today); the context is
  "could not build the HTTP client". Providers using `?` into `String` get `RuntimeError`'s
  conversion at their own boundary: where the provider trait's error type is `String`, wrap with
  `chain(&e)` at that edge (providers are not human edges but their trait is outside this issue's
  scope; record in Decisions). `FlowError::Invalid(String)` in `opencode_go.rs:336` becomes
  `FlowError::Invalid(String)` fed with `chain(&e)` — unchanged type, but the cause now survives.

### Rendering at the human edges

Replace `{e}` with `fontaine_tools::error_chain::chain(&e)` at: `session.rs` (five compaction
warnings; the `model.call` error must be rendered with its full chain, which is the acceptance
test), `agent.rs:488`, `permissions.rs:1478` (and make the `:1470` mapping keep the `io::Error`
as a typed value rather than formatting `path: e` early — render once at the notice), the TUI
slash-command handlers (`ui/commands/model.rs`, `workspace.rs`, `appearance.rs`, `ui/mod.rs:291`,
`ui/mcp.rs:43`, `lib.rs` sites) and `fontaine/src/lib.rs:642`. Sites that wrap a third-party error
without a `source()` keep `{e}` with a one-line comment saying so (acceptance criterion 3).

### `HookError`

```rust
pub enum HookError {
    Failed { context: &'static str, source: Box<dyn Error + Send + Sync> },
    Malformed { context: &'static str, source: Box<dyn Error + Send + Sync> },
}
```

Typed because `permissions.rs` dispatches on the two kinds (contributes no decision either way;
this stays so). Implements `Display` (context) and `Error::source`. `user_hooks.rs` keeps the
`io::Error`/`serde_json::Error`. Wherever a `HookError` is rendered for a human (`rg HookError`
in `permissions.rs`), use `chain`. The hook outcome stays "no decision": no permission behaviour changes.

### Scanner loaders (`fontaine-guardian`)

Keep the crate dependency-light: add a small private-to-crate `LoadError { context: String,
source: Option<Box<dyn Error + Send + Sync>> }` (no new dependency) in `scanner.rs`, used by the
`pg2` `Loaded::open` path and the command scanner's internal `failed(...)` inputs so
`io`/`serde_json`/tokenizer causes are kept. Where the scanner turns a failure into a fail-closed
verdict/`Unavailable` cause, the text is `chain(&e)`-style rendered by a local helper (the guardian crate
cannot depend on `fontaine-tools`). Fail-closed behaviour and every Deny stay exactly as they are; error
text only explains.

### PromptGuard download notice

`scanner_stage.rs:229` keeps `eprintln!`: it must appear before the TUI takes the terminal (a
returned notice would show only after the slow download it announces). Add a comment saying so
and record a Decision. `scanner_stage.rs` failure causes (`:238`+) already use `chain`; the
remaining `e.to_string()` of `FetchError` (`:240`) becomes `chain(&e)` after `FetchError::Failed`
keeps a source (`scanner_model.rs:95`, `:148` build `Failed(format!("… {e}"))` from `io::Error`): change
`FetchError::Failed` to `{ context: String, source: Option<Box<dyn Error + Send + Sync>> }`.

## Implementation Steps

1. Add `runtime_error.rs` with unit tests (Display, `source()`, `chain` output); export it.
2. `transport.rs` and `accounts.rs`: new signatures; fix providers, `device_flow`, `opencode_go`,
   `fontaine-evals` and tests that call them.
3. `agent.rs`: convert every listed signature, `persist`/`Switched::persist_error`, `guardian_for`;
   render `:488` with `chain`.
4. `HookError` + `user_hooks.rs` + `permissions.rs:1470`–`:1478`; update `tests/hook_slot.rs`,
   `permissions_auto.rs`, `permissions_screen.rs` constructors.
5. `session.rs` compaction warnings with `chain`.
6. `FetchError::Failed` source; `scanner_stage.rs` comment and `chain`.
7. `fontaine-guardian` `LoadError` for scanner.rs / pg2.rs.
8. TUI: update every slash handler, `lib.rs`, `mcp.rs`, `turn.rs`, `provider_dialog.rs`; headless `fontaine/src/lib.rs`.
9. Sweep: `rg 'format!\("[^"]*\{e\}' rust/fontaine-core/src rust/fontaine-tui/src` — fix or comment each remaining site.
10. Docs and CHANGELOG entry; `cargo fmt`, clippy (`-D warnings`, also the Windows target), tests.

## Files to Modify

`fontaine-core/src/{runtime_error.rs (new), lib.rs, agent.rs, accounts.rs, transport.rs, session.rs,
permissions.rs, user_hooks.rs, scanner_stage.rs, scanner_model.rs, providers/*}`,
`fontaine-guardian/src/{scanner.rs, scanner/pg2.rs}`, `fontaine-tui/src/{lib.rs, provider_dialog.rs,
ui/mod.rs, ui/mcp.rs, ui/turn.rs, ui/commands/*}`, `fontaine/src/lib.rs`, `fontaine-evals/src/live.rs`,
plus the affected tests under `fontaine-core/tests/` and `CHANGELOG.md`.

## Trade-offs

- **Enum vs struct for `RuntimeError`**: an enum of kinds with no dispatching caller is speculative; a
  context + `source()` struct matches the `AGENTS.md` rule and is trivially extended.
- **`thiserror`/`anyhow`**: not adopted; the codebase uses hand-written `Display`/`Error` impls
  (`AccountsError`), so this follows them.
- **Notice routing for the download message**: routing through notices loses its purpose (see
  Decisions), so stderr stays.

## Decisions

- Struct, not enum, for `RuntimeError`: no caller dispatches on a kind today.
- The PromptGuard download notice stays on stderr: it is shown before the TUI starts, which a
  returned notice cannot do.
- Model-facing `ToolExecutionError` text and the guardian crate's `Deny` explanations are untouched;
  error text never changes a Deny.
- Provider trait signatures that take a `String` error stay; the cause is rendered with `chain` at that edge.

## Documentation

No `docs/` page describes these internals. `CHANGELOG.md` gets an entry (user-visible: failures now
show their full cause). Check `docs/` for any page quoting a changed message (`rg` the old texts).

## Testing Strategy

- Unit: `RuntimeError` display/source/chain (in `runtime_error.rs`); `HookError` keeps its source
  (`user_hooks` test with a failing command and malformed stdout, asserting `source()` is the
  `io`/`serde_json` error); `FetchError::Failed` and the guardian `LoadError` keep sources.
- Regression for the dropped cause: a compaction test (in `fontaine-core/tests/sessions.rs`, using
  `wiremock` for a refused connection, or a deterministic model whose error has a nested source)
  asserts the compaction warning contains the inner cause.
- A test that `AgentRuntime::new` with an unbuildable network config (bad CA bundle) returns an
  error whose `chain` contains the TLS cause.
- A test that the rule-save failure notice (`permissions.rs`) contains the `io::Error` cause.
- Existing permission tests stay green unchanged, proving no decision changed; the choke-point test stays green.
- Acceptance sweep: `rg 'Result<[^>]*, String>' fontaine-core/src/agent.rs` has no hit in public methods.

## Open Questions

- Do any `AgentRuntime` callers outside this repo's crates (e.g. `fontaine-evals`) depend on the `String`
  error beyond `Display`? Expected no; the compiler settles it.
