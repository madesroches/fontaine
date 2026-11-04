# Ctrl-O Transcript View Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/233

## Overview

Ctrl-O toggles a transcript view, as in Claude Code: the full, untruncated tool calls (long and
multi-line commands and arguments), complete tool output, full reasoning, and the full reason behind
each denial, instead of the capped one-line cards. The task list moves to Ctrl-T, as in Claude Code, and
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
- `TuiAction::Todos` default becomes `ctrl+t`; `TuiAction::AgentsPanel` has no default key
  (`default_keystroke` returns `Option`) and is reached with `/agents`. A user's
  existing `"ctrl+o" = "todos"` entry keeps working through the normal override path.
- `/transcript` command mirrors the key, as `/todos` does; `/todos` stays.

### The view

- `App::transcript_view: Option<TranscriptView>` with `scroll: usize` (row offset from the top),
  `max_scroll: Cell<usize>` and a clamp, as `approval.rs` does (`scroll` :70, `max_scroll` :72, clamp
  :102). `overlay()` computes `max = verbose_lines.len().saturating_sub(body_rows)`, stores it in
  `max_scroll`, and passes only the window sliced by `body_rows` (as the pickers do,
  `view.rs:390-423`). `verbose_lines` is expensive (markdown, wrapping, up to 64 KiB of kept
  output per call) and `overlay()` runs about three times per frame, so `TranscriptView` holds a
  `RefCell` cache of the rendered lines keyed on (width, viewed agent, a `Transcript` change
  counter, last-entry length); `overlay()` only slices the cached lines and rebuilds them when the
  key differs. `Transcript` gains `changes: u64`, bumped by every mutation of `entries` (push,
  status settle, retire, text append), so no entry diffing is needed; the last-entry length is
  belt-and-braces for streaming growth. The view opens pinned to the tail, not at
  scroll 0. `view::overlay` takes `&App` and cannot write `scroll`, so following is a
  `follow: Cell<bool>`: set on open and whenever the view is scrolled to the bottom
  (Down/PgDn/End), cleared by any upward scroll. While it is set the window computation resolves
  the offset to the current `max`, so content growth keeps a streaming reply in view; a
  scrolled-up window stays put as entries are appended.
- `Transcript::verbose_lines(width, theme) -> Vec<Line>` renders `shown()`'s entries with the
  full forms: tool cards via `tool_lines(.., full = true)`, the whole kept reasoning (clipped
  form; with `show_reasoning` off it stays hidden), everything else as `entry_lines`. It skips
  every `Entry::Expanded` (`expand()` pushes a snapshot clone that stays in `entries`, which
  would duplicate the card) and every index in `follow_ups` (appended follow-up copies), as
  `recommit` does. Pending calls appear with their
  full call. The view shows the entries of the agent being viewed (`shown()`), so it works inside a
  subagent view too.
- `view.rs` adds an overlay `"transcript"`: body is the visible window of `verbose_lines` sized by
  `body_rows`, footer `Up/Down PgUp/PgDn scroll · Home/End · Esc or q close`. The overlay
  counts in `covered()` so typing, history and completion are suppressed while open (the input
  box stays, dimmed with no cursor, and pastes are ignored: `covered()` side effects,
  `view.rs:633-638`, `app.rs:819`; confirm by observation). Like the agents panel, the open view
  counts in `covered()`, so replay of a queued modal command (`serve_modal`, `ui/turn.rs:636`)
  waits until the view is closed; `screen.rs` sets `committed_under_overlay` for it as for
  the agents panel, so closing it leaves the last message on screen (the #296 behaviour).
- Keys while open: Up/Down/PgUp/PgDn/Home/End scroll; the `transcript` key, Esc, `q` and Ctrl-C
  close; all other keys are ignored. `TranscriptView::handle_key` takes the mapped `TuiAction`
  so a rebound `transcript` key closes it. The early-return key block goes immediately after the
  agents-panel block (`app.rs:1068`), before the questions block (`:1069-1072`), the Ctrl+P/N
  alias, the `agent_list.focused` `list_key` arm and the mapped
  `AgentsPanel`/`AttachClipboard`/`Todos`/`Reprint` arms (`:1093-1108`), the Esc arms
  (running-turn interrupt, double-Esc, `:1109`) and the vim `esc_leaves_insert` arm (`:1114`), so
  none of them acts while the view is open. It applies only when
  `view::transcript_has_keys(app)`: the view is open and no overlay ahead of it in
  `view::overlay`'s precedence (denied, unavailable-guardian, server, guardian-off, skill-trust,
  setup, pickers, config, providers, agents panel, approval, questions) is open. `view::overlay`
  uses that same helper at its slot, so key routing and drawing cannot drift; an overlay ahead
  takes the slot, and like the agents panel the view stays open under it and returns once it is
  dismissed. The `Transcript` key's open arm sits beside the
  `Todos` mapped arm (`app.rs:1101`) with the `!typing_reason` gate.
  The `agent_list.focused && !control && !self.covered()` arm (`app.rs:1090`) runs before the
  mapped arms; its `!control` gate already lets the Ctrl chords reach their arms, so it is unchanged.
  Ctrl+C is reserved and maps to `TuiAction::Interrupt`, whose arm (`app.rs:890-899`) sits at the
  top of `handle_key`, before every overlay block: that arm first checks
  `view::transcript_has_keys(app)` (so an overlay above the view, such as the mid-turn
  unavailable-guardian prompt, leaves Ctrl-C to its normal Interrupt/`close_palette` path), ahead of the existing
  `!self.covered() && self.close_palette()` check (`app.rs:891-893`), and then closes the view and
  returns `Action::None` (no Interrupt/Quit), consuming
  `committed_under_overlay` as the other close paths do; it does not go through `close_palette`.
- Ignored while the agents panel has the keys, as Ctrl-O was.

### Full denial reasons

`ToolStatus::Denied` keeps the whole explanation, passed through `display_text` (the denial text
does not go through it today, unlike call/output/reasoning). Its consumers are `tool_lines`
(`transcript.rs:1173`/`:1176` and the multi-line `prefixed` branch at `:1216-1224`, which when
collapsed shows only the first line) and `linear.rs:276`/`:322`; the collapsed renderer and linear
apply `first_line` (`linear.rs:276` is its collapsed arm). In `tool_lines` the collapsed
`first_line` for the Denied branch goes at `transcript.rs:1223` (that branch returns before
`detail`, `:1173`/`:1176`, is used); the full renderer wraps all of it. `linear.rs:322`, inside the
`Expanded` arm, goes through `labelled` (`linear.rs:223-231`), which already splits on `'\n'`. The
tests at `transcript.rs:1939` and `linear.rs:1085` use one-line reasons and pass unchanged; add
new multi-line assertions.

### Screen-reader mode

In linear mode the overlay is not opened. The transcript key and `/transcript` print each shown
entry once: Tool and Reasoning entries in linear's `Expanded` form (the `Entry::Expanded` arm,
`linear.rs:298-349`, handles only those two), every other entry in the normal linear `label` form.
It skips `Entry::Expanded` snapshots and `follow_ups`, as `verbose_lines` does, through one shared
`Transcript` accessor (`follow_ups` is private today). Mechanism: a one-shot `app.transcript_print`
flag on `App` (like `app.reprint`), set by the key arm and `/transcript` when `app.linear` is set;
`Linear::render` consumes it (next to the `reprint` reset at `linear.rs:411`) and prints the
entries directly, Tool/Reasoning through a helper over `&Entry` that shares the `Expanded` arm's
wording. Only settled entries print (not streaming or pending ones), so nothing prints twice when
it later commits. It is never a silent no-op: with nothing settled it prints `no transcript`.

### Task list

Unchanged except the key. When the list is empty the toggle still flips the flag; `/todos` and the
new key additionally print a one-line notice, `no tasks`, so the key is never silent. The key arm
(`app.rs:1101-1103`) runs outside `run_immediate`, so it writes the notice with `shown_mut()` to
keep it out of the main transcript while a subagent view is open.

## Implementation Steps

1. `fontaine-core/src/keybindings.rs`: add `TuiAction::Transcript`; `Todos` default `ctrl+t`, `AgentsPanel` default none;
   update `ALL` (length 13), `as_str`, `default_keystroke`.
2. `fontaine-tui/src/keymap.rs`: defaults table (`defaults()`, not compiler-checked: `ctrl+o` →
   `Transcript`, `ctrl+t` → `Todos`, no default for `AgentsPanel`) and its tests (override/displacement cases keep passing with
   the swapped keys).
3. `transcript.rs`: keep the full denial text, through `display_text`; `first_line` at render
   only in the collapsed `tool_lines` path (`transcript.rs:1223`) and the collapsed arm of
   `linear.rs` (`:276`); the `Expanded` arm (`linear.rs:322`) already prints the whole reason split
   on `'\n'` via `labelled`; add `verbose_lines`.
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
   `:1060`) do. The `Interrupt` arm (`app.rs:890-899`) also closes the view (checked first, before
   the `close_palette` check, same gate and same consumption) and returns `Action::None`. Update the test at `app.rs:3164`,
   `ctrl_o_toggles_the_task_list_unless_the_agents_panel_has_the_keys`: switch it to `ctrl+t`,
   rename it, and add a twin for the transcript key.
6. `view.rs`: overlay + footer (same order as the key block); add the transcript view to the
   precedence doc comment on `view::overlay()` (`view.rs:328-333`) at its slot after the
   questions; `screen.rs`: set `committed_under_overlay` for the new overlay.
7. `commands.rs` / `ui/mod.rs`: `/transcript` command and dispatch: a BUILTINS row
   (`commands.rs:432-438`, after `todos`, before `vim`; the table must stay sorted, test
   `the_table_is_sorted_by_name`) and a `Command::Transcript` arm in `run_immediate_inner` next to
   `Command::Todos` (`ui/mod.rs:1079`; its `_ => {}` fallthrough would silently ignore it); add
   `Command::Transcript` to `runs_mid_turn` (`commands.rs:121-141`) and `"/transcript"` to the test
   `runs_mid_turn_marks_the_immediate_ui_set_and_nothing_else` (`commands.rs:983-1003`), so it acts
   immediately like `/todos`.
8. `linear.rs` / `app.rs` / `transcript.rs`: the key arm and `/transcript` set the one-shot
   `app.transcript_print` flag when `app.linear` (no overlay opens); `Linear::render` consumes it
   and prints each settled shown entry once: Tool and Reasoning through a helper over `&Entry` in
   the `Expanded` wording, every other entry through the normal `label` form, skipping
   `Entry::Expanded` snapshots and `follow_ups` via the shared `Transcript` accessor.
9. `reports.rs`: help keys line (`reports.rs:60`: `ctrl+o transcript`, `ctrl+t todos`; no agents hint) and its test
   at `:931` (new expected keys line). `/keybindings` follows `TuiAction::ALL` automatically
   (`ui/commands/keybindings.rs`); no change.
10. `tests/todos.rs`: the test at `:38` presses Ctrl+O for tasks; switch it to `ctrl+t` via
    `press_with` and rename it.
11. Docs and Decisions (see below).

## Files to Modify

- `rust/fontaine-core/src/keybindings.rs`
- `rust/fontaine-tui/src/{keymap,app,transcript,view,screen,commands,linear,reports,lib}.rs`,
  `rust/fontaine-tui/src/ui/mod.rs`, new `rust/fontaine-tui/src/transcript_view.rs`,
  `rust/fontaine-tui/tests/todos.rs`
- `docs/tui.md` (keys table :227, example :238, `/expand` text ~:48-64 and :577 gets a pointer,
  `/todos` :603, task panel :897, new "The transcript view" section), `docs/configuration.md`
  (:1074 example, :1102 action table), `docs/terminal.md` `CHANGELOG.md` `[Unreleased]`
- `tasks/fontaine_harness_plan.md` Decisions

## Trade-offs

- Alternate screen for the view: rejected; Fontaine is inline-scrollback by design and entering an
  alternate screen would hide scrollback and break native selection.
- Re-printing the full cards as new lines (what `/expand` does): kept as the per-card tool, but
  it appends to scrollback and duplicates content; a toggled view leaves scrollback clean.
- Keep Ctrl-O on tasks and put the view elsewhere: rejected by the issue owner; Claude Code parity
  and the silent no-op both argue for moving the tasks.
- Ctrl-T for tasks as in Claude Code, with the agents panel left without a default key (it keeps
  `/agents`) rather than an Alt chord for tasks: Claude Code binds Alt+T to the thinking toggle.

## Decisions

- 2026-11-04 (owner comment on #233): the task list gets its own shortcut, not Ctrl-O. Claude
  Code binds Ctrl+T to the task checklist and Fontaine follows it; the agents panel loses its
  default key and keeps `/agents`.
  The 2026-11-04 `### TUI` line also records each observed difference from Claude Code's view
  (input box kept, dimmed with no cursor, and pastes ignored while the view is open; scroll/close
  keys; anything else seen in tmux).
- AFK: `verbose_lines` output is cached in `TranscriptView` (RefCell) keyed on width, viewed agent, a `Transcript` change counter and last-entry length, and `overlay()` only slices it, because it runs ~3 times per frame at 80 ms while streaming (most conservative: no per-frame re-render).
- AFK: screen-reader mode sets a one-shot `app.transcript_print` flag (no overlay) that `Linear::render` consumes to print settled shown entries once (Tool/Reasoning in the `Expanded` wording, others via `label`, skipping `Expanded` snapshots and `follow_ups` through a shared `Transcript` accessor), unsettled entries excluded so nothing prints twice.
- AFK: the Ctrl-C close of the transcript view and the view's key block are gated on a single helper `view::transcript_has_keys` shared with `view::overlay`'s precedence, so an overlay above the view (e.g. the mid-turn unavailable-guardian prompt) keeps Ctrl-C as Interrupt.

## Documentation

- `docs/tui.md`: key table, `/transcript`, a section describing the view and its keys, task-panel
  text now naming Ctrl-T; a pointer from the `/expand` text to the view.
- `docs/terminal.md`: the "Screen-reader mode" section (~:146) describes the key and
  `/transcript` printing the entries once.
- `CHANGELOG.md`: an `[Unreleased]` entry.
- `docs/configuration.md`: `transcript` action row, `todos` default `ctrl+t`, `agents_panel` default none, example rebind.
- Plan Decisions: replace the 2026-10-25 "Ctrl-O and `/todos` both toggle the task panel" line
  (`tasks/fontaine_harness_plan.md:830`, under `### Tools`) to say Ctrl-T and `/todos`, and amend its
  "(`/todos` does not print)" to cover the empty-list `no tasks` notice; amend the :948 2026-10-31
  reasoning decision that cites Claude Code's "Ctrl-O transcript view" as lacking in Fontaine to say
  Fontaine's Ctrl-O view now shows the full kept reasoning; add a 2026-11-04 line for the transcript view and the move; amend the
  2026-10-26 verdict-reason deviation (`tasks/fontaine_harness_plan.md:352`, under `### Agents`) to say the reason is inline and also complete in the view;
  mark the `tasks/fontaine_harness_plan.md:932` line ("No key is rebound for tool cards; Ctrl-O
  stays the task list, ...") superseded by the new 2026-11-04 line, which goes at the end of the
  `### TUI` subsection (starts ~:889).

## Testing Strategy

- Unit (`keybindings`, `keymap`): defaults map `ctrl+o` → transcript and `ctrl+t` → todos, and `agents_panel` has no default key; an
  override of `ctrl+o` to `todos` displaces `transcript` and is reported as overridden.
- Unit (`transcript`): `verbose_lines` shows every row of a long multi-line command, the whole
  output, the full reasoning, and the full multi-line denial reason; an `Expanded` entry and a
  stale pending row are not duplicated; collapsed rendering of the same denial stays one line.
- Unit (`transcript_view`): the line cache is reused when nothing changed (a second `overlay()`
  call does not re-render) and rebuilt on width change, viewed-agent change, a new entry and
  streaming growth of the last entry.
- Unit (`transcript_view`): the view opens pinned to the tail (following, not at scroll 0), scroll clamp at both ends, `follow` keeps the tail in view as entries grow, an upward scroll clears `follow` so the window stays put as entries are appended, and scrolling back to the bottom resumes following.
- Unit (`transcript`): `Decision` Deny followed by `guardian_denied` with the pending row already
  committed leaves the original `Entry::Tool` holding the `[rule] explanation` status, and
  `verbose_lines` shows it rather than the agent-facing note.
- Unit (`app`): the key opens and closes the view; Esc, `q` and Ctrl-C (through the `Interrupt`
  arm, with no Interrupt/Quit returned) close it, but with an overlay above the view (the
  unavailable-guardian prompt) open Ctrl-C does not close it and still yields Interrupt; typing is ignored while
  open, and so are Ctrl-T, Ctrl-V and Ctrl-L; the key is ignored while the agents panel is open; an approval request takes precedence and the view is still open after the approval closes;
  Ctrl-T toggles the task list while the agent list is focused; the todos key still toggles and an empty list yields the `no tasks` notice (in the viewed
  subagent's transcript when one is open).
- Screen (`screen.rs` with `TestBackend`): opening over a tall transcript leaves the last message
  in scrollback on close (regression in the style of #296).
- Linear: the key prints settled Tool and Reasoning entries once in the `Expanded` form and every
  other settled entry in the normal `label` form, skips `Entry::Expanded` snapshots, `follow_ups`
  and unsettled entries, prints `no transcript` rather than nothing, consumes the one-shot flag
  (a second render prints nothing more), and opens no overlay.

## Manual Verification

1. In a real tmux (see `tasks/design/testing.md`, "Reproducing terminal layout bugs"), run
   `cargo run -p fontaine`, have the model run a multi-line shell command, press Ctrl-O, and
   check the whole command and output show, scrolling works, and Ctrl-O/Esc return to the prompt
   with scrollback intact. Not automated because only a real terminal reproduces live-area
   layout.
