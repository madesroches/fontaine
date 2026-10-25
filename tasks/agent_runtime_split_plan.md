# AgentRuntime Split Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/187

## Overview

`AgentRuntime` (`rust/fontaine-core/src/agent.rs`) keeps the session's start mode and root list
in two public fields that duplicate `SessionState`, whose live values change during a session. It
also builds the whole session in one 241-line `new`, and the root and child agents are assembled
by two separate `AgentBuilder` chains. This plan removes the duplicated state, splits `agent.rs`
into submodules with no function over about 80 lines, and gives root and child agents one shared
place for their common builder configuration. There is no behavior change and no change to the
headless contract schema.

## Current State

- `agent.rs` is 1,492 lines with no tests in the file. `AgentRuntime` (`agent.rs:264`) has 24
  fields, 13 of them `pub`. `AgentRuntime::new` (`agent.rs:430`–`:664`) does state, guardian and
  screen, hooks and pipeline, result stages, extensions, child runner, then calls `build_agent`.
- `pub mode` is `setup.mode` and never changes. The live mode is `SessionState::mode()`
  (`gate.rs:318`), changed by Shift+Tab, `/plan` and the unavailable-guardian "switch to manual"
  answer. Readers of the stale field: `fontaine/src/headless.rs:343` (init event),
  `:347` (suspended rules), `:501` (notice gating), and `record_session_start`
  (`agent.rs:1288`). The TUI already uses `runtime.state.mode()`.
- `pub roots` duplicates `state.scope().roots()`. `add_dir` (`agent.rs:944`–`:974`) updates and
  rolls back both. Other readers: `agent.rs` (`mcp_plan`/`connect_mcp` cwd, `approve_mcp_server`,
  `record_session_start`, `refresh_files`, `rebuild`), `fontaine/src/headless.rs:319`,
  `fontaine-tui/src/ui/commands/reports.rs:18,36`.
- Root agent: `build_agent` (`agent.rs:~1410`) ends in a builder chain with
  `record_content_telemetry`, `CurrentCall`, `SteeringHook`. Child agent:
  `child_runner.rs:~276` has its own chain with `record_content_telemetry` and `CurrentCall`.
  Both compute the "record content" flag from `config.telemetry` separately (`agent.rs:~551`
  and `~1472`).
- Other concerns in the same impl: MCP planning/connecting (`mcp_plan`, `connect_mcp`,
  `approve_mcp_server`, `server_digest`, `server_description`, `McpServerPlan`, `McpOutcome`),
  model and guardian switching and listings (`switch_main_model`, `install_main`,
  `model_listings`, `account_added/removed`, `removable`, `credentials_changed`,
  `set_guardian_model`, `retry_guardian`, `use_guardian`, `install_guardian`,
  `guardian_candidates`, `Switched`, `CredentialEffect`), and session facts
  (`record_session_start`, `refresh_files`, `context_usage`, `clear_conversation`).

## Design

### Single source for mode and roots

- Remove `AgentRuntime::mode` and `AgentRuntime::roots`.
- `AgentRuntime::mode() -> PermissionMode` returns `self.state.mode()`.
- `AgentRuntime::start_mode() -> PermissionMode` returns the mode the session opened in, kept in
  a private `start_mode` field (the one place the start value is needed: the headless `init`
  event and the `SessionStart` ledger record). It is the same value `SessionState` was created
  with, so it is not a second copy of live state.
- `AgentRuntime::roots() -> Vec<PathBuf>` returns `self.state.scope().roots().to_vec()`; a
  `primary_root()` helper (first root) replaces the `self.roots[0]` uses. `add_dir` loses its
  `self.roots.push`/`pop`; its rollback is only `set_scope(previous_scope)`.
- `rebuild` takes the roots from the scope in `state` (`Parts.roots` becomes the scope's roots
  slice, obtained once per rebuild).
- Consumers: `headless.rs` `init_event` reports `start_mode()` for `permission_mode` and uses
  `start_mode()` for the suspended-rules and notice-gating checks (headless never changes the
  mode, so output is unchanged); `reports.rs` uses `runtime.roots()`.
- `record_session_start` records `start_mode()`: it runs at session start, where the start and
  live modes coincide, and `SessionStart` is by definition the opening state.

### Private fields

After the accessors exist, fields that nothing outside `fontaine-core` reads become private
(compiler-driven: make a field private, fix the breakage with an accessor only if the read is
real). Expected: `guardian_profile`, `origin`, `notices`, `scanner_notices` get accessors
(`guardian_profile()`, `origin()`, `notices()`, `scanner_notices()`); `agent`, `state`, `session`,
`models`, `config`, `steering`, `files` stay `pub` where the TUI, headless and the cross-crate
tests construct turns from them directly. The goal is to stop growing the public surface, not to
rewrite every call site.

### Module split

`agent.rs` stays the home of `AgentRuntime`, `AgentSetup`, `SessionOrigin`, the small public
types and the thin accessor methods; the rest moves to submodules of `agent` (files under
`fontaine-core/src/agent/`, `mod` declarations in `agent.rs`, public paths unchanged by
re-exports):

- `agent/setup.rs` — the steps `new` is split into, each a function of at most ~80 lines:
  `session_state(&AgentSetup, &Scope, &AgentTree, ..) -> Arc<SessionState>`,
  `attach_screen(&SessionState, &Config, Profile, &dyn Approver)`,
  `permission_pipeline(..) -> (Arc<PermissionPipeline>, UserHooks)`,
  `result_pipeline(.., hooks) -> (Arc<ResultPipeline>, Option<llm scanner slot>)`,
  `child_runner(..) -> Arc<CoreChildRunner>`, and `build_guardian`/`build_screen`/`agent_limits`
  /`build_agent_types` (moved with their callers, re-exported from `agent`). `new` becomes a
  sequence of these calls plus the final struct literal.
- `agent/build.rs` — `Parts`, `build_agent` (split into `prompt_layers`, `register_tools`,
  `finish_builder`) and the shared builder function below.
- `agent/models.rs` — model/guardian switching, listings, accounts, credential effects, and
  `Switched`/`CredentialEffect`.
- `agent/mcp.rs` — MCP plan/connect/approve and `McpServerPlan`/`McpOutcome`, `server_digest`,
  `server_description`.
- `CurrentCall` stays reachable at `agent::CurrentCall` (re-export if moved to `build.rs`).

Multiple `impl AgentRuntime` blocks across the submodules are fine; private fields are visible
to child modules of `agent`.

### One shared builder configuration

```text
pub(crate) fn apply_common<M>(builder: AgentBuilder<M>, state: &Arc<SessionState>,
                              record_content: bool) -> AgentBuilder<M>
    = builder.record_content_telemetry(record_content).add_hook(CurrentCall { state })
```

Lives in `agent/build.rs`. `build_agent` calls it and then adds root-only configuration
(`SteeringHook`, conversation log, session memory, output schema); `child_runner.rs` calls it
and adds its in-memory `MemoryStore` and `"main"` conversation. The generic bound and the exact
rig typestate (`AgentBuilder` changes type as methods are applied) are settled when
implementing; if one function cannot be generic over the states both chains are in, apply it at
the first step of each chain, which is what makes the common part live in one place.

The "record content" flag becomes one method, `TelemetryConfig::records_content()` (in
`config`), replacing the two hand-written copies, and `reveal_mcp_names`/`hide_mcp_names` keep
using the existing `enabled && log_tool_details` expression through the same place if it is
equally a one-liner there.

## Implementation Steps

1. Add `TelemetryConfig::records_content()` in `fontaine-core/src/config` and use it in
   `new`/`build_agent` and `child_runner`'s `ChildContext::record_content` initialization.
2. Replace `mode`/`roots` fields with `start_mode` (private) plus `mode()`, `start_mode()`,
   `roots()`, `primary_root()`; update every in-crate reader, `add_dir`, `rebuild`,
   `record_session_start`, `refresh_files`; update `headless.rs` and `fontaine-tui` `reports.rs`.
3. Move MCP code to `agent/mcp.rs` and model/guardian code to `agent/models.rs` (pure moves,
   re-exports keep `fontaine_core::agent::{McpServerPlan, McpOutcome, Switched,
   CredentialEffect, build_guardian, build_screen, build_agent_types}` valid).
4. Move `Parts`/`build_agent` to `agent/build.rs`, split it into named steps, add
   `apply_common`, and use it from `child_runner.rs`.
5. Split `AgentRuntime::new` into `agent/setup.rs` steps.
6. Make fields private where no outside reader exists; add the small accessors.
7. Add `too-many-lines-threshold = 80` to the workspace `clippy.toml` (create if absent) and
   `#![deny(clippy::too_many_lines)]` at the top of each `agent` submodule file and `agent.rs`
   (inner attribute on the module file), so the size criterion is enforced by
   `cargo clippy -D warnings`. Split anything it flags that was missed.
8. Docs and CHANGELOG (below); run the full command set from `AGENTS.md`.

## Files to Modify

- `rust/fontaine-core/src/agent.rs` (shrinks; keeps struct, setup types, accessors)
- `rust/fontaine-core/src/agent/{setup,build,models,mcp}.rs` (new)
- `rust/fontaine-core/src/child_runner.rs` (use `apply_common`)
- `rust/fontaine-core/src/config/` (`records_content()`)
- `rust/fontaine/src/headless.rs` (`start_mode()`, `roots()`)
- `rust/fontaine-tui/src/ui/commands/reports.rs`, `rust/fontaine-tui/src/lib.rs` (field → accessor
  where privatized)
- `rust/clippy.toml` (line-count threshold)
- `CHANGELOG.md`

## Trade-offs

- **Keep `mode` as a field named `start_mode`, versus deriving the start mode from the ledger or
  an `Option` in `SessionState`.** A private field set once at construction is the smallest
  change and cannot drift into a "current" reading because the only public name says "start".
- **Generic `apply_common` versus a shared builder factory.** A factory returning a fully
  configured `AgentBuilder` would have to take the memory/conversation differences as
  parameters and is a bigger interface; a function that applies just the common hooks keeps root
  and child differences visible at their own call sites.
- **Making every field private.** The TUI, headless and the cross-crate tests build turns from
  `agent`/`state`/`session`/`models`/`config`; hiding them would need a wide accessor surface
  for no safety gain, so only the fields that mirror other state or that nobody reads outside
  core are closed.

## Documentation

- `docs/architecture.md`: check whether it names `AgentRuntime` fields or `agent.rs`; update the
  module description if so. No user-facing docs change (no behavior or schema change).
- `CHANGELOG.md`: one line under the unreleased section.

## Testing Strategy

- Existing suites must stay green unchanged except for renamed accessors: `cargo test`, the
  headless contract/schema tests in `fontaine/tests`, the TUI tests, and the fixture evals.
- New unit/integration tests in `fontaine-core/tests` using the existing `built.runtime` rig:
  - `mode()` follows `state.set_mode`/the guardian-unavailable switch while `start_mode()` keeps
    the opening value (pins the stale-field bug class).
  - `add_dir` makes `roots()` include the new root, and when `rebuild` fails the scope and
    `roots()` both return to the previous list (single source rolls back with the scope).
  - `apply_common` hooks: a root agent and a child agent both install `CurrentCall`, observable
    by a dispatch recording the current call id in `state` for a root and a child call.
- `cargo clippy --all-targets -- -D warnings` enforces the ~80-line limit through the lint above.
- Headless contract: the existing init-event schema test confirms `permission_mode` and
  `workspace_roots` are unchanged.

## Open Questions

- Whether `AgentBuilder`'s typestate allows one generic `apply_common` for both chains (see
  Design); the fallback is stated there.
