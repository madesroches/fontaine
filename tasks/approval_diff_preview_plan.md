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
  scrollable lines (PageUp/PageDown, `scroll`/`max_scroll`); `footer` (choices, rule line) is
  always visible. A long shell command already scrolls inside the box (`docs/tui.md`, "Approval prompts").
- The guardian's explanation on an `escalate` verdict already reaches the prompt as the
  `[Rule] explanation` reason (`docs/tui.md`, "An `escalate` verdict opens the usual approval prompt").
- Edit args are `{path, edits: [{old_text, new_text, replace_all}]}`; write args are `{path, content}`.
  `rig_tools::edit::apply(path, text, &[Replacement])` applies edits to a string without touching the
  file, exactly as the tool will, returning the new text and a `FileChange` with hunks; it errors
  when an edit matches nowhere or more than once.
- `similar` 2.7 is already in `Cargo.lock` (transitively). Paths resolve through `Scope`
  (`rust/fontaine-tools/src/scope.rs`), which knows the workspace roots.

## Design

### Preview data (fontaine-core)

New `rust/fontaine-core/src/preview.rs`:

```
pub struct DiffPreview { pub path: String, pub added: usize, pub removed: usize,
                         pub lines: Vec<DiffLine>, pub truncated: bool }
pub struct DiffLine { pub kind: DiffKind /* Context | Added | Removed | Gap */,
                      pub old_no: Option<usize>, pub new_no: Option<usize>, pub text: String }
pub fn preview(tool: &str, args: &Value, scope: &Scope) -> Option<DiffPreview>
```

- `edit`: read the file (UTF-8, at most the tool's own read limit), run `rig_tools::edit::apply`
  to get the text the tool would write; `write`: before = existing file text or empty (a created
  file shows all lines added), after = `content`. Both diff before/after with `similar::TextDiff`
  grouped into hunks with 3 lines of context, `Gap` lines between hunks. One diff path for both
  tools.
- Returns `None` (prompt falls back to today's path-only text) when: the tool is not `edit`/`write`;
  the path resolves outside the scope roots (no reading files the call could not touch anyway);
  the file is unreadable, non-UTF-8 or over the limit; `apply` fails (the tool will fail the same
  way, so there is nothing to approve that a diff could show); or args do not parse.
- Display safety: every line's text goes through the same control-character stripping as
  `action_summary` (control chars to spaces, tabs expanded) so file content cannot inject terminal
  escapes, and through `redact::text` so credential-shaped text is not painted. Total lines are
  capped (200); past that `truncated` is set and the TUI says so.
- The preview is display-only: it is not sent to the guardian, hooks, telemetry or the ledger, and
  does not influence any decision (error text may explain, never change, a decision). It is computed
  in `ask_human` only, after the pipeline has already decided a human must answer.

### Request and prompt

- `ApprovalRequest` gains `preview: Option<DiffPreview>`; `ask_human` fills it. All other
  construction sites (tests in `fontaine-tui`, `fontaine-core/tests/common`) add `preview: None`.
- `Approval::body` renders, after the question and summary and before the reason, a header
  `path  +A -R` followed by the diff lines: line-number gutter (width from the largest number), `+`/`-`
  prefix, added lines in `theme.success`, removed in `theme.error`, context and gaps dim. Lines are
  wrapped to the box width via the existing `wrap`, so scrolling (PageUp/PageDown) works unchanged.
  Colors come from the existing `Theme`; no new theme fields. The linear (non-fullscreen) renderer
  (`linear.rs`) prints the same lines via the shared `body`.
- A `truncated` preview ends with `… N more changed lines (not shown)`.

## Implementation Steps

1. Add `similar` to `fontaine-core/Cargo.toml` (workspace dependency, pinned like the others).
2. Add `fontaine-core/src/preview.rs` with the types and `preview()`; export from `lib.rs`.
3. Add `preview` to `ApprovalRequest`; fill it in `ask_human`; fix the construction sites.
4. Render the preview in `Approval::body` (`fontaine-tui/src/approval.rs`), covering the
   non-fullscreen path if it builds its own text.
5. Update docs (below), `CHANGELOG.md`, and the plan's `## Decisions` in the master plan.

## Files to Modify

- `rust/fontaine-core/Cargo.toml`, `rust/Cargo.toml` (workspace dep)
- `rust/fontaine-core/src/preview.rs` (new), `src/lib.rs`, `src/permissions.rs`
- `rust/fontaine-core/tests/common/pipeline.rs` and other `ApprovalRequest` literals
- `rust/fontaine-tui/src/approval.rs`, `src/linear.rs`, `src/screen.rs`, `src/app.rs`, `src/ui/tests.rs` (literals)
- `docs/tui.md`, `docs/permissions.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md`

## Trade-offs

- **Diff in the TUI from `args` vs. in core.** The TUI has no scope or file access, and the
  headless approver could reuse a core-built preview, so core builds it.
- **Use `FileChange` hunks from `apply` vs. one `similar` path.** `apply` only covers `edit`;
  `write` needs its own diff anyway, so one path handles both and one renderer.
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

## Documentation

- `docs/tui.md` "Approval prompts": the diff preview, when it is absent, truncation.
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
- Truncation at the line cap sets `truncated`.

TUI tests (`TestBackend`, `fontaine-tui/src/ui/tests.rs`): a prompt with a preview shows
gutter, `+`/`-` lines and header; a long diff scrolls with PageDown; a request without a preview
renders exactly as before (snapshot unchanged). A `fontaine-core` pipeline test asserts that the
request handed to the approver for an `edit` call carries a preview and that denying/allowing is
unaffected by it.

## Manual Verification

1. Run `cargo run -p fontaine`, in manual mode ask the model to edit a file in the workspace.
   Expect the `Allow edit?` box to show the diff above the choices. Not automated: only a real
   terminal shows whether the box looks right at narrow widths (tmux, per `tasks/design/testing.md`).

## Open Questions

- Should the preview line cap (200) and file-size limit be configurable? Default: no, constants.
