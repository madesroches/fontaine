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
(`Arc<std::sync::Mutex<HashMap<AgentId, PathBuf>>>`, poisoning panics: it feeds permission
decisions) holding each agent's current shell directory. There is no per-agent toolset:
`tools::registry` builds one set of shell tools and subagents reuse the parent's wrapped instances,
telling agents apart by the `AgentId` in `ToolContext`. So one tracker is shared by all per-root
shell instances; a missing entry means the primary root, which gives a subagent its fresh start (it
never inherits the parent's). The root agent's entry is exposed on `AgentRuntime` so the TUI can
read it.

- `current(agent) -> PathBuf`
- `observe(agent, reported: &Path, scope: &Scope) -> Option<Reset { from }>`: if `scope.find(reported)`
  accepts the path (inside a root, symlinks resolved) and it is a directory, store it and return
  `None`; otherwise store the primary root and return `Reset`.

### Capturing the final directory

Foreground calls (and only those; background jobs never move the directory) launch through a
fixed wrapper that receives the user's command as a positional argument, never interpolated. The
wrapper moves the temp path into a variable and clears the positional parameters before `eval`, so
the command sees the same `$#`, `$@` and `shift` as under `sh -c <command>`:

```
sh -c 'f=$2; c=$1; set --; eval "$c"; rc=$?; pwd -P >"$f"; exit "$rc"' sh <command> <tempfile>
```

(exact text settled at implementation). Properties that matter:

- The command text is data, so quoting, newlines and heredocs behave as under `sh -c <command>`.
- Exit status is preserved. A command that `exec`s or `exit`s never reaches the trailer, so the
  directory stays unchanged: fail-safe.
- The temp file lives in the session scratch directory, unique per call, and is removed after
  being read. A missing or unreadable file means the directory is unchanged.
- Windows, Git Bash: that path carries the command in the env var `FONTAINE_SH_COMMAND` with
  `RUN_COMMAND_SCRIPT` (`git_bash.rs`) because Git Bash re-parses its command line and halves
  backslashes, so the script must stay backslash-free. The trailer is a change to
  `sh_command`/`RUN_COMMAND_SCRIPT` that keeps the command in the env var and passes the temp path
  through env too (cleared before the command runs). The trailer uses `pwd -W` (builtin, prints the
  native `C:/...` path without spawning; combination with `-P` to be checked on Windows);
  `scope::translate` (`/c/...` to `C:\...`) is the fallback, and anything untranslatable is `Reset`.
- `powershell`: the command is spliced into a UTF-16LE `-EncodedCommand` script and a failing last
  statement exits 1. The trailer captures `$?`/`$LASTEXITCODE` first, writes
  `(Get-Location).ProviderPath` to a temp path passed via env var, then exits with the preserved
  code. A non-FileSystem provider counts as `Reset`.
- After the job ends, `run_job` calls `observe`. On `Reset` it appends one line to the tool
  result, e.g. `[shell working directory reset to <root>: <path> is outside the workspace]`, so the
  model learns why. This is Claude Code's behavior.
- A call that moves to the background on timeout (`run_job` returns) does not move the directory,
  as for background jobs; `run_job` removes its temp file at that point. The wrapper applies on the
  foreground `run_job` path only, never in `launch()` (`monitor` calls `shell.launch`).
- Subagent foreground shell calls currently go straight to rig's `shell.call`; they are routed
  through the same wrapper path so subagent directories are tracked per `AgentId`.
- `monitor` (shell-class, no `cwd` field) keeps starting at the primary root and never moves the
  tracked directory.

The call's explicit `cwd` argument stays a per-call starting directory; the directory the command
*ends* in is what gets tracked, as in an interactive shell.

### Starting directory and the permission pipeline

One resolution point turns "no `cwd` argument" into "the tracked directory": in `gated()`
(`guarded.rs`), right after `normalize(args)` and before `CallView` is built, an optional pre-gate
preparer on `GuardedTool` (set only on the shell/powershell per-root registrations in
`builtins.rs`) inserts the tracked cwd when `cwd` is absent. The gate, floor, Tier-0.5
pre-classifier, scope validator, hooks, the guardian prompt and the launch then all see the same
string, so a decision can never be made against one directory and run in another (even with
parallel tool calls: the value is pinned per call). With this, no consumer in the pipeline changes:
they already resolve against a call's `cwd`. The tool itself never reads the tracker when starting a
call (a missing `cwd` still means `launcher.cwd`). The rewrite is a pure function of the tracker,
never of model input. An explicit relative `cwd` still resolves against the primary root, as today
(`Scope::find`, `preclassify.rs`).

Constraints: the tracker is written only by `observe` after scope validation; config cannot set it;
the deterministic floor is unchanged.

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
2. `fontaine-tools/src/builtins.rs`: wrapper launch for foreground `shell`/`powershell` calls
   (including subagent calls, routed through `run_job`), temp file handling, `run_job` calls
   `observe` and appends the reset notice; update both tool descriptions, replacing "Each call starts
   a fresh sh: a cd or a variable does not carry over" (and the powershell equivalent) with: only
   the working directory persists between calls, env vars and variables do not.
3. Wire one `ShellCwd` where the tools are built (`fontaine-core/src/tools.rs` `registry`, shared by
   all per-root shell instances); expose the root agent's entry on `AgentRuntime`
   (`fontaine-core/src/agent.rs`).
4. Add the pre-gate preparer to `GuardedTool` in `fontaine-tools/src/guarded.rs` (applied in
   `gated()` after `normalize`) and set it on the shell/powershell registrations; confirm
   `permissions.rs`, `floor.rs`, `preclassify.rs` and `scope.rs` need no change beyond tests.
5. `fontaine-core/src/git_head.rs` and its tests.
6. `fontaine-tui`: `App.cwd`/`branch`, `draw` wiring with the HEAD cache, `statusbar::render`
   segment and path shortening via an existing clip helper.
7. Docs, `CHANGELOG.md`, plan `## Decisions` lines (see below).

## Files to Modify

- `rust/fontaine-tools/src/shell_cwd.rs` (new), `rust/fontaine-tools/src/builtins.rs`,
  `rust/fontaine-tools/src/lib.rs`
- `rust/fontaine-core/src/agent.rs`, `rust/fontaine-core/src/tools.rs`,
  `rust/fontaine-tools/src/guarded.rs`
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

## Decisions` lines (see below).

## Files to Modify

- `rust/fontaine-tools/src/shell_cwd.rs` (new), `rust/fontaine-tools/src/builtins.rs`,
  `rust/fontaine-tools/src/lib.rs`
- `rust/fontaine-core/src/agent.rs`, `rust/fontaine-core/src/tools.rs`,
  `rust/fontaine-tools/src/guarded.rs`
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
- Hook payload `cwd` stays the primary root (`workspace_view()`, documented in `docs/hooks.md`); the
  tracked directory is visible as `tool_input.cwd`.

## Documentation

- `docs/tools.md` (`## Shell`, `## PowerShell`): the directory persists, resets when leaving the
  roots, the reset notice.
- `docs/permissions.md` and `docs/guardian.md`: relative paths resolve against the tracked directory.
- `docs/tui.md` (`## Status bar`): directory and branch. The `!` note (`docs/tui.md:181-182`, runs in
  the primary root) stays true.
- `tasks/design/permissions.md` lines 46/82/198: "the call's `cwd`" becomes "the call's effective
  `cwd` (argument, else the tracked directory)".
- `CHANGELOG.md` entry. Plan Decisions in `tasks/fontaine_harness_plan.md`: deviation lines under
  `### Tools` (Fontaine resets to the *primary root*, Claude Code resets to the project directory),
  and under `### TUI` the `!` line and the status-bar directory/branch segment (a Fontaine
  addition). Observe Claude Code's `!`-mode cwd behaviour with `claude -p` (manual, not in tests)
  and record any deviation as a dated line.

## Testing Strategy

Unit tests (no live APIs):

- `shell_cwd`: `observe` accepts a subdirectory, rejects a path outside every root (`Reset`),
  rejects a symlink pointing outside, handles a removed directory, and returns `None` on an
  identical path.
- Wrapper: exit code preserved; `exit 3` and `exec` leave the directory unchanged; a command with
  quotes, newlines and a heredoc runs intact; a missing temp file leaves the directory unchanged; background jobs
  and timeout-to-background calls do not observe; `$#`, `shift` and `set -- a b` in the command
  behave as under `sh -c`; Git Bash `RUN_COMMAND_SCRIPT` stays backslash-free
  (extend `the_wrapper_script_survives_the_windows_reparse`); the powershell trailer preserves a
  failing exit code.
- Pipeline: the gate sees the resolved cwd (`rust/fontaine-tools/tests/guarded.rs`, `Scripted` gate
  recording `seen` args).
- Subagent: a child `cd` does not move the root's directory.
- Guardian: add `kind: guardian` dataset cases in `evals/dataset` with a tracked in-root cwd (none
  has `cwd` in args today; guardian input gains an absolute cwd on every unqualified shell call),
  and run the live guardian eval.
- Permission regression (silent class, so automated): after the tracker moves into `sub/`, a
  `cat .env`/`cat secrets` style command resolves its operand against `sub/` in the pre-classifier,
  the secret-file read rule and the floor, exactly as an explicit `cwd="sub"` does today; a
  tracker value never reaches the gate unresolved.
- `git_head`: normal repo, detached HEAD, worktree `gitdir:` file, nested directory walk-up, no
  repo, unreadable HEAD.
- Path truncation reuses an existing clip/truncate helper (`transcript.rs`, `config_panel.rs`,
  `provider_dialog.rs`) rather than a new one.
- Status bar via `TestBackend`: segment present with branch, without branch, detached form, long
  path truncated with `…`, home shown as `~`, hint still last, subagent view keeps the main segment.
- Integration through headless `fontaine -p` (`rust/fontaine/tests/common/mod.rs`
  `Endpoint`/`Turn::tool`/`Sandbox`, in `rust/fontaine/tests/headless_tools.rs`) with a deterministic
  completion model: call 1
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

None.
