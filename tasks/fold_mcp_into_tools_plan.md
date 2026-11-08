# Fold fontaine-mcp into fontaine-tools Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/485

## Overview

Issue #485 asks whether to merge `fontaine-tools`, `fontaine-agents` and `fontaine-mcp` into
`fontaine-core`. This plan takes the issue's smaller alternative: fold only `fontaine-mcp`, about
690 lines of library code, into `fontaine-tools` as a `mcp` module. The workspace goes from 8
crates to 7. `fontaine-tools` and `fontaine-agents` stay separate, so the compiler still enforces
that tool, agent and MCP code cannot reach the permission pipeline, session state or guardian
wiring.

## Current State

- `rust/fontaine-mcp/` is a library crate (`src/{lib,interaction,register,servers}.rs`, 690
  lines) plus a test (`tests/mcp_registration.rs`), test helpers (`tests/common/{mod,server}.rs`)
  and an example binary (`examples/mcp_double.rs`, a stdio MCP server that the test runs from
  `target/<profile>/examples/`).
- Its only Fontaine dependency is `fontaine-tools` (`error_chain`, `guarded`, `lock`, `risk`,
  `tls`). Its only consumer is `fontaine-core`:
  - `fontaine-core/src/agent.rs:14`: `use fontaine_mcp::Connected;`
  - `fontaine-core/src/agent/mcp.rs:11`: `use fontaine_mcp::{ServerSpec, Transport, connect as connect_server};`
  - `fontaine-core/src/permissions.rs:902`: `fontaine_mcp::REQUIRES_USER_INTERACTION`
  - `fontaine-core/tests/choke_point.rs:7,25,410`: `#[path]` include of
    `fontaine-mcp/tests/common/server.rs`, `connect_transport` and `McpConnectionHandle`
  - `fontaine/examples/repo_mcp_server.rs:4`: `#[path]` include of the same `server.rs`
- Its runtime dependencies beyond what `fontaine-tools` already has: `rig-rmcp` and `rmcp`
  (`futures`, `reqwest`, `rig-core`, `serde_json`, `tokio` and `tracing` are already there).
  Dev-dependencies beyond `fontaine-tools`'s: `rig-agent` and `rig-core` with `test-utils`,
  `rmcp` with `server` and `transport-io`, and `tokio` with `test-util`.
- No tracing target, log filter or doc refers to the `fontaine_mcp` module path. The
  `fontaine.mcp.call` span (asserted in `fontaine/tests/telemetry_otlp.rs:765`) is named
  explicitly, so the move doesn't change it.
- The crate appears in `rust/Cargo.toml` (`[workspace.dependencies]`), `AGENTS.md` (crate list),
  `tasks/fontaine_harness_plan.md` (workspace layout and dependency graph) and
  `docs/architecture.md` (workspace map, crate table, dependency graph). `ci.yml` and `deny.toml`
  don't name it, because the workspace uses `members = ["*"]`.

## Design

Move the crate's contents into `fontaine-tools` and keep the public surface intact, one level
down:

```
fontaine-tools/src/mcp.rs          ← fontaine-mcp/src/lib.rs (module doc + re-exports)
fontaine-tools/src/mcp/interaction.rs
fontaine-tools/src/mcp/register.rs
fontaine-tools/src/mcp/servers.rs
fontaine-tools/tests/mcp_registration.rs
fontaine-tools/tests/mcp_common/mod.rs     ← fontaine-mcp/tests/common/mod.rs
fontaine-tools/tests/mcp_common/server.rs  ← fontaine-mcp/tests/common/server.rs
fontaine-tools/examples/mcp_double.rs
```

- `fontaine_mcp::X` becomes `fontaine_tools::mcp::X` for every re-exported item
  (`REQUIRES_USER_INTERACTION`, `GuardedSink`, `mcp_tool_name`, `McpConnectionHandle`,
  `Connected`, `McpError`, `ServerSpec`, `Transport`, `connect`, `connect_transport`). The
  `register` and `servers` submodules stay `pub`, `interaction` stays private.
- Inside the moved files, `fontaine_tools::…` imports become `crate::…`, and the existing
  `crate::interaction::…` and `crate::register::GuardedSink` imports (`register.rs`, `servers.rs`)
  become `super::interaction::…` and `super::register::…`.
- The test helpers go to `tests/mcp_common/`, not `tests/common/`. `fontaine-tools/tests/common/`
  already exists with a different `mod.rs` that every built-in-tool test pulls in, and the two
  helper sets share nothing.
- `mcp_double` keeps its name. `fontaine-tools/examples/` holds only `fake_lsp.rs`, so nothing
  clashes, and the test's `double_binary()` lookup (`target/<profile>/examples/mcp_double`) is
  unaffected by which crate builds the example.
- The `#[path]` includes in `fontaine-core/tests/choke_point.rs` and
  `fontaine/examples/repo_mcp_server.rs` point at `fontaine-tools/tests/mcp_common/server.rs`.
- `fontaine-tools/Cargo.toml` gains `rig-rmcp` and `rmcp` as dependencies, and the
  dev-dependencies listed in Current State. `rmcp` in `[dev-dependencies]` is written with the
  extra features (`server`, `transport-io`), as `fontaine-mcp` does today. `fontaine-core`'s
  `rmcp` dev-dependency (`server`) is unchanged.
- `fontaine-core/Cargo.toml` drops `fontaine-mcp`; it already depends on `fontaine-tools`.

The resulting dependency graph:

```
fontaine-core     -> fontaine-tools, fontaine-guardian, fontaine-agents
fontaine-agents   -> fontaine-tools
fontaine-tools    -> (no Fontaine crates — never fontaine-core)
```

MCP code still cannot name a `fontaine-core` item, because the `fontaine-tools` boundary holds.
`fontaine-agents` now has `rmcp` in its build graph through `fontaine-tools`, but every binary
already links it through `fontaine-core`, so the set of compiled crates doesn't change.

## Implementation Steps

1. `git mv` the four `src/` files, the test, the two helper files and the example to the paths
   above (`lib.rs` → `src/mcp.rs`, `tests/common/` → `tests/mcp_common/`). Using `git mv` keeps
   rename history.
2. In the moved sources, rewrite `fontaine_tools::` to `crate::`, the existing `crate::interaction::`/`crate::register::` to `super::`, and keep `mod`/`pub mod`
   visibility as it was. In the moved test and example, rewrite `fontaine_mcp::` to
   `fontaine_tools::mcp::` and `mod common;` to `mod mcp_common;` (with the uses that go with it).
   Fix the example's `#[path = "../tests/common/server.rs"]` to `../tests/mcp_common/server.rs`.
3. Add `pub mod mcp;` to `fontaine-tools/src/lib.rs` in alphabetical position.
4. Update `fontaine-tools/Cargo.toml` with the new dependencies and dev-dependencies.
5. Update the consumers: `fontaine-core/src/agent.rs`, `fontaine-core/src/agent/mcp.rs`,
   `fontaine-core/src/permissions.rs`, `fontaine-core/tests/choke_point.rs` and
   `fontaine/examples/repo_mcp_server.rs`.
6. Remove `fontaine-mcp` from `fontaine-core/Cargo.toml` and from `[workspace.dependencies]` in
   `rust/Cargo.toml`, delete the now-empty `rust/fontaine-mcp/` directory, and let Cargo
   refresh `Cargo.lock`.
7. Update the docs and the plan (see Documentation).
8. Run the core CI steps from `rust/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   the Windows clippy target, `cargo test`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
   and `cargo deny check`.

## Files to Modify

- Moved: `rust/fontaine-mcp/{src,tests,examples}/**` → `rust/fontaine-tools/…` (paths above)
- Deleted: `rust/fontaine-mcp/Cargo.toml` (and the directory)
- `rust/Cargo.toml`, `rust/Cargo.lock`
- `rust/fontaine-tools/Cargo.toml`, `rust/fontaine-tools/src/lib.rs`
- `rust/fontaine-core/Cargo.toml`, `rust/fontaine-core/src/agent.rs`,
  `rust/fontaine-core/src/agent/mcp.rs`, `rust/fontaine-core/src/permissions.rs`,
  `rust/fontaine-core/tests/choke_point.rs`
- `rust/fontaine/examples/repo_mcp_server.rs`
- `AGENTS.md`, `tasks/fontaine_harness_plan.md`, `docs/architecture.md`, `CHANGELOG.md`

## Trade-offs

- **Full merge into `fontaine-core` (the issue's proposal).** This removes three crates but turns
  the "tools never reach core" rule from a compiler guarantee into a convention that `pub(crate)`
  can't enforce. It also makes `core` about 92k lines, so every edit to it recompiles more code, and
  Cargo can no longer build `tools` alongside `guardian`. The project's core property is that every
  call passes through the gate, and the plumbing saved by the merge isn't worth losing that boundary.
- **Fold `fontaine-mcp` into `fontaine-core`.** The issue allows either target. `fontaine-tools`
  is the better one: `fontaine-mcp` already depends only on `tools`, its job is the
  gate-wrapping registration that `tools` already does for the built-ins, and keeping it there
  preserves the compiler-enforced boundary for MCP code too. Folding it into `core` would also
  put a second `mcp` module next to the existing `fontaine-core/src/agent/mcp.rs`.
- **Fold `fontaine-agents` too.** It is 7k lines with its own interface seam (`ChildRunner`,
  `RootLedger`), and the crate boundary is what keeps it from reaching core. It stays separate.
- **Do nothing.** This is cheap, but the 690-line crate has its own manifest, features and
  workspace entry and protects nothing that `fontaine-tools` doesn't already protect.

## Decisions

## Documentation

- `docs/architecture.md`: remove the `fontaine-mcp/` line from the workspace map and add "MCP servers" to the map's
  `fontaine-tools/` line, remove its crate
  table row, add MCP servers (transports and wrapped registration, see [MCP servers](mcp.md)) to
  the `fontaine-tools` row, and remove the `fontaine-mcp` lines from the dependency graph.
- `tasks/fontaine_harness_plan.md`: remove `fontaine-mcp/` from the workspace layout and add
  "MCP servers (`mcp`): rig-rmcp wiring, tools registered as `RiskClass::Arbitrary`" to the
  `fontaine-tools/` entry, update the crate dependency graph, and add a dated line at the end of
  `### Project` in `## Decisions`: fold `fontaine-mcp` into `fontaine-tools` (8 → 7 crates); the
  full merge into `fontaine-core` proposed in #485 was declined to keep the compiler-enforced
  tools/agents boundary.
- `CHANGELOG.md`: a `### Changed` entry under `## [Unreleased]`: #485, `fontaine-mcp` folded into
  `fontaine-tools` as the `mcp` module (8 → 7 crates).
- `AGENTS.md`: drop `fontaine-mcp` from the crate list under Project layout.
- `docs/mcp.md` names no crate, so it is unchanged.

## Testing Strategy

The change only moves code, so the existing tests are the coverage:
- `fontaine-tools/tests/mcp_registration.rs` (moved unchanged apart from paths) still exercises
  registration, gating, `requiresUserInteraction` and the stdio environment scrub through the
  `mcp_double` example.
- `fontaine-core/tests/choke_point.rs` still checks that MCP tools register behind the gate as
  `Arbitrary`.
- `fontaine/tests/telemetry_otlp.rs` still checks the `fontaine.mcp.call` span name.
- The unit tests in `interaction.rs`, `register.rs` and `servers.rs` move with their modules.
- `cargo doc` with `-D warnings` catches broken intra-doc links to the old paths.

No new test is needed. The tools/core boundary that the issue wanted a lint for is still a crate
boundary, and Cargo rejects any dependency cycle.
