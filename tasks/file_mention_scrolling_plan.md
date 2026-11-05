# `@` File Mention Completion Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/419

**Status**: Round 1 (scrolling) is implemented on this branch and unmerged. Round 2
(directories and common-prefix Tab) is design only — no code is written yet; this section
is awaiting review before implementation starts.

## Overview

Two rounds on the `@` file-mention popup:

1. **Round 1 (done, on this branch):** the popup truncated its results to the first 8
   matches, so files that ranked below the cut could never be selected. `rank()` now
   returns the full list and the view windows the visible rows around the selection, the
   way the slash-command menu already does.
2. **Round 2 (designed, not implemented):** `@task<tab>` completes to
   `rust/fontaine-core/src/tasks.rs` instead of `tasks/`. Directories never appear in the
   completion list at all, and `@` completion accepts the selected row on the first Tab
   instead of first extending the token to the common prefix of the matches (the `/` menu
   has done the latter since #294). Round 2 puts directories in the list and gives `@`
   completion the same Tab contract as the `/` menu.

## Round 2: directories and common-prefix Tab

### Current State

The completion list comes from `FileIndex::build`
(`rust/fontaine-core/src/ledger.rs:730`), which only collects entries whose file type
is a file (`kind.is_file()`); directories are skipped. `Agent::set_files`
(`rust/fontaine-core/src/agent.rs:723`, called from `record_session_start`, and
`refresh_files` at `agent.rs:835`) hands `index.paths` to the TUI, so the `@` list can
never contain `tasks/`.

Ranking (`rust/fontaine-tui/src/complete.rs:56`) computes the basename with
`lower.rsplit(['/', '\\']).next()`, so the basename of a trailing-slash path (`tasks/`)
is the empty string and every directory would land in match class 2 even if it were in
the list.

`App::completion_key` (`rust/fontaine-tui/src/app.rs:1471`) accepts the selected row on
Tab (`accept_completion`, `app.rs:603`), inserting `@{path} ` — always with a trailing
space. The `/` menu instead narrows first: `App::slash_key` (`app.rs:1428`) extends the
token to `common_prefix` of the matches when more than one match shares a prefix longer
than the query, and only accepts on a second Tab (`commands.rs:575`).

The ledger also consumes `FileIndex`: `Record::FileIndex` is replayed into
`Ledger::index`, which drives `Ledger::existence`
(`rust/fontaine-core/src/ledger.rs:459`) — the pre-existing vs created-this-session
classification behind the `tracked_edit` fast path. That record must stay files-only.

### Design

1. **`fontaine-core/src/ledger.rs` — index directories alongside files.** `FileIndex`
   gains `pub dirs: Vec<String>`. `build` collects directories too, skipping the walk
   root (depth 0) and `.git` (already filtered by `filter_entry`); the existing cap
   counts files and directories together. `Record::FileIndex` is unchanged and keeps
   receiving `paths` (files only), so `existence()` semantics are untouched. A new
   accessor `completion_paths()` returns files plus directories, each directory marked
   with a trailing `/` (on all platforms; the string is only ever matched and inserted,
   never opened).
2. **`fontaine-core/src/agent.rs` — feed completion the directory list.**
   `record_session_start` and `refresh_files` call `set_files` with
   `index.completion_paths()` instead of `index.paths`.
3. **`fontaine-tui/src/complete.rs` — rank directories by their real basename.** Trim
   trailing separators before taking the basename, so `tasks/` basename-matches `task`
   (class 0). Also skip entries equal to the query (case-insensitively): a fully typed
   path has nothing left to complete, and a directory must not list itself among its own
   children when the query is its path followed by `/` (accepting `tasks/` reopens the
   list with query `tasks/`, which the directory itself would otherwise match).
4. **`fontaine-tui/src/app.rs` — the `/` menu's Tab contract for `@`.**
   - `completion_key` Tab first extends the token to the common prefix of the current
     matches — the existing `common_prefix` helper, case-insensitive, spelled as in the
     first match — when there is more than one match and the prefix both starts with the
     query and is longer than it; only when nothing more can be extended does Tab accept
     the selected row. Enter keeps accepting the selection directly.
   - `accept_completion` inserts a match ending in `/` without the trailing space, so the
     refreshed list opens on the directory's children and the user can keep completing
     into it.
5. **`docs/tui.md` — describe both behaviors** in the "File mentions" paragraph:
   directories are offered (marked with `/`) and can be completed into; Tab narrows to
   the common prefix of the matches before accepting one.

Resulting flow for the reported case: `@task` matches `tasks/` (class 0, 6 chars) ahead
of `rust/fontaine-core/src/tasks.rs` (class 0, 31 chars, shorter-paths-first inside the
class). Their common prefix is `task`, already typed, so Tab accepts `tasks/` without a
space; the list reopens on the children of `tasks/`.

### Implementation Steps (not started)

1. `fontaine-core/src/ledger.rs`: add `dirs` to `FileIndex`, collect directories in
   `build` (depth-0 skip, shared cap), add `completion_paths()`.
2. `fontaine-core/src/agent.rs`: use `completion_paths()` in `record_session_start` and
   `refresh_files`; leave the `Record::FileIndex` append on `index.paths`.
3. `fontaine-tui/src/complete.rs`: basename trimming and the exact-match skip in `rank()`.
4. `fontaine-tui/src/app.rs`: common-prefix narrowing on Tab in `completion_key`;
   trailing-space suppression for directory matches in `accept_completion`.
5. `docs/tui.md` "File mentions" paragraph.
6. `CHANGELOG.md` under `[Unreleased]` → `Fixed`.

### Files to Modify (Round 2)

- `rust/fontaine-core/src/ledger.rs`
- `rust/fontaine-core/src/agent.rs`
- `rust/fontaine-tui/src/complete.rs`
- `rust/fontaine-tui/src/app.rs`
- `docs/tui.md`
- `CHANGELOG.md`

### Trade-offs

- **Directories in `FileIndex` vs. a separate directory walk for completion**: one walk,
  one cap, and the ledger record stays files-only because `completion_paths()` is a view
  over the same build. A second walk in `refresh_files` would double the I/O for the
  same data.
- **Trailing `/` as the directory marker** (vs. a structured entry type): the TUI list is
  `Vec<String>` end to end — the popup, the ranking and the input edit — so a marker
  inside the string is the smallest change; `rank()` already treats both separator
  flavours.
- **Tab narrows before accepting** (vs. accepting on the first Tab as today): matches the
  `/` menu's contract since #294, which is the behavior the maintainer pointed at as
  working; it also means a shared stem (`@task` → `tasks/`, `tasks.rs`) pauses on the
  stem instead of jumping into the top-ranked file.
- **Skipping exact matches in `rank()`**: without it, accepting a directory inserts its
  path and immediately re-lists the directory itself as its own child, so a second Tab
  seals the directory with a space instead of completing into it.

### Decisions (to record in the master plan on merge)

- 2026-11-05: `@` completion offers directories, marked with a trailing `/` and inserted
  without the trailing space so they can be completed into, and Tab extends the token to
  the longest common prefix of the matches before accepting the selected row — both
  mirroring the `/` menu (#294). The ledger's `Record::FileIndex` stays files-only so
  `existence()` and the `tracked_edit` fast path are unchanged (#419).

### Testing Strategy (Round 2)

- `complete.rs` unit tests: a directory `tasks/` basename-matches `task` ahead of a
  longer `tasks.rs` path; a fully typed path (exact match, case-insensitive) is not
  offered; query `tasks/` lists the children of `tasks/` but not `tasks/` itself.
- `app.rs` unit tests: `@task` + Tab inserts `@tasks/` with no trailing space and reopens
  the list on the children; `@src/m` + Tab narrows to `@src/ma` (list still open) and a
  second Tab accepts `src/main.rs`.
- `fontaine-core/tests/ledger.rs`: extend the gitignore fixture to assert directories are
  collected (an unignored dir present, an ignored dir and `.git` absent, the walk root
  itself absent); assert the cap counts files and directories together. The existing cap
  test keeps passing because its fixture has no subdirectories.
- Manual tmux check per the repo testing strategy (real-terminal overlay rendering).

### Manual Verification (Round 2)

1. `cargo run -p fontaine` in the fontaine repo, type `@task` + Tab: `tasks/` is
   inserted with no trailing space and the popup shows the children of `tasks/`.
2. Continue into the directory with Tab/typing; complete a file inside it and confirm the
   message sends with the typed path.
3. Type `@src/m` + Tab: the token extends to `@src/ma`, the list stays open; a second Tab
   accepts `src/main.rs`.
4. Compare Tab behavior with the `/` menu: both narrow to the common prefix first.

## Round 1: scrolling (implemented on this branch)

The original plan is preserved below as the record of what Round 1 changed. Its steps
are all done; the only change since it was written is that `rank()` now returns the full
match list instead of truncating at 8.

---

### Original Round 1 plan

**Overview.** The `@` file-mention popup truncates its results to the first 8 matches, so
files that rank below the cut can never be selected. Return the full ranked list from
`rank()` and window the visible rows in the view around the selection, exactly the way
the slash-command menu already does — Up/Down then reach every matching file.

**Current state (pre-Round-1).** `complete::rank()` collected matching files, sorted them
by match class then path length, and hard-truncated with `.take(MAX_MATCHES)` where
`MAX_MATCHES = 8`. The view rendered every stored match and the footer already advertised
"↑/↓ select". `App::completion_key` clamped Down to `matches.len() - 1`, so selection
scrolling was correct — it just had nothing to scroll into past row 8 because the list
was cut.

The slash-command menu solved the identical problem differently: `App::slash_menu()`
returns an unbounded `matches`, the slash-menu Up/Down handling moves the selection
through all of them, and `menu_lines` shows only a `MAX_MENU_ROWS = 8` window that
follows the selection.

**Design.**

1. `complete.rs` — stop truncating: `rank()` returns the full sorted match list; delete
   `.take(MAX_MATCHES)` and the `MAX_MATCHES` constant.
2. `view.rs` — window the rows: render at most `MAX_MENU_ROWS` rows of the completion
   list, with the same `(selected + 1).saturating_sub(MAX_MENU_ROWS)` window the slash
   menu uses, extracted into a shared helper.
3. `app.rs` — no behavior change needed; `completion_key` already clamps and
   `refresh_completion` preserves the selection.
4. `docs/tui.md` — describe scrolling instead of "up to eight workspace files".

**Implementation steps (all done).**

1. In `rust/fontaine-tui/src/complete.rs`, remove `MAX_MATCHES` and the `.take()` in
   `rank()`; adjust the truncation test into one asserting the full match set is
   returned.
2. In `rust/fontaine-tui/src/view.rs`, extract the slash-menu window line into a helper
   used by both `menu_lines` and the completion overlay.
3. Add a view test mirroring the slash-menu window test.
4. Add an `app.rs` test for the completion-list selection clamp.
5. Update the "File mentions" section of `docs/tui.md`.

**Files modified (Round 1).**

- `rust/fontaine-tui/src/complete.rs`
- `rust/fontaine-tui/src/view.rs`
- `docs/tui.md`

**Testing strategy (Round 1).** `complete.rs` unit tests (full result set returned,
ranking order unchanged); `view.rs` unit tests (row count capped at the window, window
follows the selection both directions); `app.rs` unit test (Down clamps at
`matches.len() - 1`); manual tmux check for the real-terminal rendering.

**Manual verification (Round 1).** In a workspace with more than 8 files matching a
prefix, hold Down: the selection moves past row 8 and the window scrolls, ending on the
last match; the popup never grows taller than 8 rows plus its footer; identical windowing
to the slash menu.
