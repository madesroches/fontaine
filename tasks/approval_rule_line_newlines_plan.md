# Approval Prompt Rule Line Newlines Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/229

## Overview
The approval prompt's footer shows the rule the lasting answers would add. A multi-line shell
command's rule contains real newlines; the line is wrapped by width only, and ratatui drops the
control characters, so lines run together (`…import medianprint(…`). Show each newline as a
visible `\n` marker in that line. The saved rule is unchanged.

## Current State
`ApprovalPrompt::footer` (`rust/fontaine-tui/src/approval.rs:263-272`) builds
`"{what}: {rule}"` and feeds it to `input::wrap` (`rust/fontaine-tui/src/input.rs:197`), which
chunks by char count and passes `\n` through inside a `Line`. The body (`body`, line 233) already
splits with `str::lines()` first, so it is unaffected. No permission outcome depends on the
display string.

## Design
In `footer`, escape the rule for display before wrapping: replace `\r\n`, `\n` and `\r` with the
two characters `\n`. Add a small private helper in `approval.rs`, `display_rule(&str) -> String`,
so it is unit-testable. A literal backslash-n already in a command is left as is; the display is
advisory and the command pane shows the exact text.

## Implementation Steps
1. `rust/fontaine-tui/src/approval.rs`: add `display_rule`, use it at the `wrap(&format!(...))`
   call in `footer`.
2. Add tests (below).
3. `CHANGELOG.md`: add a Fixed entry.

## Files to Modify
- `rust/fontaine-tui/src/approval.rs`
- `CHANGELOG.md`

## Trade-offs
Splitting the rule into real lines (the issue's other option) would make a rule read as several
rules and costs footer rows; a one-line `\n` marker keeps the footer height predictable.

## Documentation
`docs/tui.md` has no description of the rule line's newline handling; no change unless it
describes the proposed-rule line, in which case add one sentence that newlines show as `\n`.

## Testing Strategy
Unit tests in `approval.rs`'s test module: `display_rule` maps `a\nb`, `a\r\nb`, and `a\rb` to
`a\nb` (literal backslash-n), leaves single-line rules untouched; and a footer test with a request
whose `proposed_rule` contains a newline asserts the rendered rule line contains `\n` text and
the pieces of both lines in order (pins the reported `medianprint(` bug).
