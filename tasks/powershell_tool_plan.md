# PowerShell Tool for Windows Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/28

## Overview

Claude Code ships a `PowerShell` tool on Windows alongside `Bash`. Fontaine's Windows `shell` runs
through Git Bash, so a model asked to use `Remove-Item`, `Get-ChildItem` or a `.ps1` script has no
native way to do so. This adds a separate, Windows-only `powershell` tool that never pre-classifies
(there is no sound PowerShell lexer, so every call reaches the guardian) and extends the
deterministic critical-path check to `Remove-Item` targets.

## Current State

- `shell` is rig's `Shell` wrapped by `ShellTool` (`rust/fontaine-tools/src/builtins.rs:283`),
  registered per root in `builtins::registry` (`builtins.rs:~780`) with the risk class from
  `descriptor::TOOLS` (`rust/fontaine-tools/src/descriptor.rs:75`). On Windows it runs `sh -c`
  from Git Bash (`rust/fontaine-tools/src/git_bash.rs`).
- Everything command-aware keys off `descriptor::is_shell_class` (`Specifier::Shell`): the POSIX
  `preclassify::analyze` used for destructive-class derivation (`fontaine-core/src/destructive.rs:296`),
  scope path extraction (`fontaine-tools/src/scope.rs:52`: `fields_of` maps every shell-class tool
  to `Optional("cwd")` via `is_shell_class`; `powershell` returns `None` today and needs its own arm), rule matching
  (`permission_rules.rs:410,672`), the critical-path check (`permissions.rs:734`,
  `floor::critical_shell` at `fontaine-core/src/floor.rs:163`) and the approval-display text
  (`summarize()`, `permissions.rs:1921`). Tier 0.5 (`Preclassifier::read_paths`) is gated on `tool == "shell"`
  (`permissions.rs:793`).
- `monitor` is the precedent for a second shell-class tool that is never pre-classified.
- `pwsh`/`powershell` are already in the code-execution interpreter list
  (`fontaine-core/src/code_execution.rs:43`), so `shell(pwsh *)` style rules already suspend.
- Design docs record PowerShell as backlog (`tasks/design/platforms.md:15`, master plan Decisions
  2026-10-21).

## Design

**Tool.** `PowerShellTool` in `fontaine-tools` (new `powershell.rs`), same call shape as
`ShellCall` minus `run_in_background` handling differences: `command`, `timeout_secs`, `cwd`,
`run_in_background`. rig's `Shell::run` hardwires `sh -c`, so a launcher prefix cannot work
(it would yield `pwsh -Command sh -c ...`). `PowerShellTool` has its own run/launch path that
spawns the interpreter with explicit argv `-NoProfile -NonInteractive -EncodedCommand <base64>`
(UTF-16LE base64, avoiding Windows command-line re-parsing so PowerShell parses exactly what was
judged). The launch helper is extracted from `ShellTool` taking program and argv as parameters, and
shares timeout clamp, output cut and `Jobs::start` handling. The plain command is what `Jobs` and
the foreground display show; the encoded form is never shown to the guardian or user.
Hooks and `!` lines stay on Git Bash (non-goal, as in Claude Code); `agent/build.rs:193` and
`skills.rs:183` `!` execution stay keyed on `"shell"`.

**Interpreter discovery.** Prefer `pwsh.exe` (PowerShell 7) on PATH, then `powershell.exe`
(Windows PowerShell 5.1 from `%SystemRoot%\System32\WindowsPowerShell\v1.0`).
`FONTAINE_POWERSHELL_PATH` overrides. The tool is registered **only on Windows and only when an
interpreter is found**; absence is not a startup error (unlike Git Bash), the tool simply does not
exist. On other platforms it is never registered, so the model is not offered it.

**Descriptor row.** `powershell`: `RiskClass::Arbitrary`, `path_free: false`,
`Destructive::Always`, `Specifier::None` (already rejects specifiers and stays out of
`is_shell_class`, so none of the POSIX analyses ever run on PowerShell text), Claude name
`PowerShell`. Consequences, each handled explicitly:
- No Tier 0.5: already true, because the gate is `tool == "shell"`.
- Rules: a bare `powershell` rule names the tool whole. `tool_matches` (`permission_rules.rs:410`)
  gets an explicit `powershell` clause so bare `shell` deny/ask rules also cover `powershell`
  (lenient posture, as for `monitor`); specifier'd `shell(...)` rules do not match it. Allow rules
  cover only the tool they name. Specifiers (`powershell(Get-ChildItem *)`) are rejected by
  `Specifier::None`.
- Scope: `scope.rs` `fields_of` gets an explicit `"powershell"` arm returning
  `PathFields::Optional("cwd")`; no extraction of paths from the command text. `destructive.rs`
  `touched_paths` is intentionally empty for powershell.
- Guardian: receives the plain command as `command`, tool name `powershell`. Unparseable-by-design,
  so every call is judged; fail-closed behavior is unchanged. The approval-prompt display text
  (`summarize()` in `permissions.rs:~1921`; fallback `args.to_string()`) gets an explicit
  `powershell` arm showing the plain `command`.
- `auto`-mode rule suspension: `is_broad_code_execution` (`permission_rules.rs:669-684`) gets a
  `("powershell", None)` arm so a blanket `powershell` allow is suspended like a blanket `shell`
  allow.

**Critical path for `Remove-Item`.** New `floor::critical_powershell(command, cwd, env)` beside
`critical_shell`, reusing `is_critical_target` and `CriticalEnv`. It is a deliberately small,
token-level scan, not a PowerShell parser, and its soundness rule mirrors the POSIX one: anything
it cannot positively read as safe while a deleter is mentioned is critical.
- Deleters (case-insensitive): `Remove-Item`, `ri`, `rm`, `rmdir`, `rd`, `del`, `erase`,
  plus `[IO.Directory]::Delete`,
  `[System.IO.Directory]::Delete`, `[IO.File]::Delete` and `cmd /c rd|rmdir|del`. The cmdlet and
  alias names are detected as whole tokens, case-insensitive, at command position (start, or after
  `|`, `;`, `&`, `(` or a newline), so `Write-Host`, `Format-Table` and `Get-Process` never match.
  Substring scanning of the raw text is used only for the `[IO.File]::Delete` /
  `[IO.Directory]::Delete` forms and `cmd /c rd|rmdir|del`. `iex`/`Invoke-Expression` and a
  non-literal command position (`&`, `.` or `$`-led) stay critical.
- Tokenizer: splits on whitespace honoring single and double quotes and backtick escapes; a token
  is "literal" only when it contains no `$`, `(`, `)`, `@`, `{`, `}`, `%` or `*`/`?` wildcard.
  A non-literal command position (`&`, `.` or `$`-led) is critical.
- Wildcards: judge the literal prefix; critical if the pattern is bare `*` or the prefix is critical.
- Operands: positional operands and values of `-Path`, `-LiteralPath`, `-Include`, `-Exclude`,
  `-Filter`; `-Param:value` forms are split and the value judged. Other options starting with `-`
  (`-Recurse`, `-Force`, `-WhatIf`) are skipped. A comma list (`a,b`) is split. Zero resolved
  operands is critical. `$env:USERPROFILE`, `$HOME` and `~` resolve to the home directory; `%` is
  always unresolved (PowerShell does not expand `%VAR%`) except `%USERPROFILE%`, mapped to home
  conservatively. Any other variable, subexpression or pipeline-supplied target
  (`Get-ChildItem | Remove-Item`, `... | % { Remove-Item $_ }`) is critical ("target cannot be
  resolved").
- Chains: when a deleter token is found and the text contains `;`, `|`, `&&`, `||`, `&`, a newline or
  `-Command`/`Invoke-Expression`/`iex`, it is critical ("deleter inside a command that is not one
  simple command") — the same conservatism as the POSIX path.
- Targets are judged by `is_critical_target`: drive roots, top-level directories, `%USERPROFILE%`,
  a workspace root or its parent, and symlink/junction resolution, all case-insensitive on Windows
  (already the behavior of the shared helpers). PowerShell drive-qualified forms (`C:\`, `C:`,
  `\\server\share`, `FileSystem::C:\...`) are normalized first.
- Non-FileSystem providers (`HKLM:\`, `Env:`, `Cert:`) are not file deletes; `Remove-Item` on them
  is left to the guardian rather than the critical-path tier.

The `permissions.rs:734` gate (`is_shell_class(tool) && ...`) is rewritten as a match/extra branch:
`shell`/`monitor` → `critical_shell`, `powershell` → `critical_powershell`; the
`Tier::CriticalPath` prompt/deny semantics are unchanged.

**Jobs and subagents.** The job registry, `job_output`, `job_cancel` and background semantics are
shared with `shell` through the common `Launcher`; the launch helper spawns through the same
`workspace.spawn`/`Process`/`Jobs` path as `shell`, so cancel and timeout kill only the direct
child on Windows (rig-tools `Process::kill` is `child.kill()`). A subagent's tool list may name `powershell`; effective-mode and
restriction inheritance are unchanged because they key off tool names generically. The tool
description tells the model PowerShell syntax applies, that each call is a fresh process, and to
prefer the dedicated file tools.

## Implementation Steps

1. **Descriptor and names.** Add the `powershell` row (`Arbitrary`, `Destructive::Always`,
   `Specifier::None`, Claude name `PowerShell`); update the descriptor table test and `names` tests.
2. **Discovery.** `fontaine-tools/src/powershell.rs`: `discover()` (PATH `pwsh`, then
   `powershell.exe`, `FONTAINE_POWERSHELL_PATH`), plus the `-EncodedCommand` encoder. Unit-test
   encoding round-trips (quotes, backslashes, non-ASCII, newlines).
3. **Tool.** `PowerShellTool` + `powershell_for(root, settings)`; add `powershell:
   Option<PowerShellSettings>` to `ShellSettings`/`BuiltinEnv`; register in `builtins::registry`
   when `Some` (Windows only; `None` elsewhere). Extract the launch helper from `ShellTool`
   taking program and argv as parameters (sharing timeout, cut and `Jobs::start`); it also serves
   the foreground and non-root path, with the same `JobError::NotRoot` refusal of
   `run_in_background` as `ShellTool::call` (`builtins.rs:~523`). `PowerShellTool` calls it with
   `-NoProfile -NonInteractive -EncodedCommand <base64>`. `powershell_for` does not inherit the Git
   Bash PATH or `through_env` setup from `shell_for`.
4. **Floor.** `floor::critical_powershell` with the tokenizer and deleter table; wire into
   `permissions.rs`.
5. **Rules and config.** `permission_rules.rs`: explicit `powershell` clause in `tool_matches` so
   bare `shell` deny/ask rules cover `powershell`; `("powershell", None)` arm in
   `is_broad_code_execution`. Extension `map_tool_entry` and the Claude-name map accept `PowerShell`.
6. **Wiring.** Discover once at startup in `fontaine/src/lib.rs` / `agent/setup.rs` alongside Git
   Bash; pass into `ShellSettings`. Hooks and `!` lines stay on Git Bash.
7. **Guardian prompt.** Confirm the tool name and `command` field render unchanged in
   `prompt_builder`; add a one-line note that `powershell` commands are not pre-parsed.
8. **Docs and changelog.** See Documentation.

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs`, `names.rs` (tests), `builtins.rs`, `lib.rs`,
  new `powershell.rs`, `scope.rs`
- `rust/fontaine-core/src/floor.rs`, `permissions.rs` (incl. `summarize`), `permission_rules.rs`,
  `destructive.rs`, `extensions.rs`, `tools.rs`, `agent.rs`, `agent/build.rs` (plumbing only),
  `agent/setup.rs` (the powershell setting follows `git_bash`'s chain)
- `rust/fontaine-agents/src/registry.rs` (~190 worker tool list: the worker agent does NOT get
  `powershell`)
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-core/tests/docs.rs` (`docs/tools.md` needs a row for every `TOOLS` row)
- `rust/fontaine-evals/src/baseline.rs` (add a `PowerShell` Claude-name entry)
- `rust/fontaine-core/tests/choke_point.rs` (~211 lists `monitor`)
- `rust/fontaine-tools/tests/choke_point.rs` (EXPECTED of 12; the test injects a PowerShell
  interpreter setting; expected set depends on platform)
- `rust/fontaine/tests/headless_smoke.rs` (~43)
- `docs/tools.md`, `docs/permissions.md`, `docs/guardian.md`, `docs/configuration.md` (~566 tool
  names), `docs/extensions.md` (~67 Claude-name table), `tasks/design/platforms.md`,
  `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Separate tool vs. a `shell` mode.** A separate tool matches Claude Code, keeps the POSIX
  parser away from PowerShell text, and lets rules and guardian context name the language.
- **Token-level critical scan vs. a PowerShell parser.** No sound lexer exists in-tree; a scan
  that treats everything unreadable near a deleter as critical errs toward prompting, which is
  the safe direction for the floor.

## Decisions

- Cancel/timeout of a `powershell` call kills only the direct child process on Windows, same as
  `shell`; no process-tree kill is added.

## Documentation

- `docs/tools.md`: the `powershell` tool, Windows-only, discovery and `FONTAINE_POWERSHELL_PATH`.
- `docs/configuration.md` and `docs/extensions.md`: add `powershell` / `PowerShell` to the tool-name
  lists and the Claude-name table.
- `docs/permissions.md`: never pre-classified; bare-name rules only; `shell` deny/ask covers it;
  the `Remove-Item` critical-path behavior.
- `docs/guardian.md`: every `powershell` call is judged.
- `tasks/design/platforms.md` and the master plan Decisions: replace "PowerShell tool is backlog"
  with a dated line recording the tool and its no-pre-classify rule.
- `CHANGELOG.md` under Unreleased.

## Testing Strategy

Unit tests (no live APIs, runnable on every OS since the scan and encoder are pure):
- `critical_powershell`: critical — `Remove-Item -Recurse -Force C:\`, `ri C:\Windows`,
  `del $env:USERPROFILE`, `Remove-Item ~ -Recurse`, `rm -r (Get-Location).Path`-style
  subexpressions, pipeline-fed deleters, chained commands with a deleter, workspace root and its
  parent, `-LiteralPath`/`-Path` forms, case variants, comma lists, `FileSystem::` prefix,
  junction-to-critical; not critical — `Remove-Item .\build\out.txt`, `Remove-Item -Recurse
  target` inside the workspace, a registry-provider path, a command with no deleter, `Write-Host`/`Format-Table`/`Get-Process` in
  a pipeline (deleter names inside other tokens do not match).
  A golden adversarial set mirrors the POSIX one in `tasks/design/testing.md`.
- Encoder round-trip; discovery order with a fake PATH.
- Descriptor table and choke-point tests: `powershell` has a row and risk class; the registered
  set equals the documented one with and without a PowerShell interpreter.
- Permissions: never reaches Tier 0.5; reaches the guardian in `manual`/`auto`; `shell` deny/ask
  rules cover it; `powershell(...)` specifier rejected (`Specifier::None`); specifier'd `shell(...)` rules do not
  match it; critical path prompts in every mode.
- Windows-only integration test (`rust/fontaine-tools/tests/shell.rs` is the model: `#![cfg(windows)]`, like
  `a_double_backslash_reaches_git_bash_as_written`; runs in the Windows CI leg):
  a headless `powershell` call round-trips output, cwd and exit code, and runs a command
  containing quotes and a backslash path unchanged — catches the command-line re-parse a fake
  launcher could not.
