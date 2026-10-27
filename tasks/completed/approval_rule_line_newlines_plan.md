# Approval Prompt Rule Line Newlines Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/229

## Overview
The approval prompt's footer shows the rule the lasting answers would add. A multi-line shell
command's rule contains real newlines; the line is wrapped by width only, and ratatui drops the
control characters, so lines run together (`…import medianprint(…`). Show each newline, tab
and other control character as a visible escape in that line. The saved rule is unchanged.

## Current State
`ApprovalPrompt::footer` (`rust/fontaine-tui/src/approval.rs:263-272`) builds
`"{what}: {rule}"` and feeds it to `input::wrap` (`rust/fontaine-tui/src/input.rs:197`), which
chunks by char count and passes `\n` through inside a `Line`. The body (`body`, line 233) already
splits with `str::lines()` first, so it is unaffected. No permission outcome depends on the
display string.

## Design
In `footer`, escape the rule for display before wrapping: map `\r\n` and `\n` to the two
characters `\n`, a lone `\r` to `\r`, `\t` to `\t`, and any other control character to `\u{hex}`. Add a small private helper in `approval.rs`, `display_rule(&str) -> String`,
so it is unit-testable. A literal backslash-n already in a command is left as is; the display is
advisory and the command pane shows the exact text.

## Implementation Steps
1. `rust/fontaine-tui/src/approval.rs`: add `display_rule`, use it at the `wrap(&format!(...))`
   call in `footer`.
2. Add tests (below).
3. `docs/tui.md`: amend the `a`-choice sentence (see Documentation).
4. `CHANGELOG.md`: add a Fixed entry.

## Files to Modify
- `rust/fontaine-tui/src/approval.rs`
- `docs/tui.md`
- `CHANGELOG.md`

## Trade-offs
Splitting the rule into real lines (the issue's other option) would make a rule read as several
rules and costs footer rows; a one-line `\n` marker keeps the footer height predictable.

## Documentation
`docs/tui.md` (line ~192) says the `a` choice "shows the exact rule that will be added to
`[permissions] allow`"; amend that sentence to say newlines and tabs in the rule are shown as `\n` and `\t`, and other control characters as `\u{..}`.

## Testing Strategy
Unit tests in `approval.rs`'s test module: `display_rule` maps `a\nb` and `a\r\nb` to
`a\nb` (literal backslash-n), `a\rb` to `a\rb`, tabs to `\t`, other control characters to `\u{hex}`, leaves single-line rules untouched; and a footer test with a request
whose `proposed_rule` contains a newline asserts the rendered rule line contains `\n` text and
the pieces of both lines in order (pins the reported `medianprint(` bug).
