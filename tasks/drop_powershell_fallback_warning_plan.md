# Drop the PowerShell Fallback Startup Warning Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/513

## Overview

On Windows without Git for Windows, Fontaine starts with a warning: "Git for Windows was not
found: shell commands, hooks and the status line run in PowerShell...". Running in PowerShell is a
supported configuration (#510), so the warning is noise. Remove it. The model-facing text already
names `powershell` when `shell` is absent, so nothing else needs to tell anyone which shell is in
use.

## Current State

- `rust/fontaine/src/lib.rs` `Shells` carries `warning: Option<&'static str>`; `choose_shells`
  sets it to `POWERSHELL_FALLBACK_WARNING` in the `(NotFound, Some(powershell))` arm.
- `prepare_session` destructures `warning: shell_warning` and pushes it onto `warnings`
  (`warnings.extend(shell_warning...)`).
- Tests: the unit tests in `lib.rs` (`git_bash_found_starts_as_before_with_no_warning`,
  `without_git_bash_powershell_starts_with_one_warning`) and
  `rust/fontaine/tests/windows_without_git_bash.rs` (asserts the warning on stderr).
- Docs: `docs/tools.md` ("Fontaine starts with a warning and PowerShell runs..."), `CHANGELOG.md`
  (#510 entry: "with one startup warning"), master plan Decisions line 2026-11-08 (#510).
- The `NotFound`/no-PowerShell case stays an exit-2 error naming what to install or set.

## Design

Remove the warning entirely rather than gate it on a bash-specific request: nothing in Fontaine
requests bash specifically (the model is simply offered `powershell` instead of `shell`).

- Delete `POWERSHELL_FALLBACK_WARNING` and the `warning` field of `Shells`; the `Shells` struct
  keeps `git_bash` and `powershell`.
- `prepare_session` stops extending `warnings` with a shell warning.
- `FONTAINE_GIT_BASH_PATH` pointing at a non-Git-Bash stays an exit-2 error (unchanged).

## Implementation Steps

1. `rust/fontaine/src/lib.rs`: remove the constant, the `warning` field, its set in
   `choose_shells`, and the destructure/extend in `prepare_session`.
2. Update tests: in `lib.rs` drop `warning` from the `Shells` expectations and change
   `without_git_bash_powershell_starts_with_one_warning` into a test asserting the fallback
   selects PowerShell with `git_bash == None`; in `windows_without_git_bash.rs` replace the
   stderr-contains assertion with one that stderr does not contain "Git for Windows was not found".
3. Docs: `docs/tools.md` — "Fontaine starts and PowerShell runs..." (no "with a warning").
4. `CHANGELOG.md`: under Unreleased `### Changed`, add an entry referencing #513; the #510 entry's
   "with one startup warning" is an earlier unreleased line, so edit it to drop the phrase.
5. Master plan: append a dated Decisions line at the end of `### Tools` superseding the "one
   startup warning" part of the 2026-11-08 (#510) line.

## Files to Modify

- `rust/fontaine/src/lib.rs`
- `rust/fontaine/tests/windows_without_git_bash.rs`
- `docs/tools.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- Show the warning only when a bash-specific feature is requested (the issue's alternative): no
  such request exists in Fontaine, so it would be dead code. Rejected.
- Demote to a debug log: adds a channel for no consumer. Rejected.

## Documentation

`docs/tools.md` Windows paragraph; `CHANGELOG.md`.

## Testing Strategy

Unit tests on `choose_shells` (fallback yields PowerShell, no Git Bash, no warning field) and the
existing `windows_without_git_bash` integration test, flipped to assert the warning is absent while
the `powershell` tool still runs the command. These cover everything; no manual verification.
