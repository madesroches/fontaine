# Working Spinner Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/355

Issue: #355

## Overview
While a turn runs, the only in-flight cue is the status-bar text `working · esc to interrupt`.
Replace it with an animated spinner row above the message box: a cycling glyph plus a label whose
colour shimmers, like Claude Code's. It covers the whole turn (reasoning, tool waits, answer
streaming). The status bar keeps `esc to interrupt` but drops `working`. The spinner is display
only: it never gates input and Esc behaves as before.

## Current State
- `statusbar.rs:54-74` builds the hint; `app.running` yields `working · esc to interrupt · shift+tab mode`;
  an agent view shows `viewing <id> · working · …` from the agent's row status.
- `view.rs` lays the live area out vertically: transcript, overlay, queued rows, todo panel,
  input, status bar, agent list (`view::draw`, `view::chrome`, `view::height`). `chrome` is the
  single count of non-transcript rows that `screen.rs` budgets the transcript against, so any new
  row must be added to both `draw` and `chrome`.
- Redraws happen only on events. `ui/mod.rs:run` (idle loop) and `ui/turn.rs` (turn loop, ~l.822)
  each build a `tick` for timers: `LIST_TICK` (1 s) for the agent list, `IDLE_POLL` for the scheduler.
- `Theme` (`theme.rs`) holds semantic colours; the default `dark-ansi` is 16-colour, only the
  daltonized presets use RGB (Decision 2026-10-24: no truecolor default, no terminal-capability probing).
- `Glyphs` has a Unicode and an ASCII set (`FONTAINE_ASCII_GLYPHS`).
- `linear.rs` (screen-reader mode) has no status bar and no animation.

## Design
Chosen layout: Option A (see Mockups). A row of its own above the message box rather than a
transcript entry, so it never interacts with `commit_within`/scrollback, and the row simply
vanishes when the turn ends.

**State.** `App` gains `spinner_frame: u64`, set by `UiLoop::draw` from the elapsed time of a
monotonic clock (`frame = elapsed / SPINNER_STEP`, 80 ms) so a delayed tick skips frames instead
of slowing the animation. Rendering takes the frame from `App`, so tests set it directly and no
test depends on a clock.

**New module `rust/fontaine-tui/src/spinner.rs`** (pure, unit-tested):
- `lines(app, width) -> Vec<Line>`: empty unless the spinner is active, else one row:
  `<glyph> <label> (<elapsed> · ↓ <tokens> tok · esc to interrupt)`, truncated to `width`.
- Active when: the shown conversation is working and no overlay is waiting on the human
  (approval, question, guardian prompt, pickers). A waiting prompt is not "working". Main view: `app.running`;
  agent view: the viewed row's status `is_running()`. In an agent view the `esc to interrupt`
  clause is omitted (Esc does not stop it from there, per the existing note in `statusbar.rs`).
- `glyph(frame, glyphs)`: breathing-dot frames `·∙•●•∙` (the mockup's style #6); ASCII `.oO0Oo`. New `Glyphs.spinner: &'static [&'static str]` (data only, so other styles can be added later) so the ASCII
  set stays the one place that names fallbacks.
- `glyph_style(theme, frame) -> Style`: the glyph's own colour animation. RGB theme colour: the hue drifts one full turn over
  ~4 s (fixed HSL saturation/lightness, converted to `Color::Rgb`). Named ANSI colour: cycle the six base ANSI hues, one step
  per ~0.7 s, since a gradient is not portable there.
- `shimmer_style(theme, frame, index, len) -> Style`: a highlight sweeps left to right across the label
  and wraps, with a gap before it repeats.
  - RGB theme colour: interpolate between a dimmed and a lightened shade of `theme.spinner`
    (linear blend, distance from the sweep centre).
  - Named ANSI colour (the default): no interpolation is portable across 16-colour palettes, so step through modifiers
    around the base colour: `DIM` → normal → `BOLD`. Same sweep, stepped.
- Elapsed text from the turn start, then `↓ <tokens> tok` (`fontaine_tools::display::compact_count` of `TurnTokens::shown()`,
  below), the token clause omitted while 0 so a first-call wait shows no `↓ 0 tok`. Reuse the existing duration formatter used for goals
  (`fontaine_core::goal::duration_text`) rather than a new one; `App` gains `turn_started: Option<Instant>`,
  set where `running` becomes true.

**Turn token counter.** `App` gains `turn_tokens: TurnTokens` (in `spinner.rs`), reset with `turn_started`:
- `settled: u64`: the sum of `output_tokens` over the turn's `SessionEvent::Usage` events (`App::on_event` sees them;
  `Transcript::on_event` ignores `Usage` today).
- `inflight_chars: usize`: characters of `AssistantDelta` and `ReasoningDelta` since the last `Usage`; cleared by it.
- `shown() = settled + inflight_chars / 4`. Real usage only arrives when a call ends, so the 4-chars-per-token estimate keeps
  the counter moving while streaming; the reported figure replaces it at the call's end, so it may step down slightly. A call
  whose provider reports no `output_tokens` keeps its estimate. Output tokens only (what Claude Code's `↓` shows), not input,
  which every call re-reads in full and would swamp the figure.
- Only the main conversation's events feed it.

**Theme.** Add `spinner: Color` to `Theme` and every preset (accent that already reads on that
palette: reuse each preset's `heading` value as the starting point). The `every_preset_is_distinct`
test stays valid.

**Layout.** `view.rs` adds a `spinner` row (Length 0 or 1) between the todo panel and the message box,
in `draw`, and `+ spinner::lines(..).len()` in `chrome` so the screen's budgeting stays exact. The
row appearing/disappearing changes live-area height exactly as the queued rows already do.

**Status bar.** `statusbar.rs`: `working · esc to interrupt · shift+tab mode` becomes
`esc to interrupt · shift+tab mode`; the agent-view state label `working` stays (the spinner row
is below, not a replacement for the agent's state text there).

**Redraw tick.** Add `SPINNER_TICK = 80 ms` in `ui/mod.rs`; both loops fold it into their `tick` (min with the existing
ones) when `spinner::active(app)`. Extract the "min of optional waits" fold that `mod.rs`
already does inline for `LIST_TICK` into one helper used by both loops (DRY). ratatui diffs
the frame, so an idle tick writes only the few changed cells; no work happens when
the spinner is inactive. A multi-minute tool call costs ~12 tiny redraws a second, which is
acceptable; if it ever shows in profiling, the step can be raised.

**Linear/screen-reader mode.** No change: no animation by definition and `thinking:` precedent
(settled-only output). `docs/terminal.md` already lists the missing working indicator.

## Mockups
Open in a browser (animated):
- `tasks/spinner_indicator_mockups/option-a-row-above-input.html` — chosen. Dedicated row above the input, truecolor shimmer
  and the 16-colour stepped variant. Costs one row only while running.
- `tasks/spinner_indicator_mockups/option-b-transcript-tail.html` — spinner as the last live transcript line; reads as part of the
  conversation but is a new live-area element that `commit_within` must budget around.
- `tasks/spinner_indicator_mockups/spinner-styles.html` — ten glyph styles (braille, star bloom, scanner, blocks, orbit,
  breathing dot, ripple, chevron, braille snake, quadrant), each with its own colour animation and an ASCII fallback. Orthogonal
  to the layout options above; pick one as the default.
- `tasks/spinner_indicator_mockups/option-c-prompt-glyph.html` — spinning prompt glyph and pulsing border, no extra row; least
  noticeable, no space for elapsed time.

## Implementation Steps
1. `theme.rs`: `Glyphs.spinner` frames (Unicode and ASCII); `Theme.spinner` on every preset.
2. `spinner.rs` (new, register in `lib.rs`): `active`, `lines`, `glyph`, `shimmer_style`, elapsed text.
3. `app.rs`: `spinner_frame`, `turn_started`; set/clear with `running`.
4. `view.rs`: spinner row in `draw` and `chrome`.
5. `statusbar.rs`: drop `working` from the running hint.
6. `ui/mod.rs` and `ui/turn.rs`: set `spinner_frame` in `draw`; `SPINNER_TICK` via the shared fold helper.
7. Docs, CHANGELOG, Decisions line.

## Files to Modify
- `rust/fontaine-tui/src/theme.rs`, `spinner.rs` (new), `lib.rs`, `app.rs`, `view.rs`, `statusbar.rs`
- `rust/fontaine-tui/src/ui/mod.rs`, `ui/turn.rs`
- `docs/tui.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions → TUI)

## Trade-offs
- Row above the input over a transcript tail (B): B must be budgeted by `commit_within` and risks the
  live row reaching scrollback; A is one more counted chrome row, a pattern the queued/todo rows already use.
- Over a prompt glyph (C): too easy to miss during long tool waits, which is the case the issue is about.
- Stepped modifiers on named colours over forcing RGB: respects the 2026-10-24 decision to stay on the
  terminal's own palette instead of probing truecolor support.
- Frame from elapsed time over a counter incremented per tick: a stalled loop skips frames instead of slowing.

## Decisions
- 2026-11-01: `esc to interrupt` stays in the status bar (minus `working`); moving it into the spinner row would
  lose it whenever the row is hidden for a waiting prompt.
- 2026-11-01 (user): the glyph is the breathing dot (`·∙•●•∙`) with a drifting hue; `⬤` is excluded because terminals draw it wider than the other frames.
- 2026-11-01: No spinner in screen-reader/linear mode; settled-only output, as for `thinking:`.

## Documentation
- `docs/tui.md` "Status bar" (l.106-116): hint text and the new spinner row (elapsed time and `↓` output tokens, estimated while streaming), hidden while a prompt waits, ASCII fallback
  via `FONTAINE_ASCII_GLYPHS`, 16-colour theme gets a stepped rather than gradient shimmer.
- `docs/terminal.md` limitation (l.197) unchanged; `CHANGELOG.md` entry.
- Master plan `## Decisions` → TUI: one line recording the spinner's 16-colour stepped fallback.

## Testing Strategy
Unit tests (all pure):
- `shimmer_style`: RGB sweep peaks at the centre index and is the dim shade at the far end; wraps; named colour yields
  DIM/normal/BOLD steps; zero-length label does not panic.
- `TurnTokens`: the estimate grows with deltas (chars/4, rounds down); `Usage` replaces it with the reported `output_tokens`;
  a missing report keeps the estimate; sums across several calls in one turn; reset on a new turn; the row hides the clause at 0.
- `glyph_style`: the RGB hue advances with the frame and wraps after a full turn; named-colour mode cycles the six ANSI hues.
- `glyph`: every frame of every set is exactly one terminal column wide (ratatui's `Line::width()`, already a dependency, so the label never shifts; this rules out `⬤` and emoji-presentation glyphs); cycles through every frame and wraps; ASCII set used under `FONTAINE_ASCII_GLYPHS`.
- `active`/`lines`: running → one row; idle → none; hidden while an approval/question overlay is open; agent view follows
  the row's status; width truncation; esc clause omitted in the agent view.
- `statusbar`: running hint has `esc to interrupt` and no `working`; refusal/attach-notice precedence unchanged.
- `view::chrome`/`height`: includes the spinner row exactly when active (screen budgeting stays exact).
- Existing `screen.rs` bottom-row invariants re-run with `running = true`.
- Tick selection helper: spinner tick wins over `LIST_TICK`/`IDLE_POLL` only while active.

Real tmux (per `tasks/design/testing.md`): the row appearing and disappearing resizes the live area,
which is a layout-bug class; reproduce a run through a long tool call in tmux, including a resize
while spinning, and assert no stale live rows reach scrollback.

## Manual Verification
1. `cargo run -p fontaine`, start a turn that runs `sleep 20`. Expect the glyph spinning and the label
   shimmering above the box, `esc to interrupt` in the status bar, row gone on completion. Not automated: the
   animation itself is only judged by eye.
2. Repeat with `theme = "dark-daltonized"` (RGB gradient) and `FONTAINE_ASCII_GLYPHS=1` (`|/-\`).

## Open Questions
- Label wording: `Working…` (mockup) vs. a rotating verb list like Claude Code's. Plan assumes the fixed `Working…`.
- In an agent view there is no token feed (only the main conversation's events count), so the clause is omitted there; say if
  the agent row's spend should be shown instead.
- Should the spinner also show for a scheduled/goal turn? Plan assumes yes, since they set `app.running`.
