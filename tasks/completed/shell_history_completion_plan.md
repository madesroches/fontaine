# `!` Shell Command History Completion Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/682

## Overview

Extend the prompt box's existing history ghost text to `!` shell lines, matching Claude Code's bash
mode: as you type after `!`, the newest earlier `!` command that starts with the typed text is shown
dimmed after the cursor; Tab or Right accepts it, Esc dismisses it. It is a typing aid only: the
accepted text is ordinary input, classified, run and ledgered exactly as typed.

## Current State

- `History` (`rust/fontaine-tui/src/history.rs`) already records every submitted input, `!` lines
  included, verbatim with the `!` (docs/tui.md "History"). No new store is needed.
- `History::completion` (history.rs:158) returns the remainder of the newest single-line entry that
  extends `typed`, but deliberately returns `None` for any `typed` starting with `/` or `!`.
- `App::ghost_text` (app.rs:747) gates on `ghost_allowed`, `history_completion` and cursor-at-end,
  then calls `history.completion`. `accept_ghost` (app.rs:804) handles Tab/Right; `view.rs:837`
  renders the ghost dimmed and clipped.
- Esc dismisses only a model suggestion in an empty input (app.rs:1860).
- docs/tui.md "Prompt suggestions and history completion" states history completion never applies
  to `!` lines; "Shell commands" documents the mode.

## Design

1. **Suggestion source.** In `History::completion`, drop `!` from the excluded prefixes (keep `/`).
   A `!` line is only completed from entries that also start with `!`, because the match is a
   plain case-sensitive prefix of the full typed text (including the `!`). Conversely, a non-`!`
   typed text can never match a `!` entry. Require at least one character after the `!`
   (typed `!` alone yields `None`) so the box does not pre-fill the last command on the bare
   prefix. Most recent match first, single-line entries only, existing behavior.
2. **Dismissal.** Add `ghost_dismissed: Option<String>` to `App`: the exact input text whose
   shell ghost Esc dismissed. `ghost_text` returns `None` for a shell line while the input text
   equals it. After each key, alongside `refresh_slash`/`refresh_completion` in the key path,
   clear it once the input text differs from it (mirroring `slash_dismissed`), so typing a char
   and backspacing back to the dismissed text shows the ghost again. In the key path, after the
   `wakeup_pending` arm and before the double-Esc/`message_key` handling, an Esc with a visible
   shell ghost, only when idle and not in vim Insert mode, sets it and returns `Action::None`.
   While a turn runs Esc still interrupts (earlier arm, app.rs ~1839), and vim Insert Esc
   switches to Normal first.
3. **Accept.** Unchanged: `accept_ghost` inserts the remainder. Nothing else changes — `shell_mode`
   and submission read the input text, so classification and gating are untouched.
4. **Config.** Governed by the existing `[tui] history_completion` switch; no new key.
5. Rendering needs no change; the existing ghost path draws it.

## Implementation Steps

1. `history.rs`: change `completion` guard and doc comment; require a character after `!`; update
   its tests (`!l` case currently asserts `None`).
2. `app.rs`: add `ghost_dismissed`, honor it in `ghost_text`, set it on Esc.
3. Tests (below).
4. `docs/tui.md`: update the two sections.
5. `CHANGELOG.md` entry.

## Files to Modify

- `rust/fontaine-tui/src/history.rs`
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/view.rs` (test only)
- `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- Reusing the unified history over a separate `!`-only store: no new persistence, and `!` lines are
  already recorded per project.
- Esc state keyed by the dismissed text rather than a flag: avoids hooking every edit path; it is
  cleared post-key once the text differs, as `slash_dismissed` is.

## Decisions

- 2026-11-16: Claude Code's exact bash-mode behavior was not observed with `claude -p` (it is an
  interactive UI feature `-p` cannot exercise); the issue's description is taken as the spec.
- 2026-11-16: A bare `!` shows no ghost, to avoid pre-filling a command before the user has typed
  one.

## Documentation

`docs/tui.md`: remove "never for ... `!` shell line" from the history-completion bullet, describe
shell completion and Esc, and mention it under "Shell commands".

## Testing Strategy

- `history.rs` unit tests: `!gi` after `!git status` gives `t status`; newest match wins; a `!`
  entry never completes plain text and vice versa; bare `!` gives `None`; multi-line entries are
  skipped; `/` still excluded.
- `app.rs` tests (in `mod ghost`): ghost follows typing and deleting; Tab and Right accept and
  Enter submits `Action::Shell("git status")`; Esc dismisses (idle); typing another character brings it back, and so does typing then backspacing to the
  dismissed text; Esc while a turn runs still interrupts; off with `history_completion = false`.
- `view.rs` test: shell ghost renders dim after the typed text and is clipped.

## Open Questions

None.
