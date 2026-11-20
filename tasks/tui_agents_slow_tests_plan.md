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
- Measured on a warm tree: run together, three of the four took about 10 s each (lock queueing
  behind a nested build); run alone, one took 0.9 s; the whole `agents` binary takes 4.7 s and
  the whole crate 42 s with no SLOW and no LEAK lines. The >60 s in the issue needs a cold or
  stale target dir, which is where the nested compile is long. The slow set is exactly the set
  of tests that call `example_bin`, which is the evidence for this cause.
- `rust/.config/nextest.toml` sets only `slow-timeout`; there is no setup script.
- The leak output was never captured. The first implementation step is to reproduce it.

## Design

Build the example binaries once, before any test process starts, instead of once per test
process.

1. Add a nextest setup script in `rust/.config/nextest.toml` that runs
   `cargo build --workspace --examples` (or the specific examples the tests need) and attach it
   to the default profile for all tests. After it, each test's nested `cargo build` in
   `example_bin` is a no-op freshness check and no longer queues behind a compile.
2. Keep `example_bin` as is: it still covers plain `cargo test` and per-package runs, where no
   setup script runs.
3. Leaks: reproduce first (`cargo nextest run -p fontaine-tui` on a cold target dir, recording
   every `LEAK`). Fix what the reproduction shows. Likely causes named in the issue: a child
   process (an MCP server or hook the test started) or a thread still holding stdout/stderr after
   the test returns. If the leaking tests are the ones that start `touch_marker` /
   `repo_mcp_server`, make the test shut the server down (the `World` drive ends the interface;
   confirm the MCP child is reaped on drop) rather than silencing the warning in nextest config.

## Implementation Steps

1. Reproduce on a cold build: touch a source file in `fontaine-sdk` (or remove the example
   artifacts from `rust/target/debug/examples`), run `cargo nextest run -p fontaine-tui`, record
   the SLOW and LEAK lines and which tests they name.
2. Add the nextest setup script and its filter to `rust/.config/nextest.toml`.
3. Re-run the reproduction; confirm the four tests finish in a few seconds and no SLOW remains.
4. Diagnose each LEAK from step 1 and fix its cause in the test or in the code it exercises.
5. Re-run the full `cargo nextest run -p fontaine-tui` and `cargo test` (plain, no setup script)
   to confirm both paths still pass.

## Files to Modify

- `rust/.config/nextest.toml`
- Whatever the leak diagnosis points at (likely `rust/fontaine-tui/tests/agents.rs`,
  `rust/fontaine-tui/tests/common/mod.rs`)

## Trade-offs

- A setup script vs. changing `example_bin` to share one build across processes with a file lock:
  the lock still serializes the first compile and adds platform-specific code to the testkit; the
  setup script moves the compile out of the tests entirely and costs only config.
- Raising `slow-timeout` was rejected: it hides the problem.

## Documentation

If `CONTRIBUTING.md` or `tasks/design/testing.md` describe how to run the tests, note the setup
script there.

## Testing Strategy

The change is test infrastructure; verification is the nextest runs in the implementation steps
(cold and warm), plus a plain `cargo test -p fontaine-tui --test agents` to confirm the fallback
path. No new unit tests.

## Open Questions

- Which tests leak, and why? Settled by the step 1 reproduction.
