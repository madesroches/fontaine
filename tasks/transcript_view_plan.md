# Ctrl-O Transcript View Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/233

## Overview

Ctrl-O toggles a transcript view, as in Claude Code: the full, untruncated tool calls (long and
multi-line commands and arguments), complete tool output, full reasoning, and the full reason behind
each denial, instead of the capped one-line cards. The task list moves to its own key (`alt+t`) and
keeps `/todos`. Settled lines live in terminal scrollback and cannot be redrawn, so the view is a
live-area overlay drawn from the in-memory entries, like the agents panel, and leaves scrollback
untouched.

## Current State

- `rust/fontaine-tui/src/keymap.rs:44` binds `ctrl+o` to `TuiAction::Todos`
  (`fontaine-core/src/keybindings.rs:206`, default at `:271`); `app.rs:1101` toggles
  `App::todos`. With an empty list `TodoPanel::lines` returns nothing, so the key does nothing
  visible (the problem in #233).
- `Transcript` (`transcript.rs`) keeps every entry of the session in `entries`; only a cursor
  (`committed`) says what has reached scrollback. `/clear` retires entries.
- `tool_lines(entry, width, full, ..)` (`transcript.rs:1141`) already has a `full` mode that shows
  every call row and the whole kept output (`OUTPUT_KEEP_BYTES`); `/expand` uses it by appending an
  `Entry::Expanded` that prints as new lines. The collapsed form caps the call at `CALL_ROWS` and
  the output at `OUTPUT_ROWS`.
- A denial keeps only the first line of its explanation: `ToolStatus::Denied(first_line(..))`
  (`transcript.rs:503-511`, `:541-548`), so the full guardian reason is lost.
- Overlays in the live area are built in `view.rs` (the agents panel at `view.rs:425`); a tall
  overlay makes `screen.rs` release live rows into scrollback first (`committed_under_overlay`,
  `screen.rs:426`). There is no alternate screen, by design.
- Screen-reader mode (`linear.rs`) echoes every overlay as text (`view::overlay` +
  `OverlayEcho`, `linear.rs:134-173`, `:444`) and has its own worded full form for
  `Entry::Expanded` (the `label` arm, `linear.rs:298-349`; `docs/terminal.md`).
- Claude Code: Ctrl-O toggles the transcript (verbose) view with full tool calls and results and
  thinking; `q`, `Ctrl+C` and `Esc` leave it. Claude Code binds Ctrl+T to the task checklist.
  Confirm during implementation by observation (`claude -p`
  cannot show the TUI, so a manual tmux run of `claude`) and record any difference.

## Design

### Keys

- New `TuiAction::Transcript` (`transcript` in config), default `ctrl+o`; add to `ALL` (13 actions)
  and `as_str`/`default_keystroke`.
- `TuiAction::Todos` default becomes `alt+t`. `alt+enter` is already a default, so Alt chords
  reach the app; a terminal that does not send Alt as Escape-prefix can rebind `todos`. A user's
  existing `"ctrl+o" = "todos"` entry keeps working through the normal override path.
- `/transcript` command mirrors the key, as `/todos` does; `/todos` stays.

### The view

- `App::transcript_view: Option<TranscriptView>` with `scroll: usize` (rows above the tail) and
  follow-the-tail behaviour while `scroll == 0`, so a streaming reply stays in view.
- `Transcript::verbose_lines(width, theme) -> Vec<Line>` renders `shown()`'s entries with the
  full forms: tool cards via `tool_lines(.., full = true)`, the whole kept reasoning (clipped
  form; with `show_reasoning` off it stays hidden), everything else as `entry_lines`. It skips
  every `Entry::Expanded` (`expand()` pushes a snapshot clone that stays in `entries`, which
  would duplicate the card) and every index in `follow_ups` (appended follow-up copies), as
  `recommit` does. Pending calls appear with their
  full call. The view shows the entries of the agent being viewed (`shown()`), so it works inside a
  subagent view too.
- `view.rs` adds an overlay `"transcript"`: body is the visible window of `verbose_lines` sized by
  `body_rows`, footer `Up/Down PgUp/PgDn scroll · Home/End · Ctrl-O, Esc or q close`. The overlay
  counts in `covered()` so typing, history and completion are suppressed while open (the input
  box stays; confirm by observation); `screen.rs` sets `committed_under_overlay` for it as for
  the agents panel, so closing it leaves the last message on screen (the #296 behaviour).
- Keys while open: Up/Down/PgUp/PgDn/Home/End scroll; the `transcript` key, Esc, `q` and Ctrl-C
  close; all other keys are ignored. `TranscriptView::handle_key` takes the mapped `TuiAction`
  so a rebound `transcript` key closes it. The early-return key block sits after the agents-panel
  block and before the Esc arms (running-turn interrupt, double-Esc), and applies only when
  `approval.is_none() && questions.is_empty()`; `view::overlay` uses the same order, so an
  approval prompt or question takes the overlay slot; like the agents panel, the view stays open
  under it and returns once it is answered, and the open arm keeps the `!typing_reason` gate.
  Ctrl+C is reserved and maps to `TuiAction::Interrupt`, whose arm (`app.rs:890-899`) sits at the
  top of `handle_key`, before every overlay block: that arm closes the view, when it is open and
  there is no approval/question, and returns `Action::None` (no Interrupt/Quit), as its
  `close_palette` branch does, consuming
  `committed_under_overlay` as the other close paths do.
- Ignored while the agents panel has the keys, as Ctrl-O was.

### Full denial reasons

`ToolStatus::Denied` keeps the whole explanation, passed through `display_text` (the denial text
does not go through it today, unlike call/output/reasoning). Its consumers are `tool_lines`
(`transcript.rs:1173`/`:1176` and the multi-line `prefixed` branch at `:1216-1224`, which when
collapsed shows only the first line) and `linear.rs:276`/`:322`; the collapsed renderer and linear
apply `first_line` (`linear.rs:276` is its collapsed arm); the full renderer wraps all of it, and
`linear.rs:322`, inside the `Expanded` arm, prints the whole reason split on `'\n'`. Tests at `transcript.rs:1939` and
`linear.rs:1085` are updated to the new stored form.

### Screen-reader mode

In linear mode the overlay is not opened. The transcript key and `/transcript` print each shown
entry once: Tool and Reasoning entries in linear's `Expanded` form (the `Entry::Expanded` arm,
`linear.rs:298-349`, handles only those two), every other entry in the normal linear `label` form.
It skips `Entry::Expanded` snapshots and `follow_ups`, as `verbose_lines` does. It is never a
silent no-op.

### Task list

Unchanged except the key. When the list is empty the toggle still flips the flag; `/todos` and the
new key additionally print a one-line notice, `no tasks`, so the key is never silent. The key arm
(`app.rs:1101-1103`) runs outside `run_immediate`, so it writes the notice with `shown_mut()` to
keep it out of the main transcript while a subagent view is open.

## Implementation Steps

1. `fontaine-core/src/keybindings.rs`: add `TuiAction::Transcript`; `Todos` default `alt+t`;
   update `ALL` (length 13), `as_str`, `default_keystroke`.
2. `fontaine-tui/src/keymap.rs`: defaults table (`defaults()`, not compiler-checked: `ctrl+o` →
   `Transcript`, `alt+t` → `Todos`) and its tests (override/displacement cases keep passing with
   the swapped keys).
3. `transcript.rs`: keep the full denial text, through `display_text`; `first_line` at render
   only in the collapsed `tool_lines` path and the collapsed arm of `linear.rs` (`:276`); the
   `Expanded` arm (`linear.rs:322`) prints the whole reason split on `'\n'`; add `verbose_lines`.
3a. `transcript.rs`: `settle` and `guardian_denied` write the new status to every `Entry::Tool`
   sharing the id of the entry `find_tool` returned (not the raw `id` argument: `apply_event`
   passes `""` when `tool_use_id` is absent and `find_tool` then falls back to the tool name), so the original (pending row already in scrollback) holds the final state:
   `apply_event` (`ui/turn.rs:217-240`) runs `on_event(Decision)` first, which settles the original
   and pushes a follow-up copy, then `guardian_denied`, whose `find_tool` searches from the end
   (`transcript.rs:376-386`) and would otherwise reach only the follow-up copy.
4. New `transcript_view.rs`: `TranscriptView` (scroll state, `handle_key`, window slice).
5. `app.rs`: field, `covered()`, key dispatch (Transcript key toggles; Todos keeps its arm and
   gains the empty-list notice via `shown_mut()`), the early-return key block after the
   agents-panel block and before the Esc arms, gated on no approval/question; closing consumes
   `committed_under_overlay` and re-prints, as the agents-panel close arms (`app.rs:1051`,
   `:1060`) do. The `Interrupt` arm (`app.rs:890-899`) also closes the view (same gate and same
   consumption) and returns `Action::None`, as its `close_palette` branch does. Update the test at `app.rs:3164`,
   `ctrl_o_toggles_the_task_list_unless_the_agents_panel_has_the_keys`: switch it to `alt+t`,
   rename it, and add a twin for the transcript key.
6. `view.rs`: overlay + footer (same order as the key block); add the transcript view to the
   precedence doc comment on `view::overlay()` (`view.rs:328-333`) at its slot after the
   questions; `screen.rs`: set `committed_under_overlay` for the new overlay.
7. `commands.rs` / `ui/mod.rs`: `/transcript` command and dispatch; add `Command::Transcript` to
   `runs_mid_turn` (`commands.rs:121-127`) and `"/transcript"` to the test
   `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else` (`commands.rs:983-1003`), so it acts
   immediately like `/todos`.
8. `linear.rs`: on the key/command, print each shown entry once: Tool and Reasoning through the
   `Expanded` form, every other entry through the normal `label` form, skipping `Entry::Expanded`
   snapshots and `follow_ups`; no overlay.
9. `reports.rs`: help keys line (`reports.rs:60`: `ctrl+o transcript`, `alt+t todos`) and its test
   at `:931` (new expected keys line). `/keybindings` follows `TuiAction::ALL` automatically
   (`ui/commands/keybindings.rs`); no change.
10. `tests/todos.rs`: the test at `:38` presses Ctrl+O for tasks; switch it to `alt+t` via
    `press_with` and rename it.
11. Docs and Decisions (see below).

## Files to Modify

- `rust/fontaine-core/src/keybindings.rs`
- `rust/fontaine-tui/src/{keymap,app,transcript,view,screen,commands,linear,reports,lib}.rs`,
  `rust/fontaine-tui/src/ui/mod.rs`, new `rust/fontaine-tui/src/transcript_view.rs`,
  `rust/fontaine-tui/tests/todos.rs`
- `docs/tui.md` (keys table :227, example :238, `/expand` text ~:48-64 and :577 gets a pointer,
  `/todos` :603, task panel :897, new "The transcript view" section), `docs/configuration.md`
  (:1074 example, :1102 action table), `docs/terminal.md` (`alt+t` in the "Option as Meta on macOS" paragraph, ~:93-101, since Option+t types a character otherwise), `CHANGELOG.md` `[Unreleased]`
- `tasks/fontaine_harness_plan.md` Decisions

## Trade-offs

- Alternate screen for the view: rejected; Fontaine is inline-scrollback by design and entering an
  alternate screen would hide scrollback and break native selection.
- Re-printing the full cards as new lines (what `/expand` does): kept as the per-card tool, but
  it appends to scrollback and duplicates content; a toggled view leaves scrollback clean.
- Keep Ctrl-O on tasks and put the view elsewhere: rejected by the issue owner; Claude Code parity
  and the silent no-op both argue for moving the tasks.
- `alt+t` for tasks over another Ctrl chord (Claude Code binds Ctrl+T to the task checklist, but
  Ctrl-T is Fontaine's agents panel): the remaining free Ctrl letters collide with readline
  editing keys the input already honors.

## Decisions

- 2026-11-04 (owner comment on #233): the task list gets its own shortcut, not Ctrl-O. Claude
  Code binds Ctrl+T to the task checklist, but Ctrl-T is Fontaine's agents panel, hence `alt+t`.
  The 2026-11-04 `### TUI` line also records each observed difference from Claude Code's view
  (input box kept, scroll/close keys, anything else seen in tmux).

## Documentation

- `docs/tui.md`: key table, `/transcript`, a section describing the view and its keys, task-panel
  text now naming `alt+t`; a pointer from the `/expand` text to the view.
- `docs/terminal.md`: the "Screen-reader mode" section (~:146) describes the key and
  `/transcript` printing the entries once.
- `CHANGELOG.md`: an `[Unreleased]` entry.
- `docs/configuration.md`: `transcript` action row, `todos` default `alt+t`, example rebind.
- Plan Decisions: replace the 2026-10-25 "Ctrl-O and `/todos` both toggle the task panel" line to
  say `alt+t` and `/todos`; add a 2026-11-04 line for the transcript view and the move; amend the
  2026-10-26 verdict-reason deviation to say the reason is inline and also complete in the view;
  mark the `tasks/fontaine_harness_plan.md:932` line ("No key is rebound for tool cards; Ctrl-O
  stays the task list, ...") superseded by the new 2026-11-04 line, which goes at the end of the
  `### TUI` subsection (starts ~:889).

## Testing Strategy

- Unit (`keybindings`, `keymap`): defaults map `ctrl+o` → transcript and `alt+t` → todos; an
  override of `ctrl+o` to `todos` displaces `transcript` and is reported as overridden.
- Unit (`transcript`): `verbose_lines` shows every row of a long multi-line command, the whole
  output, the full reasoning, and the full multi-line denial reason; an `Expanded` entry and a
  stale pending row are not duplicated; collapsed rendering of the same denial stays one line.
- Unit (`transcript_view`): scroll clamp at both ends, follow-the-tail at `scroll == 0`.
- Unit (`transcript`): `Decision` Deny followed by `guardian_denied` with the pending row already
  committed leaves the original `Entry::Tool` holding the `[rule] explanation` status, and
  `verbose_lines` shows it rather than the agent-facing note.
- Unit (`app`): the key opens and closes the view; Esc, `q` and Ctrl-C (through the `Interrupt`
  arm, with no Interrupt/Quit returned) close it; typing is ignored while
  open; the key is ignored while the agents panel is open; an approval request takes precedence and the view is still open after the approval closes;
  the todos key still toggles and an empty list yields the `no tasks` notice (in the viewed
  subagent's transcript when one is open).
- Screen (`screen.rs` with `TestBackend`): opening over a tall transcript leaves the last message
  in scrollback on close (regression in the style of #296).
- Linear: the key prints Tool and Reasoning entries once in the `Expanded` form and every other
  entry in the normal `label` form, skips `Entry::Expanded` snapshots and `follow_ups`, never
  prints nothing, and opens no overlay.

## Manual Verification

1. In a real tmux (see `tasks/design/testing.md`, "Reproducing terminal layout bugs"), run
   `cargo run -p fontaine`, have the model run a multi-line shell command, press Ctrl-O, and
   check the whole command and output show, scrolling works, and Ctrl-O/Esc return to the prompt
   with scrollback intact. Not automated because only a real terminal reproduces live-area
   layout.
