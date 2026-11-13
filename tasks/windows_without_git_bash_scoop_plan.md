# Windows-without-Git-Bash tests on scoop machines Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/604

## Overview
The three tests in `rust/fontaine/tests/windows_without_git_bash.rs` fail on a Windows machine
whose Git comes from scoop, because the test sandbox hides Git from discovery incompletely. Make
the sandbox hide every location discovery searches, so the tests pass regardless of the host.

## Current State
- `without_git` (`windows_without_git_bash.rs`) sets `PATH` to System32 and points `ProgramFiles`,
  `ProgramW6432`, `ProgramFiles(x86)` and `LocalAppData` at an empty directory.
- `git_bash::discover` (`rust/fontaine-tools/src/git_bash.rs`) also searches scoop roots:
  `SCOOP` (default `%USERPROFILE%\scoop`) and `SCOOP_GLOBAL` (default `%ProgramData%\scoop`),
  each at `apps\git\current\cmd\git.exe`. None of `SCOOP`, `SCOOP_GLOBAL`, `USERPROFILE`,
  `ProgramData` is scrubbed, so a scoop Git on the host is still found.

## Design
Product code is correct; only the test sandbox changes. In `without_git`, also point `SCOOP`,
`SCOOP_GLOBAL`, `USERPROFILE` and `ProgramData` at the empty directory. `SCOOP`/`SCOOP_GLOBAL`
are set explicitly (not left to their defaults) so the scrub does not depend on how discovery
derives the defaults. Update the doc comment on `without_git` to say it covers the scoop roots.

Do not touch `USERPROFILE` more broadly in `Sandbox`: other tests may rely on it, and this is
scoped to the Git-less helper.

## Implementation Steps
1. `rust/fontaine/tests/windows_without_git_bash.rs`: extend the variable list in `without_git`
   with `SCOOP`, `SCOOP_GLOBAL`, `USERPROFILE`, `ProgramData`; adjust the doc comment.

## Files to Modify
- `rust/fontaine/tests/windows_without_git_bash.rs`

## Trade-offs
- Setting only `USERPROFILE` (the issue's suggestion) would still leave a host `SCOOP` or
  `SCOOP_GLOBAL` variable, and a global scoop install under `ProgramData`, visible.

## Documentation
None; test-only change.

## Testing Strategy
The three existing tests are the regression coverage: they fail on a scoop-Git host today and
must pass after the change. Run `cargo test -p fontaine --test windows_without_git_bash` on this
scoop machine, plus `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`.
