# Shell Backgrounding Note Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/792

## Overview

When a main-conversation `shell` (or `powershell`) call reaches its timeout or its 60 s idle
limit, the command moves to a background job and the result ends with a note that lists
`job_output` and `job_cancel` as equals. Agents that follow skills written for Claude Code read it
as "stuck, rerun" and cancel healthy work. In a real session a `gh pr checks --watch` was cancelled
after 10 minutes and restarted. Reword the note so it says the move is normal, that nothing failed,
and that waiting with `job_output` is the default, and say in the tool descriptions that a timeout
does not stop the command.

## Current State

- `rust/fontaine-sdk/src/tools/builtins.rs:610-622` (`render_foreground`, `None` arm): one
  `format!` serves both paths. `why` is `printed nothing for {quiet}s` (idle) or
  `still running after {timeout}s`, followed by
  `; moved to background job {id}. Its output so far is above. Use job_output to read more or wait
  for it, job_cancel to stop it. You are told when it ends.]`
- The `shell` description (`builtins.rs:706-722`) and the `powershell` description
  (`builtins.rs:~903-912`) say a command past its timeout "keeps running as a background job and
  you are told when it ends", but not that the timeout leaves it running rather than failing.
- `job_cancel` already reports how long the job ran: `render_read` ends with `state_line`, which
  renders `[{id} ended: cancelled, after {age}]` (`jobs/tools.rs:58-62`, checked by
  `tests/jobs.rs:154`). The issue's third "Related" item needs no change.
- Tests pinning the current text: `rust/fontaine-sdk/tests/jobs.rs:95` (`starts_with("start\n[still
  running after 1s; moved to background job job-")`), `tests/tools_choke_point.rs:351`
  (`contains("still running after 1s; moved to background job")`), `tests/shell_cwd.rs:198` and
  `fontaine-tui/tests/tui.rs:1523` (`contains("moved to background job")`).
- The cassettes under `rust/fontaine/tests/cassettes/` embed the tool descriptions in recorded
  request bodies, but replay does not match on bodies (#786 changed the `shell` description after
  they were recorded and they still pass), so they need no re-recording.
- Docs: `docs/tools.md` "Timeout" bullet (line ~285) and "Background jobs" section (line ~361).

## Design

### The note

Replace the `None` arm's text with:

```
[{why}, not an error: the command moved to background job {id} and keeps running. Its output so
far is above. Wait for it with job_output (wait_secs); you are told when it ends. job_cancel
throws away the work done so far.]
```

where `why` stays `still running after {timeout}s` or `printed nothing for {quiet}s`. `why` holds
the reason the call returned; the clause after it carries the issue's three points: not an
error, keeps running, wait is the default.

The `run_in_background` start note (`builtins.rs:565`) is not changed: there the model chose the
background itself, so there is nothing to misread.

### The tool descriptions

In both the `shell` and `powershell` descriptions, change "keeps running as a background job and
you are told when it ends" to "is not stopped: it keeps running as a background job and you are
told when it ends". One phrase, no new sentence, since both descriptions are long already.

## Implementation Steps

1. `rust/fontaine-sdk/src/tools/builtins.rs`: reword the note in `render_foreground`; add "is not
   stopped:" to the `shell` and `powershell` descriptions.
2. Update the tests that pin the old text:
   - `tests/jobs.rs:95`: `starts_with("start\n[still running after 1s, not an error: the command
     moved to background job job-")`; replace the `job_output`/`job_cancel` `contains` check with
     checks for `"keeps running"`, `"Wait for it with job_output"` and `"job_cancel throws away"`.
     `job_id` (first `job-` plus 8 chars) still finds the id, since no earlier `job-` appears in the new text.
   - `tests/tools_choke_point.rs:351`: new prefix.
   - `tests/shell_cwd.rs:198` and `fontaine-tui/tests/tui.rs:1523`: `"moved to background job"`
     still matches; leave them.
3. Add a test for the idle path's wording: a `shell` call with no `timeout_secs` that prints
   nothing would wait 60 s, which is too slow, so cover it as a unit test of `render_foreground`
   in `builtins.rs`'s test module with a constructed `Foreground { id, chunk: Chunk::default(), end: None, quiet: true }`,
   `Shell::default()` and `quiet = Some(60s)`, asserting the text starts with `[printed nothing for 60s, not an
   error:` and contains `Wait for it with job_output`.
4. Docs: `docs/tools.md`, "Background jobs" — after "the command keeps running", add that this is
   the normal outcome, not a failure, and the result says to wait with `job_output`; cancelling
   throws away the work done. Keep the "Timeout" bullet as is (it already says the command is not
   stopped).
5. `CHANGELOG.md` entry at the end of `### Changed` under `[Unreleased]`, citing #792.

## Files to Modify

- `rust/fontaine-sdk/src/tools/builtins.rs`
- `rust/fontaine-sdk/tests/jobs.rs`
- `rust/fontaine-sdk/tests/tools_choke_point.rs`
- `docs/tools.md`
- `CHANGELOG.md`

## Trade-offs

- **Separate wording per path** (timeout vs idle) was considered; the shared clause reads right
  for both because `why` already differs, so one string stays.
- **A system-prompt line** about timeouts was rejected: the note is the one place the model sees
  at the moment it decides, and the description change covers the up-front case.
- **Hiding `job_cancel` from the note** was rejected: a hung command still needs a way out, so the
  note names the cost instead of the tool's absence.

## Decisions

- 2026-11-25: `why` stays first in the note so the reason the call returned leads; "Its output so far is above" stays so the model knows nothing was lost.
- 2026-11-25: the issue's "discards" becomes "throws away" for plain wording.

## Documentation

`docs/tools.md` "Background jobs" (step 4). No other page quotes the note.

## Testing Strategy

Unit/integration only, no live API: the updated `tests/jobs.rs` and `tests/tools_choke_point.rs`
assertions pin the timeout path's prefix and its key phrases end to end through the registry, and the new
`render_foreground` unit test pins the idle path. `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings` and `cargo test` from `rust/`.

## Open Questions

None.
