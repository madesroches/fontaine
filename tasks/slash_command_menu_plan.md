# Slash-Command Completion Menu Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/71

## Overview

Typing `/` already shows a read-only list of commands and skills above the input. It cannot be
navigated: there is no selection, Tab does nothing, Enter submits whatever was typed, and Esc
does not close it. This plan makes the menu interactive as in Claude Code: Up/Down select, Tab
completes the name, Enter runs the selection, Esc closes it. Filtering widens from prefix-only
to prefix-then-substring over names and aliases. The command table (`commands.rs`) already feeds
both the parser and the menu, so no table work is needed beyond carrying aliases into the rows.
The change stays inside `fontaine-tui`.

## Current State

- `rust/fontaine-tui/src/commands.rs`: `BUILTINS` is the single table (name, aliases, hint,
  description, builder); `parse_command` and `builtin_menu()` read it. `MenuEntry` has
  `name`, `hint`, `description` and no aliases. `skill_menu` builds rows for user-invocable
  skills, and `lib.rs:1700` appends them to `App::menu` and sorts by name.
- `rust/fontaine-tui/src/view.rs:274` `menu_lines` renders rows when the input is a bare
  `/name` (prefix filter on the name only, no selection, no footer). `overlay()` (`view.rs:170`)
  returns it last in the precedence chain, after the `@` completion list.
- `rust/fontaine-tui/src/app.rs:412` `handle_key` routes keys: covering panels
  (denied, unavailable, server, models, providers, agents, approval) first, then history search,
  then the `@` completion list (`completion_key`), then `message_key`. `covered()` (`app.rs:402`)
  lists the overlays that own the keyboard. `close_palette` closes search/completion on Esc and
  Ctrl-C.
- `App::enter` (`app.rs:~620`) submits the input; while a turn runs, a recognised command gets a
  "a turn is running" notice instead of being queued.
- `docs/tui.md` documents keys and `@` mentions but not the `/` menu.
- Existing integration test: `tests/extensions.rs` `the_slash_menu_lists_commands_…`.

## Design

### Matching (pure, in `commands.rs`)

Add `aliases: Vec<String>` to `MenuEntry` (filled from `Builtin::aliases`; empty for skills).
Add `pub fn match_menu<'a>(menu: &'a [MenuEntry], typed: &str) -> Vec<&'a MenuEntry>`:
case-insensitive; entries whose name or any alias starts with `typed` first, then entries whose
name or alias merely contains it; each group keeps the menu's name order; no duplicates. An empty
`typed` matches everything. The view and the key handling both call this, so they cannot
disagree.

### State and keys (`app.rs`)

The menu is *open* when all hold: input text starts with `/`, the text after the `/` has no
whitespace and the cursor is within it (single-line input), nothing in `covered()` is up, no
history search is active, no `@` list is open, the match list is non-empty, and the human has
not dismissed it for this exact text.

State on `App`: `slash_selected: usize` and `slash_dismissed: Option<String>` (the input text
at the time of Esc). Selection resets to 0 whenever the typed name changes and is clamped to the
match count. A single `fn slash_matches(&self) -> Vec<&MenuEntry>` returns the empty vec when
the menu is not open, and is what `view.rs` renders.

Key handling is a new `slash_key(&mut self, key) -> Option<Action>` called in `handle_key`
right after the `@` completion branch, so `covered()` panels, search and the `@` list all take
their keys first:

| Key | Effect |
|---|---|
| Up / Down | Move the selection (clamped, no wrap, like the `@` list). |
| Tab | Replace the input with `/name`, plus a trailing space when the entry has an argument hint. |
| Enter (no Shift/Alt) | If the entry has a *required* argument (hint starts with `<`), behave as Tab, because it cannot run bare. Otherwise set the input to `/name` and run it through `enter()`, so the "a turn is running" rule and history recording are unchanged. |
| Esc | Dismiss the menu until the text changes. While a turn runs, this Esc closes the menu rather than interrupting, as the `@` list does (`close_palette`). |
| anything else | Falls through to the input as today. |

After Tab the cursor sits at the end; with a trailing space the menu closes (whitespace in the
name region), with none it stays open on the exact match, and a further Tab is a no-op.

### Rendering (`view.rs`)

`menu_lines` takes the match list and the selection; the selected row is marked `> ` and bold
like the `@` list, the others indented two spaces. Rows keep the `/name hint  description`
shape and show the matched alias in parentheses when the match came from an alias only
(`/clear (reset)`). At most `MAX_MENU_ROWS = 8` rows are drawn, windowed so the selection is
visible; a footer line `↑/↓ select · Tab complete · Enter run · Esc close` replaces the empty
footer. The 8-row cap bounds the live-area growth.

## Implementation Steps

1. `commands.rs`: add `aliases` to `MenuEntry`, fill it in `builtin_menu`, add `match_menu`
   with unit tests.
2. `app.rs`: add `slash_selected`, `slash_dismissed`, `slash_matches`, `slash_key`; wire into
   `handle_key`, `close_palette` and the post-key refresh so the selection resets on edits.
3. `view.rs`: render selection, alias hint, row window and footer from `slash_matches`.
4. Update the existing `tests/extensions.rs` assertions if the new footer or marker changes
   what they match; add the integration test below.
5. `docs/tui.md`: add a "Slash-command menu" section and a Tab-row mention; `CHANGELOG.md`
   entry.

## Files to Modify

- `rust/fontaine-tui/src/commands.rs`
- `rust/fontaine-tui/src/app.rs`
- `rust/fontaine-tui/src/view.rs`
- `rust/fontaine-tui/tests/extensions.rs` (and `tests/tui.rs` if a better home)
- `docs/tui.md`, `CHANGELOG.md`

## Trade-offs

- Derive matches from the input on each call rather than caching a list: the menu is small
  (tens of rows) and this removes any chance of a stale list. Only selection and dismissal are
  stored.
- Enter on a required-argument command completes instead of running: running `/add-dir` bare
  only yields an error, and completing is what the user wants next.

## Decisions

## Documentation

`docs/tui.md`: a "Slash-command menu" section (opens on `/`, filtering, keys, alias display,
argument hints, never opens over a panel) and the Tab row in the key table.

## Testing Strategy

- `commands.rs` unit tests: prefix before substring; alias matches; case-insensitivity; empty
  query returns all; no duplicates; skills (no aliases) match by name.
- `app.rs` unit tests: the menu opens only for a bare `/name`; Up/Down clamp; Tab completes with
  a trailing space for hinted commands and without for bare ones; Enter runs a no-argument
  command and completes a required-argument one; Esc dismisses until the text changes and does
  not interrupt a running turn; the menu does not open and takes no keys while approval, denied,
  unavailable, server, models, providers or agents panels are up, nor during history search or
  with an `@` list open; with the menu closed, Up/Down/Tab/Enter behave as before (history
  recall, submit).
- `view.rs` `TestBackend` render test: open menu shows the `>` marker on the selected row, the
  alias annotation, the footer, and windows to 8 rows with the selection visible.
- Integration (`tests/extensions.rs`): type `/`, Down, Tab, assert the input holds the next
  command's name; type `/mod` Enter and assert the model picker opens.

## Open Questions

- Should Enter on a required-argument command complete (as planned) or run and show the usage
  error? Defaulted to complete.
