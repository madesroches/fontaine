# fontaine-tui agents tests: slow and leaking Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/723

## Overview

Four `fontaine-tui` `agents` tests exceed nextest's 60 s slow threshold, and some tests are
reported as leaking. Make the four finish in a few seconds and leave no test reported as
leaking.

## Current State

- The four tests (`rust/fontaine-tui/tests/agents.rs`) are the ones that call
  `fontaine_testkit::example_bin` (`touch_marker`, `repo_mcp_server`) in the test body or its
  setup: `a_repository_mcp_json_server_is_approved_at_the_prompt_and_listed_with_its_tool_count`,
  `a_server_in_the_users_own_settings_starts_without_asking`,
  `a_hook_the_repository_defines_does_not_run_when_the_user_declined_trust`,
  `a_project_local_server_waits_for_workspace_trust_too`.
- `example_bin` (`rust/fontaine-testkit/src/lib.rs`) runs a nested `cargo build -p <pkg>
  --example <name>` once **per test process**. nextest runs every test in its own process, so each
  of these tests spawns its own nested cargo. They serialize on cargo's build-directory lock, and
  the first one compiles the `fontaine` example (`repo_mcp_server`) when the outer
  `cargo nextest run -p fontaine-tui` did not build it (a per-package run builds only that
  package's targets).
- The >60 s in the issue comes from a cold or stale target dir, where the nested compile is long. Every test that calls
  `example_bin` (nine in `agents.rs`, plus `hooks.rs` and `common/mod.rs`) queues on cargo's lock;
  which four exceed 60 s depends on timing.
- `rust/.config/nextest.toml` sets only `slow-timeout`; there is no setup script.
- The leak output was never captured. The first implementation step is to reproduce it.

## Design

Build the example binaries once, before any test process starts, instead of once per test
process.

1. Add a nextest setup script in `rust/.config/nextest.toml`. A script is a single command and CI
   runs on Linux, macOS and Windows, so it is one cross-platform cargo invocation (verified: one
   cargo command takes several `-p` and `--example` flags and builds each example from the package
   that has it):
   `cargo build --profile test -p fontaine-tui -p fontaine --example touch_marker --example repo_mcp_server --example hook_marker --example fontaine_claude_cli_double`.
   Default features and no workspace-wide build, because feature
   unification could make the fingerprints differ from the nested build's. Non-default profiles
   and targets (`--cargo-profile`, `--target`) are out of scope: the script only covers the
   default `cargo nextest run`; otherwise the nested build still works, just slower. Attach the
   script with the filter `package(fontaine-tui)` only. After it, each test's nested `cargo build` in
   `example_bin` is a no-op freshness check and no longer queues behind a compile.
2. Keep `example_bin` as is: it still covers plain `cargo test` and per-package runs, where no
   setup script runs.
3. Leaks: reproduce first (`cargo nextest run -p fontaine-tui` on a cold target dir, recording
   every `LEAK`). Fix what the reproduction shows. Likely causes named in the issue: a child
   process (an MCP server or hook the test started) or a thread still holding stdout/stderr after
   the test returns. If the leaking tests are the ones that start `touch_marker` /
   `repo_mcp_server`, make the test shut the server down rather than silencing the warning in nextest config. MCP
   and hook children already use `kill_on_drop(true)` (`servers.rs` ~659/921, `user_hooks.rs`
   ~104), so a LEAK would come from a piped stderr/stdout handle outliving the test (the stderr
   drain at `servers.rs` ~926; `user_hooks.rs` ~103); diagnose there.

## Implementation Steps

1. Reproduce on a cold build: touch a source file in `fontaine-sdk` (or remove the example
   artifacts from `rust/target/debug/examples`), run `cargo nextest run -p fontaine-tui`, record
   the SLOW and LEAK lines and which tests they name.
2. Add the nextest setup script to `rust/.config/nextest.toml`: the single `cargo build --profile
   test -p fontaine-tui -p fontaine --example ...` command from Design 1, attached with filter
   `package(fontaine-tui)` only. The config shape (nextest 0.9.146) is a `[scripts.setup.<name>]`
   definition with `command` plus a `[[profile.default.scripts]]` entry with
   `filter = 'package(fontaine-tui)'` and `setup = "<name>"`.
3. Re-run the reproduction; confirm the four tests finish in a few seconds and no SLOW remains,
   and that the nested build is a no-op (the exact nested form, `cargo build -v --profile test -p <pkg>
   --example <name>`, shows `Fresh`, no `Compiling`).
4. Diagnose each LEAK from step 1 and fix its cause in the test or in the code it exercises. If
   the cold run shows no LEAK, record that in `## Decisions` as an `AFK:` line and close the leak
   half as not reproducible; do not invent a fix.
5. Re-run the full `cargo nextest run -p fontaine-tui` and `cargo test` (plain, no setup script)
   to confirm both paths still pass.
6. Update the `example_bin` paragraph in `tasks/design/testing.md` ("no-op under CI's workspace
   `cargo test`", "tests never prebuild") to describe the nextest setup script, and the module doc in `rust/fontaine-testkit/src/lib.rs`
   (line 10: `example_bin` is "a no-op under the workspace run", silent on the setup script).
7. Add a `CHANGELOG.md` `[Unreleased]` / Fixed entry citing #723.

## Files to Modify

- `rust/.config/nextest.toml`
- `tasks/design/testing.md`
- `rust/fontaine-testkit/src/lib.rs` (module doc)
- `CHANGELOG.md`
- Whatever the leak diagnosis points at (likely `rust/fontaine-tui/tests/agents.rs`,
  `rust/fontaine-tui/tests/common/mod.rs`)

## Trade-offs

- A setup script vs. changing `example_bin` to share one build across processes with a file lock:
  the lock still serializes the first compile and adds platform-specific code to the testkit; the
  setup script moves the compile out of the tests entirely and costs only config.
- Raising `slow-timeout` was rejected: it hides the problem.

## Documentation

Update the `example_bin` paragraph in `tasks/design/testing.md` (about lines 113-118) to describe
the nextest setup script (implementation step 6).

## Decisions

- AFK: the setup script is attached with filter `package(fontaine-tui)` only; reason: narrowest
  scope that satisfies the issue, other crates have the same pattern and can follow.
- AFK: the setup script is one `cargo build --profile test` command with multiple `-p` and
  `--example` flags (verified to work), since a nextest script is a single command and CI spans
  three OSes; `--profile test` matches `example_bin`; `--cargo-profile`/`--target` runs are out
  of scope (nested build still works, slower).
- AFK: which tests leak is settled during implementation by the step 1 cold-run reproduction: if no LEAK reproduces, the leak half is closed as not reproducible (no invented fix); if one does, its cause is fixed in the test or code, never by silencing nextest's leak detection; reason: most conservative reading of the issue, whose leak output was never captured.

## Testing Strategy

The change is test infrastructure; verification is the nextest runs in the implementation steps
(cold and warm), plus a plain `cargo test -p fontaine-tui --test agents` to confirm the fallback
path. No new unit tests.
