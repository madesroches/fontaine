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
  `switch_main_model`, `install_main`, `set_guardian_model`, `retry_guardian`, `use_guardian` (`:1236`, which also builds `Switched`),
  `model_listings` (`:1066`), `build_screen` (`:167`) and `build_agent` return `Result<_, String>`. `guardian_for` does `.map_err(|e| e.to_string())` and `persist`
  returns `Option<String>` built with `{e}`.
- `accounts.rs:178`–`:208` (`check_name`, `check_name_in`, `build`, and the private `check_base_url` at `:199`) and
  `transport.rs:125`/`:132` (`caused_client`, `reqwest_client`) return `Result<_, String>`.
  `reqwest_client` already renders the chain once, into the string, at the point of failure.
- Callers: `fontaine/src/lib.rs:642` (headless: `eprintln!("fontaine: {message}")`),
  `fontaine-tui/src/lib.rs:183` (`?` into a `String` error), the `fontaine-tui/src/ui/commands/*`
  slash handlers, `ui/mcp.rs:43`, `ui/turn.rs:112`, `provider_dialog.rs:370`, providers
  (`caused_client(...)?` in `anthropic.rs:34`, `openai.rs:34`, `gemini.rs:34`,
  `openai_compatible.rs:47`, `ollama.rs:57`, `opencode_go.rs:115`, in a function whose other errors (`:93`, `:111`) are also `String`; `Provider::connect`
  (`providers/mod.rs:52`) returns `Result<_, String>`; `reqwest_client` also called from
  `device_flow.rs:137` and `agent.rs:208`; `opencode_go.rs:336` maps into `FlowError::Invalid(String)`),
  `fontaine-evals/src/live.rs:168` (`build_screen`) and tests. Only `live.rs:168` and
  `fontaine-tui/src/lib.rs:183` use the `String` beyond `Display`; there are no external callers.
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

Display is only the context, so `chain(&e)` shows `context: cause: …` once.

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
  "could not build the HTTP client". There is no `From<RuntimeError> for String`: every provider site
  (the six `caused_client(...)?` calls above, and `device_flow.rs:137`) uses an explicit
  `.map_err(|e| chain(&e))` because `Provider::connect` returns `Result<_, String>`.
  `opencode_go.rs:336` becomes `|e| FlowError::Invalid(chain(&e))`: unchanged type, fed with `chain(&e)`.
  `agent.rs:208` propagates `RuntimeError`. `fontaine-evals/src/live.rs:168` and
  `fontaine-tui/src/lib.rs:183` render with `chain` at their own edge.
- `fontaine-tui/src/ui/commands/model.rs:144` `finish_switch(message, persist_error: Option<String>)`
  (callers `:21`, `:76`, `:197`) takes `Option<RuntimeError>` and renders with `chain`;
  `appearance.rs:43-53` already uses `chain`.

### Rendering at the human edges

Replace `{e}` with `fontaine_tools::error_chain::chain(&e)` at: `session.rs` (five compaction
warnings; `:421` and `:437` have no source, so only `:407`, `:428`, `:448` change output, but all five
use `chain` for consistency; the `model.call` error must be rendered with its full chain, which is the acceptance
test), `agent.rs:488`, `permissions.rs:1478` (and make the `:1470` mapping keep the `io::Error`
as a typed value rather than formatting `path: e` early — the rule-save error becomes
`RuntimeError::caused("could not write <path>", io_err)`, rendered once with `chain` at the notice so
the settings-file path is kept; the `None` arm is left as-is), the TUI
slash-command handlers (`ui/commands/model.rs`, `workspace.rs`, `appearance.rs`, `ui/mod.rs:288-291` (the consumer of `expand_serving`, chain at the print site),
`ui/mcp.rs:43`, `lib.rs` sites) and `fontaine/src/lib.rs:642`. Sites that wrap a third-party error
without a `source()` keep `{e}` with a one-line comment saying so (acceptance criterion 3).

### `HookError`

```rust
pub enum HookError {
    Failed { context: &'static str, source: Option<Box<dyn Error + Send + Sync>> },
    Malformed { context: &'static str, source: Option<Box<dyn Error + Send + Sync>> },
}
```

`source` is optional: seven constructors in `user_hooks.rs` (`:167`, `:173`, `:180`, `:260`, `:275`,
`:339`, `:352`) have no underlying cause and use a context-only constructor; `:88`, `:91`, `:101`,
`:165` keep the `io::Error`/`serde_json::Error`. Implements `Display` (context) and `Error::source`.
Nothing dispatches on the kinds; the benefit is that the `tracing::warn!(error = ?e)` sites
(`permissions.rs:495-497`, `:978-979`, `user_hooks.rs:374`, `:435`, `:454`) now carry the source in
their Debug output. The hook outcome stays "no decision": no permission behaviour changes.

### Scanner loaders (`fontaine-guardian`)

Scanner errors stay `String` (`Scan::Failed { reason }`, `Pg2Scanner::load -> Result<_, String>`,
public API). At each point of failure in `scanner.rs` and `scanner/pg2.rs`, render the `io`/
`serde_json`/tokenizer/candle cause into the string, as `transport::reqwest_client` does, reusing the
crate's existing chain helper. It is private at `screen.rs:392` and `mod screen` is private (scanner and
`pg2` are sibling modules, `pg2` feature-gated), so it moves to a small ungated `pub(crate)` module
(e.g. `error_text`) used by all three; the guardian crate cannot depend on `fontaine-tools`. No new type, no signature change. Fail-closed behaviour and every
Deny stay exactly as they are; error text only explains.

### PromptGuard download notice

`scanner_stage.rs:229` keeps `eprintln!` (see Decisions); add a comment saying why. Failure causes
(`:238`+) already use `chain`; the remaining `e.to_string()` of `FetchError` (`:240`) becomes
`chain(&e)` once `FetchError` implements `Error` (today it has only `Display`, `scanner_model.rs:33`)
and `FetchError::Failed` keeps a source: change it to
`{ context: String, source: Option<Box<dyn Error + Send + Sync>> }`.

## Implementation Steps

1. Add `runtime_error.rs` with unit tests (Display, `source()`, `chain` output); export it.
2. `transport.rs` and `accounts.rs`: new signatures; fix providers, `device_flow`, `opencode_go`,
   `fontaine-evals` and tests that call them.
3. `agent.rs`: convert every listed signature, `persist`/`Switched::persist_error`, `guardian_for`;
   render `:488` with `chain`.
4. `HookError` + `user_hooks.rs`; update the constructor in `tests/hook_slot.rs:47-48`
   (`permissions_auto.rs` and `permissions_screen.rs` only name the type).
5. `session.rs` compaction warnings with `chain`; `permissions.rs:1470`–`:1478` rule-save notice.
6. `FetchError`: implement `Error`, `Failed` keeps a source (constructors at `scanner_model.rs:95`,
   `:102` already renders the chain into the text, so it keeps the typed source with `without_url()` instead, `:112`, `:132`, `:144`, `:148`, `:162`); update `tests/scanner_model.rs:180` and `:228` to
   `FetchError::Failed { .. }`; `scanner_stage.rs` comment and `chain`.
7. `fontaine-guardian`: render causes into the `String` at the failure points in scanner.rs / pg2.rs
   with the shared `pub(crate)` chain helper (moved out of `screen.rs` into an ungated module).
8. TUI: `expand_serving` (`ui/turn.rs:106-112`, inner `Result<String, String>` → `RuntimeError`) and its
   consumer `ui/mod.rs:288-291`; update every slash handler (`model.rs` `finish_switch` takes `Option<RuntimeError>`,
   `appearance.rs`), `lib.rs`, `mcp.rs`, `turn.rs`, `provider_dialog.rs`; headless `fontaine/src/lib.rs`.
9. Sweep: `rg -U 'format!\("[^"]*\{e\}' rust/fontaine-core/src rust/fontaine-tui/src` plus
   `rg -n -e '\{e\}'` on the same dirs, then a manual pass: the single-line form misses multi-line
   `format!(` calls (`session.rs:446-448`, `mcp.rs:42-45`). Fix or comment each remaining site.
10. Docs and CHANGELOG entry; `cargo fmt`, clippy (`-D warnings`, also the Windows target), tests.

## Files to Modify

`fontaine-core/src/{runtime_error.rs (new), lib.rs, agent.rs, accounts.rs, transport.rs, session.rs,
permissions.rs, user_hooks.rs, scanner_stage.rs, scanner_model.rs, providers/*}`,
`fontaine-guardian/src/{scanner.rs, scanner/pg2.rs}`, `fontaine-tui/src/{lib.rs, provider_dialog.rs,
ui/mod.rs, ui/mcp.rs, ui/turn.rs, ui/commands/*}`, `fontaine/src/lib.rs`, `fontaine-evals/src/live.rs`,
plus the affected tests under `fontaine-core/tests/` and `CHANGELOG.md`.

`RuntimeError` implements `Debug` (derive), so tests that call changed functions and only `unwrap`/
`expect`/`assert!(.is_err())` compile unchanged (`account_ops.rs`, `network.rs`, `config.rs:1200`,
`account_session.rs:219`, `extensions.rs:545`, `output_styles.rs`, `common/runtime.rs:122`); only
tests that compare the error to a `String`/`&str` need edits.

`agent.rs:253` `main_error: Option<String>` and `:332` `error: Option<String>` are in scope only as
rendered text: convert them only if they originate from a `RuntimeError`, rendering with `chain` at
the assignment.

## Trade-offs

- **`thiserror`/`anyhow`**: not adopted; the codebase uses hand-written `Display`/`Error` impls
  (`AccountsError`), so this follows them.

## Decisions

- Struct, not enum, for `RuntimeError`: no caller dispatches on a kind today.
- The PromptGuard download notice stays on stderr: it is shown before the TUI starts, which a
  returned notice cannot do.
- Scanner errors stay `String` in the guardian crate; causes are rendered into the text at the failure point (no new error type).
- `HookError.source` is optional: some failures have no underlying cause.
- Model-facing `ToolExecutionError` text and the guardian crate's `Deny` explanations are untouched;
  error text never changes a Deny.
- Rule-save failure is a `RuntimeError::caused("could not write <path>", io_err)` rendered once at the notice, so the path survives.
- Provider trait signatures that take a `String` error stay; the cause is rendered with `chain` at that edge.

## Documentation

No `docs/` page describes these internals. `CHANGELOG.md` gets an entry (user-visible: failures now
show their full cause). Check `docs/` for any page quoting a changed message (`rg` the old texts).

## Testing Strategy

- Unit: `RuntimeError` display/source/chain (in `runtime_error.rs`); `HookError` keeps its source
  (`user_hooks` test with a failing command and malformed stdout, asserting `source()` is the
  `io`/`serde_json` error); `FetchError::Failed` keeps its source; a scanner load failure's string contains the inner cause.
- Regression for the dropped cause: a compaction test (in `fontaine-core/tests/ledger.rs`, via `compact` `:93` /
  `compact_if_needed` `:129`, extending the existing tests that use the `Collect` approver, `:213` and `:486`; rig's
  `MockTurn`/`MockError` cannot produce a nested source, so the test uses a small hand-written
  `CompletionModel` whose error wraps a source) asserts the compaction warning contains the inner cause.
- A test of `caused_client`/`reqwest_client` directly with a crafted `LoadedNetwork` (as
  `tests/network.rs` does) asserting the TLS cause is in the `chain`.
- A test that the rule-save failure notice (`permissions.rs`) contains the `io::Error` cause; it must
  force an io error, e.g. the `always_allow_file` rig in `tests/common/pipeline.rs` with a settings
  path under a regular file.
- Existing permission tests stay green unchanged, proving no decision changed; the choke-point test stays green.
- Acceptance sweep: `rg -n ', String>' rust/fontaine-core/src/agent.rs` has no `Result` hit (the
  `Result<[^>]*, String>` form misses `build_screen` and `model_listings`, whose types nest `>`).
