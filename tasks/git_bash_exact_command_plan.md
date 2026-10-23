# Git Bash Exact Command Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/78

## Overview

On Windows, Fontaine runs hook commands, the `shell` tool and `!` commands as `sh -c <command>`
through Git Bash, started from a native process. The MSYS runtime rebuilds `sh`'s `argv` by
re-parsing the Windows command line with its own rules, which differ from the MSVC rules Rust
quotes by, so every `\\` run in the command reaches `sh` halved. The command that the permission
pipeline, the guardian and the audit log judged is then not the command that runs. This plan stops
putting the command text on the command line on Windows: it goes in an environment variable, and
`sh -c` runs a fixed script that evaluates it. Linux and macOS are unchanged.

## Current State

- **Hooks.** `HookProcess::run` (`rust/fontaine-core/src/user_hooks.rs:56`) builds
  `tokio::process::Command::new("sh").arg("-c").arg(command)`, and when `git_bash` is set it puts
  Git Bash first on `PATH` (`user_hooks.rs:71`). stdin carries the event JSON.
- **`shell` tool and `!` commands.** `shell_for` (`rust/fontaine-tools/src/builtins.rs:224`)
  returns rig-tools' own `Shell`, with Git Bash put first on the launcher's `PATH`
  (`builtins.rs:228`). rig's `Shell::start` (rig-tools 0.45.0 `src/shell.rs:240`) spawns `sh` with
  `["-c", args.command]` through `Workspace::spawn` → `Launcher::spawn`, so the text goes on the
  Windows command line. `shell_for` has two callers: the registered `shell` tool
  (`builtins.rs:321`, through `GuardedTool::per_root`) and `AgentRuntime::run_user_shell`
  (`rust/fontaine-core/src/user_shell.rs:37`, which calls `Shell::run`). The choke-point test
  `the_shell_is_confined_clamped_and_cut` (`rust/fontaine-tools/tests/choke_point.rs:127`) reads
  the returned `Shell`'s fields.
- **Wrapper precedent.** `Ls`, `Glob` and `Grep` (`builtins.rs:26` on) are Fontaine `Tool`s that
  hold a rig tool and delegate to it.
- **Test workarounds.** Commits 73e121a and de65b56 strip the `\\?\` prefix from paths placed in
  hook commands: `command_path` in `rust/fontaine/tests/common/mod.rs:481` (used by
  `parallel_agents.rs:743` and `project_executables.rs:130,170`) and a copy in
  `rust/fontaine-tui/tests/agents.rs:297`.
- **Windows-only tests.** `rust/fontaine-tools/tests/shell.rs` (`#![cfg(windows)]`) runs a
  command through the real Git Bash. `rust/fontaine-core/tests/user_hooks.rs` runs the
  `hook_double` example as a real hook process, with `common::git_bash()` (real Git Bash on
  Windows, `None` elsewhere).

### Reproduction (Git for Windows 2.45.2, this machine)

A native process (Python `subprocess`, which quotes by the same MSVC rules as Rust) ran each
command through Git's `usr\bin\sh.exe`:

| Command text | `sh -c <text>` printed | via environment variable |
|---|---|---|
| `printf '%s\n' '\\a'` | `\a` | `\\a` |
| `printf '%s\n' '\\server\share'` | `\server\share` | `\\server\share` |
| `printf '%s\n' 'a\\\\b'` | `a\\b` | `a\\\\b` |
| `printf '%s\n' '\\?\C:\x'` | `\?\C:\x` | `\\?\C:\x` |
| `printf '%s\n' 'tail\\'` | `tail\` | `tail\\` |
| `x\"y`, `"a b"`, `é ✓`, embedded newline | intact | intact |

Claude Code shows the same collapse: a `\\` in a command passed to its Bash tool on Windows reached
Git Bash as `\` during this research. Fixing the collapse is a deviation from Claude Code, and it is
recorded below.

## Design

### The exact-command form

Add to `rust/fontaine-tools/src/git_bash.rs`, next to the discovery code both callers already
import:

```rust
/// Carries the command to Git Bash's `sh`, which would otherwise re-parse it off the Windows
/// command line and halve every `\\`.
pub const COMMAND_VAR: &str = "FONTAINE_SH_COMMAND";

/// What `sh -c` runs in place of the command. The variable is expanded before `eval` runs, so
/// the command itself never sees it.
pub const RUN_COMMAND_VAR: &str = "eval \"unset FONTAINE_SH_COMMAND\n$FONTAINE_SH_COMMAND\"";

/// The `sh -c` script and the variable to set so `sh` runs `command` as written.
pub struct ShCommand<'a> {
    pub script: &'a str,
    pub env: Option<(&'static str, &'a str)>,
}

/// `command` itself when it can go on the command line; through `COMMAND_VAR` for Git Bash.
pub fn sh_command(command: &str, through_env: bool) -> ShCommand<'_>
```

`RUN_COMMAND_VAR` contains no backslash, so the MSYS re-parse leaves it intact; only `"` and a
newline need quoting, and both survive (reproduction table). The result was checked against plain
`sh -c` for `exit 3`, `false`, `set -e`, a function's `return`, `$0`, stdin, and whether the
variable is visible to the command or its children (it is not). Two things differ: a syntax error
is reported as `sh: eval: line N` rather than `sh: -c: line N`, and `$LINENO` is offset by two.

### Hooks

`HookProcess::run` calls `sh_command(command, self.git_bash.is_some())`, passes `script` after
`-c`, and sets `env` (after the scrub loop, so a scrubbed name can never remove it). stdin keeps
carrying the event.

### `shell` and `!`

A new Fontaine tool in `builtins.rs`, following the `Ls` pattern:

```rust
/// rig's `shell`, with the command kept off the Windows command line when it runs through
/// Git Bash.
#[derive(Clone, Debug)]
pub struct ShellTool {
    pub shell: Shell,
    pub through_env: bool,
}
```

- `NAME = Shell::NAME` and `Args = ShellArgs`. `description`, `parameters` and `annotations`
  delegate to `self.shell`, so the model-facing definition is unchanged.
- `prepared(&self, args) -> (Shell, ShellArgs)`: calls `sh_command(&args.command,
  self.through_env)`, so hooks and the shell tool share one route. When it returns an `env`, a
  clone of `self.shell` whose launcher has `with_env(name, value)` (`Launcher::with_env` takes
  `impl Into<OsString>`; rig applies env after unset_env), and `args` with `command` replaced by
  `script`. Otherwise the shell and the args unchanged.
- `call` runs `prepared` and then rig's `Shell::call`, so the `ExitStatus` and `OutputCut` results
  are still inserted into the context. `pub async fn run(&self, args)` does the same with
  `Shell::run`, for `run_user_shell`.
- `shell_for` returns `ShellTool { shell, through_env: settings.git_bash.is_some() }`.

The rewrite happens inside the gated tool, after `GuardedTool` has run the floor, the hooks, the
guardian and the audit on the original `args`. What they judge and what Git Bash runs are the same
text. `user_shell.rs` needs no change.

## Implementation Steps

1. `rust/fontaine-tools/src/git_bash.rs`: add `COMMAND_VAR`, `RUN_COMMAND_VAR`, `ShCommand` and
   `sh_command`, with unit tests for both branches. Update the module doc comment, which currently
   speaks only of discovery.
2. `rust/fontaine-core/src/user_hooks.rs`: run the hook through `sh_command`.
3. `rust/fontaine-tools/src/builtins.rs`: add `ShellTool`. `shell_for` returns it, and `build`
   registers it.
4. `rust/fontaine-tools/tests/choke_point.rs`: read the fields through `.shell`, and assert
   `through_env` follows `git_bash`.
5. Remove the test workarounds: delete `command_path` from `rust/fontaine/tests/common/mod.rs` and
   `rust/fontaine-tui/tests/agents.rs`, and put the paths into the hook commands as they were before
   73e121a / de65b56 (`git show` those commits for the original lines). These tests then fail on
   Windows CI again if the fix regresses. The TUI test world
   (`rust/fontaine-tui/tests/common/mod.rs:313`) builds `AgentSetup { git_bash: None, .. }`, so hooks
   would still take the command-line route; have it set `git_bash` on Windows from discovery, the way
   `fontaine-core/tests/common::git_bash()` does. The `rust/fontaine` tests already run the real
   binary, which discovers Git Bash itself.
6. Add the tests in **Testing Strategy**.
7. Record the deviation in `tasks/fontaine_harness_plan.md` `## Decisions` (see **Decisions**).
8. Docs (see **Documentation**).

## Files to Modify

- `rust/fontaine-tools/src/git_bash.rs`
- `rust/fontaine-tools/src/builtins.rs`
- `rust/fontaine-core/src/user_hooks.rs`
- `rust/fontaine-tools/tests/choke_point.rs` (also holds the cross-platform `ShellTool` tests)
- `rust/fontaine-tools/tests/shell.rs`
- `rust/fontaine-core/tests/user_hooks.rs`
- `rust/fontaine/tests/common/mod.rs`, `rust/fontaine/tests/parallel_agents.rs`,
  `rust/fontaine/tests/project_executables.rs`, `rust/fontaine-tui/tests/agents.rs`,
  `rust/fontaine-tui/tests/common/mod.rs`
- `docs/tools.md`, `docs/hooks.md`, `tasks/design/platforms.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Command text on stdin (`sh -s`).** Rejected. A hook's stdin is the event JSON, and a
  `shell` command that reads stdin would read its own script.
- **Quoting for the MSYS parser with `raw_arg`.** Rejected. It means re-implementing the
  undocumented `dcrt0.cc` rules, including its glob and `@file` handling, which can change
  between Cygwin and MSYS releases. The environment block carries the text as written.
- **A `Workspace` wrapper around rig's `Local` that rewrites `spawn`.** It would leave
  `shell_for`'s return type alone, but it delegates more than a dozen trait methods so it can
  change one. The `Ls`-style wrapper is the existing pattern and is smaller.
- **The environment route on every OS.** Rejected. Linux and macOS have no re-parse, and keeping
  `sh -c <command>` there keeps Claude Code's error text and `$LINENO` exactly.
- **Switching the Windows shell to `cmd` or PowerShell.** Out of scope. Git Bash is Claude Code
  parity (a 2026-10-21 decision), and model-issued and hook commands are POSIX. `cmd` re-parses
  its command line too, with rules that are harder to quote for. A PowerShell tool is issue #28.
- **`eval "$FONTAINE_SH_COMMAND"` without the `unset`.** Rejected. The command, and every
  process it starts, would inherit a copy of its own text.

## Decisions

- 2026-10-22: On Windows the command text reaches Git Bash in `FONTAINE_SH_COMMAND`, not on the
  command line, unlike Claude Code. Reason: the judged command must be the one that runs.

## Documentation

- `docs/tools.md`, the **Windows** bullet of the shell section: the command reaches Git Bash as
  written, including `\\`. A syntax error names `eval` rather than `-c`.
- `docs/hooks.md`, the `command` row: the same sentence for hook commands.
- `tasks/design/platforms.md`: one sentence on how the command is handed to Git Bash, and why.

## Testing Strategy

Every test below is automated. Windows CI (`windows-latest`, Git for Windows installed) is where
the regression actually reproduces, because only a real MSYS `sh` started from a native process
halves the backslashes.

- **Unit (`git_bash.rs`)**: `sh_command(c, false)` gives `script == c` and no env.
  `sh_command(c, true)` gives `RUN_COMMAND_VAR` and `(COMMAND_VAR, c)`. Also assert
  `RUN_COMMAND_VAR` contains no `\`, since that is the property that makes it survive the re-parse.
  Assert `RUN_COMMAND_VAR == format!("eval \"unset {COMMAND_VAR}\n${COMMAND_VAR}\"")` so the two
  constants cannot drift apart.
- **Cross-platform `ShellTool` semantics** (in `fontaine-tools/tests/choke_point.rs`'s
  `#[cfg(unix)] mod unix`, around line 150, which drives `shell` via `env_with`/`registry`/`call`):
  `env.shell.git_bash = Some(GitBash { bin: <tmp>/nope/bin, usr_bin: <tmp>/nope/usr/bin })`
  (nonexistent dirs prepended to `PATH`, so the system `sh` still resolves) forces the environment
  route on Linux and macOS; these tests do not run on Windows. Through the `shell` tool, check that:
  - output of `printf '%s\n' '\\a' 'tail\\' '\\?\C:\x'` is exact;
  - `exit 3` reports exit code 3;
  - `printf '%s' "${FONTAINE_SH_COMMAND-absent}"` prints `absent`;
  - `false && echo x; echo after` keeps ordinary sequencing.

  Also run `the_shell_runs_a_posix_command_through_git_bash`'s command through it.
- **Windows regression, `shell` (`fontaine-tools/tests/shell.rs`)**: through the real Git Bash,
  `printf '%s\n' '\\a' '\\server\share' 'tail\\' '\\?\C:\x'` comes back exactly. This pins #78 for
  the `shell` tool and, through `shell_for`, for `!` commands.
- **Regression, hooks (`fontaine-core/tests/user_hooks.rs`)**:
  - a hook `hook_double --stderr 'a\\b\\' --exit 2` denies with `(a\\b\\)`, parentheses included,
    in the message (the hook command in the deny message already contains `a\\b\\`, so the bare
    text proves nothing), or the hook is called directly and `reason == Some(r"a\\b\\")`;
  - a hook `--record` to a canonicalized temp path (`\\?\`-prefixed on Windows) writes the event
    there. This is the failure PR #75's Windows CI hit.

  Both run on every OS, and on Windows they go through the real Git Bash.
- **Restored integration tests**: `parallel_agents`, `project_executables` and the TUI `agents`
  test go back to canonicalized paths in hook commands (step 5), so the original failure from the
  wild is pinned at the tier where it was seen.
- **Choke point**: the updated `the_shell_is_confined_clamped_and_cut`.

## Open Questions

None. The issue's open points are settled above: the collapse was reproduced directly, and
Claude Code behaves the same way.
