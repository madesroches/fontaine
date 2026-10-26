# Approval Prompt Diff Preview Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/45

## Overview

Claude Code's permission dialog shows the proposed diff for an edit before the human decides.
Fontaine's `Allow edit?` prompt shows only the path (`summary`) and the reason, so the human
approves a change they cannot see (study capture `e11_fontaine/05_`). This plan adds a diff
preview to the approval prompt for `edit` and `write` calls. The issue's other two asks are
already met and need no code (see Current State); they are recorded in Decisions.

## Current State

- `ApprovalRequest` (`rust/fontaine-core/src/permissions.rs:46`) carries `summary`, `reason`,
  `args`, `offers`, `proposed_rule`. `ask_human` (~line 1500) builds it; `summarize`
  (`permissions.rs:1689`) maps `edit`/`write`/`read` to the path only and `shell` to the full command.
- `Approval::body` (`rust/fontaine-tui/src/approval.rs`) wraps `summary` then `reason` into
  scrollable lines (PageUp/PageDown, `scroll`/`max_scroll`; `docs/tui.md` keys table, ~line 80); `footer` (choices, rule line) is
  always visible. A long shell command already scrolls inside the box (`docs/tui.md`, "Approval prompts").
- The guardian's explanation on an `escalate` verdict already reaches the prompt as the
  `[Rule] explanation` reason (`docs/tui.md`, "An `escalate` verdict opens the usual approval prompt").
- Edit args are `{path, edits: [{old_text, new_text, replace_all}]}`; write args are `{path, content}`.
  `rig_tools::edit::apply(path, text, &[Replacement])` applies edits to a string without touching the
  file, exactly as the tool will, returning `Applied { text, change, replaced, notes }` (`change` is a `FileChange` with hunks); it preserves BOM and CRLF, and errors
  when an edit matches nowhere or more than once, or when the edits change nothing.
- `rig_tools::fs::file_change(path, Option<&str> before, Option<&str> after) -> FileChange` is public
  and diffs two texts into hunks (3 lines of context; `Hunk { old: Lines, new: Lines, lines: Vec<HunkLine> }`,
  `HunkLine::Same/Added/Removed(String)` with the line ending kept; `Lines { start, count }` from 1).
  Paths resolve through `Scope`
  (`rust/fontaine-tools/src/scope.rs`), which knows the workspace roots.

## Design

### Preview data (fontaine-core)

New `rust/fontaine-core/src/preview.rs`:

```
pub struct DiffPreview { pub path: String, pub added: usize, pub removed: usize,
                         pub lines: Vec<DiffLine>, pub hidden: usize /* changed lines not shown */ }
pub struct DiffLine { pub kind: DiffKind /* Context | Added | Removed | Gap */,
                      pub old_no: Option<usize>, pub new_no: Option<usize>, pub text: String }
pub fn preview(tool: &str, args: &Value, scope: &Scope) -> Option<DiffPreview>
```

- `edit`: read the file (UTF-8, at most the tool's own read limit), run `rig_tools::edit::apply`
  to get `Applied.change`; `write`: `rig_tools::fs::file_change(path, before, Some(content))` with
  before = existing file text or `None` (a created file shows all lines added). Both yield a
  `FileChange`; its hunks map to `DiffLine`s (`HunkLine::Same/Added/Removed` to `Context/Added/Removed`,
  line numbers counted up from each hunk's `old.start`/`new.start`, trailing line ending trimmed), with a
  `Gap` line between hunks. One mapping for both tools, no diff code of our own.
- Returns `None` (prompt falls back to today's path-only text) when: the tool is not `edit`/`write`;
  the path resolves outside the scope roots (no reading files the call could not touch anyway);
  the file is unreadable, non-UTF-8 or over the limit; `apply` fails, including when the edits change nothing (the tool will fail the same
  way, so there is nothing to approve that a diff could show); or args do not parse.
- Display safety: `preview.rs` has its own sanitizer (`action_summary` is a single-line summarizer
  and does not expand tabs): expand tabs, replace other control chars with spaces, so file content
  cannot inject terminal escapes, then `rig_core::redact::text` so credential-shaped text is not painted.
  Total lines are capped (200); past that `hidden` counts the changed lines not shown and the TUI says so.
- The file read happens inside async `ask_human`, so the preview is computed with
  `tokio::task::spawn_blocking` and a preview-specific size cap constant `PREVIEW_MAX_BYTES`
  (1 MiB), much smaller than rig-tools' `MAX_FILE_BYTES` (16 MiB).
- The preview is display-only: it is not sent to the guardian, hooks, telemetry or the ledger, and
  does not influence any decision (error text may explain, never change, a decision). It is computed
  in `ask_human` only, after the pipeline has already decided a human must answer.

### Request and prompt

- `ApprovalRequest` gains `preview: Option<DiffPreview>`; `ask_human` fills it. `DiffPreview` and
  `DiffLine` derive `Clone, Debug` because `ApprovalRequest` does. Other literal sites (`permissions.rs`,
  `fontaine-tui` `approval.rs` tests (~286), `linear.rs`, `screen.rs`, `app.rs`, `ui/tests.rs`) add `preview: None`.
- `Approval::body` renders, after the question and summary and before the reason, a header
  `path  +A -R` followed by the diff lines: line-number gutter (width from the largest number), `+`/`-`
  prefix, added lines in `theme.success`, removed in `theme.error`, context and gaps dim. Scrolling
  (PageUp/PageDown) works unchanged. The existing `wrap` (`input.rs:197-207`) just chunks
  chars by width and knows nothing about prefixes, so diff text is wrapped to `width - gutter_width - 1`;
  continuation rows get a blank gutter and a space in place of the sign.
  Colors come from the existing `Theme`; no new theme fields. The linear (non-fullscreen) renderer
  (`linear.rs`) prints the same lines via the shared `body`.
- A preview with `hidden > 0` ends with `… N more changed lines (not shown)`, N = `hidden`.

## Implementation Steps

1. Add `fontaine-core/src/preview.rs` with the types, `preview()` and the `FileChange` to `DiffLine` mapping
   (`rig_tools` is already a dependency; no new crate); export from `lib.rs`.
2. Add `preview` to `ApprovalRequest`; fill it in `ask_human`; fix the construction sites.
3. Render the preview in `Approval::body` (`fontaine-tui/src/approval.rs`). Linear mode uses the
   shared `body` via `view::overlay` and is plain text (styling dropped), so the sign and gutter
   carry the meaning.
4. Update docs (below), `CHANGELOG.md`, and the plan's `## Decisions` in the master plan.

## Files to Modify

- `rust/fontaine-core/src/preview.rs` (new), `src/lib.rs`, `src/permissions.rs`
- `rust/fontaine-tui/src/approval.rs`, `src/linear.rs`, `src/screen.rs`, `src/app.rs`, `src/ui/tests.rs` (literals)
- `docs/tui.md`, `docs/terminal.md`, `docs/permissions.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Diff in the TUI from `args` vs. in core.** The TUI has no scope or file access, and the
  headless approver could reuse a core-built preview, so core builds it.
- **Use rig-tools' `FileChange` hunks vs. our own `similar` diff.** `apply` returns the change for
  `edit` and the public `rig_tools::fs::file_change` produces one for `write`, so both tools share
  one source and one renderer with no new dependency and no diff code. Cost: hunk context is fixed
  at 3 lines and line endings must be trimmed when mapping.
- **Show `old_text`/`new_text` pairs straight from args.** No file read and no failure modes, but
  it is not what the file will look like (no line numbers, loose matching, `replace_all` hidden).
  Rejected: the point is to show the real change.

## Decisions

- 2026-10-26: The shell half of the issue ("lexed command") is already met: the prompt shows the
  full command, wrapped and scrollable. No lexing or regex on arguments is added (floor rule).
- 2026-10-26: The guardian explanation on escalate already appears as the prompt's reason; no change.
- 2026-10-26: MCP and other tools get no preview; only `edit` and `write` have a known file effect.
- 2026-10-26: The preview is computed from the file at prompt time. The file can change before the
  tool runs; the tool applies its edits to the file as it then is, so the preview is advisory
  while the permission decision is unchanged.
- 2026-10-26: AFK: Build the preview from rig-tools `FileChange` hunks (`Applied.change` for `edit`, public `rig_tools::fs::file_change` for `write`) instead of adding `similar` and a diff of our own; verified `file_change` is `pub` in rig-tools 0.45.0 and returns `FileChange`.
- 2026-10-26: AFK: Pinning `similar = "=2.7.0"` is moot: no `similar` dependency is added to fontaine-core.
- 2026-10-26: The preview line cap and file-size limit are constants, not configurable.

## Documentation

- `docs/tui.md` "Approval prompts": the diff preview, when it is absent, truncation.
- `docs/terminal.md`: the linear "Permission Required:" output now includes the diff preview (plain text).
- `docs/permissions.md`: one sentence that the preview is display-only and affects no decision.
- `CHANGELOG.md`: user-facing entry.

## Testing Strategy

Unit tests in `preview.rs` (constructed inputs against a tempdir):
- edit: single and multi-edit and `replace_all` diffs with correct line numbers and counts;
  write over an existing file; write creating a file (all added).
- `None` cases: other tool, path outside roots, missing/unreadable file for `edit`, non-UTF-8,
  over the size limit, an edit that matches nowhere.
- Escape injection: a file line containing `\x1b[31m` renders with no control characters; a
  credential-shaped string is redacted.
- Truncation at the line cap sets `hidden` to the number of changed lines not shown.
- CRLF and lone `\r` in file content are sanitized; an edit on a BOM/CRLF file previews without error.

TUI tests (`TestBackend`, plain assertions, in `fontaine-tui/src/view.rs` and `approval.rs` where
rendering tests live): a prompt with a preview shows gutter, `+`/`-` lines and header; a long diff
scrolls with PageDown; at a narrow width, diff text wraps to `width - gutter_width - 1` with a blank gutter and space sign on continuation rows (`Approval::body(narrow_width, theme)` in `approval.rs`); a request without a preview renders exactly the same rows as before. A `fontaine-core` pipeline test asserts that the
request handed to the approver for an `edit` call carries a preview and that denying/allowing is
unaffected by it.

## Manual Verification

1. Run `cargo run -p fontaine`, in manual mode ask the model to edit a file in the workspace.
   Expect the `Allow edit?` box to show the diff above the choices. Not automated: only a real
   terminal shows the colors and overall look (tmux, per `tasks/design/testing.md`).
2. Repeat in a short terminal (a few rows tall): the approval body is capped at about half the
   screen height (`view.rs` `shown_body_rows`), so the diff scrolls with PageUp/PageDown and the
   footer choices stay visible.
