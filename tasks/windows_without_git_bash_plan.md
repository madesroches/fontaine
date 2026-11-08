# Windows Without Git Bash Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/510

## Overview

On Windows, Fontaine refuses to start when Git for Windows is missing (`fontaine: Git for Windows
was not found: ...`, exit 2). Claude Code does not: "If Git for Windows is not installed, Claude
Code uses PowerShell as the shell tool instead" (code.claude.com/docs/en/setup). This plan makes
Fontaine start without Git Bash, with PowerShell as the command shell for the model, for hooks, for
the status line and for `!` lines, matching Claude Code. Git Bash stays the preferred shell when it
is found.

## Current State

- **Startup.** `rust/fontaine/src/lib.rs:892` `git_bash(host)` calls
  `fontaine_tools::git_bash::discover` and maps any `GitBashError` to exit 2 through `usage_code`;
  `prepare_session` (`lib.rs:748`) propagates it. `powershell(host)` (`lib.rs:~910`) is discovered
  separately and returns `Option` (absence is not an error).
- **Discovery.** `rust/fontaine-tools/src/git_bash.rs`: `discover` returns `GitBashError::NotFound`
  or `BadOverride` (`FONTAINE_GIT_BASH_PATH` set to something that is not a Git Bash `bash.exe`).
- **`shell` tool.** `rust/fontaine-tools/src/builtins.rs:1140` always registers `shell`
  (per root) and `monitor` (`builtins.rs:1170`), both built by `shell_for`, which spawns `sh`. With
  `git_bash: None` on Windows, `sh` would not resolve. `powershell` is registered only when an
  interpreter was found (`builtins.rs:1190`).
- **Hooks.** `rust/fontaine-core/src/user_hooks.rs:65` `HookProcess::run` spawns `sh -c` (through
  Git Bash's PATH when `git_bash` is `Some`). A `pre_tool_use` hook exiting other than 0 or 2 is a
  `HookError` that "decides nothing" (`user_hooks.rs:268`, `tasks/design/permissions.md` §User
  hooks).
- **Status line.** `rust/fontaine-core/src/statusline.rs:175` spawns `sh -c` the same way.
- **`!` lines.** `rust/fontaine-core/src/user_shell.rs:39` builds a `shell_for` tool and runs the
  command in it.
- **Skill `!` lines.** `rust/fontaine-core/src/skills.rs:189` `run_command` already degrades: it
  answers `[command not run: the shell tool is not available]` when the skill shell is `None`, and
  `[command not run: this agent has no shell tool]` when the agent lacks `shell`.
- **Editor launch** (`/memory`). `rust/fontaine-tui/src/ui/commands/memory.rs:26`
  `Launcher::for_platform` already handles Windows without Git Bash (`Launcher::Words`).
- **Subagents.** `rust/fontaine-agents/src/registry.rs:190` the builtin `worker` type names
  `shell` and not `powershell` (Decision 2026-10-30: "The worker agent does not get
  `powershell`"); `AgentType::toolset` (`registry.rs:~118`) intersects the type's list with the
  parent's tools, so without `shell` a worker would get no command tool.
- **Permission rules.** Bare `shell` deny/ask rules already cover `powershell`
  (`permission_rules.rs:410`, `permissions.rs:899`); the floor's critical-path check for
  `powershell` is `floor::critical_powershell`. Every `powershell` call goes to the guardian (no
  Tier 0.5).
- **Docs** stating the prerequisite: `docs/tools.md:293` and `:300-329`, `docs/hooks.md:36`,
  `docs/installing.md:102`, `README.md:39`;
  `tasks/design/platforms.md` ("not found ⇒ startup error on Windows (exit 2 …)").

## Design

### Shell selection at startup

`git_bash(host)` on Windows maps `GitBashError::NotFound` to `Ok(None)`; `BadOverride` stays an
exit-2 error (the user asked for a specific bash and it is wrong — failing loudly beats silently
switching shells). After discovery, `prepare_session` checks the combination:

| Git Bash | PowerShell | Result |
|---|---|---|
| found | any | today's behavior |
| not found | found | start; PowerShell fallback; one startup warning |
| not found | not found | exit 2: `no shell found: install Git for Windows, or set FONTAINE_GIT_BASH_PATH or FONTAINE_POWERSHELL_PATH` |

The warning goes into `SessionPrep.warnings` as a `Notice::warning` (the existing startup-warning
channel, so it reaches the user through the `Approver` once):
`Git for Windows was not found: shell commands, hooks and the status line run in PowerShell. Install Git for Windows, or set FONTAINE_GIT_BASH_PATH, to use Git Bash.`

On Linux/macOS nothing changes (`git_bash` is `None` and `sh` is native).

### One shared "command shell" for hooks, status line

Add to `fontaine-tools` (new `rust/fontaine-tools/src/command_shell.rs`, re-exported in `lib.rs`):

```rust
/// How a configured command string (a hook, the status line) is run.
pub enum CommandShell {
    /// `sh -c` with the command on the command line (Linux, macOS).
    Sh,
    /// Git Bash's `sh -c`, the command carried in `FONTAINE_SH_COMMAND`, its dirs on PATH.
    GitBash(GitBash),
    /// PowerShell with the command `-EncodedCommand`ed (Windows without Git Bash).
    PowerShell(PowerShellSettings),
}

impl CommandShell {
    pub fn select(
        windows: bool,
        git_bash: Option<GitBash>,
        powershell: Option<PowerShellSettings>,
    ) -> Option<Self>;
    /// Program, arguments and the extra environment (after `set_env`) for `command`.
    pub fn invocation(&self, command: &str, set_env: &[(OsString, OsString)]) -> Invocation;
}

/// What to spawn: the program, its arguments and the environment pairs to set.
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}
```

`select` takes the platform as a parameter (like `posix_shell_available`) and returns `Sh` off
Windows, `GitBash` when found, else `PowerShell` when found, else `None`. Production sites pass
`cfg!(windows)` and `expect` the startup check's guarantee when it returns `None`. `invocation` reuses `git_bash::sh_command` /
`GitBash::path_value` and `powershell::launch_args` — no new quoting logic. `HookProcess` and
`StatuslineScript` replace their `git_bash: Option<GitBash>` field with `shell: CommandShell` and
build their `tokio::process::Command` from `invocation`, deleting their duplicated Git Bash
branches.

### The model's tools

`ShellSettings` (in `builtins.rs`) already carries `git_bash` and `powershell`. Registration
changes:

- `shell` and `monitor` are registered only when `settings.posix_shell_available(cfg!(windows))`,
  a helper on `ShellSettings` that returns `!windows || git_bash.is_some()`. Claude Code: Monitor
  "is available only when Git Bash is installed" on Windows.
- `powershell` registration is unchanged; in the fallback it is the only command tool.
- No new tool, no risk-class change. The shared test settings (`ShellSettings::default()`,
  `fontaine-tools/tests/common/mod.rs:120`) have `git_bash: None`, so on Windows CI
  `the_registered_set_is_exactly_the_documented_one` and
  `powershell_exists_only_when_an_interpreter_was_found` (`choke_point.rs:38`, `:59`) would lose
  `shell`/`monitor`. The shared test env therefore carries a fake `GitBash` on Windows, as
  `fontaine-tools/tests/choke_point.rs:225-231` does.
- Model-facing text names `shell` only when it is registered. The `powershell` description
  (`builtins.rs:732-733`: "use `shell` for that"), `fontaine-core/prompts/system_prompt.md` (lines
  9, 17, 26, 36, including the denial-evasion guidance), the `read`/`ls`/`glob`/`grep`
  descriptions (`builtins.rs:57,184,243`) and the `job_output` description
  (`jobs/tools.rs:293`) name `powershell` instead when `shell` is not registered.

Nothing about `powershell`'s gating changes: always guardian-judged, `floor::critical_powershell`
on the floor, bare `shell` rules covering it.

### `!` lines

`AgentRuntime::run_user_shell` builds `powershell_for(primary, &settings, interpreter)` when
`git_bash` is `None` on Windows (it already has `setup.powershell`), and `shell_for` otherwise.
`ShellTool::run` delegates to rig-tools' `Shell::run`, whose start/finish are private and hard-code
`sh`, so `PowerShellTool::run` cannot mirror it; and `PowerShellTool`'s only existing path is
`run_job`, which needs a `&mut ToolContext` and `Jobs` and returns a `String`. `PowerShellTool`
instead gains a foreground `pub async fn run(&self, args: ShellArgs) -> Result<ShellRun,
ToolExecutionError>` over its existing `launch` (`builtins.rs:690`) and `launch_in`: spawn, wait
under the same timeout and no-output timeout, cut at the same `OutputLimit`, return `ShellRun`.
`run_user_shell` dispatches through a small private enum over the two tools (rig's `Tool` is not
object-safe, so no `Box<dyn …>`), keeping the call site one line. The ledger/audit record keeps the tool name `shell`
(it records the user's action, not the interpreter).

### Hooks without Git Bash fail closed (Claude Code parity)

Claude Code: "Without Git Bash, a `PreToolUse` … command hook that fails can block the tool call …
blocking because Git Bash wasn't found, so only exit code 0 or 2 counts as this hook's verdict."
Fontaine matches: when the hook shell is `CommandShell::PowerShell`, a `pre_tool_use` hook that
exits with anything other than 0 or 2 (or fails to start, or times out) returns a `deny` decision
whose reason says `blocking because Git Bash was not found, so only exit code 0 or 2 counts as this
hook's verdict`, instead of a `HookError`. Rationale: a POSIX hook run by PowerShell will typically
fail, and a deny guard that silently "decides nothing" would loosen the user's setup. This only
tightens, so it respects "hooks never loosen on failure". Malformed JSON on exit 0 keeps today's
"decides nothing" path (Claude Code's text concerns exit codes). `post_tool_use`,
`permission_denied`, `task_completed` and `teammate_idle` keep today's failure handling.

Implementation: the hook timeout is enforced in `run_hooks` (`permissions.rs:742-751`,
`tokio::time::timeout(hook.timeout(), hook.run(event))`), outside `CommandHook`. So
`fails_closed(&self) -> bool` goes on the `PreToolUseHook` trait (`permissions.rs:313-337`; default
`false`; `true` for a `CommandHook` whose shell is `CommandShell::PowerShell`). `run_hooks` turns only
`Ok(Err(HookError::Failed { .. }))` and `Err(_)` (timeout) into the Git Bash deny when it returns
true. `HookError::Malformed` (unparseable JSON, unknown `permissionDecision`) reaches the same arm
but still decides nothing. The `other =>` arm in `user_hooks.rs` keeps returning
`HookError::failed` for non-0/2 exits and start/encode failures, with no branching of its own.

### Subagent toolsets

`AgentType::toolset`: when the parent has no `shell` tool, a type entry naming bare `shell` (in
`tools` or `disallowed`) also covers `powershell`; a specifier entry such as `shell(git *)` in `tools` does
not, since child `only` limits match on tool name. So the builtin `worker`, and any user type that
lists `shell`/`Bash`, gets `powershell` in the fallback, and a type that disallows `shell` still has
no command tool. With Git Bash present nothing changes, so the 2026-10-30 "worker does not get
`powershell`" Decision stands there. A child's toolset still only narrows the parent's; the mode
invariant is untouched. The worker's description stays as is ("runs shell commands").

Skill `!` lines keep their existing behavior: without Git Bash the skill shell is `None` and the
command is reported as not run. Running POSIX skill snippets in PowerShell would mostly fail or do
something else; Claude Code requires an explicit `shell: powershell` frontmatter for that, which is
a separate feature (see Trade-offs).

## Implementation Steps

1. **Command shell abstraction** — add `rust/fontaine-tools/src/command_shell.rs` with
   `CommandShell`, `select`, `invocation`, unit tests; `pub mod command_shell` in
   `fontaine-tools/src/lib.rs`.
2. **Startup** — `rust/fontaine/src/lib.rs`: Windows `git_bash()` maps `NotFound` → `Ok(None)`;
   `prepare_session` errors with exit 2 when both are absent, pushes the fallback warning when only
   Git Bash is absent. `GitBashError::NotFound`'s Display is no longer user-facing (the no-shell
   message is composed in `prepare_session`): trim its prerequisite wording, and update the
   `git_bash.rs` module doc (lines 1-3) and the `rust/fontaine/src/lib.rs:890` comment.
3. **Tool registration** — `rust/fontaine-tools/src/builtins.rs`: register `shell` and `monitor`
   only when `ShellSettings::posix_shell_available(windows: bool)` holds; registration and
   `run_user_shell` call it with `cfg!(windows)`, tests pass the platform explicitly. Update the
   shared test env / `choke_point.rs` expectations as described in Design §The model's tools.
4. **Hooks** — `rust/fontaine-core/src/user_hooks.rs`, `rust/fontaine-core/src/permissions.rs`:
   `HookProcess.shell: CommandShell`, spawn through `invocation`; `PreToolUseHook::fails_closed`
   and the `Failed`/timeout fail-closed mapping in `run_hooks`. Update every
   `HookProcess { git_bash: … }` construction (`agent/setup.rs`, tests). State the fail-closed
   exception in the `PreToolUseHook` trait doc (`permissions.rs:311-312`) and the `user_hooks.rs`
   module doc (line 7).
5. **Status line** — `rust/fontaine-core/src/statusline.rs`: same field swap. The construction site
   is `rust/fontaine-tui/src/lib.rs:383-391` (`StatuslineScript::new(..., runtime.git_bash().clone(),
   primary)`); `AgentRuntime` (`agent.rs:632-635`) exposes only `git_bash()`, so add a
   `command_shell()` accessor and update that call site.
6. **`!` lines** — `rust/fontaine-core/src/user_shell.rs`: PowerShell when there is no POSIX shell.
   `rust/fontaine-tools/src/builtins.rs`: give `PowerShellTool` a
   `pub async fn run(&self, args: ShellArgs) -> Result<ShellRun, ToolExecutionError>` mirroring
   `ShellTool::run` (`builtins.rs:353`).
7. **Model-facing text** — `rust/fontaine-tools/src/builtins.rs`, `jobs/tools.rs`,
   `rust/fontaine-core/src/prompt.rs`, `prompts/system_prompt.md`: the `powershell`, `read`/`ls`/
   `glob`/`grep` and `job_output` descriptions and the system prompt name `powershell` when `shell`
   is not registered. `prompt.rs` `base()` (68-104) cannot see the registered tools, so pass it
   whether `shell` is available; update the `system_prompt__the_assembled_prompt_is_stable`
   snapshot. Unit test: the fallback prompt names `powershell` and not `shell`.
8. **Subagents** — `rust/fontaine-agents/src/registry.rs`: `toolset` maps bare `shell` to also
   cover `powershell` when the parent lacks `shell`.
9. **Plan and design records** — `tasks/design/platforms.md`: replace line 10 "Git for Windows is a
   prerequisite" and "not found ⇒ startup error" with the fallback table, and "Hooks and `!` lines
   stay on Git Bash" with the fallback behavior. `tasks/design/permissions.md` §User hooks (lines
   ~259-261): state the fallback's fail-closed `pre_tool_use` exception.
   Master plan `## Decisions`: dated lines at the end of `### Tools` (fallback replaces the startup
   error; Monitor absent without Git Bash; worker gets
   `powershell` only in the fallback, narrowing the 2026-10-30 line; no cmd shell: Claude Code has
   none, cmd has no encoded-command form and re-parses its command line (`%VAR%`, `^`), so the
   judged string is not the one that runs, and PowerShell runs `cmd /c` when needed), `### Build and
   release` (the superseded 2026-10-21 Git-for-Windows prerequisite line, `fontaine_harness_plan.md:435`;
   the `### Tools` 2026-10-21 line at `:923` is discovery only and stays) and `### Extensibility`
   (pre_tool_use hooks fail closed in the fallback; skill `!` lines not run in the fallback).
10. **Docs and changelog** — see Documentation.

## Files to Modify

- `rust/fontaine-tools/src/command_shell.rs` (new), `rust/fontaine-tools/src/lib.rs`
- `rust/fontaine-tools/src/git_bash.rs` (error text, if touched)
- `rust/fontaine-tools/src/builtins.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-core/src/user_hooks.rs`, `rust/fontaine-core/src/permissions.rs`,
  `rust/fontaine-core/src/statusline.rs`, `rust/fontaine-core/src/user_shell.rs`,
  `rust/fontaine-core/src/agent/setup.rs`, `rust/fontaine-core/src/agent.rs` (`command_shell()`),
  `rust/fontaine-tui/src/lib.rs`
- `rust/fontaine-agents/src/registry.rs`
- Tests constructing `HookProcess`/`StatuslineScript`/`ShellSettings`
  (`fontaine-core/tests/user_hooks.rs`, `fontaine-core/tests/project_origins.rs`,
  `fontaine-core/tests/common/*`, `fontaine-tui/tests/common/mod.rs`,
  `fontaine-tools/tests/common/mod.rs`, `fontaine-tools/tests/choke_point.rs`,
  `fontaine-core/tests/choke_point.rs` (:189), `fontaine-core/tests/shell_settings.rs` (:19-20),
  `fontaine-core/tests/common/runtime.rs` (:459), the `statusline.rs` unit test (~:333))
- `rust/fontaine-tools/src/jobs/tools.rs`, `rust/fontaine-core/src/prompt.rs`,
  `rust/fontaine-core/prompts/system_prompt.md`
- `CHANGELOG.md`
- `docs/tools.md`, `docs/hooks.md`, `docs/installing.md`, `docs/agents.md`, `docs/tui.md`,
  `README.md`
- `tasks/design/platforms.md`, `tasks/design/permissions.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Fallback vs. hard prerequisite.** Keeping the prerequisite is simpler, but blocks every Windows
  user without Git and deviates from Claude Code. The fallback reuses the existing, already
  hardened `powershell` tool, so no new fast path or gate is introduced.
- **Hooks fail closed in the fallback vs. "decide nothing".** The cost is that a broken POSIX hook blocks calls until the user fixes it or installs Git — the
  reason text says how.
- **No hook `shell` field / skill `shell: powershell`.** Claude Code has both; they are independent
  features usable with Git Bash present too, so they are left out to keep this change to the
  missing-prerequisite fix.

## Documentation

- `docs/tools.md`: Windows bullet — Git Bash preferred, PowerShell fallback, no-shell error,
  `monitor` absent without Git Bash; table rows for `shell` (:19) and `monitor` (:23) gain "only
  with Git Bash on Windows"; PowerShell section (`:300-329`) — "beside `shell`" becomes conditional, and
  "Hooks, `!` commands and skill `!` lines keep running through Git Bash" becomes the fallback
  description.
- `docs/hooks.md:36`: hooks run in PowerShell without Git Bash; `pre_tool_use` fail-closed rule.
- `docs/installing.md:102`, `README.md:39`: Git for Windows recommended, not required.
- `docs/tui.md` §Custom status line: same fallback note; §Shell commands (~309-320): without Git
  Bash, `!` lines run in PowerShell.
- `CHANGELOG.md`: an entry under `[Unreleased]` citing #510.
- `docs/agents.md` (built-in types table, ~line 87, worker row): `powershell` in place of `shell` on
  Windows without Git Bash.

## Testing Strategy

Unit tests (all platforms, through the existing `Probe`/constructed-settings seams):

- `command_shell`: `select` for each (os, git_bash, powershell) combination; `invocation` for
  `PowerShell` yields the interpreter path and `-EncodedCommand` args that decode to the command
  under the preamble; `GitBash` carries the command in `FONTAINE_SH_COMMAND` and prepends its dirs;
  `Sh` puts it on the command line.
- Startup selection: factor the table into a pure function in `fontaine/src/lib.rs` (git result ×
  powershell option → Ok(prep inputs + warning?) / Err(code)) and test all rows, including
  `BadOverride` still exiting 2.
- Registration: `fontaine-tools/tests/choke_point.rs` — a Windows-shaped `ShellSettings` without
  Git Bash registers no `shell`/`monitor` and does register `powershell`; with Git Bash both.
  (`posix_shell_available` takes the platform as a parameter so this runs off Windows.)
- Hooks: test the `run_hooks` mapping with a fake `PreToolUseHook` whose `fails_closed` is true,
  following `Double` in `fontaine-core/tests/hook_slot.rs:31` (its `Reply::Fail`, `Malformed` and
  `Hang` replies): `Failed` → `Deny` with the Git Bash reason; timeout → the same `Deny`;
  `Malformed` → no decision; with `fails_closed` false, `Failed` → `HookError` (unchanged).
  `user_hooks` tests cover exit 1 → `HookError::Failed`.
- Subagents: `registry.rs` — parent `[read, powershell]`: worker gets `powershell`; type
  disallowing `shell` gets none; parent `[read, shell, powershell]`: worker gets `shell` only; a
  type listing only `shell(git *)` does not get `powershell`.

Windows-only integration tests (CI `windows-latest`, which has Git installed — the run hides it):

- `fontaine -p` with `PATH` reduced to `%SystemRoot%\System32`, `ProgramFiles`,
  `ProgramFiles(x86)`, `LocalAppData` pointed at an empty temp dir and `FONTAINE_GIT_BASH_PATH`
  removed from the env, scripting a guardian verdict (judge account + `--permission-mode auto`,
  following `rust/fontaine/tests/headless_tools.rs` ~102-106, 145): starts (exit 0 with a
  deterministic model), prints the fallback warning, and a scripted `powershell` call runs. Pins the
  reported bug (startup exit 2 without Git for Windows).
- Same environment, a `pre_tool_use` hook `exit 1`: the call is denied with the Git Bash reason —
  silent failure class (permissions), so automated.

## Manual Verification

1. On a Windows machine without Git for Windows (or a VM): `fontaine` → TUI opens with the fallback
   warning; ask the model to list files → it uses `powershell`; `!dir` runs. Not automated because
   CI images always have Git installed; the integration tests simulate it by hiding Git.
