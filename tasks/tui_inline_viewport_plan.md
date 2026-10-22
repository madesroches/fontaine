# TUI Inline Viewport Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/70

## Overview

The TUI draws on the alternate screen, so inside tmux (default mouse bindings) drags and wheel
events are forwarded to Fontaine, which does not capture the mouse. Text selection fails and so
does wheel scrolling. This plan moves the TUI to an inline viewport, as Claude Code does. Only the
live area (the unsettled transcript tail, any open panel, the input box and the status bar) is
redrawn. Settled transcript lines are written once into the terminal's real scrollback with
`Terminal::insert_before`. tmux then sees `alternate_on = 0`, and its own selection, wheel
scrolling and search work. The change stays inside `fontaine-tui`, plus one workspace feature flag
and the docs.

## Current State

- `fontaine-tui/src/lib.rs:546` `run` calls `ratatui::try_init()`, which enables raw mode, enters
  the alternate screen and installs ratatui's panic hook. `TerminalGuard` (`lib.rs:517`) disables
  bracketed paste and calls `ratatui::restore()`, which also leaves the alternate screen.
- `drive` (`lib.rs:488`) takes a `render: impl FnMut(&App)`. `Ui::draw` (`lib.rs:106`) calls it
  after every change. `run` passes a closure that calls `terminal.draw(|f| view::draw(f, app))`.
- `view::draw` (`view.rs:43`) splits the full frame into transcript (`Min(1)`), overlay
  (approval / guardian-unavailable / `/denied`, capped at `area.height - 4`), input (wrapped rows
  up to `MAX_INPUT_ROWS = 6`, plus a border) and a one-row status bar. `draw_transcript`
  (`view.rs:109`) re-wraps every entry each frame and shows a window offset by `App::scroll`.
- `Transcript` (`transcript.rs`) is a `Vec<Entry>`. Its entries are user messages, assistant text
  with a `streaming` flag, tool calls whose `ToolStatus` starts as `Pending`, notices and errors.
  Events mutate entries in place: deltas extend the last streaming assistant entry, and
  `find_tool` locates a call by id (falling back to the latest pending call of the same tool
  name) to set its result or denial. `Ui::on_event` (`lib.rs:438`) calls `guardian_denied`
  right after the `Decision` event that denied the call.
- `input::wrap` (`input.rs:173`) is a hard character-chunk wrap, so a wrapped row is final as
  soon as a later row exists.
- `App::scroll` and PgUp/PgDn in `App::message_key` (`app.rs:219`) scroll the transcript.
  Approval-panel scrolling is separate (`approval.rs:135`, `Approval::scroll`/`set_max_scroll`).
- Tests: `view.rs` has one full-frame test (long-command scrolling in the approval panel). The
  integration harness `tests/common/mod.rs:238` renders into `Terminal::new(TestBackend::new(120,
  40))` and exposes the frame as `Ui::screen()`, which every `tests/tui.rs` test polls.
- ratatui is pinned at `=0.30.2` with default features (`rust/Cargo.toml:71`). `Viewport::Inline(h)`
  is fixed when the `Terminal` is constructed; there is no way to change its height and no
  `into_backend`. `Terminal::with_options` for an inline viewport calls
  `backend.get_cursor_position()`, which on crossterm is a DSR round trip through stdin.
  `insert_before` without the `scrolling-regions` feature clears and redraws the viewport.

## Design

### Commit-then-forget transcript

`Transcript` keeps its `entries` (so `find_tool`, `set_status` and `guardian_denied` keep working
on calls already in scrollback) and gains a commit cursor:

```rust
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// Entries fully written to scrollback.
    committed: usize,
    /// Chars of `entries[committed]`'s text already written, when it is a streaming reply.
    committed_chars: usize,
}
```

- An entry is **settled** when it is not `Assistant { streaming: true }` and not a `Tool` with
  `ToolStatus::Pending`.
- `pub fn commit(&mut self, width) -> Vec<Line<'static>>` walks from `committed`. A settled entry
  emits all of its lines (minus the rows already committed for a reply that just finished) and
  advances the cursor. For a streaming reply, it emits every wrapped row except the last, records
  in `committed_chars` the char offset just past those rows (including a `'\n'` that ends a
  committed line), and stops. It also stops at a pending tool call. Order is preserved, so
  anything after an unsettled entry stays live until that entry settles.
- `pub fn live_lines(&self, width) -> Vec<Line<'static>>` renders `entries[committed..]`, with
  the first entry's text starting at `committed_chars`.
- Rows are counted by char offset, not row index. A resize mid-stream then re-wraps only the
  uncommitted remainder at the new width, and no text is dropped or repeated. Assistant text has
  an empty prefix, so continuing it from a row boundary renders the same as wrapping it whole.
- The existing `lines(width)` stays for tests and is the reference the commit property is checked
  against.
- Once committed, a status change (for example a late `ToolResult` on a call already shown as
  denied) is not redrawn. `set_status` already never overwrites `Denied`, and `guardian_denied`
  runs in the same `on_event` call as the `Decision`, before any render. No committed entry
  changes after it is written.

### Turn end settles the tail

`Ui::finish_turn` runs `transcript.interrupt_pending()` (finish streaming, mark `Pending` calls
`Interrupted`) on every turn end, not only on interrupt. A call that never got a result would
otherwise hold the live region, and every later line behind it, until the session ends.

### Live area layout and height

`view` gains `pub fn height(app, width, max_height) -> u16`. It returns the live rows the next
frame needs: `live_lines` + overlay (body + footer + 2 borders when open) + input rows + 2 + 1
status row, clamped to `max_height` (the terminal height). `view::draw` keeps its layout with the
transcript slot holding `live_lines` (tail shown when clipped). The overlay cap changes from
`area.height - 4` to `area.height - input_height - 1`, so the input box and status bar always
fit. Approval-body scrolling (`set_max_scroll`) is unchanged.

`App::scroll`, `SCROLL_STEP`, `SCROLL_CAP` and the transcript PgUp/PgDn arms are removed. PgUp
and PgDn now only scroll an open approval prompt. The terminal's own scrollback (tmux copy mode,
the terminal emulator's wheel) replaces transcript scrolling.

### `Screen<B>`: inline terminal with a resizable live area

New module `fontaine-tui/src/screen.rs`. It is generic over `ratatui::backend::Backend` so tests
drive it with `TestBackend`:

```rust
pub struct Screen<B: Backend> {
    terminal: Terminal<B>,
    height: u16,
    /// A stand-in left in the old `Terminal` while its backend moves to the new one.
    placeholder: fn() -> B,
}

impl<B: Backend> Screen<B> {
    pub fn new(backend: B, placeholder: fn() -> B) -> io::Result<Self>;
    /// Commit settled lines, fit the live area, draw it.
    pub fn render(&mut self, app: &mut App) -> io::Result<()>;
    /// Clear the live area and leave the cursor at its top, for exit.
    pub fn clear_live(&mut self) -> io::Result<()>;
    pub fn backend(&self) -> &B;
}
```

`render` does, in order:
1. `lines = app.transcript.commit(width)`. If any, call `terminal.insert_before(n, ...)`, which
   renders `Paragraph::new(lines)` into the buffer. The lines are already wrapped at `width`.
2. `h = view::height(app, width, term_height)`. If `h != self.height`, rebuild (below).
3. `terminal.draw(|f| view::draw(f, app))`.

**Height change = rebuild the `Terminal`** (the issue's open point, decided here). ratatui 0.30.2
cannot resize an inline viewport, so the rebuild:
1. moves the cursor to the current viewport's top row (`viewport_area` is captured from the last
   `CompletedFrame`/`get_frame().area()`), and `clear_region(ClearType::AfterCursor)`;
2. takes the real backend out with
   `std::mem::replace(self.terminal.backend_mut(), (self.placeholder)())`;
3. builds `Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(h) })`.
   ratatui anchors the viewport at the cursor row. When it grows, ratatui appends lines and
   scrolls the screen, pushing older lines into scrollback. When it shrinks, the area below is
   already clear and the next `insert_before` fills it.

Vendoring a custom terminal (Codex CLI's approach) was rejected; see Trade-offs.

`Terminal::with_options` asks the backend for the cursor position. On crossterm that is a DSR
round trip: a network round trip over ssh, and a 2 s timeout on terminals that do not answer,
racing the input reader thread. To avoid it, `run` uses a thin backend wrapper, `Anchored<B>`, in
`screen.rs`. It delegates every `Backend` method, remembers the position from the last
`set_cursor_position`, and forgets it on any call that moves the cursor (`draw`, `append_lines`,
`scroll_region_up/down`, `clear`). `get_cursor_position` answers from the remembered position and
falls back to the inner backend when it has none. At startup (before the reader thread starts)
and on a terminal resize, ratatui still asks the real terminal.

**Terminal resize** is left to ratatui's `autoresize` inside `draw`, which re-anchors an inline
viewport and clears the screen on a horizontal shrink. Lines already in scrollback keep the width
they were wrapped at, as in Claude Code.

### Driver signature

`drive(..., render: impl FnMut(&mut App))`: the render callback needs `&mut App` to advance the
commit cursor. `Ui<R: FnMut(&mut App)>` and `Ui::draw` pass `&mut self.app`. `App` itself stays
terminal-free.

### Startup and exit

`run` no longer calls `ratatui::try_init()`/`ratatui::restore()`: both touch the alternate screen,
and a stray `ESC[?1049l` restores a saved cursor position on xterm-likes. Instead:
- startup: `enable_raw_mode`, `EnableBracketedPaste`, install a panic hook that runs the same
  restore before chaining to the previous hook, then
  `Screen::new(Anchored::new(CrosstermBackend::new(stdout())), ...)`, then spawn the reader
  thread;
- `TerminalGuard` (Drop, so panics unwind through it) holds the restore: `clear_live` (so the
  input box and status bar do not stay in scrollback; the guard owns the `Screen`, or `run`
  calls `clear_live` before dropping it), then `DisableBracketedPaste`, `Show` cursor and
  `disable_raw_mode`.

### Feature flag

`rust/Cargo.toml`:
`ratatui = { version = "=0.30.2", features = ["scrolling-regions"] }`. This makes `insert_before`
scroll a region (DECSTBM) instead of clearing and redrawing the viewport on every commit, so the
live area does not flicker. Windows 10+ consoles support DECSTBM through crossterm's VT mode.

## Implementation Steps

1. **Feature flag.** Add `scrolling-regions` to the workspace `ratatui` dependency.
2. **Transcript commit cursor** (`transcript.rs`): add `committed`/`committed_chars`, `commit`,
   `live_lines` and an `is_settled` helper. Factor `entry_lines` so an assistant entry can be
   rendered from a char offset. Unit tests below.
3. **Turn end** (`lib.rs`): `finish_turn` calls `interrupt_pending` on every end. Keep the
   `"interrupted"` notice and the feed drain only for `TurnEnd::Interrupted`.
4. **App** (`app.rs`): remove `scroll`, `SCROLL_STEP`, `SCROLL_CAP`, the PgUp/PgDn arms and
   `self.scroll = 0` in `enter`.
5. **View** (`view.rs`): add `height`. `draw_transcript` renders `live_lines` showing the tail.
   Change the overlay cap to `area.height - input_height - 1`.
6. **Screen** (new `screen.rs`, `pub mod screen` in `lib.rs`): `Screen<B>` with `render`, the
   rebuild, `clear_live`, and the `Anchored<B>` backend wrapper.
7. **Driver** (`lib.rs`): `render: impl FnMut(&mut App)`, `Ui<R: FnMut(&mut App)>`, `Ui::draw`
   passes `&mut self.app`. Update the unit-test `ui()` helper.
8. **run / exit** (`lib.rs`): raw mode + bracketed paste + panic hook. `Screen` over
   `Anchored<CrosstermBackend<Stdout>>`. The render closure calls `screen.render(app)`.
   `TerminalGuard` clears the live area and restores, with no alternate-screen calls.
9. **Test harness** (`tests/common/mod.rs`): build `Screen::new(TestBackend::new(120, 40), ||
   TestBackend::new(1, 1))`. After each render, `Ui::screen()` is the backend's scrollback rows
   followed by its buffer rows, so `wait_for("all done")` finds text whether it is in scrollback
   or live. Existing `tests/tui.rs` assertions should keep passing unchanged; fix any that
   asserted the absence of text that now legitimately remains in scrollback.
10. **Docs** (`docs/tui.md`, master plan): see Documentation.

## Files to Modify

- `rust/Cargo.toml`: `scrolling-regions` feature on `ratatui`
- `rust/fontaine-tui/src/transcript.rs`: commit cursor, `commit`, `live_lines`
- `rust/fontaine-tui/src/app.rs`: drop transcript scrolling
- `rust/fontaine-tui/src/view.rs`: `height`, live-tail transcript, overlay cap, tests
- `rust/fontaine-tui/src/screen.rs` (new): `Screen<B>`, `Anchored<B>`
- `rust/fontaine-tui/src/lib.rs`: `drive`/`Ui` signature, `finish_turn`, `run`, `TerminalGuard`
- `rust/fontaine-tui/tests/common/mod.rs`: harness over `Screen<TestBackend>`
- `docs/tui.md`
- `tasks/fontaine_harness_plan.md`: `## Decisions` line
- `CHANGELOG.md` (via the PR step)

## Trade-offs

- **Inline viewport vs. mouse capture with our own selection and OSC 52 copy:** capture is far
  more work and would break native selection where it works today. Inline matches Claude Code.
- **Rebuilding the `Terminal` on height change vs. vendoring a custom terminal (Codex CLI):**
  the rebuild is ~30 lines on top of the pinned ratatui and reuses its inline anchoring,
  `insert_before` and diffing. A vendored terminal is several hundred lines that must track
  ratatui internals by hand. The rebuild costs one full repaint of the live area per height
  change, which is a few rows.
- **`Anchored` backend wrapper vs. letting each rebuild do a DSR:** height changes happen on
  input wrapping, panels and tool calls. Paying a cursor query each time is a visible stall over
  ssh (the reported setup) and races the reader thread. The wrapper is plain delegation.
- **Char-offset vs. row-count commit cursor for streaming replies:** row counts break when the
  width changes mid-stream. A char offset re-wraps only the uncommitted remainder.
- **Keeping committed entries in `entries` vs. dropping them:** dropping would make `find_tool`'s
  name fallback match the wrong call once the right one left the vector. Memory is the same as
  today.

## Decisions

- 2026-10-22: Height changes rebuild the inline `Terminal` (with a cursor-anchored backend
  wrapper) rather than vendoring a custom terminal; resolves the issue's open technical point.
- 2026-10-22: Every turn end marks still-pending tool calls `Interrupted`, so an orphaned call
  cannot hold the live region open.

## Documentation

- `docs/tui.md`: describe the layout. Finished output goes into the terminal's normal scrollback
  and the live area at the bottom holds the input box, status bar, open prompts and the unfinished
  tail. Scroll back with the terminal or tmux copy mode. Mouse selection works as in any shell
  program. Change the PageUp/PageDown row to "Scroll the command in an open approval prompt".
  Note that lines already printed keep their width after a resize.
- `tasks/fontaine_harness_plan.md` `## Decisions`: `2026-10-22: The TUI draws inline (live area
  plus terminal scrollback) instead of on the alternate screen, matching Claude Code; reason:
  alternate-screen apps lose mouse selection and wheel scrolling under tmux (#70).`

## Testing Strategy

Unit tests (`transcript.rs`):
- Streaming commit property: for a reply fed in arbitrary delta chunks (including `'\n'`
  boundaries and lines exactly `width` chars), concatenate every `commit` output and the final
  `commit` after `Assistant(full)`. The result equals `lines(width)` of the same transcript, with
  no row repeated or missing.
- The same property when `width` changes between deltas: committed text plus the remainder
  equals the full text (compare the concatenated row strings with the newlines removed).
- A pending tool call blocks commits of entries after it. Once it settles, they commit in order.
- `live_lines` holds the partial last row of a streaming reply and nothing already committed.
- `guardian_denied` and a late `ToolResult` still find a call whose entry is already committed
  (by id), and do not change a different pending call of the same tool.

Unit tests (`lib.rs`): a finished turn with a call that never got a result leaves no `Pending`
entry.

Unit tests (`view.rs` / `screen.rs`, `TestBackend` with scrollback):
- `Screen::render` writes a user message and a finished reply into `backend().scrollback()` and
  buffer rows above the viewport, not into the live area. The live area shows input + status only.
- Streaming: after a partial delta, nothing of the reply is in scrollback. After more text that
  completes a row, exactly that row is.
- Opening an approval prompt grows the live area. Closing it shrinks the area, and the rows it
  freed hold no stale panel text.
- The existing long-command approval test is ported to `Screen` and keeps passing: the tail is
  reachable with PgDn inside a viewport that fits the terminal.
- `Anchored`: `get_cursor_position` returns the last set position, and after a `draw` or
  `append_lines` it asks the inner backend.
- `clear_live` leaves the scrollback intact and the live rows blank.

Integration (`tests/tui.rs` through the harness): the whole existing suite runs on the inline
screen. Add one test: a session with two turns ends up with both replies in order in scrollback,
and the live area shows only the idle prompt.

## Manual Verification

These steps check real terminal and tmux behaviour, which `TestBackend` cannot reproduce. Any
breakage is obvious on the next interactive run.

1. `tmux new -s t` with `set -g mouse on`, then `cargo run -p fontaine` from `rust/`, send a
   message that produces a long reply. Expected: the reply scrolls into the pane's history; a
   mouse drag enters tmux copy mode and selects text; the wheel scrolls back through the
   transcript.
2. Type a message long enough to wrap to several rows, then trigger an approval prompt.
   Expected: the live area grows and shrinks without flicker or leftover rows.
3. Quit with Ctrl-C. Expected: the shell prompt appears right under the transcript with no input
   box or status bar left behind.
4. Resize the terminal narrower mid-session. Expected: the live area redraws at the new width,
   and earlier lines keep their old wrapping.
