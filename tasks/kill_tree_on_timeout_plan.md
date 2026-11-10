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
plan makes every timeout-drop path kill the whole process tree, on both platforms, through one
shared helper, and pins the bug with a regression test.

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
- Git Bash discovery already follows a scoop `git.shim` once
  (`fontaine-tools/src/git_bash.rs:145`, `resolve_candidate`), so the issue's "discovery does
  not recognize a shimmed `git.exe`" half is fixed. In production on Windows, `CommandShell` is
  `GitBash` (real `usr\bin\sh.exe` first on PATH) or `PowerShell`; the bare-`Sh` path that hit
  the shim only runs in the test, which constructs `CommandShell::Sh` directly. The leak shape
  still applies to PowerShell (a `pwsh -c` that spawns children) and to any shell whose command
  backgrounds a process, so the fix belongs in the timeout path, not in shell resolution.
- Other `kill_on_drop(true)` users, for the audit trail: `lsp/client.rs` and `websearch.rs`
  already stop through `procgroup` and gain tree kill automatically once `procgroup` grows it.
  `mcp/servers.rs` hands the child to rmcp's transport, whose stop kills the direct child only.
  `project_env/discover.rs` runs `gh --version` with no timeout — it blocks forever rather than
  leaking on timeout, a different problem.

## Design

### 1. Whole-tree kill in `fontaine-tools::procgroup`

Give the existing functions whole-tree semantics and add one entry point for the timeout path:

- Unix: unchanged — `terminate`/`force_kill` already signal the child's process group; the new
  obligation is on callers to spawn with `process_group(0)` (statusline and hooks today do not).
- Windows: `force_kill` gains a `taskkill /T /F /PID <pid>` step (the implementation lifted
  from `claude_subscription/cli.rs`, which then imports the shared helper and drops its private
  copy), then keeps its current direct-child `start_kill` fallback. `terminate` stays a no-op:
  Windows has no graceful tree stop for console applications. Because every existing call path
  ends in `force_kill` (`settle` waits out the grace, then forces), Windows tree kill needs no
  new async surface — it lands inside the functions callers already use.
- Add `procgroup::kill_timed_out(child, pid, grace)`: `force_kill` immediately, then a bounded
  `child.wait()` so the caller can reap before returning. Used by the statusline timeout arm.

### 2. Statusline (`fontaine-core/src/statusline.rs`)

- Record `pid = process.id()` right after spawn; add `#[cfg(unix)] process_group(0)` to the
  spawn (a group kill must never target our own group).
- In the timeout arm, instead of returning with the future dropped: call
  `procgroup::kill_timed_out(&mut process, pid, grace)`, then return `Ok(None)`. Killing the
  tree closes the grandchild's inherited pipe handles, which unblocks the dropped
  `wait_with_output`'s `ReadFile`; the bounded wait reaps the direct child. A pathological case
  (tree kill fails) degrades to today's behavior — the drop — instead of hanging.

### 3. Hooks (`fontaine-core/src/user_hooks.rs`)

The caller-owned timeout in `answer_within` stays — the pre-tool pipeline tells a timeout from
a failure by it, and `UserHook::answer`'s doc comment records why. Because the drop happens
outside `HookProcess::run`, the cleanup has to live in a `Drop` guard, the same pattern as
`Running` in `websearch.rs:191`:

- Wrap the spawned `Child` plus its recorded `pid` in a small guard struct (in
  `fontaine-tools::procgroup`, next to the functions it calls) whose `Drop` runs
  `force_kill` + `detached_settle`. On Unix the spawn gains `process_group(0)` exactly like
  the LSP client.
- `run` disarms the guard on normal completion (after `wait_with_output` returns), so a hook
  that finished is not re-killed; a hook cut off by the timeout drops with the guard and takes
  its whole tree down. Statusline could share the guard, but its timeout arm benefits from the
  explicit, synchronous `kill_timed_out` call, which the guard cannot express — keep both:
  guard for drops you cannot see, explicit kill where you can.

### 4. Guardian BYO scanner (`fontaine-guardian/src/scanner.rs`)

`CommandScanner::run` grows the same treatment with a tiny local helper rather than a
fontaine-tools dependency, respecting the lift-out constraint: on timeout (and on drop before
completion) kill the tree — Unix by signalling the group after `process_group(0)` at spawn,
Windows by a local `taskkill /T /F` (~15 lines, mirroring the lifted helper).

### 5. Audit the remaining `kill_on_drop` sites

- `lsp/client.rs`, `websearch.rs`: no change beyond `procgroup`'s — their stop paths gain
  Windows tree kill automatically.
- `mcp/servers.rs`: rmcp owns the child; its stop kills the direct child only. Out of scope —
  MCP servers are long-lived and stopped at shutdown, not on a timeout-drop. Recorded in
  Decisions.
- `project_env/discover.rs`: no timeout exists; nothing to fix here.

## Implementation Steps

1. `fontaine-tools/src/procgroup.rs`: add the Windows `taskkill /T /F` step to `force_kill`;
   add `kill_timed_out(child, pid, grace)`; add the drop guard struct (`TreeGuard`-style: child
   + pid + grace, `disarm()`). Extend the module doc comment to state whole-tree semantics.
2. `fontaine-core/src/providers/claude_subscription/cli.rs`: delete the private `taskkill`
   and delegate the Windows `kill_tree` to the shared helper.
3. `fontaine-core/src/statusline.rs`: record pid, Unix `process_group(0)`, timeout arm calls
   `procgroup::kill_timed_out`.
4. `fontaine-core/src/user_hooks.rs`: `HookProcess::run` records pid, Unix `process_group(0)`,
   wraps the child in the guard, disarms on completion.
5. `fontaine-guardian/src/scanner.rs`: local tree-kill helper + guard for `CommandScanner`.
6. Regression tests (see Testing Strategy) in `fontaine-tools` (procgroup), `fontaine-core`
   (statusline, hooks), `fontaine-guardian` (scanner).
7. Docs: CHANGELOG entry; `docs/hooks.md` timeout wording ("cut off" now means the whole
   process tree is stopped); check `docs/configuration.md` statusLine wording likewise.

## Files to Modify

- `rust/fontaine-tools/src/procgroup.rs`
- `rust/fontaine-core/src/providers/claude_subscription/cli.rs`
- `rust/fontaine-core/src/statusline.rs`
- `rust/fontaine-core/src/user_hooks.rs`
- `rust/fontaine-guardian/src/scanner.rs`
- `CHANGELOG.md`, `docs/hooks.md`, `docs/configuration.md` (wording check)

## Trade-offs

- **Job Objects vs `taskkill /T /F`.** `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is the robust
  Windows answer (kill at handle close, no pid races) but needs a new dependency or a block of
  `windows-sys` FFI at spawn time for every child. `taskkill /T /F` is what the claude CLI
  provider already ships and proves sufficient for the same launcher chain; reusing it keeps
  the change inside `procgroup`. Revisit if a launcher is ever found that escapes `taskkill`'s
  tree walk (it cannot escape a Job Object).
- **Guard vs explicit kill in hooks.** Moving the timeout inside `HookProcess::run` would allow
  an explicit kill but changes `UserHook`'s documented contract (caller owns the timeout so the
  pipeline can tell timeout from failure) and would re-arm `answer_within`'s outer timeout as a
  second layer. The guard keeps the contract and fixes the drop.
- **Not resolving `sh` away from launchers.** Fixing shell resolution instead (always run the
  real `usr\bin\sh.exe`) addresses the test's shim but not the production PowerShell fallback
  or any command that backgrounds a process; tree kill covers all of them from one place.

## Decisions

- `taskkill /T /F` is the Windows tree-kill mechanism, shared with the claude CLI provider; Job
  Objects are deferred.
- The hook timeout stays caller-owned; cleanup rides a `Drop` guard.
- MCP server children (rmcp-owned) and `project_env/discover.rs` (no timeout) are out of scope.
- `fontaine-guardian` keeps its lift-out constraint: local helper, no fontaine-tools dependency.

## Documentation

- CHANGELOG.md entry under Unreleased.
- `docs/hooks.md` (`timeout_secs` wording at line 57 and the "cut off" phrasing): a timed-out
  hook has its whole process tree stopped, not just the direct child.
- `docs/configuration.md`: same one-line check for the `statusLine` timeout description, if it
  names kill semantics.

## Testing Strategy

This bug deadlocked a test in the wild; it gets automated regression coverage per the
verification tiers.

- Unit: `procgroup` — after `force_kill`/`kill_timed_out`, a child that backgrounded a 30 s
  sleeper inheriting its pipes leaves no survivor. Unix asserts by process-group probe
  (`kill -0 -- -<pgid>` fails). Windows asserts by pid probe through `tasklist`/`taskkill`
  exit codes; skip when no suitable shell is on PATH. This is the test that reproduces the
  issue's mechanism without needing a scoop shim: a backgrounded job is exactly the
  "grandchild outlives the direct child, pipes stay open" shape.
- Unit: statusline — keep `timeout_keeps_the_previous_line`; add a test whose command
  backgrounds a long sleeper (`sleep 30 & echo hi`, 50 ms timeout) asserting `run` returns
  `None` and no descendant survives (Unix pgid probe; on Windows the test completing at all —
  runtime drop not blocking — is the assertion, since the blocked `ReadFile` is what hung the
  original test).
- Unit: hooks — a command hook shaped the same way times out via `answer_within` and leaves no
  survivor; a normal hook still completes and is not re-killed (guard disarm).
- Unit: guardian — `CommandScanner` timeout leaves no surviving descendant, Unix-gated probe
  plus Windows-agnostic completion assertion as above.
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
list. Not automated because CI's `windows-latest` image does not have a launcher-style `sh` on
PATH, so only this machine reproduces the original deadlock; the automated tests reproduce the
mechanism, not the shim itself.

## Open Questions

- Whether CI's `windows-latest` `sh` is the real `usr\bin\sh.exe` (the issue marks it
  unverified). Only affects how strongly the Windows unit test exercises the launcher chain;
  the mechanism test covers the leak either way.
