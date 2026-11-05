# `@` File Mention Scrolling Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/419

## Overview

The `@` file-mention popup truncates its results to the first 8 matches, so files that
rank below the cut can never be selected. Return the full ranked list from `rank()` and
window the visible rows in the view around the selection, exactly the way the slash-command
menu already does — Up/Down then reach every matching file.

## Current State

`complete::rank()` (`rust/fontaine-tui/src/complete.rs:56`) collects matching files, sorts
them by match class then path length, and hard-truncates with `.take(MAX_MATCHES)` where
`MAX_MATCHES = 8` (`complete.rs:8`). The result feeds `App::refresh_completion()`
(`rust/fontaine-tui/src/app.rs:572`), which stores it in a `CompletionList`
(`app.rs:138`) of `mention`, `matches`, `selected`.

The view renders every stored match (`rust/fontaine-tui/src/view.rs:529-552`) and the
footer already advertises "↑/↓ select". `App::completion_key` (`app.rs:1471`) clamps Down
to `matches.len() - 1`, so selection scrolling is correct — it just has nothing to scroll
into past row 8 because the list was cut.

The slash-command menu solves the identical problem differently: `App::slash_menu()`
returns an unbounded `matches`, `completion_menu_key` (`app.rs:1426-1429`) moves the
selection through all of them, and `menu_lines` (`view.rs:575-588`) shows only a
`MAX_MENU_ROWS = 8` (`view.rs:17`) window that follows the selection:

```rust
let first = (menu.selected + 1).saturating_sub(MAX_MENU_ROWS);
```

The docs describe the truncation as the behavior: `docs/tui.md:320` says the list shows
"up to eight workspace files".

## Design

Follow the slash-menu pattern; no new concepts.

1. **`complete.rs` — stop truncating.** `rank()` returns the full sorted match list;
   delete the `.take(MAX_MATCHES)` and the `MAX_MATCHES` constant. Sorting is unchanged
   (tuple `(class, char_len, path)` sorts deterministically). Per-keystroke cost is the
   same scan the code already does; only the result Vec is larger, bounded by the number
   of workspace files.
2. **`view.rs` — window the rows.** Render at most `MAX_MENU_ROWS` rows of the completion
   list, with the same `first = (selected + 1).saturating_sub(MAX_MENU_ROWS)` window the
   slash menu uses, so the selection always stays visible. Extract that one-line window
   computation into a small helper shared by `menu_lines` and the completion overlay
   rather than duplicating it.
3. **`app.rs` — no change needed.** `completion_key` already clamps Down to
   `matches.len() - 1` and Up with `saturating_sub`, and `refresh_completion` preserves
   the selection with `.min(matches.len() - 1)` when the query is unchanged.
4. **`docs/tui.md` — describe scrolling.** Rewrite the "File mentions" paragraph to say
   the list holds every matching file and Up/Down scroll through them (windowed display),
   instead of "up to eight".

## Implementation Steps

1. In `rust/fontaine-tui/src/complete.rs`, remove `MAX_MATCHES` and the `.take()` in
   `rank()`; adjust the test `at_most_eight_are_offered` into a test asserting the full
   match set is returned (e.g. 20 matching files → 20 entries, order still ranked).
2. In `rust/fontaine-tui/src/view.rs`, extract the slash-menu window line into a helper
   (e.g. `fn window_first(selected: usize, max: usize) usize`), use it in `menu_lines`,
   and apply the same window in the completion overlay body (build rows from
   `matches.iter().enumerate().skip(first).take(MAX_MENU_ROWS)`).
3. Add a view test mirroring the existing slash-menu window test (`view.rs:832-841`):
   more matches than `MAX_MENU_ROWS`, Down past the window, assert the rendered row count
   stays at `MAX_MENU_ROWS` and the selected row is the last rendered one.
4. Update the "File mentions" section of `docs/tui.md` (~line 320).

## Files to Modify

- `rust/fontaine-tui/src/complete.rs`
- `rust/fontaine-tui/src/view.rs`
- `docs/tui.md`

## Trade-offs

- **Unbounded list vs. a higher fixed cap** (e.g. 50): unbounded matches the slash menu's
  established behavior, needs no magic number, and the cost is a longer Vec per keystroke
  over the file index the session already holds in memory. A cap would re-introduce the
  bug at a higher rank.
- **Shared window helper vs. inlining a second copy**: one line today, but the two call
  sites must move together or the two popups scroll differently; the helper makes the
  "selection stays visible" invariant explicit.

## Decisions

## Documentation

`docs/tui.md` "File mentions" section: replace "up to eight workspace files" with the
scrolling description. The keybindings table (Up/Down already "move the selection") needs
no change.

## Testing Strategy

- `complete.rs` unit tests: full result set returned, ranking order unchanged. Pure
  function, unit tier.
- `view.rs` unit tests (TestBackend-style, following the existing slash-menu window test):
  row count capped at the window, window follows the selection in both directions.
- `app.rs` selection-clamping tests already cover Up/Down bounds; they keep passing with a
  longer list.
- Per the repo testing strategy for terminal layout behavior, the manual tmux check below
  confirms the real-terminal rendering that `TestBackend` cannot (overlay clipping against
  actual terminal height).

## Manual Verification

1. `cargo run -p fontaine` in a workspace with more than 8 files matching a prefix (e.g.
   type `@tasks/` in the fontaine repo).
2. Hold Down: the selection moves past row 8 and the window scrolls, ending on the last
   match; the popup never grows taller than 8 rows plus its footer.
3. Compare with the slash menu (`/dev-skills:`): both popups window identically.
   Not automated because overlay height against a real terminal is exactly the layout
   class of bug `TestBackend` misses.

## Open Questions
