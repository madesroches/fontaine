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
or reads on docs.rs. The current
`fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` become **modules** of it:

- `fontaine_sdk::core`, `fontaine_sdk::tools`, `fontaine_sdk::agents`, `fontaine_sdk::guardian` hold the existing source trees
  unchanged (`core/src/*` -> `sdk/src/core/*`, and so on).
- The embedder-facing facade (`SessionBuilder`, `Session`, `Event`, `HostTool`, and curated
  re-exports of `Approver`, `PermissionMode`, `CallbackHook`, `HookTransport`, `HookEvent` and the like) sits at the crate root
  and in a few top-level modules (`fontaine_sdk::permissions`, `::config`, ...), so the docs lead
  with the facade. The module trees stay `pub` for the binary, the TUI and the evals, but only the
  root facade is the documented, semver-stable surface; the rustdoc landing page says so, and the
  trees are `#[doc(hidden)]`-free so links in the facade's docs resolve.
- The merge is mechanical: `git mv` the trees, `fontaine_core::` -> `fontaine_sdk::core::` (and
  likewise) across the roughly 340 files that import them, and the four `Cargo.toml` dependency
  lists fold into one. It first rewrites each moved tree's own `crate::X` paths (code and intra-doc links) to
  `crate::<tree>::X` (e.g. core's `use crate::tools::{...}` in `agent/build.rs` becomes `crate::core::tools`), and only
  then every `fontaine_{core,tools,agents,guardian}::` path inside the four moved trees
  (core uses `fontaine_guardian::`; agents uses `fontaine_tools::` in registry/spawn/tasks/tree) to
  `crate::{core,tools,agents,guardian}::`, so the second pass's output is not rewritten again;
  moves `fontaine-core/prompts/` to `fontaine-sdk/prompts/core/` (`include_str!("../../prompts/core/...")` in
  `core/prompt.rs`, `core/goal.rs`), `fontaine-guardian/prompts/` to `fontaine-sdk/prompts/guardian/` and
  `fontaine-guardian/data/` to `fontaine-sdk/data/` (`include_str!("../../prompts/guardian/...")` and
  `"../../data/guardian_models.toml"` in `guardian/{critique,env_scan,evasion,profile,scanner,models}.rs`); fixes the `../../../research/`
  include in core's `usage_limit.rs` for the extra directory level; renames and re-accepts the 7
  insta snapshots in `fontaine-guardian/src/snapshots` (`fontaine_guardian__prompt_builder__tests__...`);
  renames the colliding integration tests (`choke_point.rs`, `lsp.rs`, `network.rs`, `scanner.rs`,
  and the `common/` dirs, e.g. `core_common/`, `tools_common/`); repoints the `fontaine-guardian/`
  paths hard-coded in `DEFAULT_MODELS_FILE` (`fontaine-evals/src/runner.rs`, now
  `fontaine-sdk/data/guardian_models.toml`), `evals/README.md`, `docs/evals.md` and `docs/guardian.md`
  (the prompt and data files at their new `fontaine-sdk/{prompts/guardian,data}/` paths);
  fixes the `example_bin("fontaine-core"|"fontaine-tools", ...)` call sites and the `#[path]`
  includes of the other crates' tests and examples; and re-points the `pg2` feature from
  `fontaine/Cargo.toml` and `fontaine-evals/Cargo.toml`; it also rewrites crate names in code and doc comments (`tools/gate.rs:2`, `todo.rs:55`, `skill.rs:15`, `agents/runner.rs:2`, the `cargo test -p fontaine-guardian --features pg2` line in `scanner/pg2.rs`) and removes the `#[cfg(feature = "composition")]` gates (guardian `lib.rs`, `tests/composition.rs`), since core needs composition unconditionally. It lands as its own commit, ahead of any
  behavior change; the suites pass with only the listed renames and snapshot re-accepts, no assertion changes.
- The Rune scripting dependency of `fontaine-guardian` becomes a non-optional dependency of the SDK: core uses
  `guardian::composition` unconditionally, so a feature to turn it off could not compile. The guardian's `pg2` feature
  folds into the SDK's existing `pg2` feature. The guardian is no longer a separately liftable crate; a consumer
  wanting only the guardian pins an older `fontaine-guardian` release or vendors the module. What stays enforced is
  module independence: `guardian/**` names no `crate::{core,tools,agents}` path, and `tools/**` and `agents/**` name
  no `crate::core` path, checked by a test that scans those source trees. `AGENTS.md`'s lift-out rule is reworded to
  "keep the SDK's `guardian` module dependency-light and free of `core`/`tools`/`agents` imports (test-enforced)".
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
    pub fn include_partial_messages(self, on: bool) -> Self;  // default false; maps to Options.include_partial_messages, which gates `assistant_delta`
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
    pub fn send(&mut self, prompt: impl Into<String>) -> Turn<'_>;   // Turn: Stream<Item = Result<Event, EmbedError>>; ends after its `result`
    pub fn interrupt_handle(&self) -> InterruptHandle;
    pub async fn close(self) -> Result<Option<AfkReport>, EmbedError>; // SessionEnd hooks, scratch cleanup; the trailing afk_report; Err if the task ended non-zero
}
// Drop without close(): cancels, closes the input, the task ends on its own; no SessionEnd hooks.
// Session::send feeds a long-lived run_session task; see Session over the run loop.
pub enum Event { /* the headless-v1 events, typed; serde-serializable to the identical JSON */ }
```

Decisions that shape this surface:

- **Event stream = `headless-v1`.** `Event` is a typed, `#[non_exhaustive]` enum with one variant
  per output kind (12 of the 13 kinds of the closed `oneOf`: all except `user_input`, the input
  line, which `HeadlessV1Sink` never prints; no notice kind). It lives in
  `fontaine_sdk::events`, together with the whole `SessionEvent` -> `Event` conversion (assistant,
  assistant_delta with the `include_partial_messages` gate, tool_call, tool_result, model_call and
  `gate_decision_event`, besides init/result/usage/goal/afk). `HeadlessV1Sink` and the embedded
  channel sink both call it, and `HeadlessV1Sink` becomes a thin writer of
  `serde_json::to_string(&event)`. `Event` serializes untagged (or by hand-written `Serialize`),
  each struct declaring its payload fields first and `type`, `schema_version`, `session_id` last,
  matching `event()` in `headless.rs` (serde_json `preserve_order`); a `#[serde(tag = "type")]`
  enum would write the tag first and fail the key-order unit test. The schema version stays `1`; embedders see `SCHEMA_VERSION` re-exported. The existing
  `headless_contract` tests (which validate every line against `headless-v1.schema.json`) and a
  unit test that builds each of the 12 `Event` variants from fixed values and asserts
  `serde_json::to_string` equals a literal string captured from today's builder output (before step 2
  deletes the `json!` builders) pin the key order.
- **Approvals.** `build()` with an approver sets `HumanChannel::Delegate`; without one it sets
  `Absent` backed by `DenyAll`, as the CLI does (every would-prompt is a denial). `Present` is not exposed: it means "a person at a
  terminal". The approver is reached only where the TUI would ask a person, after the floor, hooks,
  rules, mode and guardian, and is never offered "always allow" (existing `Delegate` semantics).
- **Host tools.** `HostTool` is a trait (`name`, `description`, JSON-schema `parameters`, `fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<String, ToolExecutionError>>` (object-safe for `Arc<dyn HostTool>`, like `Approver::approve` and `HookTransport::call`), with rig's `ToolExecutionError` re-exported from the SDK, the error type `guard_callback` already works in). A host tool
  declares no risk: risk lives in the descriptor table, never on the tool, so every host tool is
  registered as `RiskClass::Arbitrary`, like SDK MCP tools. Each host tool is wrapped by `guard_callback` (as MCP tools are), which gains a
  `ToolSource` parameter, at `RiskClass::Arbitrary`, and pushed into the registry as a `Registered`
  beside the built-ins, so every call still passes the gate. `Registered.name` is `&'static str`;
  `build()` leaks each host tool's name once (a bounded, per-tool `Box::leak`) and the retained
  setup keeps that `&'static str`, so plugin reloads reuse it. `ToolSource` gains a `Host` variant
  (`"host"` on telemetry's `tool_source`); the `hide_mcp_names` redaction applies to `Mcp` only, so
  host tool names, the embedder's own, are shown. A host tool is never on the read-only fast path
  (same as MCP tools). The pipeline branches on tool name (the protected-path floor for
  `edit`/`write`/`notebook_edit`, `descriptor::find` for path-free, `afk_report.rs`,
  `spawn_changes_state`), so `build()` rejects with `EmbedError::Config` a host tool whose name
  matches a descriptor row or the `mcp__` prefix, or duplicates another host tool, or any name outside `[A-Za-z0-9_-]`, empty, or longer than 64 characters. Host tools keep bare names, so they are not addressable by permission rules (`parse_rule` accepts only descriptor rows and `mcp__` names), `allowed_tools`/`disallowed_tools`, agent-type `tools` lists or hook matchers: `build()` rejects a host-tool name in `allowed_tools` or `disallowed_tools` with `EmbedError::Config`, `builtin_tools` entries are not validated against the descriptor table (the list must name each host tool to keep it), and `hook()` and the approver are the ways to tighten a host tool. The registry change is
  additive: a `host_tools: Vec<(&'static str, Arc<dyn HostTool>)>` field on `AgentSetup` (the name leaked once in
  `build()`), kept in the retained setup so plugin reloads rebuild it without leaking again, and subject to `root_tools`/toolset cutting like built-ins, so
  a subagent never has a host tool its parent lacks.
- **No way to loosen security.** The builder exposes no guardian-off switch, no floor override, no
  `bypass` mode and no project-scope loosening. Project settings still load as tighten-only; user
  scope is read from the supplied `config_dir`.
- **Embedded session settings.** An embedded session takes headless semantics: `headless = true`,
  `scheduling = false`, `confirm_project_servers = false`, and `project_executables` follows
  `[headless] require_trust` and the trust store. `PermissionMode::Afk` maps as the CLI's
  `session_mode` does: `Auto` plus `setup.afk = true`.
- **Stability.** The SDK starts at `0.x`, `publish = false` like the workspace (`publish.workspace`), and the
  facade is `#[non_exhaustive]` on enums.
- **Errors.** `EmbedError` is a typed enum only where an embedder dispatches (`Config`, `Session`,
  `Credentials`, `Model`, `Runtime`); each carries its source (see AGENTS.md error handling). The SDK
  writes nothing to stderr itself; every line goes through the approver's `warn`, whose trait default writes to stderr unless the embedder overrides it (`DenyAll`'s defaults apply when no approver is supplied): the assembly warnings reach it from `build()`, the run loop's lines through the embedded channel sink, and the PromptGuard download message in `scanner_stage`
  through the `StartParams` notice callback the embedded session sets.
- **Builder validation.** `build()` applies the CLI's checks as `EmbedError::Config`: `max_turns > 0`,
  `max_budget_usd` finite and non-negative, `json_schema` a JSON object convertible to a `Schema`.

### Assembly extraction

The pure parts of `prepare_session` (roots, scratch, memory root, plugin load, extensions, model
resolution, scanner start, `requested_mode`/`session_mode`, the project-executables trust check from
`run()`, `AgentSetup` construction) move into `fontaine-sdk` as functions
over plain parameter structs, returning `Result<_, EmbedError>` and warnings as data. The CLI keeps
only flag-to-params translation and `eprintln!`/exit-code mapping. Assembly returns startup warnings as data (`prepare_session`'s warnings, and the trust check's "workspace is not trusted..." line); the CLI prints them to stderr as today, and `SessionBuilder::build` sends every returned warning to the approver's `warn` (`DenyAll`'s stderr default when there is no approver), covered by a unit test. Moves are mechanical; behavior
is held by the existing `headless_*`, `resume`, `claude_sdk_compat` and `cassettes` tests, which must
stay green with no assertion changes. The headless run loop (`run_session`, `run_one`, `run_attempt`, settle, cancel,
`EventSink`, `InputSource`) moves too, so `Session` and the CLI share the model-list fall-through,
usage-limit wait, budget stop, `--json-schema` output, AFK stop, stop-hook continuation, goals and
interrupt.

`Options` moves with the loop, holding SDK-owned, clap-free input/output format enums that the CLI
maps from its `InputFormat`/`OutputFormat`. The `pub(crate)` items `claude_compat` imports
(`EventSink`, `ResultInfo` with its private `goal`/`afk` fields, `Outcome` with its `pub(crate)` fields, `Subtype`,
`Usage` with its `pub(crate)` fields (`claude_compat` reads `info.usage.main`), `session_cost`, `main_usage`, `Cancel`, `PauseObserver`, `InputSource`, `Options`, `run_session`) become
public in the SDK tree; the `#[cfg(test)] pub(crate)` `interrupted_for_tests` helper is replaced by a public constructor. Every `eprintln!` in the moved loop (the `announce()` scanner/facts/guardian-unavailable lines and
the AFK `set_mode` error line) goes through a new `EventSink` method for these lines.
`HeadlessV1Sink` and the Claude SDK mode sink write them to stderr exactly as today, so CLI and SDK
mode output is unchanged; the embedded channel sink forwards them to the approver's `warn`.

The startup steps of `start_headless`/`run_headless` that `prepare_session` does not cover move
too, and `SessionBuilder::build` runs them: `runtime.connect_mcp(&[])` after `AgentRuntime::new`
(so configured MCP servers connect), the `guardian_login` lookup that `run_session` takes as a
parameter, and the refusal of `max_budget_usd` when the model has no known price (an `EmbedError`
at `build`). `build()` also returns `EmbedError::Credentials` when `guardian_login` is `Some` and the
mode is Auto/Afk (the check `run_session` makes first, which otherwise returns 1 with empty `Turn`
streams). The staged-plugin swap (`plugin_auto_update::apply_staged`, run by `prepare_session` when
`!cli.print`) stays in the CLI, in the binary crate, and runs before the SDK's plugin load (interactive only).

The assemble functions return the `AgentRuntime` and `guardian_login`, and take a hook that edits the
`AgentSetup` before `AgentRuntime::new` (the `tweak` of `start_headless`, which `claude_compat::run`
uses to set the approver, `human_channel`, `callback_hooks`, `host_mcp` and `root_tools`).

### Session over the run loop

`run_session` is a pull loop holding `&mut AgentRuntime` for the whole session, so the spawned task
owns the runtime and returns it with the exit code when it finishes; `close()` takes both from the
join handle before `fire_session_end`. A task that ends with a non-zero code surfaces as an
`Err(EmbedError::Runtime)` item on the active `Turn` stream and as `Err` from `close()`. The loop records the
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

Turn lifecycle: one persistent event channel feeds Turns in order. `init` is delivered as the first item
of the first Turn; each Turn ends after its `result`; events emitted between turns (`usage_paused`/`usage_resumed`)
are buffered into the next Turn; the trailing `afk_report` is not on any Turn but returned by `close()`. A Turn
dropped early drains its remaining events up to its `result`, so the next Turn starts clean. Once the task has
exited (e.g. `run_session` ending with `EXIT_USAGE_LIMIT`) while the `Session` lives, `send` returns a Turn whose
only item is `Err(EmbedError::Runtime)`.

Dropping a `Session` without `close()` triggers the shared `Cancel`, closes the input and moves the `Scratch`
guard into the task so it outlives the run; the task ends on its own. `SessionEnd` hooks fire only through `close()`.

## Implementation Steps

1. Merge `fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` into the new
   `rust/fontaine-sdk` as the `core`, `tools`, `agents` and `guardian` modules (one mechanical commit, see One crate for the SDK);
   repoint every dependent crate and the workspace manifest; the existing test suites must pass
   with only the listed renames and snapshot re-accepts; no assertion changes.
2. Define typed `Event` structs for the 12 output kinds (all but `user_input`) in `fontaine-sdk::events` and move their
   builders (`init_event`, `result_event`, `usage_event`, the goal and AFK helpers, `Outcome`/`Usage`)
   and the whole `SessionEvent` -> `Event` conversion (assistant, assistant_delta with the
   `include_partial_messages` gate, tool_call, tool_result, model_call, `gate_decision_event`)
   out of `fontaine/src/headless.rs`, so `HeadlessV1Sink` and the embedded channel sink share it; make `Outcome`, `Usage` and `Subtype` and their fields public; rewire `HeadlessV1Sink`. First capture today's builder output for each of the 12 kinds as literal
   strings, then add the unit test that serializes each `Event` variant built from fixed values and
   asserts equality with them.
3. Move the headless run loop (`run_session`, `run_one`, `run_attempt`, settle, cancel, `EventSink`,
   `InputSource`, and `Options` with SDK-owned format enums) from `fontaine/src/headless.rs` into
   the SDK; make the remaining items `claude_compat` imports public (the full list is in Assembly extraction); route every `eprintln!` through a new `EventSink`
   method (stderr in `HeadlessV1Sink` and the SDK-mode sink). Tighten
   `Ctx.pending` and the two `wait_for_news` parameters from `&dyn Fn() -> bool` to
   `&(dyn Fn() -> bool + Sync)` so the `run_session` future is `Send` and the task `Session`
   spawns compiles. The CLI calls it. No behavior change in CLI output.
4. Move session assembly (`load_config`, `resolve_roots`, `open_session`, `prepare_session`,
   `resolve_models`, `start_scanner`, `requested_mode`/`session_mode`, the trust check, `AgentSetup`
   construction (the SDK's config loader has no telemetry wrapper; the CLI keeps `telemetry::with_debug_only` around its call), plus the `connect_mcp(&[])` call, the `guardian_login` lookup and the unpriced-model
   `max_budget_usd` refusal from `start_headless`/`run_headless`) from `fontaine/src/lib.rs` into
   `fontaine-sdk::assemble`, whose functions return the `AgentRuntime` and `guardian_login` and keep a
   pre-build `AgentSetup` tweak hook; the CLI, `claude_compat` and the TUI path call them; the
   staged-plugin swap stays in the CLI. `scanner_stage::StartParams` gains an optional notice callback for
   the PromptGuard download line (`scanner_stage.rs:261`, a direct `eprintln!` today): embedded sessions
   set it to the approver's `warn`, the CLI leaves it unset so its stderr line stays. Update the other `StartParams` literal sites
   (`fontaine-sdk/tests/core_scanner_model.rs:67`, pg2 only, and `fontaine-evals/src/live.rs:653`).
   No behavior change.
5. Add `HostTool` and `AgentSetup::host_tools`; keep it in the retained setup and pass it through
   `RegistryParams` in `agent/build.rs` so plugin reloads keep it; wrap every host tool with
   `guard_callback` (new `ToolSource` parameter, new `ToolSource::Host`) at `RiskClass::Arbitrary`;
   validate names in `build()`; update the `guard_callback` callers (`tools/src/mcp/register.rs`,
   `tools/tests/guarded.rs`, at their post-merge paths under `fontaine-sdk`); update the `AgentSetup` literal sites (the SDK `assemble` module,
   `fontaine-sdk/tests/core_common/runtime.rs`, `fontaine-tui/src/ui/tests.rs`,
   `fontaine-tui/tests/common/mod.rs`, ...), and the `RegistryParams` literal sites the new field breaks
   (`fontaine-sdk/tests/core_choke_point.rs`, the `params` helper, and `fontaine-sdk/tests/extensions.rs:437`); extend the choke-point test to cover host tools.
6. Write `SessionBuilder`, `Session`, `Turn`, `InterruptHandle` over the step 4 `assemble` functions (`SessionBuilder::build` calls them) and the
   run loop moved in step 3, not over `drive`: `Session` runs `run_session` on a task with a
   channel-backed, `long_lived()` `InputSource`; events reach each `Turn` through an `EventSink`;
   the task owns the runtime and returns it; `close()` drops the input, awaits the task and takes the runtime back before `fire_session_end`, and returns the trailing `afk_report`; `Drop` cancels and hands the `Scratch` guard to the task (see Session over the run loop); the input source resets
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
  `core/tools.rs`, `core/scanner_stage.rs`, `tools/guarded.rs`, `tools/risk.rs`, `tools/mcp/register.rs` (all under
  `fontaine-sdk/src/`), `fontaine-sdk/tests/guarded.rs` (the `guard_callback` callers) and
  `fontaine-sdk/tests/core_choke_point.rs` (host tools)
- the `scanner_stage::StartParams` literal sites (`fontaine-sdk/tests/core_scanner_model.rs`,
  `fontaine-evals/src/live.rs`) and the `AgentSetup`/`RegistryParams` literal sites
  (`fontaine-tui/src/ui/tests.rs`, `fontaine-tui/tests/common/mod.rs`,
  `fontaine-sdk/tests/core_common/runtime.rs`, `fontaine-sdk/tests/extensions.rs`)
- `docs/telemetry.md` and `tasks/design/observability.md` (new `host` value of `tool_source`)
- `fontaine-evals/src/runner.rs`, `evals/README.md`, `docs/evals.md`, `docs/guardian.md`
  (`fontaine-guardian/` paths)
- `docs/embedding.md` (new), `docs/SUMMARY.md`, `docs/index.md`, `docs/architecture.md`,
  `docs/agent-sdk.md`, `docs/headless.md` (cross-links)
- `tasks/fontaine_harness_plan.md` (Decisions, workspace layout), `CHANGELOG.md`, `AGENTS.md`
  (crate list; the guardian lift-out rule; the `fontaine_tools::error_chain::chain` and `fontaine_tools::lock::{lock, read, write}` paths in Error handling), `CONTRIBUTING.md` (paths into the merged crates), `tasks/design/*.md` and docs that
  name the merged crates

## Decisions

- 2026-11-14: Rust embedding is a typed, in-process facade whose event stream serializes to `headless-v1` unchanged; foreign languages use the existing stdio surfaces, with no third protocol.
- 2026-11-14: (user call) The Rust SDK is a single crate, `fontaine-sdk`; `fontaine-core`, `fontaine-tools`, `fontaine-agents` and `fontaine-guardian` become its modules in this same change. Deviates from the guardian crate's lift-out rule: the guardian is no longer separately liftable (a guardian-only consumer pins an older `fontaine-guardian` release or vendors the module); its independence is kept by a test that forbids `crate::{core,tools,agents}` paths in the guardian module.
- 2026-11-14: Publication is out of scope; the publish set would be `fontaine-sdk` alone.
- 2026-11-14: Embedded approvals use `HumanChannel::Delegate`, not a new channel: the delegate semantics (asked only after the floor, hooks, rules and guardian; no "always allow") are exactly what an embedder needs.
- 2026-11-14: `AgentSetup` stays reachable through the `pub` `core` tree but is not part of the semver-stable root facade.
- 2026-11-14: Provider warnings go through the process-global `providers::warn::set_sink` (latest session wins), so with several embedded sessions they reach the most recently built session's approver.
- 2026-11-14: The SDK crate is named `fontaine-sdk`: Rust crate names are hyphenated, and the issue frames it as SDK parity. The bare name `fontaine` stays the binary's package, so no release plumbing is renamed.
- 2026-11-14: AFK: The guardian is not separately liftable after the merge and has no "default features off" path (core uses `guardian::composition` unconditionally, so Rune is a non-optional SDK dependency); guardian independence is kept by a test forbidding `crate::{core,tools,agents}` paths in `guardian/**` and `crate::core` in `tools/**`/`agents/**`.
- 2026-11-14: AFK: Host tools keep bare names and are not addressable by permission rules, `allowed_tools`/`disallowed_tools`, agent-type `tools` lists or hook matchers; `build()` rejects host-tool names in `allowed_tools`/`disallowed_tools`, and `hook()` and the approver tighten them. Deviates from Claude Code, where custom tools are MCP tools and rule-addressable: extending rule parsing to host tools is left for later.
- 2026-11-14: AFK: One persistent event channel feeds Turns in order: `init` is the first item of the first Turn, each Turn ends after its `result`, between-turn events go to the next Turn, a dropped Turn drains to its `result`, `send` after the task exited yields `Err(EmbedError::Runtime)`, and `close()` returns the trailing `afk_report` as `Result<Option<AfkReport>, EmbedError>`.
- 2026-11-14: AFK: Dropping a `Session` without `close()` triggers the shared `Cancel`, closes the input and moves the `Scratch` guard into the task; `SessionEnd` hooks fire only through `close()`.

## Documentation

New `docs/embedding.md` (choosing a route, builder walkthrough, approver, host tools, security
guarantees the builder cannot loosen; `builtin_tools` must also name every host tool to keep; host tools
cannot be named by permission rules, `allowed_tools`/`disallowed_tools`, agent-type `tools` lists or hook
matchers, and are tightened through `hook()` and the approver; the Turn lifecycle (`init` on the first Turn,
between-turn events on the next, the trailing `afk_report` from `close()`); dropping a `Session` without
`close()` cancels the run and fires no `SessionEnd` hooks; the
`HookTransport` JSON contract; provider warnings reach the most recently built session's approver;
`[telemetry]` export is installed by the binary only (`telemetry::install` in `fontaine/src/lib.rs`),
so an embedded session's tracing goes to the embedder's subscriber).
`docs/telemetry.md` and `tasks/design/observability.md` list `host` as a `tool_source` value. Cross-links from `agent-sdk.md` and `headless.md`; crate
table in `architecture.md`. Docs describe current behavior only.

## Testing Strategy

Unit tests for builder validation (including `max_turns`, `max_budget_usd`, `json_schema`) and defaults (no approver → `Absent`; mode mapping; resume/fork).
Integration tests with `rig_core::test_utils` deterministic models, no live APIs:
- wiring: builder plus approver gives `HumanChannel::Delegate`, and a prompt reaches the approver
  (`fontaine-sdk/tests/permissions_delegate.rs` covers the delegate semantics);
- `build()` rejects a host tool named like a descriptor row or `mcp__*`, duplicating
  another host tool, outside `[A-Za-z0-9_-]`, empty, or longer than 64 characters, or a host-tool name in
  `allowed_tools`/`disallowed_tools`, with `EmbedError::Config` (unit test);
- module boundary: a test scans `src/guardian/**` for `crate::{core,tools,agents}` paths and `src/{tools,agents}/**`
  for `crate::core` paths, and fails on any;
- Turn lifecycle: `init` is the first item of the first Turn; a `usage_paused`/`usage_resumed` pair emitted
  between turns arrives on the next Turn; an AFK run's `afk_report` comes from `close()`; a Turn dropped
  mid-stream leaves the next Turn starting at its own first event; `send` after the task exited
  (`EXIT_USAGE_LIMIT`) yields `Err(EmbedError::Runtime)` at once;
- dropping a `Session` without `close()` cancels the active turn, the task ends, the scratch directory is removed
  after the task ends, and no `SessionEnd` hook fires;
- `hook()` lands in `AgentSetup::callback_hooks` (unit test; the tighten-only, allow-ignored semantics are covered by `fontaine-sdk/tests/permissions_delegate.rs`);
- a host tool call is guardian-judged in `auto` and prompts in `manual`;
- a recorded `fontaine -p` run and an
  embedded run of the same scripted model produce the same event sequence (normalizing
  `session_id`, `duration_ms` and the scratch entry of `workspace_roots`), including with
  `max_turns`, `max_budget_usd` and `json_schema` set; the `fontaine -p` side is a subprocess, so
  both sides use the same wiremock `Endpoint` script (`rust/fontaine/tests/common/mod.rs`) and set the same `include_partial_messages` flag;
- interrupt ends a turn with the same `result` as Claude SDK mode's (`interrupted_outcome`, router; the CLI cannot interrupt), and a turn after an interrupted turn
  runs normally.
