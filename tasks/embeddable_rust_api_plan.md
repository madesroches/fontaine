# Embeddable Rust API Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/15

## Overview

Let another Rust program embed the guarded Fontaine agent: build a session, register its own tools
and hooks, answer approvals through a callback trait, and consume an event stream that is the same
`headless-v1` stream-json the CLI prints. Programs in other languages already have two routes over
stdio: `headless-v1` and the Claude Agent SDK mode (`docs/agent-sdk.md`). This plan adds the Rust
route and documents all three as one embedding story.

## Current State

- `fontaine-core` holds the runtime pieces: `AgentSetup` / `AgentRuntime::new`
  (`rust/fontaine-core/src/agent.rs`), the `Approver` trait and `HumanChannel::{Present, Absent,
  Delegate}` (`permissions.rs`), `PreToolUseHook`, `CallbackHook`, `HostMcp`, `SessionEvent` and
  `run_turn` / `drive` (`turn.rs`). `Delegate` and callback hooks were built for the SDK mode.
- Session assembly is in the `fontaine` binary crate's `lib.rs`: `load_config`, `resolve_roots`,
  `open_session`, `prepare_session` (scratch, memory root, plugins, extensions), `resolve_models`,
  `start_scanner`. All of it takes a clap `Cli`, prints to stderr and returns `u8` exit codes, so an
  embedder cannot call it.
- The event stream is produced by `headless.rs`' `EventSink` trait; `HeadlessV1Sink` writes the
  `headless-v1` JSON (`docs/schemas/headless-v1.schema.json`). `EventSink`, `run_session` and
  `InputSource` are `pub(crate)`.
- `AgentSetup` has about 35 public fields, many of them frontend-internal (`notices`,
  `startup_warnings`, `warn_no_search`, `scheduling`), and no stability promise.
- Registering a host *tool* is not possible today: `HostMcp` (SDK-transport MCP servers) is the only
  host extension point for tools besides callback hooks.

## Design

### One crate for the SDK

The SDK is **one crate**, `rust/fontaine-sdk`, the only crate a Rust embedder names in `Cargo.toml`
or reads on docs.rs. Many crates on crates.io make the docs hard to follow, so the current
`fontaine-core`, `fontaine-tools` and `fontaine-agents` become **modules** of it:

- `fontaine_sdk::core`, `fontaine_sdk::tools`, `fontaine_sdk::agents` hold the existing source trees
  unchanged (`core/src/*` -> `sdk/src/core/*`, and so on). Module names collide across the three
  crates (`tasks`, `effort`, `schedule`, `checkpoint`, `tools`), which is why each tree keeps its
  own namespace instead of being flattened.
- The embedder-facing facade (`SessionBuilder`, `Session`, `Event`, `HostTool`, and curated
  re-exports of `Approver`, `PermissionMode`, `HookDecision` and the like) sits at the crate root
  and in a few top-level modules (`fontaine_sdk::permissions`, `::config`, ...), so the docs lead
  with the facade. The module trees stay `pub` for the binary, the TUI and the evals, but only the
  root facade is the documented, semver-stable surface; the rustdoc landing page says so, and the
  trees are `#[doc(hidden)]`-free so links in the facade's docs resolve.
- The merge is mechanical: `git mv` the trees, `fontaine_core::` -> `fontaine_sdk::core::` (and
  likewise) across the roughly 340 files that import them, and the three `Cargo.toml` dependency
  lists fold into one. It also rewrites `crate::` paths inside the moved trees and cross-tree
  `fontaine_tools::`/`fontaine_agents::` paths inside core to `crate::tools::`/`crate::agents::`;
  moves `fontaine-core/prompts/` next to the new source (`include_str!` in `prompt.rs`, `goal.rs`);
  renames the colliding integration tests (`choke_point.rs`, `lsp.rs`, `network.rs`, `common/`);
  fixes the `example_bin("fontaine-core"|"fontaine-tools", ...)` call sites and the `#[path]`
  includes of the other crates' tests and examples; and re-points the `pg2` feature from
  `fontaine/Cargo.toml` and `fontaine-evals/Cargo.toml`. It lands as its own commit, ahead of any
  behavior change, so the tests are the proof it changed nothing.
- `fontaine-guardian` stays its own crate: the repo requires it to be dependency-light and
  lift-out-able by other harnesses, and folding it in would end that. It is the SDK's one
  re-exported dependency (`fontaine_sdk::guardian`). An embedder still names one crate.
- Out of the SDK: `fontaine-tui`, `fontaine-remote`, `fontaine-evals`, `fontaine-testkit` and the
  `fontaine` binary. They depend on the SDK.

### Public surface

```rust
pub struct SessionBuilder { /* private */ }
impl SessionBuilder {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self;
    pub fn dirs(self, config_dir: PathBuf, data_dir: PathBuf) -> Self;   // default: dirs::resolve
    pub fn env(self, env: EnvLookup) -> Self;                             // default: process env
    pub fn mode(self, mode: PermissionMode) -> Self;                      // default: [permissions] default_mode, then Manual
    pub fn model(self, reference: &str) -> Self;
    pub fn system_prompt(self, p: String) -> Self;  pub fn append_system_prompt(self, p: String) -> Self;
    pub fn json_schema(self, schema: serde_json::Value) -> Self;
    pub fn max_turns(self, n: usize) -> Self;       pub fn max_budget_usd(self, usd: f64) -> Self;
    pub fn allowed_tools(self, rules: Vec<String>) -> Self;  // CLI-scope allow rules, below the floor
    pub fn disallowed_tools(self, rules: Vec<String>) -> Self;
    pub fn builtin_tools(self, names: Vec<String>) -> Self;  // maps to AgentSetup::root_tools
    pub fn add_dir(self, dir: PathBuf) -> Self;
    pub fn resume(self, id: SessionRef) -> Self;    // Continue | Id(String), plus .fork()
    pub fn approver(self, a: Arc<dyn Approver>) -> Self;     // sets HumanChannel::Delegate
    pub fn hook(self, h: CallbackHook) -> Self;              // tighten-only; appended to AgentSetup::callback_hooks
    pub fn tool(self, t: impl HostTool + 'static) -> Self;
    pub async fn build(self) -> Result<Session, EmbedError>;
}

pub struct Session { /* owns AgentRuntime + Scratch guard */ }
impl Session {
    pub fn id(&self) -> &str;
    pub fn send(&mut self, prompt: impl Into<String>) -> Turn<'_>;   // a Stream of Event
    pub fn interrupt_handle(&self) -> InterruptHandle;
    pub async fn close(self);                                         // SessionEnd hooks, scratch cleanup
}
pub enum Event { /* the headless-v1 events, typed; serde-serializable to the identical JSON */ }
```

Decisions that shape this surface:

- **Event stream = `headless-v1`.** `Event` is a typed, `#[non_exhaustive]` enum with one variant
  per schema kind (the 13 kinds of the closed `oneOf`; no notice kind). It lives in
  `fontaine-sdk::events`, and `HeadlessV1Sink` becomes a thin writer of
  `serde_json::to_string(&event)`. Field order reproduces today's key order. A test asserts the typed value serializes to JSON that validates
  against `headless-v1.schema.json`, which pins "identical to stream-json" mechanically. The schema
  version stays `1`; embedders see `SCHEMA_VERSION` re-exported. The existing `headless_contract`
  tests and a byte-equality check on a recorded stream pin the key order.
- **Approvals.** `build()` with an approver sets `HumanChannel::Delegate`; without one it sets
  `Absent` (every would-prompt is a denial). `Present` is not exposed: it means "a person at a
  terminal". The approver is reached only where the TUI would ask a person, after the floor, hooks,
  rules, mode and guardian, and is never offered "always allow" (existing `Delegate` semantics).
- **Host tools.** `HostTool` is a trait (`name`, `description`, JSON-schema `parameters`, async
  `call(args) -> Result<String, ToolError>`, with `ToolError` re-exported from the SDK). A host tool
  declares no risk: risk lives in the descriptor table, never on the tool, so every host tool is
  registered as `RiskClass::Arbitrary`, like SDK MCP tools. Registration goes through the existing
  registry so the choke-point rule (every tool has a risk class, every call passes the gate) holds;
  a host tool is never on the read-only fast path (same as MCP tools). The registry change is
  additive: a `host_tools: Vec<Arc<dyn HostTool>>` field on `AgentSetup`, kept in the retained
  setup so plugin reloads rebuild it, and subject to `root_tools`/toolset cutting like built-ins, so
  a subagent never has a host tool its parent lacks.
- **No way to loosen security.** The builder exposes no guardian-off switch, no floor override, no
  `bypass` mode and no project-scope loosening. Project settings still load as tighten-only; user
  scope is read from the supplied `config_dir`.
- **Embedded session settings.** An embedded session takes headless semantics: `headless = true`,
  `scheduling = false`, `confirm_project_servers = false`, and `project_executables` follows
  `[headless] require_trust` and the trust store. `PermissionMode::Afk` maps as the CLI's
  `session_mode` does: `Auto` plus `setup.afk = true`.
- **Stability.** `AgentSetup` stays unstable and un-promised; only the root facade is the semver
  surface. It starts at `0.x`, `publish = false` like the workspace (`publish.workspace`), and the
  facade is `#[non_exhaustive]` on enums.
- **Errors.** `EmbedError` is a typed enum only where an embedder dispatches (`Config`, `Session`,
  `Credentials`, `Model`, `Runtime`); each carries its source (see AGENTS.md error handling). The SDK
  writes to stderr only through the `Approver::warn`/`notice` defaults, which an embedder overrides;
  the PromptGuard download message in `scanner_stage` goes through `notice` when embedded.

### Assembly extraction

The pure parts of `prepare_session` (roots, scratch, memory root, plugin load, extensions, model
resolution, scanner start, `requested_mode`/`session_mode`, the project-executables trust check from
`run()`, `AgentSetup` construction) move into `fontaine-sdk` as functions
over plain parameter structs, returning `Result<_, EmbedError>` and warnings as data. The CLI keeps
only flag-to-params translation and `eprintln!`/exit-code mapping. Moves are mechanical; behavior
is held by the existing `headless_*`, `resume`, `claude_sdk_compat` and `cassettes` tests, which must
stay green unchanged. The headless run loop (`run_session`, `run_one`, `run_attempt`, settle, cancel,
`EventSink`, `InputSource`) moves too, so `Session` and the CLI share the model-list fall-through,
usage-limit wait, budget stop, `--json-schema` output, AFK stop, stop-hook continuation, goals and
interrupt.

### Non-Rust languages

No new protocol. The two stdio routes stay as they are. The docs page states which to choose:
`headless-v1` (Fontaine-native, flat events, no approval callback) vs Claude Agent SDK mode
(Claude's message shapes, `canUseTool`, hooks, SDK MCP servers).

## Implementation Steps

1. Merge `fontaine-core`, `fontaine-tools` and `fontaine-agents` into the new `rust/fontaine-sdk` as
   the `core`, `tools` and `agents` modules (one mechanical commit, see One crate for the SDK);
   repoint every dependent crate and the workspace manifest; the existing test suites must pass
   unchanged.
2. Define typed `Event` structs for all 13 schema kinds in `fontaine-sdk::events` and move their
   builders (`init_event`, `result_event`, `usage_event`, the goal and AFK helpers, `Outcome`/`Usage`)
   out of `fontaine/src/headless.rs`; rewire `HeadlessV1Sink`. Add the schema-validation test and the
   byte-equality check on a recorded stream.
3. Move the headless run loop (`run_session`, `run_one`, `run_attempt`, settle, cancel, `EventSink`,
   `InputSource`) from `fontaine/src/headless.rs` into the SDK; the CLI calls it. No behavior change.
4. Move session assembly (`load_config`, `resolve_roots`, `open_session`, `prepare_session`,
   `resolve_models`, `start_scanner`, `requested_mode`/`session_mode`, the trust check, `AgentSetup`
   construction) from `fontaine/src/lib.rs` into `fontaine-sdk::assemble`; the CLI calls it. No
   behavior change.
5. Add `HostTool` and `AgentSetup::host_tools`; keep it in the retained setup and pass it through
   `RegistryParams` in `agent/build.rs` so plugin reloads keep it; register every host tool as
   `RiskClass::Arbitrary`; update the `AgentSetup` literal sites (`fontaine/src/lib.rs`,
   `fontaine-sdk/tests/common/runtime.rs`, `fontaine-tui/src/ui/tests.rs`,
   `fontaine-tui/tests/common/mod.rs`, ...); extend the choke-point test to cover host tools.
6. Write `SessionBuilder`, `Session`, `Turn`, `InterruptHandle` over `AgentRuntime::new` and the
   run loop moved in step 3, not over `drive`.
6. Add `fontaine-sdk/examples/embed.rs` (a 30-line program: builder, approver, one host tool, print
   events) and `docs/embedding.md`; link it from `docs/SUMMARY.md`, `docs/index.md`, and the
   architecture crate table; add a `CHANGELOG.md` entry; record the Decisions in the master plan.

## Files to Modify

- `rust/Cargo.toml` (core, tools and agents removed, sdk added)
- `rust/fontaine-sdk/**` (new; receives `fontaine-core/**`, `fontaine-tools/**`, `fontaine-agents/**`)
- every `Cargo.toml` and import in `fontaine`, `fontaine-tui`, `fontaine-remote`, `fontaine-evals`,
  `fontaine-testkit`
- `rust/fontaine/src/lib.rs`, `rust/fontaine/src/headless.rs`, `rust/fontaine/Cargo.toml`
- `rust/fontaine-sdk/src/core/agent.rs`, `rust/fontaine-sdk/src/core/tools.rs` (host tools)
- `docs/embedding.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/architecture.md`,
  `docs/agent-sdk.md`, `docs/headless.md` (cross-links)
- `tasks/fontaine_harness_plan.md` (Decisions, workspace layout), `CHANGELOG.md`, `AGENTS.md`
  (crate list), `CONTRIBUTING.md` (paths into the merged crates), `tasks/design/*.md` and docs that
  name the merged crates

## Trade-offs

- **Run the binary as a subprocess from Rust:** needs no new code, but gives no in-process tools or
  typed callbacks. Kept as the documented route for other languages only.
- **Expose `AgentSetup` directly:** cheapest, but promises 35 internal fields.

## Decisions

- 2026-11-14: Rust embedding is a typed, in-process facade whose event stream serializes to `headless-v1` unchanged; foreign languages use the existing stdio surfaces, with no third protocol.
- 2026-11-14: (user call) The Rust SDK is a single crate, `fontaine-sdk`; `fontaine-core`, `fontaine-tools` and `fontaine-agents` become its modules in this same change. `fontaine-guardian` stays separate for lift-out and is re-exported.
- 2026-11-14: Publication is out of scope; the publish set would be `fontaine-sdk` and `fontaine-guardian`.
- 2026-11-14: Embedded approvals use `HumanChannel::Delegate`, not a new channel: the delegate semantics (asked only after the floor, hooks, rules and guardian; no "always allow") are exactly what an embedder needs.

## Documentation

New `docs/embedding.md` (choosing a route, builder walkthrough, approver, host tools, security
guarantees the builder cannot loosen). Cross-links from `agent-sdk.md` and `headless.md`; crate
table in `architecture.md`. Docs describe current behavior only.

## Testing Strategy

Unit tests for builder validation and defaults (no approver → `Absent`; mode mapping; resume/fork).
Integration tests with `rig_core::test_utils` deterministic models, no live APIs:
- wiring: builder plus approver gives `HumanChannel::Delegate`, and a prompt reaches the approver
  (`fontaine-core/tests/permissions_delegate.rs` covers the delegate semantics);
- a host callback hook can tighten but its allow is ignored;
- a host tool call is guardian-judged in `auto` and prompts in `manual`;
- `Event` JSON validates against `headless-v1.schema.json`, and a recorded `fontaine -p` run and an
  embedded run of the same scripted model produce the same event sequence, including with
  `max_turns`, `max_budget_usd` and `json_schema` set;
- interrupt ends a turn with the same `result` as the CLI.
The existing headless, resume, SDK-compat and cassette suites are the regression net for the
assembly move.
