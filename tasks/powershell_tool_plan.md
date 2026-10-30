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
  scope path extraction (`fontaine-tools/src/scope.rs:52`), rule matching
  (`permission_rules.rs:410,672`), the critical-path check (`permissions.rs:734`,
  `floor::critical_shell` at `fontaine-core/src/floor.rs:163`) and the rule-specifier text
  (`permissions.rs:1921`). Tier 0.5 (`Preclassifier::read_paths`) is gated on `tool == "shell"`
  (`permissions.rs:793`).
- `monitor` is the precedent for a second shell-class tool that is never pre-classified.
- `pwsh`/`powershell` are already in the code-execution interpreter list
  (`fontaine-core/src/code_execution.rs:43`), so `shell(pwsh *)` style rules already suspend.
- Design docs record PowerShell as backlog (`tasks/design/platforms.md:15`, master plan Decisions
  2026-10-21).

## Design

**Tool.** `PowerShellTool` in `fontaine-tools` (new `powershell.rs`), same call shape as
`ShellCall` minus `run_in_background` handling differences: `command`, `timeout_secs`, `cwd`,
`run_in_background`. It reuses rig's `Shell` with a launcher prefix of
`<powershell> -NoProfile -NonInteractive -Command` so scope confinement, timeout clamp, output
cut and the job machinery are shared rather than copied. The command text is passed so PowerShell
parses it exactly as judged (via `-EncodedCommand` UTF-16LE base64, avoiding Windows command-line
re-parsing; the encoded form is an implementation detail never shown to the guardian or user, who
see the plain command).

**Interpreter discovery.** Prefer `pwsh.exe` (PowerShell 7) on PATH, then `powershell.exe`
(Windows PowerShell 5.1 from `%SystemRoot%\System32\WindowsPowerShell\v1.0`).
`FONTAINE_POWERSHELL_PATH` overrides. The tool is registered **only on Windows and only when an
interpreter is found**; absence is not a startup error (unlike Git Bash), the tool simply does not
exist. On other platforms it is never registered, so the model is not offered it.

**Descriptor row.** `powershell`: `RiskClass::Arbitrary`, `path_free: false`,
`Destructive::PowerShell` (new variant: always derived as destructive-unknown, since no parse can
clear it), `Specifier::PowerShell` (new variant), Claude name `PowerShell`. A distinct specifier
keeps it out of `is_shell_class`, so none of the POSIX analyses ever run on PowerShell text.
Consequences, each handled explicitly:
- No Tier 0.5: already true, because the gate is `tool == "shell"`.
- Rules: a bare `powershell` rule names the tool whole. Deny and ask rules for `shell` also cover
  `powershell` (lenient posture, as for `monitor`), so a prohibition cannot be stepped around.
  Allow rules cover only the tool they name. Specifiers (`powershell(Get-ChildItem *)`) are not
  supported: `Specifier::PowerShell` rejects them at config load with a message, since wildcard
  matching on unparsed PowerShell would be the regex-on-arguments the project forbids.
- Scope: `scope.rs` treats it as having an optional `cwd` path field; no extraction of paths from
  the command text.
- Guardian: receives the plain command as `command`, tool name `powershell`. Unparseable-by-design,
  so every call is judged; fail-closed behavior is unchanged.
- `auto`-mode rule suspension: a blanket `powershell` allow rule is suspended in `auto` like a
  blanket `shell` allow (add to the code-execution set).

**Critical path for `Remove-Item`.** New `floor::critical_powershell(command, cwd, env)` beside
`critical_shell`, reusing `is_critical_target` and `CriticalEnv`. It is a deliberately small,
token-level scan, not a PowerShell parser, and its soundness rule mirrors the POSIX one: anything
it cannot positively read as safe while a deleter is mentioned is critical.
- Deleters (case-insensitive): `Remove-Item`, `ri`, `rm`, `rmdir`, `rd`, `del`, `erase`,
  plus `[IO.Directory]::Delete`,
  `[System.IO.Directory]::Delete`, `[IO.File]::Delete` and `cmd /c rd|rmdir|del`.
- Tokenizer: splits on whitespace honoring single and double quotes and backtick escapes; a token
  is "literal" only when it contains no `$`, `(`, `)`, `@`, `{`, `}`, backtick-escape-sensitive
  `%`-env or `*`/`?` wildcard that could widen the target (a wildcard is critical only when the
  literal prefix is itself critical or the pattern is bare `*`).
- Operands: positional operands and values of `-Path`, `-LiteralPath`,; options
  starting with `-` (`-Recurse`, `-Force`, `-WhatIf`) are skipped. A comma list (`a,b`) is split.
  `$env:USERPROFILE`, `$HOME`, `~` and `%USERPROFILE%` resolve to the home directory; any other
  variable, subexpression or pipeline-supplied target (`Get-ChildItem | Remove-Item`,
  `... | % { Remove-Item $_ }`) is critical ("target cannot be resolved").
- Chains: when a deleter appears and the text contains `;`, `|`, `&&`, `||`, `&`, a newline or
  `-Command`/`Invoke-Expression`/`iex`, it is critical ("deleter inside a command that is not one
  simple command") — the same conservatism as the POSIX path.
- Targets are judged by `is_critical_target`: drive roots, top-level directories, `%USERPROFILE%`,
  a workspace root or its parent, and symlink/junction resolution, all case-insensitive on Windows
  (already the behavior of the shared helpers). PowerShell drive-qualified forms (`C:\`, `C:`,
  `\\server\share`, `FileSystem::C:\...`) are normalized first.
- Non-FileSystem providers (`HKLM:\`, `Env:`, `Cert:`) are not file deletes; `Remove-Item` on them
  is left to the guardian rather than the critical-path tier.

`permissions.rs:734` dispatches by tool: `shell`/`monitor` → `critical_shell`, `powershell` →
`critical_powershell`; the `Tier::CriticalPath` prompt/deny semantics are unchanged.

**Jobs and subagents.** The job registry, `job_output`, `job_cancel` and background semantics are
shared with `shell` through the common `Launcher`; the Windows process-tree kill is whatever rig's
`Shell` already does. A subagent's tool list may name `powershell`; effective-mode and
restriction inheritance are unchanged because they key off tool names generically. The tool
description tells the model PowerShell syntax applies, that each call is a fresh process, and to
prefer the dedicated file tools.

## Implementation Steps

1. **Descriptor and names.** Add `Destructive::PowerShell` and `Specifier::PowerShell`; add the
   `powershell` row (`Arbitrary`, Claude name `PowerShell`); update the descriptor table test and
   `names` tests. Make destructive derivation (`destructive.rs`) return the always-destructive
   class for it.
2. **Discovery.** `fontaine-tools/src/powershell.rs`: `discover()` (PATH `pwsh`, then
   `powershell.exe`, `FONTAINE_POWERSHELL_PATH`), plus the `-EncodedCommand` encoder. Unit-test
   encoding round-trips (quotes, backslashes, non-ASCII, newlines).
3. **Tool.** `PowerShellTool` + `powershell_for(root, settings)`; add `powershell:
   Option<PowerShellSettings>` to `ShellSettings`/`BuiltinEnv`; register in `builtins::registry`
   when `Some` (Windows only; `None` elsewhere). Share the job-launch path with `ShellTool` rather
   than duplicating it (extract the common launch helper).
4. **Floor.** `floor::critical_powershell` with the tokenizer and deleter table; wire into
   `permissions.rs`.
5. **Rules and config.** `permission_rules.rs`: deny/ask `shell` covers `powershell`; reject
   specifiers on `powershell` rules at config load; extend the `auto` code-execution suspension.
   Extension `map_tool_entry` and the Claude-name map accept `PowerShell`.
6. **Wiring.** Discover once at startup in `fontaine/src/lib.rs` / `agent/setup.rs` alongside Git
   Bash; pass into `ShellSettings`. Hooks and `!` lines stay on Git Bash.
7. **Guardian prompt.** Confirm the tool name and `command` field render unchanged in
   `prompt_builder`; add a one-line note that `powershell` commands are not pre-parsed.
8. **Docs and changelog.** See Documentation.

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs`, `names.rs` (tests), `builtins.rs`, `lib.rs`,
  new `powershell.rs`, `scope.rs`
- `rust/fontaine-core/src/floor.rs`, `permissions.rs`, `permission_rules.rs`, `destructive.rs`,
  `code_execution.rs`, `extensions.rs`, `agent/setup.rs`
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-tools/tests/choke_point.rs` (expected set depends on platform)
- `docs/tools.md`, `docs/permissions.md`, `docs/guardian.md`, `tasks/design/platforms.md`,
  `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Separate tool vs. a `shell` mode.** A separate tool matches Claude Code, keeps the POSIX
  parser away from PowerShell text, and lets rules and guardian context name the language.
- **Token-level critical scan vs. a PowerShell parser.** No sound lexer exists in-tree; a scan
  that treats everything unreadable near a deleter as critical errs toward prompting, which is
  the safe direction for the floor.
- **`-EncodedCommand` vs. `-Command` with the text on the command line.** Encoding avoids
  Windows quoting re-parse so the judged text is what runs, the same property the Git Bash
  env-var path buys.
- **No specifier rules.** Wildcard matching on unparsed PowerShell would be matching on tool
  arguments without a sound parse.

## Decisions

## Documentation

- `docs/tools.md`: the `powershell` tool, Windows-only, discovery and `FONTAINE_POWERSHELL_PATH`.
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
  target` inside the workspace, a registry-provider path, a command with no deleter.
  A golden adversarial set mirrors the POSIX one in `tasks/design/testing.md`.
- Encoder round-trip; discovery order with a fake PATH.
- Descriptor table and choke-point tests: `powershell` has a row and risk class; the registered
  set equals the documented one with and without a PowerShell interpreter.
- Permissions: never reaches Tier 0.5; reaches the guardian in `manual`/`auto`; `shell` deny/ask
  rules cover it; `powershell(...)` specifier rejected; critical path prompts in every mode.
- Windows-only integration test (`permissions_windows.rs` style, runs in the Windows CI leg):
  a headless `powershell` call round-trips output, cwd and exit code, and runs a command
  containing quotes and a backslash path unchanged — catches the command-line re-parse a fake
  launcher could not.

## Open Questions

- Should hooks and `!` lines also be able to use PowerShell? Out of scope here; they stay on Git
  Bash as in Claude Code.
