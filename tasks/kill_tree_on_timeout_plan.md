# Kill the Whole Process Tree When a Timed-Out Command Is Dropped

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/562

## Overview

On Windows, when the `sh` on PATH is a launcher (scoop shim, Git's `bin\sh.exe`) rather than the
real shell, a command that hits its timeout is killed only at the direct child.
`TerminateProcess` does not take down descendants, so the grandchild shell keeps the inherited
stdout/stderr pipe handles open, tokio's blocking-pool `ReadFile` never returns, and the
`statusline::tests::timeout_keeps_the_previous_line` test deadlocks forever. The same spawn →
timeout → drop → `kill_on_drop` shape in the status line runner and command hooks leaks one
orphaned process tree and one blocked thread per timed-out command in a long-lived TUI. This
plan makes every timeout-drop path kill the whole process tree, on both platforms, through the
shared `procgroup` helper (and a local copy in the dependency-light `fontaine-guardian`), and pins the bug with a regression test.

## Current State

- `fontaine-core/src/statusline.rs:171` (`StatuslineScript::run_inner`) spawns `sh -c` with
  `kill_on_drop(true)`, writes the payload to stdin, and awaits
  `tokio::time::timeout(self.timeout, process.wait_with_output())`. On timeout the future is
  dropped, `kill_on_drop` terminates only the direct child, and the function returns `Ok(None)`
  leaving whatever the shell started alive and the pipes open.
- `fontaine-core/src/user_hooks.rs:91` (`HookProcess::run`) has the same shape. The timeout
  lives one level up in `UserHook::answer_within` (`user_hooks.rs:653`), which wraps the whole
  `answer()` future — so the child handle is dropped from outside `run`, and no code after the
  drop can clean up the tree.
- `fontaine-tools/src/procgroup.rs` is the shared stop helper. On Unix it signals the child's
  process group (SIGTERM, grace, SIGKILL). On Windows every function is a no-op except
  `force_kill`, which kills only the direct child — its own doc comment says so. Callers that
  spawn through a wrapper (LSP client, websearch, claude CLI) already set `process_group(0)` on
  Unix (`#[cfg(unix)]`, e.g. `lsp/client.rs:380`) and record `child.id()` before the leader can
  exit; statusline and hooks do neither.
- `fontaine-core/src/providers/claude_subscription/cli.rs:1223` holds the only whole-tree kill
  in the codebase: a private Windows `taskkill` helper (`taskkill /T /F /PID`) used by its
  private `kill_tree`.
- `fontaine-guardian/src/scanner.rs:235` (`CommandScanner::run`) repeats the statusline shape —
  `tokio::time::timeout(self.timeout, work)` around a child that reads stdout then waits — so a
  timed-out BYO scanner leaks the same way on Windows. The crate is deliberately
  dependency-light (`fontaine-guardian/Cargo.toml` has no fontaine-tools dep) so it can be
  embedded by other harnesses.
- Scoop `git.shim` following already landed (`fontaine-tools/src/git_bash.rs`,
  `resolve_candidate`), and only the test constructs `CommandShell::Sh` directly.
- Other `kill_on_drop(true)` users, for the audit trail: `lsp/client.rs` and `websearch.rs`
  already stop through `procgroup` and gain tree kill from it while the leader is alive.
  `mcp/servers.rs` hands the child to rmcp's transport, whose stop kills the direct child only.
  `project_env/discover.rs` runs `gh --version` with no timeout — it blocks forever rather than
  leaking on timeout, a different problem.

## Design

### 1. Whole-tree kill in `fontaine-tools::procgroup`

Give the existing functions whole-tree semantics:

- Unix: unchanged — `terminate`/`force_kill` already signal the child's process group; the new
  obligation is on callers to spawn with `process_group(0)` (statusline and hooks today do not).
- Windows: `force_kill` gains a `taskkill /T /F /PID <pid>` step (the implementation lifted
  from `claude_subscription/cli.rs`, which then imports the shared helper and drops its private
  copy), then keeps its current direct-child `start_kill` fallback. `terminate` stays a no-op:
  Windows has no graceful tree stop for console applications. Because every existing call path
  ends in `force_kill` (`settle` waits out the grace, then forces), Windows tree kill needs no
  new async surface — it lands inside the functions callers already use.

### 2. Statusline (`fontaine-core/src/statusline.rs`)

- Add `#[cfg(unix)] process_group(0)` to the spawn (a group kill must never target our own
  group), and wrap the child in the `procgroup` drop guard (see §3).
- Replace `wait_with_output()` (which consumes the `Child`) with the shape `scanner.rs` and
  `websearch.rs`'s `Running` use: take the stdout/stderr pipes at spawn and read them alongside
  `child.wait()` on the borrowed child from the guard's `child()` accessor.
- The timeout arm just returns `Ok(None)`; the guard's `Drop` kills the tree (synchronous
  `force_kill`, detached reaping), which closes the grandchild's inherited pipe handles. Normal
  completion disarms the guard.

### 3. Hooks (`fontaine-core/src/user_hooks.rs`)

The caller-owned timeout in `answer_within` stays — the pre-tool pipeline tells a timeout from
a failure by it, and `UserHook::answer`'s doc comment records why. Because the drop happens
outside `HookProcess::run`, the cleanup has to live in a `Drop` guard, the same pattern as
`Running` in `websearch.rs:191`:

- Wrap the spawned `Child` plus its recorded `pid` in a small guard struct (in
  `fontaine-tools::procgroup`, next to the functions it calls) whose `Drop` runs
  `force_kill` + `detached_settle`, and which exposes the child through a `child()` accessor.
  On Unix the spawn gains `process_group(0)` exactly like the LSP client.
- `run` takes the stdout/stderr pipes at spawn and reads them alongside `child.wait()` on the
  borrowed child (`wait_with_output` consumes the `Child`, so the guard could not own it).
  It disarms the guard on normal completion, so a hook that finished is not re-killed; a hook
  cut off by the timeout drops with the guard and takes its whole tree down. The statusline
  uses the same guard.

### 4. Guardian BYO scanner (`fontaine-guardian/src/scanner.rs`)

`CommandScanner::run` grows the same treatment with a tiny local helper rather than a
fontaine-tools dependency, respecting the lift-out constraint: on timeout (and on drop before
completion) kill the tree — Unix by signalling the group after `process_group(0)` at spawn,
Windows by a local `taskkill /T /F` (~15 lines, mirroring the lifted helper).

## Implementation Steps

1. `fontaine-tools/src/procgroup.rs`: add the Windows `taskkill /T /F` step to `force_kill`;
   add the drop guard struct (`TreeGuard`-style: child + pid, `child()` accessor, `disarm()`).
   Extend the module doc comment to state whole-tree semantics.
2. `fontaine-core/src/providers/claude_subscription/cli.rs`: delete the private `taskkill`
   and delegate the Windows `kill_tree` to the shared helper; replace `detached_kill`'s
   `taskkill` + `start_kill` with `procgroup::force_kill`.
3. `fontaine-core/src/statusline.rs`: Unix `process_group(0)`, wrap the child in the guard,
   read the piped stdout/stderr alongside `child.wait()` on `guard.child()`, disarm on
   completion; the timeout arm just returns.
4. `fontaine-core/src/user_hooks.rs`: `HookProcess::run` records pid, Unix `process_group(0)`,
   wraps the child in the guard, reads the pipes alongside `child.wait()` on `guard.child()`,
   disarms on completion.
5. `fontaine-guardian/src/scanner.rs`: local tree-kill helper + guard for `CommandScanner`.
6. Regression tests (see Testing Strategy) in `fontaine-tools` (procgroup), `fontaine-core`
   (statusline, hooks), `fontaine-guardian` (scanner).
7. Docs: CHANGELOG entry; `docs/hooks.md` timeout wording ("cut off" now means its process tree
   is stopped; on Windows, only while the command itself is still running); update the
   `docs/configuration.md` `[statusline]` wording likewise.
8. `tasks/fontaine_harness_plan.md`: add a dated line at the end of the `### Tools` subsection of
   `## Decisions`, superseding the 2026-10-30 entry, which is also marked "(superseded by <date> below)" in place ("On Windows only the direct language-server
   child is killed ... no new Job-object code"): `procgroup::force_kill` now tree-kills on
   Windows (while the leader lives), so the LSP client and websearch gain it.

## Files to Modify

- `rust/fontaine-tools/src/procgroup.rs`
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs`
- `rust/fontaine-core/src/statusline.rs`
- `rust/fontaine-core/src/user_hooks.rs`
- `rust/fontaine-guardian/src/scanner.rs`
- `CHANGELOG.md`, `docs/hooks.md`, `docs/configuration.md`
- `tasks/fontaine_harness_plan.md` (Decisions)

## Decisions

- `taskkill /T /F` is the Windows tree-kill mechanism, shared with the claude CLI provider; Job
  Objects are deferred; revisit if a launcher is found that escapes `taskkill`'s tree walk.
- `taskkill /T /F` cannot reach descendants of a leader that has already exited (the gap Job
  Objects would close), so Windows tree kill holds only while the leader lives.
- Shell resolution is not changed; tree kill covers launchers, PowerShell and backgrounded jobs.
- The hook timeout stays caller-owned; cleanup rides a `Drop` guard.
- MCP server children (rmcp-owned) and `project_env/discover.rs` (no timeout) are out of scope.
- `fontaine-guardian` keeps its lift-out constraint: local helper, no fontaine-tools dependency.

## Documentation

- CHANGELOG.md entry under Unreleased.
- `docs/hooks.md` (`timeout_secs` wording at line 57 and the "cut off" phrasing): a timed-out
  hook has its process tree stopped, not just the direct child (on Windows, only while the
  command itself is still running).
- `docs/configuration.md`: the `[statusline]` section's "It is killed after 5 seconds" now says
  its process tree is stopped (on Windows, only while the command itself is still running).

## Testing Strategy

This bug deadlocked a test in the wild; it gets automated regression coverage per the
verification tiers.

- Unit: `procgroup` — after `force_kill`, a child that backgrounded a 30 s
  sleeper inheriting its pipes leaves no survivor. Unix asserts by process-group probe
  (`kill -0 -- -<pgid>` fails); since `run` hides the child, the command writes `$$` (the shell's
  pid, the pgid leader) or `$!` (the sleeper) to a temp file and the test probes that pid as
  `jobs/tests.rs` does (`echo $! > file`, then `pid_in`/`gone`). Windows asserts by pid probe
  through `tasklist`/`taskkill` exit codes, recording the Windows pid first (`cat
  /proc/$!/winpid` under MSYS `sh`, since `$!` is an MSYS pid `tasklist` cannot see); skip when
  no suitable shell is on PATH. This is the test that reproduces the
  issue's mechanism without needing a scoop shim: a backgrounded job is exactly the
  "grandchild outlives the direct child, pipes stay open" shape.
- Unit: statusline — keep `timeout_keeps_the_previous_line`; add a test whose command
  backgrounds a long sleeper while the leader stays alive until the timeout (`sleep 30 & wait`,
  50 ms timeout; on Windows the leader must outlive the timeout, since `taskkill` cannot reach
  descendants of an exited leader) asserting `run` returns
  `None` and no descendant survives (Unix pgid probe; on Windows the test completing at all —
  runtime drop not blocking — is the assertion, since the blocked `ReadFile` is what hung the
  original test).
- Unit: hooks — a command hook shaped the same way times out via `answer_within` and leaves no
  survivor; a normal hook still completes and is not re-killed (guard disarm).
- Unit: guardian — `CommandScanner` timeout leaves no surviving descendant. Unix-only, like the
  existing `scanner.rs` command tests (`#[cfg(unix)]`, shell-script scanners).
- Existing suites: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, `cargo test --features fontaine/pg2`, rustdoc with `-D warnings`, on the
  platforms CI covers; `cargo check --target x86_64-pc-windows-gnu` for the Unix-only cfg
  paths.
- No live APIs involved; nothing here needs a network, container, or real provider.

## Manual Verification

On the scoop-shimmed Windows 11 machine from the issue, with `where sh` resolving to
`...\scoop\shims\sh.exe`:

```
cd rust
cargo test -p fontaine-core --lib statusline::tests::timeout_keeps_the_previous_line
```

Expected: the test finishes in seconds and no orphaned `sh -c "sleep 5"` remains in the process
list. Not automated because a launcher-style `sh` is not known to be on CI's `windows-latest`
PATH, so only this machine reproduces the original deadlock; the automated tests reproduce the
mechanism, not the shim itself.

## Open Questions

- Whether CI's `windows-latest` `sh` is the real `usr\bin\sh.exe` (the issue marks it
  unverified). Only affects how strongly the Windows unit test exercises the launcher chain;
  the mechanism test covers the leak either way.
