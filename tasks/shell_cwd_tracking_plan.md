# Track Shell Working Directory Across Calls Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/26

## Overview

Claude Code keeps the Bash working directory between calls, so `cd sub` in one call
affects the next. Fontaine's `shell` starts every call in the primary root (or the call's `cwd`
argument), so the model has to repeat `cwd` or prefix every command with `cd`. This plan adds a
session-tracked shell working directory per agent that resets to a workspace root when it would
leave the roots, makes the permission pipeline resolve relative paths against it, and shows the
current directory and git branch in the TUI status bar (an addition requested alongside the issue).

## Current State

- `ShellCall` (`rust/fontaine-tools/src/builtins.rs:260`) carries `command`, `timeout_secs`,
  `cwd`, `run_in_background`. `launch_in` (`builtins.rs:327`) picks the directory: the call's
  `cwd` joined onto `shell.launcher.cwd` (the fixed launch directory), else the launch directory.
  Nothing is remembered between calls.
- The permission pipeline resolves paths against the *call's* `cwd` argument, never a tracked
  value: `Environment::base` (`fontaine-tools/src/preclassify.rs:545`), `ruled_read_calls` /
  `secret_read_calls` (`fontaine-core/src/permissions.rs:478-530`), `critical_shell`
  (`fontaine-core/src/floor.rs:224`), and the scope validator's `PathFields::Optional("cwd")`
  (`fontaine-tools/src/scope.rs:52`). `tasks/design/permissions.md` (lines 46, 82, 198) documents
  this: a `cwd` outside every root disqualifies the fast path.
- The status bar (`fontaine-tui/src/statusbar.rs:render`) shows mode, guardian-off marker, model,
  agents, scheduled count, goal and a hint. `ui/mod.rs:draw` (line 55) copies runtime state into
  `App` each tick. No git information is read anywhere in the TUI.
- The shell uses `rig-tools` 0.45.0 `Shell`/`Launcher`; commands run as `sh -c <command>`
  (`ShellTool::launch`), through `sh_command` on Windows/Git Bash. `powershell` is a sibling tool.

## Design

### Tracker

`ShellCwd` in `fontaine-tools` (new `shell_cwd.rs`): a cheap-to-clone handle
(`Arc<std::sync::Mutex<PathBuf>>`, poisoning panics: it feeds permission decisions) holding the
agent's current shell directory, initialised to the primary root. One tracker per agent toolset.
The root agent's tracker is also held by `AgentRuntime` so the TUI can read it. A subagent gets a
fresh tracker at the primary root; it never inherits the parent's.

- `current() -> PathBuf`
- `observe(reported: &Path, scope: &Scope) -> Observation` where `Observation` is `Moved`,
  `Unchanged`, or `Reset { from }`: if `scope.find(reported)` accepts the path (inside a root,
  symlinks resolved) and it is a directory, store it; otherwise store the primary root and report
  `Reset`.

### Capturing the final directory

Foreground calls (and only those; background jobs never move the directory) launch through a
fixed wrapper that receives the user's command as a positional argument, never interpolated:

```
sh -c 'eval "$1"; rc=$?; pwd -P >"$2"; exit "$rc"' sh <command> <tempfile>
```

(exact text settled at implementation). Properties that matter:

- The command text is data, so quoting, newlines and heredocs behave as under `sh -c <command>`.
- Exit status is preserved. A command that `exec`s or `exit`s never reaches the trailer, so the
  directory stays unchanged: fail-safe.
- The temp file lives in the session scratch directory, unique per call, and is removed after
  being read. A missing or unreadable file means `Unchanged`.
- Windows: Git Bash path goes through the same wrapper with the reported path converted to a
  native path before `observe`; `powershell` reports `(Get-Location).Path` the same way.
- After the job ends, `run_job` calls `observe`. On `Reset` it appends one line to the tool
  result, e.g. `[shell working directory reset to <root>: <path> is outside the workspace]`, so the
  model learns why. This is Claude Code's behavior.

The call's explicit `cwd` argument stays a per-call starting directory; the directory the command
*ends* in is what gets tracked, as in an interactive shell.

### Starting directory and the permission pipeline

One resolution point turns "no `cwd` argument" into "the tracked directory": the shell tool wrapper
materialises the effective `cwd` into the call's arguments **before** the permission pipeline sees
them. The gate, floor, Tier-0.5 pre-classifier, scope validator, hooks, the guardian prompt and the
launch then all see the same string, so a decision can never be made against one directory and run
in another (even with parallel tool calls: the value is pinned per call). With this, no consumer
in the pipeline changes: they already resolve against a call's `cwd`. Implementation must check where
argument rewriting is allowed in the gate (the same place `updatedInput`-style hook rewrites
land) and keep the rewrite a pure function of the tracker, never of model input.

Security reasoning, because this touches the gate: the tracked value is only ever written by
`observe`, which accepts a path only if the scope validator accepts it. So it is always inside a
root and always Fontaine-computed; project config cannot set it. `cd /etc && ls` is judged by the
guardian before running as today (the command, not the starting directory, is the risk), and
afterwards the directory resets. The deterministic floor is unchanged.

### Status bar

- New `fontaine-core/src/git_head.rs` (pure filesystem, no `git` spawn): `read(dir) -> Option<Head>`
  walks up from `dir` to find `.git`, follows a `gitdir:` file (worktrees, submodules), parses
  `HEAD`: `ref: refs/heads/<name>` gives `Head::Branch(name)`, a bare hash gives
  `Head::Detached(short7)`.
- `App` gains `cwd: PathBuf` and `branch: Option<String>`. `ui/mod.rs:draw` sets `cwd` from the
  root agent's tracker and refreshes `branch` through a small cache keyed by `(cwd, HEAD mtime)`
  so a redraw does no work when nothing changed.
- `statusbar::render` adds a segment after the mode labels: ` · <dir>` plus ` (<branch>)` when
  there is one, before the model. The directory is shown with the home directory as `~`, and
  truncated from the left with `…` to at most 40 columns; the branch is truncated at 24.
  The segment is dim like the rest; no new theme colors. Detached HEAD renders as `(abc1234)`.
  The hint stays last, so on narrow terminals the existing right-clip drops the hint first.
- Viewing a subagent keeps showing the main agent's directory (the segment is session context,
  not agent state).

## Implementation Steps

1. `fontaine-tools/src/shell_cwd.rs`: `ShellCwd`, `Observation`, `observe`; export it.
2. `fontaine-tools/src/builtins.rs`: wrapper launch for foreground `shell`/`powershell` calls,
   temp file handling, `run_job` calls `observe` and appends the reset notice; materialise the
   effective `cwd` into `ShellCall`/`ShellArgs`; update both tool descriptions (they say "in the
   current directory") to say the directory persists between calls.
3. Wire one `ShellCwd` per agent toolset where the toolsets are built (root and subagent
   construction in `fontaine-agents` / `fontaine-core/src/agent.rs`); expose the root's handle on
   `AgentRuntime`.
4. Make the pre-gate rewrite point (see above) use the tracker; confirm `permissions.rs`,
   `floor.rs`, `preclassify.rs` and `scope.rs` need no change beyond tests.
5. `fontaine-core/src/git_head.rs` and its tests.
6. `fontaine-tui`: `App.cwd`/`branch`, `draw` wiring with the HEAD cache, `statusbar::render`
   segment and a path-shortening helper.
7. Docs, `CHANGELOG.md`, plan `## Decisions` lines (see below).

## Files to Modify

- `rust/fontaine-tools/src/shell_cwd.rs` (new), `rust/fontaine-tools/src/builtins.rs`,
  `rust/fontaine-tools/src/lib.rs`
- `rust/fontaine-core/src/agent.rs` (and the subagent toolset builder in `fontaine-agents`)
- `rust/fontaine-core/src/git_head.rs` (new), `rust/fontaine-core/src/lib.rs`
- `rust/fontaine-tui/src/statusbar.rs`, `app.rs`, `ui/mod.rs`
- `docs/` shell/permissions/TUI pages, `tasks/design/permissions.md`, `CHANGELOG.md`,
  `tasks/fontaine_harness_plan.md` (Decisions)

## Trade-offs

- **Persistent shell process vs. per-call wrapper.** A long-lived `sh` would keep env vars and
  functions too (Claude Code keeps only cwd and env exports through a snapshot), but costs a
  process lifecycle, output framing and a larger sandbox/guardian surface. The wrapper tracks only
  the directory, which is what the issue asks for, and every call stays a fresh process.
- **Reading `.git/HEAD` vs. spawning `git branch --show-current`.** Reading the file needs no
  process, no `git` on `PATH`, and cannot be fooled by a repo-local `core.fsmonitor`/hook config
  executing on every redraw (a spawned `git` in an untrusted repo can run configured helpers).
- **Materialising `cwd` into args vs. teaching each consumer about the tracker.** One rewrite
  point keeps decision and execution on the same value and leaves the open/closed shape of the
  pipeline intact.

## Decisions

- Explicit `cwd` argument starts a call there, and the ending directory is tracked (no separate
  "temporary cwd" notion).
- `!` user shell commands and background jobs do not move the tracked directory.
- Subagents start at the primary root and never inherit the parent's tracked directory.

## Documentation

- The shell tool page in `docs/`: the directory persists, resets when leaving the roots, the
  reset notice.
- The permissions/guardian page: relative paths resolve against the tracked directory.
- The TUI page: the status bar shows directory and branch.
- `tasks/design/permissions.md` lines 46/82/198: "the call's `cwd`" becomes "the call's effective
  `cwd` (argument, else the tracked directory)".
- `CHANGELOG.md` entry. Plan Decisions: deviation record under the shell subject: Fontaine resets
  to the *primary root* (Claude Code resets to the project directory), and the status-bar
  directory/branch segment is a Fontaine addition.

## Testing Strategy

Unit tests (no live APIs):

- `shell_cwd`: `observe` accepts a subdirectory, rejects a path outside every root (`Reset`),
  rejects a symlink pointing outside, handles a removed directory, and `Unchanged` on identical
  path.
- Wrapper: exit code preserved; `exit 3` and `exec` leave the directory unchanged; a command with
  quotes, newlines and a heredoc runs intact; a missing temp file is `Unchanged`; background jobs
  do not observe.
- Permission regression (silent class, so automated): after the tracker moves into `sub/`, a
  `cat .env`/`cat secrets` style command resolves its operand against `sub/` in the pre-classifier,
  the secret-file read rule and the floor, exactly as an explicit `cwd="sub"` does today; a
  tracker value never reaches the gate unresolved.
- `git_head`: normal repo, detached HEAD, worktree `gitdir:` file, nested directory walk-up, no
  repo, unreadable HEAD.
- Status bar via `TestBackend`: segment present with branch, without branch, detached form, long
  path truncated with `…`, home shown as `~`, hint still last, subagent view keeps the main segment.
- Integration through headless `fontaine -p` with a deterministic completion model: call 1
  `cd sub`, call 2 `pwd` prints `<root>/sub`; call 3 `cd /` then call 4 `pwd` shows the reset
  notice and the primary root.

## Manual Verification

1. `cargo run -p fontaine` in a git checkout inside tmux; expect the status bar to read
   `<mode> · ~/…/fontaine (main) · <model> …`. Why manual: the real terminal width and clip
   behavior is a layout concern (`tasks/design/testing.md`, terminal layout bugs).
2. Ask the model to run `cd rust` then `pwd`; expect the bar to show `…/fontaine/rust`.
3. In another terminal run `git switch -c scratch`; expect the bar to show `(scratch)` on the next
   redraw. Why manual: needs a real second process changing HEAD.

## Open Questions

- Should the `cwd` field of hook payloads (`user_hooks.rs:190`) report the tracked directory? Left
  as is (primary root) unless Claude Code is observed doing otherwise via `claude -p`.
- Windows path conversion for Git Bash `pwd -P` output (`/c/Users/..`): confirm `cygpath -w` is
  always available where `sh_command` already requires Git Bash.
