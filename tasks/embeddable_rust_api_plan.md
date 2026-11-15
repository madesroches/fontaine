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
`fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` become **modules** of it:

- `fontaine_sdk::core`, `fontaine_sdk::tools`, `fontaine_sdk::agents`, `fontaine_sdk::guardian` hold the existing source trees
  unchanged (`core/src/*` -> `sdk/src/core/*`, and so on). Module names collide across the four
  crates (`tasks`, `effort`, `schedule`, `checkpoint`, `tools`), which is why each tree keeps its
  own namespace instead of being flattened.
- The embedder-facing facade (`SessionBuilder`, `Session`, `Event`, `HostTool`, and curated
  re-exports of `Approver`, `PermissionMode`, `HookDecision` and the like) sits at the crate root
  and in a few top-level modules (`fontaine_sdk::permissions`, `::config`, ...), so the docs lead
  with the facade. The module trees stay `pub` for the binary, the TUI and the evals, but only the
  root facade is the documented, semver-stable surface; the rustdoc landing page says so, and the
  trees are `#[doc(hidden)]`-free so links in the facade's docs resolve.
- The merge is mechanical: `git mv` the trees, `fontaine_core::` -> `fontaine_sdk::core::` (and
  likewise) across the roughly 340 files that import them, and the four `Cargo.toml` dependency
  lists fold into one. It also rewrites `crate::` paths inside the moved trees and cross-tree
  `fontaine_tools::`/`fontaine_agents::` paths inside core to `crate::tools::`/`crate::agents::`;
  moves `fontaine-core/prompts/` next to the new source (`include_str!` in `prompt.rs`, `goal.rs`)
  and `fontaine-guardian/{prompts,data}/` next to the guardian module (`include_str!` in
  `critique`, `env_scan`, `evasion`, `profile`, `scanner`, `models`); fixes the `../../../research/`
  include in core's `usage_limit.rs` for the extra directory level; renames and re-accepts the 7
  insta snapshots in `fontaine-guardian/src/snapshots` (`fontaine_guardian__prompt_builder__tests__...`);
  renames the colliding integration tests (`choke_point.rs`, `lsp.rs`, `network.rs`, `scanner.rs`,
  and the `common/` dirs, e.g. `core_common/`, `tools_common/`); repoints the `fontaine-guardian/`
  paths hard-coded in `DEFAULT_MODELS_FILE` (`fontaine-evals/src/runner.rs`), `evals/README.md`,
  `docs/evals.md` and `docs/guardian.md`;
  fixes the `example_bin("fontaine-core"|"fontaine-tools", ...)` call sites and the `#[path]`
  includes of the other crates' tests and examples; and re-points the `pg2` feature from
  `fontaine/Cargo.toml` and `fontaine-evals/Cargo.toml`. It lands as its own commit, ahead of any
  behavior change, so the tests are the proof it changed nothing.
- The Rune scripting dependency of `fontaine-guardian` stays behind the SDK's `guardian-composition` feature (the old `composition` feature of
  `fontaine-guardian`, which core enabled, so it is on by default). The guardian's `pg2` feature
  folds into the SDK's existing `pg2` feature. `AGENTS.md`'s lift-out rule is reworded to match.
- Out of the SDK: `fontaine-tui`, `fontaine-remote`, `fontaine-evals`, `fontaine-testkit` and the
  `fontaine` binary. `fontaine-tui`, `fontaine-evals` and the binary depend on the SDK.

### Public surface

```rust
pub struct SessionBuilder { /* private */ }
impl SessionBuilder {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self;
    pub fn dirs(self, config_dir: PathBuf, data_dir: PathBuf) -> Self;   // default: dirs::resolve
    pub fn env(self, env: EnvLookup) -> Self;                             // default: process env; scrub names always come from the real process environment (`std::env::vars_os`), since an EnvLookup cannot list names
    pub fn mode(self, mode: PermissionMode) -> Self;                      // default: [permissions] default_mode, then Manual
    pub fn model(self, reference: &str) -> Self;
    pub fn system_prompt(self, p: String) -> Self;  pub fn append_system_prompt(self, p: String) -> Self;
    pub fn json_schema(self, schema: serde_json::Value) -> Self;
    pub fn max_turns(self, n: usize) -> Self;       pub fn max_budget_usd(self, usd: f64) -> Self;
    pub fn allowed_tools(self, rules: Vec<String>) -> Self;  // CLI-scope allow rules, below the floor
    pub fn disallowed_tools(self, rules: Vec<String>) -> Self;
    pub fn builtin_tools(self, names: Vec<String>) -> Self;  // maps to AgentSetup::root_tools; must also name every host tool to keep
    pub fn add_dir(self, dir: PathBuf) -> Self;
    pub fn resume(self, id: SessionRef) -> Self;    // Continue | Id(String), plus .fork()
    pub fn approver(self, a: Arc<dyn Approver>) -> Self;     // sets HumanChannel::Delegate
    pub fn hook(self, h: CallbackHook) -> Self;              // tighten-only; appended to AgentSetup::callback_hooks
    pub fn tool(self, t: impl HostTool + 'static) -> Self;
    pub async fn build(self) -> Result<Session, EmbedError>;
}

pub struct Session { /* owns the run_session task (which owns AgentRuntime) + Scratch guard */ }
impl Session {
    pub fn id(&self) -> &str;
    pub fn send(&mut self, prompt: impl Into<String>) -> Turn<'_>;   // a Stream of Event
    pub fn interrupt_handle(&self) -> InterruptHandle;
    pub async fn close(self) -> Result<(), EmbedError>;               // SessionEnd hooks, scratch cleanup; Err if the task ended non-zero
}
// Session::send feeds a long-lived run_session task; see Session over the run loop.
pub enum Event { /* the headless-v1 events, typed; serde-serializable to the identical JSON */ }
```

Decisions that shape this surface:

- **Event stream = `headless-v1`.** `Event` is a typed, `#[non_exhaustive]` enum with one variant
  per output kind (12 of the 13 kinds of the closed `oneOf`: all except `user_input`, the input
  line, which `HeadlessV1Sink` never prints; no notice kind). It lives in
  `fontaine_sdk::events`, and `HeadlessV1Sink` becomes a thin writer of
  `serde_json::to_string(&event)`. `Event` serializes untagged (or by hand-written `Serialize`),
  each struct declaring its payload fields first and `type`, `schema_version`, `session_id` last,
  matching `event()` in `headless.rs` (serde_json `preserve_order`); a `#[serde(tag = "type")]`
  enum would write the tag first and fail the byte-equality check. The schema version stays `1`; embedders see `SCHEMA_VERSION` re-exported. The existing
  `headless_contract` tests (which validate every line against `headless-v1.schema.json`) and a
  byte-equality check on a recorded stream pin the key order.
- **Approvals.** `build()` with an approver sets `HumanChannel::Delegate`; without one it sets
  `Absent` backed by `DenyAll`, as the CLI does (every would-prompt is a denial). `Present` is not exposed: it means "a person at a
  terminal". The approver is reached only where the TUI would ask a person, after the floor, hooks,
  rules, mode and guardian, and is never offered "always allow" (existing `Delegate` semantics).
- **Host tools.** `HostTool` is a trait (`name`, `description`, JSON-schema `parameters`, async
  `call(args) -> Result<String, ToolExecutionError>`, with rig's `ToolExecutionError` re-exported from the SDK, the error type `guard_callback` already works in). A host tool
  declares no risk: risk lives in the descriptor table, never on the tool, so every host tool is
  registered as `RiskClass::Arbitrary`, like SDK MCP tools. A host tool's name is chosen at
  runtime, so it cannot go through `ExtensionTool::new<T: Tool>` / `register_extension` (compile-time
  `T::NAME`, risk from `registered_risk(name)`, `ToolSource::Builtin` hard-coded in `GuardedTool`).
  Instead each host tool is wrapped by `guard_callback` (as MCP tools are), which gains a
  `ToolSource` parameter, at `RiskClass::Arbitrary`, and pushed into the registry as a `Registered`
  beside the built-ins, so every call still passes the gate. `Registered.name` is `&'static str`;
  `build()` leaks each host tool's name once (a bounded, per-tool `Box::leak`) and the retained
  setup keeps that `&'static str`, so plugin reloads reuse it. `ToolSource` gains a `Host` variant
  (`"host"` on telemetry's `tool_source`); the `hide_mcp_names` redaction applies to `Mcp` only, so
  host tool names, the embedder's own, are shown. A host tool is never on the read-only fast path
  (same as MCP tools). The pipeline branches on tool name (the protected-path floor for
  `edit`/`write`/`notebook_edit`, `descriptor::find` for path-free, `afk_report.rs`,
  `spawn_changes_state`), so `build()` rejects with `EmbedError::Config` a host tool whose name
  matches a descriptor row, a built-in name or the `mcp__` prefix, or duplicates another host tool, or any name outside `[A-Za-z0-9_-]`. The registry change is
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
  writes to stderr only through the `Approver::warn`/`notice` defaults (the `DenyAll` used without an approver), which an embedder replaces by supplying an approver;
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

`Options` moves with the loop, holding SDK-owned, clap-free input/output format enums that the CLI
maps from its `InputFormat`/`OutputFormat`. The `pub(crate)` items `claude_compat` imports
(`EventSink`, `ResultInfo` with its private `goal`/`afk` fields, `Outcome` with its `pub(crate)` fields, `Subtype`,
`session_cost`, `main_usage`, `Cancel`, `PauseObserver`, `InputSource`, `Options`, `run_session`) become
public in the SDK tree; the `#[cfg(test)] pub(crate)` `interrupted_for_tests` helper is replaced by a public constructor. Every `eprintln!` in the moved loop (announce, `run_session` and the rest)
goes through the approver's `warn`/`notice` or an `EventSink` hook that `HeadlessV1Sink` writes to
stderr, so CLI output is unchanged.

The startup steps of `start_headless`/`run_headless` that `prepare_session` does not cover move
too, and `SessionBuilder::build` runs them: `runtime.connect_mcp(&[])` after `AgentRuntime::new`
(so configured MCP servers connect), the `guardian_login` lookup that `run_session` takes as a
parameter, and the refusal of `max_budget_usd` when the model has no known price (an `EmbedError`
at `build`). `build()` also returns `EmbedError::Credentials` when `guardian_login` is `Some` and the
mode is Auto/Afk (the check `run_session` makes first, which otherwise returns 1 with empty `Turn`
streams). The staged-plugin swap (`plugin_auto_update::apply_staged`, run by `prepare_session` when
`!cli.print`) stays in the CLI, in the binary crate, and runs before the SDK's plugin load (interactive only).
Unreadable input likewise surfaces as an error.

### Session over the run loop

`run_session` is a pull loop holding `&mut AgentRuntime` for the whole session, so the spawned task
owns the runtime and returns it with the exit code when it finishes; `close()` takes both from the
join handle before `fire_session_end`. A task that ends with a non-zero code surfaces as an error
item on the active `Turn` stream and as `Err` from `close()`. The loop records the
session start, announces and calls `sink.session_start` once, and a failed turn ends the run unless
`InputSource::long_lived()`. So `Session` runs `run_session` on a task fed by a channel-backed
`InputSource` with `long_lived() = true` (like `claude_compat`'s `SdkInput`), and events flow
through a channel-backed `EventSink` that sends typed `Event`s (the usage observer included) into
each `Turn`'s stream; `HeadlessV1Sink` is the CLI's sink only. `Session` passes the stream-json
output format (so the Json/Text stdout tail of `run_session` never runs) and the stream-json input
origin. An interrupt sets the shared `Cancel` and `run_session` never clears it, so the channel-backed
input source calls `cancel.reset()` as it hands over each new message, as the SDK router does;
`InterruptHandle` wraps the same `Cancel`. `send` pushes a prompt on the channel; `close()`
drops the input, awaits the task, then fires `SessionEnd` hooks (`fire_session_end`).

### Non-Rust languages

No new protocol. The two stdio routes stay as they are. The docs page states which to choose:
`headless-v1` (Fontaine-native, flat events, no approval callback) vs Claude Agent SDK mode
(Claude's message shapes, `canUseTool`, hooks, SDK MCP servers).

## Implementation Steps

1. Merge `fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` into the new
   `rust/fontaine-sdk` as the `core`, `tools`, `agents` and `guardian` modules (one mechanical commit, see One crate for the SDK);
   repoint every dependent crate and the workspace manifest; the existing test suites must pass
   unchanged.
2. Define typed `Event` structs for the 12 output kinds (all but `user_input`) in `fontaine-sdk::events` and move their
   builders (`init_event`, `result_event`, `usage_event`, the goal and AFK helpers, `Outcome`/`Usage`)
   out of `fontaine/src/headless.rs`; rewire `HeadlessV1Sink`. Add the byte-equality check on a recorded stream.
3. Move the headless run loop (`run_session`, `run_one`, `run_attempt`, settle, cancel, `EventSink`,
   `InputSource`, and `Options` with SDK-owned format enums) from `fontaine/src/headless.rs` into
   the SDK; make the items `claude_compat` imports public (the full list is in Assembly extraction); route every `eprintln!` through the
   approver's `warn`/`notice` or an `EventSink` hook. Tighten
   `Ctx.pending` and the two `wait_for_news` parameters from `&dyn Fn() -> bool` to
   `&(dyn Fn() -> bool + Sync)` so the `run_session` future is `Send` and the task `Session`
   spawns compiles. The CLI calls it. No behavior change in CLI output.
4. Move session assembly (`load_config`, `resolve_roots`, `open_session`, `prepare_session`,
   `resolve_models`, `start_scanner`, `requested_mode`/`session_mode`, the trust check, `AgentSetup`
   construction, plus the `connect_mcp(&[])` call, the `guardian_login` lookup and the unpriced-model
   `max_budget_usd` refusal from `start_headless`/`run_headless`) from `fontaine/src/lib.rs` into
   `fontaine-sdk::assemble`; `SessionBuilder::build` runs them and the CLI calls it; the
   staged-plugin swap stays in the CLI. No behavior change.
5. Add `HostTool` and `AgentSetup::host_tools`; keep it in the retained setup and pass it through
   `RegistryParams` in `agent/build.rs` so plugin reloads keep it; wrap every host tool with
   `guard_callback` (new `ToolSource` parameter, new `ToolSource::Host`) at `RiskClass::Arbitrary`;
   validate names in `build()`; update the `guard_callback` callers (`tools/src/mcp/register.rs`,
   `tools/tests/guarded.rs`, at their post-merge paths under `fontaine-sdk`); update the `AgentSetup` literal sites (`fontaine/src/lib.rs`,
   `fontaine-sdk/tests/core_common/runtime.rs`, `fontaine-tui/src/ui/tests.rs`,
   `fontaine-tui/tests/common/mod.rs`, ...); extend the choke-point test to cover host tools.
6. Write `SessionBuilder`, `Session`, `Turn`, `InterruptHandle` over `AgentRuntime::new` and the
   run loop moved in step 3, not over `drive`: `Session` runs `run_session` on a task with a
   channel-backed, `long_lived()` `InputSource`; events reach each `Turn` through an `EventSink`;
   the task owns the runtime and returns it; `close()` drops the input, awaits the task and takes the runtime back before `fire_session_end`; the input source resets
   the shared `Cancel` per message, and `InterruptHandle` wraps it.
7. Add `fontaine-sdk/examples/embed.rs` (a 30-line program: builder, approver, one host tool, print
   events) and `docs/embedding.md`; link it from `docs/SUMMARY.md`, `docs/index.md`, and the
   architecture crate table; add a `CHANGELOG.md` entry; record the Decisions in the master plan.

## Files to Modify

- `rust/Cargo.toml` (core, tools, agents and guardian removed, sdk added)
- `rust/fontaine-sdk/**` (new; receives `fontaine-core/**`, `fontaine-tools/**`, `fontaine-agents/**`, `fontaine-guardian/**`)
- every `Cargo.toml` and import in `fontaine`, `fontaine-tui`, `fontaine-evals`
- `rust/fontaine/src/lib.rs`, `rust/fontaine/src/headless.rs`, `rust/fontaine/Cargo.toml`
- `rust/fontaine-sdk/src/core/agent.rs`, `core/agent/build.rs`, `core/agent/setup.rs`,
  `core/tools.rs`, `tools/src/guarded.rs`, `tools/src/risk.rs`, `tools/src/mcp/register.rs`,
  `tools/tests/guarded.rs` (the `guard_callback` callers) and the choke-point test (host tools)
- `docs/telemetry.md` and `tasks/design/observability.md` (new `host` value of `tool_source`)
- `fontaine-evals/src/runner.rs`, `evals/README.md`, `docs/evals.md`, `docs/guardian.md`
  (`fontaine-guardian/` paths)
- `docs/embedding.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/architecture.md`,
  `docs/agent-sdk.md`, `docs/headless.md` (cross-links)
- `tasks/fontaine_harness_plan.md` (Decisions, workspace layout), `CHANGELOG.md`, `AGENTS.md`
  (crate list; the `fontaine_tools::error_chain::chain` and `fontaine_tools::lock::{lock, read, write}` paths in Error handling), `CONTRIBUTING.md` (paths into the merged crates), `tasks/design/*.md` and docs that
  name the merged crates

## Decisions

- 2026-11-14: Rust embedding is a typed, in-process facade whose event stream serializes to `headless-v1` unchanged; foreign languages use the existing stdio surfaces, with no third protocol.
- 2026-11-14: (user call) The Rust SDK is a single crate, `fontaine-sdk`; `fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` become its modules in this same change. Deviates from the guardian crate's lift-out rule: a lift-out consumer now depends on the SDK with default features off, and the guardian module imports no other SDK module.
- 2026-11-14: Publication is out of scope; the publish set would be `fontaine-sdk` alone.
- 2026-11-14: Embedded approvals use `HumanChannel::Delegate`, not a new channel: the delegate semantics (asked only after the floor, hooks, rules and guardian; no "always allow") are exactly what an embedder needs.
- 2026-11-14: `AgentSetup` is not exposed directly to embedders: only the root facade is semver-stable.

## Documentation

New `docs/embedding.md` (choosing a route, builder walkthrough, approver, host tools, security
guarantees the builder cannot loosen; `builtin_tools` must also name every host tool to keep).
`docs/telemetry.md` and `tasks/design/observability.md` list `host` as a `tool_source` value. Cross-links from `agent-sdk.md` and `headless.md`; crate
table in `architecture.md`. Docs describe current behavior only.

## Testing Strategy

Unit tests for builder validation and defaults (no approver → `Absent`; mode mapping; resume/fork).
Integration tests with `rig_core::test_utils` deterministic models, no live APIs:
- wiring: builder plus approver gives `HumanChannel::Delegate`, and a prompt reaches the approver
  (`fontaine-sdk/tests/permissions_delegate.rs` covers the delegate semantics);
- `build()` rejects a host tool named like a descriptor row, a built-in or `mcp__*`, duplicating
  another host tool, or outside `[A-Za-z0-9_-]`, with `EmbedError::Config` (unit test);
- a host callback hook can tighten but its allow is ignored;
- a host tool call is guardian-judged in `auto` and prompts in `manual`;
- a recorded `fontaine -p` run and an
  embedded run of the same scripted model produce the same event sequence, including with
  `max_turns`, `max_budget_usd` and `json_schema` set; the `fontaine -p` side is a subprocess, so
  both sides use the same wiremock `Endpoint` script (`rust/fontaine/tests/common/mod.rs`);
- interrupt ends a turn with the same `result` as Claude SDK mode's (`interrupted_outcome`, router; the CLI cannot interrupt), and a turn after an interrupted turn
  runs normally.
The existing headless, resume, SDK-compat and cassette suites are the regression net for the
assembly move.
