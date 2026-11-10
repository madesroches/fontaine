# Example-Bin Test Helper Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/552

## Overview

Integration tests in four crates spawn helper binaries that are Cargo examples (`hook_marker`,
`repo_mcp_server`, `mcp_touch_marker`, `fontaine_fake_lsp`, `fontaine_claude_cli_double`,
`claude_cli_double`, `hook_double`, `fake_lsp`, `mcp_double`, `touch_marker`) and locate them by
path under `target/<profile>/examples/`. A workspace-wide `cargo test` builds every member's
examples, so CI is fine; a per-package run such as `cargo test -p fontaine-tui` builds only that
package's targets and silently uses whatever `fontaine` example binaries happen to be on disk. When
stale, tests fail with timeouts far from the cause (seen in #543: a stale `repo_mcp_server` listed
3 tools instead of 4, a stale `hook_marker` never fired). This plan adds one shared test helper,
`example_bin(package, name) -> PathBuf`, that builds the requested example on first use per test
process, and routes every lookup through it.

## Current State

Eighteen lookup sites in sixteen test files derive `target/<profile>/examples/<name>[.exe]` from
`std::env::current_exe()` (strip `deps/<test>`); most assert the file exists:

- `rust/fontaine/tests/`: `claude_subscription.rs:20-35` (`double()`, `fontaine_claude_cli_double`),
  `doctor_cmd.rs:6-19` (`example()`, `repo_mcp_server`), `lifecycle_hooks_headless.rs:11-24`
  (`example()`, `hook_marker`), `lsp.rs:9-22` (`example()`, `fontaine_fake_lsp`), `mcp_cmd.rs:7-20`
  (`example()`, `repo_mcp_server`, `mcp_touch_marker`), `parallel_agents.rs:1119-1131`
  (`hook_example()`, `hook_marker`), `project_executables.rs:9-22` (`example()`, `repo_mcp_server`,
  `hook_marker`), `telemetry_otlp.rs:697-710` (`example()`, `repo_mcp_server`).
- `rust/fontaine-core/tests/`: `common/claude_cli.rs:14-29` (`double()`, `claude_cli_double`),
  `lifecycle_hooks.rs:32-45` (`double()`, `hook_double`), `user_hooks.rs:26-39` (`double()`,
  `hook_double`).
- `rust/fontaine-tools/tests/`: `lsp.rs:24-34` (`fake_lsp()`, `fake_lsp`),
  `mcp_registration.rs:260-270` (`double_binary()`, `mcp_double`).
- `rust/fontaine-tui/tests/`: `agents.rs:189-201` (`touch_marker()`, `touch_marker`) and
  `agents.rs:245-256` (`repo_mcp_server()`, `repo_mcp_server`), `hooks.rs:10-22` (`touch_marker()`,
  `touch_marker`) and `hooks.rs:93-105` (`hook_marker()`, `hook_marker`), `common/mod.rs:704-718`
  (`claude_double()`, `fontaine_claude_cli_double`).

Each duplicated helper differs only in the example name and error message. The consuming test crate
is not always the example's package: `fontaine-tui` tests spawn `fontaine` and `fontaine-tui`
examples; `fontaine-core` tests spawn `fontaine-core` examples. `tasks/design/testing.md:114-115`
documents the manual prebuild workaround this plan removes.

## Design

A new dev-only workspace crate, `rust/fontaine-testkit` (picked up automatically by the workspace's
`members = ["*"]`), exposing:

```rust
pub fn example_bin(package: &str, name: &str) -> PathBuf
```

Internals (std-only, no new dependencies):

- A process-wide `static CACHE: OnceLock<Mutex<HashMap<(String, String), PathBuf>>>`; the first call
  for a `(package, name)` pair runs the build under the lock, later calls and other threads reuse
  the path. Builds after the first are fast no-ops.
- The test binary path gives everything needed: `current_exe()` is
  `target/<profile>/deps/<test>` (with an optional `<triple>` segment before `<profile>`). Strip
  `deps/<test>` to get the profile dir; the examples dir is its sibling `examples/`.
- Run `$CARGO build -p <package> --example <name>` — Cargo sets `CARGO` for test processes (fall
  back to `cargo` on the rare harness that doesn't). `current_dir` is the helper crate's
  `env!("CARGO_MANIFEST_DIR")` (inside the workspace, so package selection and the workspace target
  dir resolve). Pass `--target-dir <dir>` derived from the exe path (strip the optional triple and
  the profile segment) so a custom `CARGO_TARGET_DIR` still writes where the test looks. When a
  triple segment is present, also pass `--target <triple>`: a triple from `CARGO_BUILD_TARGET` or
  config is inherited by the child, but an explicit command-line `--target` is not, and without the
  flag the nested build would place the example where the helper does not look.
- Profile: when the profile dir is named `debug`, omit `--profile` (dev and test profiles share the
  `debug` output dir); otherwise pass `--profile <dir-name>` (covers `release` and custom profiles,
  whose dir name equals the profile name). Implementation should check with one cheap build whether
  `cargo build --profile test` is accepted for the `debug` case — `cargo test` compiles examples
  under the test profile — and use it if so, to avoid any rebuild churn when alternating between
  workspace-wide and per-package runs. Either way the result is correct; this only affects whether
  an already-fresh example is rebuilt once.
- Non-zero exit panics with cargo's combined stdout/stderr, so a build failure surfaces at the call
  site instead of as a timeout later.
- Return `<profile_dir>/examples/<name><EXE_SUFFIX>` (`std::env::consts::EXE_SUFFIX`).

Call sites become one-liners, e.g. `example_bin("fontaine", "hook_marker")`, keeping each file's
existing return type (`String` via `to_string_lossy()` where the caller wants a string).

`[workspace.dependencies]` gains `fontaine-testkit = { path = "fontaine-testkit" }`, and the four
crates add it under `[dev-dependencies]` (`.workspace = true`, matching convention).

## Implementation Steps

1. Create `rust/fontaine-testkit/`: `Cargo.toml` (workspace-inherited metadata, no dependencies) and
   `src/lib.rs` with `example_bin` plus unit tests for the pure path/profile-derivation helpers
   (feed synthetic exe paths: with/without triple, debug/release/custom profile dirs).
2. Add `fontaine-testkit` to `[workspace.dependencies]` in `rust/Cargo.toml`.
3. Add `fontaine-testkit.workspace = true` to `[dev-dependencies]` of `fontaine`, `fontaine-core`,
   `fontaine-tools`, `fontaine-tui`.
4. Replace the eighteen lookup sites listed in Current State with `example_bin(...)` calls, deleting
   the per-file helpers. Package per site: `fontaine` for the `fontaine/tests` files and for the
   `fontaine-tui` tests' `repo_mcp_server`/`hook_marker`/`claude_double` sites, `fontaine-core` for
   `common/claude_cli.rs`/`lifecycle_hooks.rs`/`user_hooks.rs`, `fontaine-tools` for
   `lsp.rs`/`mcp_registration.rs`, `fontaine-tui` for `touch_marker`.
5. Update the now-stale "prebuild it when running a test that uses it alone" doc comments in
   `rust/fontaine/tests/claude_subscription.rs:2`, `rust/fontaine-core/tests/common/claude_cli.rs:1-2`,
   and `rust/fontaine-core/tests/claude_subscription.rs:2` (drop the prebuild clause from all three).
6. Update `tasks/design/testing.md` (the prebuild passage at lines 114-115) to document the helper:
   tests never prebuild; `example_bin` builds on first use.
7. Add a one-line `fontaine-testkit` entry (dev-only test helper crate) to the crate enumerations:
   the crate list in `AGENTS.md`, the master plan `tasks/fontaine_harness_plan.md` §Workspace layout
   tree and dependency graph, and the crate tree/table in `docs/architecture.md`.
8. Run the gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
   from `rust/`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, then the per-package
   runs from Manual Verification.

## Files to Modify

- New: `rust/fontaine-testkit/Cargo.toml`, `rust/fontaine-testkit/src/lib.rs`.
- `rust/Cargo.toml` (workspace dependency).
- `rust/fontaine/Cargo.toml`, `rust/fontaine-core/Cargo.toml`, `rust/fontaine-tools/Cargo.toml`,
  `rust/fontaine-tui/Cargo.toml` (dev-dependencies).
- The sixteen test files listed in Current State.
- `tasks/design/testing.md`.
- `AGENTS.md`, `tasks/fontaine_harness_plan.md` (§Workspace layout), `docs/architecture.md` (crate
  enumerations).

## Trade-offs

- **New dev-only crate vs a module in `fontaine-tools`**: `fontaine-tools` is already a regular
  dependency of all four crates, so a `fontaine_tools::test_bins` module would need no manifest
  changes. But it would ship cargo-spawning test-only code in a production library; a dev-dependency
  keeps the intent explicit and the shipped artifact untouched, at the cost of one small crate and
  four one-line dev-dependency entries.
- **Hand-rolled vs the `escargot` crate**: escargot implements exactly this pattern, but it is a new
  pinned dependency for ~60 lines of std-only code (path derivation plus one `std::process::Command`).
- **Build-on-first-use vs an mtime staleness check**: rejected in the issue — the examples also
  depend on `fontaine-core` and other crates, so source mtimes alone cannot prove freshness.
  Delegating freshness to cargo is exact.
- **Duplicating the helper into each consuming crate**: rejected in the issue — four copies of the
  same cargo invocation.

## Documentation

`tasks/design/testing.md`: replace the prebuild workaround passage with the helper's contract
(builds on first use per test process, panics with cargo's output on failure, CI's workspace run is
a no-op).

Crate enumerations: add the `fontaine-testkit` one-liner to `AGENTS.md`'s crate list, the master
plan's §Workspace layout tree and dependency graph, and `docs/architecture.md`'s crate tree and
crate table.

## Testing Strategy

- Unit tests in `fontaine-testkit` for the pure derivation helpers (exe path → profile dir, profile
  dir → profile flag, target dir with and without a triple segment, and `--target` forwarding when
  a triple segment is present). These are constructed-input tests, no process spawning.
- The eighteen converted integration tests are themselves the coverage: under the workspace run the
  helper's nested build is a no-op and every test behaves as before; under a per-package run the
  helper rebuilds what is stale. No test hits a live API.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` from
  `rust/`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, then the per-package runs
  from Manual Verification.

## Manual Verification

The issue's "done when" is a workflow property no unit test can pin — cargo's own freshness
resolution is the mechanism under test, and simulating staleness inside a test would fight the very
cargo invocation the helper relies on. Checked by hand once:

1. `touch` a comment into `rust/fontaine/examples/hook_marker.rs`, then run
   `cargo test -p fontaine-tui --test hooks` — passes with no manual `cargo build --examples`,
   because the helper rebuilds the edited example.
2. `rm rust/target/debug/examples/hook_marker rust/target/debug/examples/repo_mcp_server*`, then run
   `cargo test -p fontaine-tui --test agents` and `cargo test -p fontaine --test mcp_cmd` — the
   helper rebuilds the missing binaries and the suites pass.
3. `cargo test -p fontaine-core --test user_hooks --test lifecycle_hooks` and
   `cargo test -p fontaine-tools --test lsp --test mcp_registration` — pass from a cold examples
   dir.

## Open Questions
