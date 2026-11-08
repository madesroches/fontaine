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

- `FONTAINE_GIT_BASH_PATH` pointing at a non-Git-Bash stays an exit-2 error (unchanged).

## Implementation Steps

1. `rust/fontaine/src/lib.rs`: remove the constant, the `warning` field, its set in
   `choose_shells`, and the destructure/extend in `prepare_session`; change the `Shells` doc
   comment (~901) from "The shells found at startup, and what to tell the user about the choice."
   to "The shells found at startup."
2. Update tests: in `lib.rs` drop `warning` from the `Shells` expectations, rename
   `git_bash_found_starts_as_before_with_no_warning` to `git_bash_found_starts_as_before`, and change
   `without_git_bash_powershell_starts_with_one_warning` into `without_git_bash_powershell_starts`,
   asserting `chosen == Shells { git_bash: None, powershell: Some(pwsh()) }`; in `windows_without_git_bash.rs` replace the
   stderr-contains assertion with one that stderr does not contain "Git for Windows was not found".
3. Docs: `docs/tools.md` — "Fontaine starts and PowerShell runs..." (no "with a warning");
   `tasks/design/platforms.md` table row (line 20): drop "with one warning" from "starts with one
   warning; PowerShell is the command shell".
4. `CHANGELOG.md`: no new entry. Amend the existing unreleased #510 entry (~lines 48-54) in place:
   drop ", with one startup warning" (~line 50) and change the closing "(#510)" at ~line 54 to
   "(#510, #513)" (precedent: "(#451, #241)").
5. Master plan: append this dated Decisions line at the end of `### Tools`: "- 2026-11-08 (#513):
   Supersedes the 'one startup warning says so' part of the 2026-11-08 (#510) line: without Git
   Bash Fontaine starts with no warning, since the model-facing text already names `powershell`."

## Files to Modify

- `rust/fontaine/src/lib.rs`
- `rust/fontaine/tests/windows_without_git_bash.rs`
- `docs/tools.md`
- `tasks/design/platforms.md`
- `CHANGELOG.md`
- `tasks/fontaine_harness_plan.md`

## Trade-offs

- Show the warning only when a bash-specific feature is requested (the issue's alternative): no
  such request exists in Fontaine, so it would be dead code. Rejected.
- Demote to a debug log: adds a channel for no consumer. Rejected.

## Documentation

See Implementation Steps 3-4.

## Testing Strategy

Unit tests on `choose_shells` (fallback yields PowerShell, no Git Bash) and the
existing `windows_without_git_bash` integration test, flipped to assert the warning is absent while
the `powershell` tool still runs the command. These cover everything; no manual verification.
