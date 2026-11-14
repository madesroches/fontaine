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

The SDK is **one crate**, `rust/fontaine-sdk`, and it is the only crate a Rust embedder names in
`Cargo.toml` or reads on docs.rs. A user following the docs should never have to work out which of
`fontaine-core`, `-tools`, `-agents` holds a type.

- It depends on `fontaine-core`, `fontaine-tools` and `fontaine-agents` and exposes everything an
  embedder needs from them through its own module tree (`fontaine_sdk::permissions::Approver`,
  `fontaine_sdk::config::PermissionMode`, `fontaine_sdk::tools::HostTool`, ...), as curated
  `pub use` re-exports with the docs attached to the SDK path. No type is promised at its internal
  path; the internal crates stay `publish = false` and unstable.
- `fontaine-guardian` stays its own crate: the repo's invariant is that it is dependency-light and
  lift-out-able by other harnesses, so folding it in would break that. The SDK re-exports its
  public types (`fontaine_sdk::guardian`) so embedders still see one crate; a guardian-only
  consumer keeps using `fontaine-guardian` directly.
- Out of the SDK: `fontaine-tui`, `fontaine-remote`, `fontaine-evals`, `fontaine-testkit` and the
  `fontaine` binary; none is something an embedding program needs.
- Publication is out of scope (workspace is `publish = false`), but the design must not block it.
  crates.io requires every dependency to be published, so shipping the SDK as one crate means
  merging core, tools and agents into it as modules. That is a mechanical but repo-wide import
  move, so it is a separate follow-up issue that gates publishing; the facade here is what makes it
  safe to do later, because the SDK paths are already the only public ones.

### Public surface

```rust
pub struct SessionBuilder { /* private */ }
impl SessionBuilder {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self;
    pub fn dirs(self, config_dir: PathBuf, data_dir: PathBuf) -> Self;   // default: dirs::resolve
    pub fn env(self, env: EnvFn) -> Self;                                 // default: process env
    pub fn mode(self, mode: PermissionMode) -> Self;                      // default: Manual
    pub fn model(self, reference: &str) -> Self;
    pub fn system_prompt(self, p: String) -> Self;  pub fn append_system_prompt(self, p: String) -> Self;
    pub fn json_schema(self, schema: Schema) -> Self;
    pub fn max_turns(self, n: usize) -> Self;       pub fn max_budget_usd(self, usd: f64) -> Self;
    pub fn allowed_tools(self, rules: Vec<String>) -> Self;  // CLI-scope allow rules, below the floor
    pub fn disallowed_tools(self, rules: Vec<String>) -> Self;
    pub fn builtin_tools(self, names: Vec<String>) -> Self;  // maps to AgentSetup::root_tools
    pub fn add_dir(self, dir: PathBuf) -> Self;
    pub fn resume(self, id: SessionRef) -> Self;    // Continue | Id(String), plus .fork()
    pub fn approver(self, a: Arc<dyn Approver>) -> Self;     // sets HumanChannel::Delegate
    pub fn hook(self, h: Arc<dyn PreToolUseHook>) -> Self;   // tighten-only, as SDK callback hooks
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

- **Event stream = `headless-v1`.** The typed `Event` enum and its serde form are moved out of
  `headless.rs`' `HeadlessV1Sink` into `fontaine-sdk::events` and the sink becomes a thin writer of
  `serde_json::to_string(&event)`. A test asserts the typed value serializes to JSON that validates
  against `headless-v1.schema.json`, which pins "identical to stream-json" mechanically. The schema
  version stays `1`; embedders see `SCHEMA_VERSION` re-exported.
- **Approvals.** `build()` with an approver sets `HumanChannel::Delegate`; without one it sets
  `Absent` (every would-prompt is a denial). `Present` is not exposed: it means "a person at a
  terminal". The approver is reached only where the TUI would ask a person, after the floor, hooks,
  rules, mode and guardian, and is never offered "always allow" (existing `Delegate` semantics).
- **Host tools.** `HostTool` is a trait (`name`, `description`, JSON-schema `parameters`, async
  `call(args) -> Result<String, ToolError>`, and a mandatory `risk: RiskClass`). Registration goes
  through the existing registry so the choke-point rule (every tool has a risk class, every call
  passes the gate) holds; a host tool is never on the read-only fast path (same as MCP tools). The
  registry change is additive: a `host_tools: Vec<Arc<dyn HostTool>>` field on `AgentSetup`.
- **No way to loosen security.** The builder exposes no guardian-off switch, no floor override, no
  `bypass` mode and no project-scope loosening. Project settings still load as tighten-only; user
  scope is read from the supplied `config_dir`.
- **Stability.** `AgentSetup` stays unstable and un-promised; only `fontaine-sdk` items are the
  semver surface. It starts at `0.x`, `publish = false` like the workspace (`publish.workspace`),
  and the facade is `#[non_exhaustive]` on enums and builders' options structs.
- **Errors.** `EmbedError` is a typed enum only where an embedder dispatches (`Config`, `Session`,
  `Credentials`, `Model`, `Runtime`); each carries its source (see AGENTS.md error handling). Nothing
  prints to stderr: warnings reach the approver's `warn`, or an `Event::Notice` stream.

### Assembly extraction

The pure parts of `prepare_session` (roots, scratch, memory root, plugin load, extensions, model
resolution, scanner start, `AgentSetup` construction) move into `fontaine-sdk` as functions
over plain parameter structs, returning `Result<_, EmbedError>` and warnings as data. The CLI keeps
only flag-to-params translation and `eprintln!`/exit-code mapping. Moves are mechanical; behavior
is held by the existing `headless_*`, `resume`, `claude_sdk_compat` and `cassettes` tests, which must
stay green unchanged.

### Non-Rust languages

No new protocol. The two stdio routes stay as they are. The docs page states which to choose:
`headless-v1` (Fontaine-native, flat events, no approval callback) vs Claude Agent SDK mode
(Claude's message shapes, `canUseTool`, hooks, SDK MCP servers).

## Implementation Steps

1. Create `rust/fontaine-sdk` crate; add it to the workspace and `deny.toml`/dist config as needed.
2. Move the `headless-v1` event model and its serialization from `fontaine/src/headless.rs` into
   `fontaine-sdk::events`; rewire `HeadlessV1Sink`. Add the schema-validation test.
3. Move session assembly (`load_config`, `resolve_roots`, `open_session`, `prepare_session`,
   `resolve_models`, `start_scanner`, `AgentSetup` construction) from `fontaine/src/lib.rs` into
   `fontaine-sdk::assemble`; the CLI calls it. No behavior change.
4. Add `HostTool` and `AgentSetup::host_tools`; register through `fontaine-core::tools` with a risk
   class; extend the choke-point test to cover host tools.
5. Write `SessionBuilder`, `Session`, `Turn`, `InterruptHandle` over `AgentRuntime::new` and `drive`.
6. Add `fontaine-sdk/examples/embed.rs` (a 30-line program: builder, approver, one host tool, print
   events) and `docs/embedding.md`; link it from `docs/SUMMARY.md`, `docs/index.md`, and the
   architecture crate table; add a `CHANGELOG.md` entry; record the Decisions in the master plan.
7. CI: add `fontaine-sdk` to the existing workspace steps (automatic); the example compiles under
   `cargo clippy --all-targets`.

## Files to Modify

- `rust/Cargo.toml`, `rust/deny.toml` (new member)
- `rust/fontaine-sdk/**` (new)
- `rust/fontaine/src/lib.rs`, `rust/fontaine/src/headless.rs`, `rust/fontaine/Cargo.toml`
- `rust/fontaine-core/src/agent.rs`, `rust/fontaine-core/src/tools.rs` (host tools)
- `docs/embedding.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/architecture.md`,
  `docs/agent-sdk.md`, `docs/headless.md` (cross-links)
- `tasks/fontaine_harness_plan.md` (Decisions, workspace layout), `CHANGELOG.md`, `AGENTS.md`
  (crate list)

## Trade-offs

- **Facade in `fontaine-core` (issue wording) vs new crate:** assembly depends on agents/tools
  wiring, and core is large and internally unstable. A new crate keeps the promised surface small.
  The issue's intent (a stabilized facade over core) is kept.
- **Several public crates (core, tools, agents, ...) vs one:** many crates on crates.io make the
  docs hard to follow, so the SDK is one crate with re-exports. Merging the internals into it now
  was rejected as a large churn unrelated to the API itself; it is a follow-up gating publication.
- **Run the binary as a subprocess from Rust:** needs no new code, but gives no in-process tools or
  typed callbacks. Kept as the documented route for other languages only.
- **Expose `AgentSetup` directly:** cheapest, but promises 35 internal fields.

## Decisions

- 2026-11-14: Rust embedding is a typed, in-process facade whose event stream serializes to `headless-v1` unchanged; foreign languages use the existing stdio surfaces, with no third protocol.
- 2026-11-14: The Rust SDK is a single crate, `fontaine-sdk`, re-exporting the needed parts of core, tools, agents and guardian under its own paths; guardian stays a separate crate for lift-out; merging the internal crates into the SDK is a follow-up that gates any crates.io publication.
- 2026-11-14: Embedded approvals use `HumanChannel::Delegate`, not a new channel: the delegate semantics (asked only after the floor, hooks, rules and guardian; no "always allow") are exactly what an embedder needs.

## Documentation

New `docs/embedding.md` (choosing a route, builder walkthrough, approver, host tools, security
guarantees the builder cannot loosen). Cross-links from `agent-sdk.md` and `headless.md`; crate
table in `architecture.md`. Docs describe current behavior only.

## Testing Strategy

Unit tests for builder validation and defaults (no approver → `Absent`; mode mapping; resume/fork).
Integration tests with `rig_core::test_utils` deterministic models, no live APIs:
- an approver that allows, denies and returns modified input, with the audit record showing
  `delegate`;
- a pre-tool hook can tighten but its allow is ignored;
- a host tool call goes through the gate, and one with a destructive class is guardian-judged;
- a floor-denied call (protected path) never reaches the approver, the exact security property;
- `Event` JSON validates against `headless-v1.schema.json`, and a recorded `fontaine -p` run and an
  embedded run of the same scripted model produce the same event sequence;
- interrupt ends a turn with the same `result` as the CLI.
The existing headless, resume, SDK-compat and cassette suites are the regression net for the
assembly move.

## Open Questions

- Should `Event` be an exhaustive typed enum, or `serde_json::Value` plus helpers? Default: typed,
  `#[non_exhaustive]`.
- Whether to publish: out of scope; the workspace is `publish = false` (see One crate for the SDK).
