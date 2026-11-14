# Final-Delivery and Compaction Evidence Prompts Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/443

## Overview
Prompt-only change in two places: the base system prompt asks for verification of the final
deliverable and honest reporting; the compaction prompts keep observed evidence distinct from
unsupported claims across (repeated) compaction.

## Current State
- `rust/fontaine-core/prompts/system_prompt.md`, `# Doing the work`: three bullets (understand,
  smallest change and check it, report unverified work). Snapshot:
  `rust/fontaine-core/tests/snapshots/system_prompt__the_assembled_prompt_is_stable.snap`;
  needle tests in `rust/fontaine-core/tests/system_prompt.rs`.
- `rust/fontaine-core/src/session.rs`: `Session::create`, `Session::open` and
  `set_autocompact_window` each build `CompactionPolicy::default()`; the last rebuilds the policy
  from scratch, so any customization made only at creation is lost when the window is set.
- rig-memory 0.46.0 `SummaryPrompts` (fields `system`, `initial`, `update`, `format`, `in_place`,
  `notes`, all `Cow<'static, str>`) is reached as `policy.summarizer.prompts`.

## Design
1. **System prompt.** Extend `# Doing the work` concisely with: match the response to the request
   (action requests are carried through; analysis, explanation and plan-only requests stay
   read-only); before calling a change complete, run the project's relevant standard checks on the
   final deliverable and exercise the affected scenario end to end where safe; never claim verified
   completion with covering checks red or unrun, stale evidence, or a partial implementation; do
   not silently shrink the deliverable or repair unrelated baseline failures, report blocked and
   unverified parts plainly; re-read the explicit requirements and inspect the final worktree state after checks,
   removing only known self-created unneeded artifacts; keep verification proportional and inside
   the permission pipeline (no production calls, destructive checks or repeated green reruns).
2. **Compaction.** One function in `session.rs`, `fn compaction_policy() -> CompactionPolicy`,
   returns `CompactionPolicy::default()` with `summarizer.prompts` customized, and is the only
   constructor: used by `create`, `open` and `set_autocompact_window`. The customization appends
   an evidence-rules block to `format` (used by both initial and update summaries; `in_place` is
   not used by Fontaine's `plan_summary` path, so it is left alone), and one sentence to `update`
   (do not upgrade unverified claims to facts; mark checks stale after later edits). The block
   asks to place the new items inside the existing `Critical context` section as fixed labelled
   sub-lists (`Unverified or stale`, `Unknowns`, `Output state`), and the appended text explicitly
   allows those labels despite "use exactly this format". The block asks to: record exact commands and touched paths with their
   concrete outcomes; list unknowns and unsupported assumptions separately from decisions; list
   unverified claims and checks made stale by later edits; state preview/truncation/spill status
   with recovery pointers, never inferring that full output was seen and listing uninspected
   ranges as unknown; never copy credentials even when preserving commands or values.
   `system`, `notes`, length-truncation rejection and failure behavior stay untouched.

## Implementation Steps
1. Edit `system_prompt.md`; update the snapshot (`cargo insta` accept) and add needle assertions.
2. Add `compaction_policy()` to `session.rs`; use it at the three sites.
3. Tests (below). 4. `CHANGELOG.md` entry; docs if a page describes compaction or the prompt.

## Files to Modify
`rust/fontaine-core/prompts/system_prompt.md`, the snapshot, `rust/fontaine-core/src/session.rs`,
`rust/fontaine-core/tests/system_prompt.rs`, `rust/fontaine-core/tests/ledger.rs`, `CHANGELOG.md`.

## Trade-offs
Appending to `format` rather than replacing prompts keeps the structured format and upstream
wording; a replacement would drift from rig-memory on upgrades.

## Decisions
- AFK: new evidence items live as labelled sub-lists inside `Critical context` (most conservative:
  keeps the six existing headings unchanged).
- No summary schema validation, no TODO/log reattachment, no blind trust of reported completion
  (per the issue).

## Documentation
No docs change: no page documents the base prompt or the compaction prompts. CHANGELOG entry only.

## Testing Strategy
- System prompt: snapshot updated; needle test for the new requirements.
- Unit test in `session.rs`: `compaction_policy()` prompts (`format`, `update`) contain the
  evidence block and keep the default structured headings; after `set_autocompact_window` the
  session's policy prompts equal `compaction_policy()`'s (pins the reset regression).
- `ledger.rs` request-level tests with `MockCompletionModel` (`requests()`), both through
  `set_autocompact_window`: one initial-summary and one update-summary (history extended after a
  first compaction) test, on a history holding a truncated/spilled-style tool result with its cut
  marker, a command with its outcome, an unread path, and a test followed by an edit (a small
  helper adds tool-call/result messages). Assert single-line fragments, since `{:?}` escapes
  newlines: the evidence instruction and the cut marker.
- No live-API tests.

## Open Questions
None.
