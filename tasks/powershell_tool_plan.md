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
  registered per root in `builtins::build` (`builtins.rs:~710`) with the risk class from
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
- `monitor` is shell-class (`Specifier::Shell`, `Destructive::Shell`) and is never pre-classified only
  because of the `tool == "shell"` gate at `permissions.rs:~793`. `powershell` is the first
  command-running tool outside `is_shell_class`.
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
judged). Argv also carries `-OutputFormat Text` (no `#< CLIXML` envelopes on stderr). The encoded
script is a fixed preamble, then a newline, then the user's command verbatim:
`$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $OutputEncoding=[Console]::OutputEncoding`
(no progress records, UTF-8 output instead of the OEM codepage). The preamble never touches the
command text, and the guardian and display see only the plain command. Exit codes: `exit N` is
preserved; no `exit $LASTEXITCODE` is appended, so a failing native command's code is not
propagated and `Write-Error` exits 0 (documented in `docs/tools.md`, as PowerShell's own
`-EncodedCommand` semantics). The launch helper is extracted from `ShellTool` taking program and argv as parameters, and
shares timeout clamp, output cut and `Jobs::start` handling. `call_job`'s timeout/cut/quiet logic
(`builtins.rs:~348-441`) is parameterized by program+argv; `ShellTool::prepared` (the
`sh_command`/`through_env` rewrite) is bypassed; a foreground wait/collect path is added for the
non-job case. The timeout cap comes from `ShellSettings.max_timeout_secs` (already present on this
branch, default 600), the same setting as `shell`; PR #308 adds only the `[shell] max_timeout_secs`
config key and the `RegistryParams.shell_max_timeout_secs` plumbing, so this work has no dependency
on it beyond that. The plain command is what `Jobs` and
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
  (lenient posture); specifier'd `shell(...)` rules do not match it. Allow rules
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
- Deleters (case-insensitive): `Remove-Item`, `ri`, `rm`, `rmdir`, `rd`, `del`, `erase`, plus
  `[IO.Directory]::Delete`, `[System.IO.Directory]::Delete`, `[IO.File]::Delete` and
  `cmd /c rd|rmdir|del`. A cmdlet or alias counts as a deleter when it appears as a whole token
  anywhere in the text (not only at command position, so `if ($x) { rm C:\ }` and `& { rm C:\ }`
  are caught); `Write-Host`, `Format-Table` and `Get-Process` never match. Substring scanning of the
  raw text is used only for the .NET delete forms: `::\s*Delete` and `\.Delete\s*\(` anywhere,
  case-insensitive (covers `[IO.Directory]:: Delete(`, `[Directory]::Delete`, `$obj.Delete()`).
  Any `cmd`/`cmd.exe` invocation (`cmd /c`, `cmd /d /c`, `cmd.exe /c "rd /s /q C:\"`) is critical
  without extracting its target. Nested interpreters and string-eval entry points are critical
  unconditionally, independent of finding a deleter token: `pwsh`, `powershell`, `-Command`, `-c`,
  `-EncodedCommand`, `-enc`, `iex`, `Invoke-Expression` (the quoted or encoded command string hides
  the deleter). A non-literal command position (`&`, `.` or `$`-led) also stays critical. The only
  non-critical shape is a single simple command with a literal, resolvable target. Tokens such as
  `Write-Host rm`, `Get-Help rm` or `git rm x` are over-flagged (zero operands -> critical); that is
  accepted, as in the POSIX scan. MSYS-style path translation and PSDrive aliases of the filesystem
  are left to the guardian.
- Tokenizer: first splits on `; | & ( ) { } ,` in addition to whitespace (so `echo hi;rm -r C:\`,
  `(rm C:\)`, `$(rm C:\)`, `a|rm x` and `{rm C:\}` expose `rm` as its own token), honoring single and
  double quotes and backtick escapes. A `Module\` prefix (e.g.
  `Microsoft.PowerShell.Management\Remove-Item`) is stripped before deleter matching. Each token
  is one of: literal; wildcard (contains `*` or `?` but nothing below); or non-literal (contains
  `$`, `(`, `)`, `@`, `{`, `}` or `%`, so the target cannot be resolved).
- Wildcards: judge the literal prefix; critical if the pattern is bare `*` or the prefix is critical.
- Operands: positional operands and values of `-Path` and `-LiteralPath`; `-Param:value` forms are
  split and the value judged. `-Include`, `-Exclude` and `-Filter` values are patterns, not targets:
  they are consumed and ignored (never judged, never treated as positional operands). An option name
  is matched case-insensitively as an unambiguous prefix (min. 2 letters, e.g. `-Pa`, `-Lit`, `-Inc`,
  `-Rec`) of the known parameters. Value-less switches (`-Recurse`, `-Force`, `-WhatIf`, `-Confirm`)
  are skipped. Any other option (unknown name, ambiguous prefix, `-Credential`, `-Stream`, ...) makes
  the call critical, because its value cannot be told apart from a positional target. A comma list (`a,b`) is split. Zero resolved
  operands is critical. `$env:USERPROFILE`, `$HOME` and `~` resolve to the home directory; `%` is
  always unresolved (PowerShell does not expand `%VAR%`) except `%USERPROFILE%`, mapped to home
  conservatively. Home-variable resolution runs before non-literal classification (so
  `$env:USERPROFILE` and `$HOME` are judged as home despite containing `$`). A provider-qualified
  operand (2+ letters before `:`, e.g. `HKLM:`, `Env:`, `Cert:`) counts as resolved and non-critical.
  Any other variable, subexpression or pipeline-supplied target
  (`Get-ChildItem | Remove-Item`, `... | % { Remove-Item $_ }`) is critical ("target cannot be
  resolved").
- Chains: when a deleter token is found anywhere and the text contains `;`, `|`, `&&`, `||`, `&` or a
  newline (the interpreter and `iex` forms are already unconditionally critical above), it is critical ("deleter inside a command that is not one
  simple command") — the same conservatism as the POSIX path.
- Targets are judged by `is_critical_target`: drive roots, top-level directories, `%USERPROFILE%`,
  a workspace root or its parent, and symlink/junction resolution, all case-insensitive on Windows
  (already the behavior of the shared helpers). PowerShell drive-qualified forms (`C:\`, `C:`,
  `\\server\share`, `FileSystem::C:\...`) are normalized first.
- Non-FileSystem providers (`HKLM:\`, `Env:`, `Cert:`) are not file deletes; `Remove-Item` on them
  is left to the guardian rather than the critical-path tier.

`permissions.rs:~734` is a single `if is_shell_class(tool) && ...`; the condition is widened to also
admit `powershell`, and inside or beside that `if` the check picks `critical_shell` (`shell`,
`monitor`) or `critical_powershell` (`powershell`) by tool name, sharing the one `Tier::CriticalPath`
body; its prompt/deny semantics are unchanged.

**Jobs and subagents.** The job registry, `job_output`, `job_cancel` and background semantics are
shared with `shell` through `Jobs::start` and `Process`, so cancel and timeout kill only the direct
child on Windows (rig-tools `Process::kill` is `child.kill()`). A subagent's tool list may name `powershell`; effective-mode and
restriction inheritance are unchanged because they key off tool names generically. Skill `!` lines run through `shell` only, so a subagent whose tools are only `powershell` has no
tool for them. Restrictions naming `shell` (e.g. `shell(git *)`) do not constrain `powershell`. The tool
description tells the model PowerShell syntax applies, that each call is a fresh process, and to
prefer the dedicated file tools.

## Implementation Steps

1. **Descriptor and names.** Add the `powershell` row (`Arbitrary`, `Destructive::Always`,
   `Specifier::None`, Claude name `PowerShell`); update the descriptor table test and `names` tests.
2. **Discovery.** `fontaine-tools/src/powershell.rs`: `discover()` (PATH `pwsh`, then
   `powershell.exe`, `FONTAINE_POWERSHELL_PATH`), plus the `-EncodedCommand` encoder. Unit-test
   encoding round-trips (quotes, backslashes, non-ASCII, newlines).
3. **Tool.** `PowerShellTool` + `powershell_for(root, settings)`; add `powershell:
   Option<PowerShellSettings>` to `ShellSettings`/`BuiltinEnv` (defaulting to `None`); register in
   `builtins::build` when `Some`. Extract the launch helper from `ShellTool` as described in Design,
   keeping the `JobError::NotRoot` refusal of `run_in_background` as in `ShellTool::call`
   (`builtins.rs:~523`). `powershell_for` does not inherit the Git Bash PATH or `through_env` setup
   from `shell_for`.
4. **Floor.** `floor::critical_powershell` with the tokenizer and deleter table; wire into
   `permissions.rs` per Design.
5. **Rules and config.** `permission_rules.rs`, `scope.rs` and `summarize()` arms per Design.
   Extension `map_tool_entry` and the Claude-name map accept `PowerShell`.
6. **Wiring.** Discover once at startup in `fontaine/src/lib.rs` / `agent/setup.rs` alongside Git
   Bash and pass into `ShellSettings`.
7. **Guardian prompt.** Confirm the tool name and `command` field render unchanged in
   `prompt_builder`; add a one-line note that `powershell` commands are not pre-parsed.
8. **Docs and changelog.** See Documentation.

## Files to Modify

- `rust/fontaine-tools/src/descriptor.rs` (incl. `rows_are_pinned`: `[Pinned; 22]` -> 23, ~353), `names.rs` (tests), `builtins.rs`, `lib.rs`,
  new `powershell.rs`, `scope.rs`
- `rust/fontaine-core/src/floor.rs`, `permissions.rs` (incl. `summarize`), `permission_rules.rs`,
  `tools.rs`, `agent.rs`, `agent/build.rs` (plumbing only),
  `agent/setup.rs` (the powershell setting follows `git_bash`'s chain)
- Test only: `destructive.rs` (`Destructive::Always` already derives true) and `extensions.rs`
  (`map_tool_entry` goes through `find_claude`).
- `rust/fontaine/src/lib.rs`
- `rust/fontaine-core/tests/docs.rs` (`docs/tools.md` needs a row for every `TOOLS` row)
- `rust/fontaine-evals/src/baseline.rs` (add a `PowerShell` Claude-name entry; the `tool == "shell"` deny/ask scoring check at ~78)
- `rust/fontaine-core/tests/choke_point.rs`: `expected` is built from all `TOOLS` minus `websearch`
  (~180-185); exclude `powershell` the same way and add a platform/setting-conditional entry to
  `built_ins` (~200-216)
- `rust/fontaine-tools/tests/choke_point.rs`: registration keys off `Some(PowerShellSettings)`, not
  `cfg(windows)`; run with and without an injected setting (13 vs 12 entries) on all OSes
- `rust/fontaine/tests/headless_smoke.rs` (~43): pins an exact tool list with `PATH=/nonexistent`;
  the System32 fallback is not PATH-based, so the test also sets `FONTAINE_POWERSHELL_PATH` to a
  nonexistent path so discovery finds nothing
- If `Default` for the new `ShellSettings`/`BuiltinEnv` field is not feasible, the literals listing
  `git_bash` need edits: `fontaine-core/tests/choke_point.rs:153`, `tests/common/runtime.rs:191`,
  `project_origins.rs:21`, `extensions.rs:323`, `user_hooks.rs:63,:703`,
  `fontaine-tui/tests/common/mod.rs:370`, `fontaine-tools/tests/choke_point.rs:159`
  (`user_shell.rs` and `user_hooks.rs` stay on Git Bash)
- `docs/tools.md`, `docs/permissions.md`, `docs/guardian.md`, `research/research-claude-code-parity.md` (~330 backlog line), `docs/configuration.md` (~566 tool
  names), `docs/extensions.md` (~67 Claude-name table), `tasks/design/platforms.md`,
  `tasks/fontaine_harness_plan.md` (Decisions), `CHANGELOG.md`

## Trade-offs

- **Separate tool vs. a `shell` mode.** A separate tool matches Claude Code, keeps the POSIX
  parser away from PowerShell text, and lets rules and guardian context name the language.
- **Token-level critical scan vs. a PowerShell parser.** No sound lexer exists in-tree; a scan
  that treats everything unreadable near a deleter as critical errs toward prompting, which is
  the safe direction for the floor.

## Decisions

- 2026-10-30: No `powershell(...)` specifiers (`Specifier::None`); bare-name rules only.
- 2026-10-30: Bare `shell` deny/ask rules also cover `powershell`; specifier'd `shell(...)` rules do not.
- 2026-10-30: Every `powershell` call goes to the guardian; no Tier 0.5.
- 2026-10-30: The worker agent does not get `powershell`.
- 2026-10-30: The tool is absent when no interpreter is found; not a startup error.
- 2026-10-30: Critical-path check is a token-level scan; non-FileSystem providers are left to the guardian.
- AFK: 2026-10-30: `-EncodedCommand` runs a fixed preamble (silent progress, UTF-8 console output) plus `-OutputFormat Text`, without appending `exit $LASTEXITCODE`; only literal `exit N` is preserved and native exit codes collapse (documented), and the guardian still sees the plain command.
- AFK: 2026-10-30: In the `Remove-Item` critical scan, `-Include`/`-Exclude`/`-Filter` values are ignored as patterns, parameter names match as unambiguous prefixes, and any unknown or ambiguous option makes the call critical.
- These lines carry into the master plan's Decisions entry replacing the "backlog" line.
- Cancel/timeout of a `powershell` call kills only the direct child process on Windows, same as
  `shell`; no process-tree kill is added.

## Documentation

- `docs/tools.md`: the `powershell` tool, Windows-only, discovery and `FONTAINE_POWERSHELL_PATH`.
- `docs/configuration.md` and `docs/extensions.md`: add `powershell` / `PowerShell` to the tool-name
  lists and the Claude-name table.
- `docs/permissions.md`: never pre-classified; bare-name rules only; `shell` deny/ask covers it;
  the `Remove-Item` critical-path behavior; skill `!` lines run through `shell` only; restrictions
  naming `shell` (e.g. `shell(git *)`) do not constrain `powershell`.
- `docs/guardian.md`: every `powershell` call is judged.
- `tasks/design/platforms.md` and the master plan Decisions: replace "PowerShell tool is backlog"
  with the dated lines in this plan's Decisions.
- `CHANGELOG.md` under Unreleased.

## Testing Strategy

Unit tests (no live APIs, runnable on every OS since the scan and encoder are pure):
- `critical_powershell` (unit tests beside `critical_shell` in `floor.rs` / its tests): critical — `Remove-Item -Recurse -Force C:\`, `ri C:\Windows`,
  `del $env:USERPROFILE`, `Remove-Item ~ -Recurse`, `rm -r (Get-Location).Path`-style
  subexpressions, pipeline-fed deleters, chained commands with a deleter, workspace root and its
  parent, `-LiteralPath`/`-Path` forms, case variants, comma lists, `FileSystem::` prefix,
  junction-to-critical; not critical — `Remove-Item .\build\out.txt`, `Remove-Item -Recurse
  target` inside the workspace, a registry-provider path, a command with no deleter, `Write-Host`/`Format-Table`/`Get-Process` in
  a pipeline (deleter names inside other tokens do not match).
  Add deleters inside `{ }` blocks, `& { }` and `if` bodies, and wildcard-vs-unresolvable targets.
  Critical, unglued/nested forms: `echo hi;rm -r C:\`, `(rm C:\)`, `$(rm C:\)`, `a|rm x`, `{rm C:\}`;
  `powershell -Command "rm -r C:\"`, `pwsh -c '...'`, `iex "rm C:\"`, `-EncodedCommand <b64>`;
  `cmd /c "rd /s /q C:\"`, `cmd.exe /c`, `cmd /d /c`, `[IO.Directory]:: Delete(`,
  `[Directory]::Delete`, `$obj.Delete()`, `Microsoft.PowerShell.Management\Remove-Item C:\`;
  `Remove-Item $env:USERPROFILE` is critical as home; `Remove-Item HKLM:\Software\X` is not.
- Encoder round-trip; discovery order with a fake PATH.
- Descriptor table and choke-point tests: `powershell` has a row and risk class; the registered
  set equals the documented one with and without a PowerShell interpreter.
- Permissions: never reaches Tier 0.5; reaches the guardian in `manual`/`auto`; `shell` deny/ask
  rules cover it; `powershell(...)` specifier rejected (`Specifier::None`); specifier'd `shell(...)` rules do not
  match it; critical path prompts in every mode.
- Windows-only integration test (`rust/fontaine-tools/tests/shell.rs` is the model: `#![cfg(windows)]`, like
  `a_double_backslash_reaches_git_bash_as_written`; runs in the Windows CI leg):
  a headless `powershell` call round-trips UTF-8 output (e.g. `é€ü`), cwd and a literal `exit N`
  exit code (not native-command codes), and runs a command
  containing quotes and a backslash path unchanged — catches the command-line re-parse a fake
  launcher could not.
