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
- Redraws happen only on events. The idle loop (`ui/mod.rs:233-282`) builds a `tick` for timers:
  `LIST_TICK` (1 s) for the agent list, `IDLE_POLL` for the scheduler. The turn loop (`ui/turn.rs:825-831`)
  has only a `has_list` bool with a fixed `sleep(LIST_TICK)`; `serve_while` (`ui/turn.rs:357-405`) has no timer arm.
- `Theme` (`theme.rs`) holds semantic colours; the default `dark-ansi` and `light-ansi` use named 16-colour values; the
  Dark, Light and daltonized presets are RGB (Decision 2026-10-24: no truecolor default, no terminal-capability probing).
- `Glyphs` has a Unicode and an ASCII set (`FONTAINE_ASCII_GLYPHS`).
- `linear.rs` (screen-reader mode) has no status bar and no animation.

## Design
Chosen layout: Option A (see Mockups).

**State.** `App` gains `spinner_frame: u64`, set by `Ui::draw` (`ui/mod.rs:41`) from the elapsed time of a
fixed `Instant` on `Ui` (`frame = elapsed / TICK`, `TICK` = 80 ms, also the redraw tick) so a delayed tick skips frames instead
of slowing the animation. `Ui::draw` also stores the main turn's elapsed `Duration` (`turn_started.elapsed()`) on `App` next to
`spinner_frame`, like `app.goal` (`ui/mod.rs:72`). Rendering reads both from `App`, so `lines` never calls `Instant::now()`,
tests set them directly and no test depends on a clock.

**New module `rust/fontaine-tui/src/spinner.rs`** (pure, unit-tested):
- `lines(app, width) -> Vec<Line>`: empty unless the spinner is active, else one row:
  `<glyph> <label> (<elapsed> · ↓ <tokens> tokens)`, truncated to `width`.
- Active when: the shown conversation is working and none of the overlays waiting on the human
  is open: approval, questions, unavailable, server (`app.rs:176-221`). A waiting prompt is not "working"; the spinner stays
  visible for every other overlay (pickers, agents panel, history search, completion, `/` menu). Main view: `app.running`;
  agent view: the viewed row's status `is_running()`. In an agent view elapsed comes from
  `app.viewed_row().elapsed` (`agent_list.rs` `ListRow`) and the token clause is omitted (the agent's list row already shows its spend).
- `glyph(frame, glyphs)`: breathing-dot frames `·∙•●•∙` (the mockup's style #6); ASCII `.oO0Oo`. New `Glyphs.spinner: &'static [&'static str]` (data only, so other styles can be added later) so the ASCII
  set stays the one place that names fallbacks.
- `glyph_style(theme, frame) -> Style`: the glyph's own colour animation. RGB theme colour: the hue drifts one full turn over
  ~4 s (saturation and lightness taken from the HSL of `theme.spinner`, only the hue rotates, converted to `Color::Rgb`). Named ANSI colour: cycle through the preset's own accent fields (`user`, `success`, `heading`, `mode_plan`, `shell`;
  duplicates dropped, so `dark-ansi` gets four hues and `light-ansi` three), one step per ~0.7 s, since a gradient is not portable there.
  Every colour comes from the theme, so hues a preset leaves out never appear.
- `shimmer_style(theme, frame, index, len) -> Style`: a highlight sweeps left to right across the label
  and wraps, with a gap before it repeats.
  - RGB theme colour: interpolate (linear blend, distance from the sweep centre) from `theme.spinner` at the peak
    to the preset's `mode_manual` grey at the far end; no lightening toward white, which loses contrast on light presets.
  - Named ANSI colour (the default): no interpolation is portable across 16-colour palettes, so step through modifiers
    around the base colour: `DIM` → normal → `BOLD`. Same sweep, stepped.
- Elapsed text from the turn start, then `↓ <tokens> tokens` (`fontaine_tools::display::compact_count` of `TurnTokens::shown()`,
  below), the token clause omitted while 0 so a first-call wait shows no `↓ 0 tokens`. Elapsed uses `fontaine_tools::jobs::format_age` (`NmSSs`, as the agent list does) rather than a new formatter; `App` gains `turn_started: Option<Instant>`,
  set where `running` becomes true. Every `running = true` site goes through one `App` setter that also sets/clears
  `turn_started`: the turn (`Ui::turn`, `ui/turn.rs:726`), goal evaluation (`ui/goal.rs:37`), `/compact`
  (`ui/commands/context.rs:66`), the `!` shell (`ui/commands/workspace.rs:37`) and skill expansion (`ui/turn.rs:444`).
  With `turn_started` None the elapsed text is omitted.

**Turn token counter.** `App` gains `turn_tokens: TurnTokens` (in `spinner.rs`), reset with `turn_started`:
- `settled: u64`: the sum of `output_tokens` over the turn's `SessionEvent::Usage` events (fed from `Ui::on_event`, `ui/turn.rs:855`;
  `Transcript::on_event` ignores `Usage` today).
- `inflight_chars: usize`: characters of `AssistantDelta` and `ReasoningDelta` since the last `Usage`; cleared by it.
- `shown() = settled + inflight_chars / 4`. Real usage only arrives when a call ends, so the 4-chars-per-token estimate keeps
  the counter moving while streaming; the reported figure replaces it at the call's end, so it may step down slightly. A call
  whose provider reports no `output_tokens` keeps its estimate. Output tokens only (what Claude Code's `↓` shows), not input,
  which every call re-reads in full and would swamp the figure.
- Only the main conversation's events feed it (`Ui::on_event` handles those only).

**Theme.** Add `spinner: Color` to `Theme` and every preset (accent that already reads on that
palette: reuse each preset's `heading` value as the starting point).

**Layout.** `view.rs` adds a `spinner` row (Length 0 or 1) between the todo panel and the message box,
in `draw`, and `+ spinner::lines(..).len()` in `chrome` so the screen's budgeting stays exact. The row count is also passed
 into `agent_list_lines` (252), `todo_lines` (267-269), `body_rows` (274-288) and the `approval_height` `.min(...)` in `draw` (168-170). The
row appearing/disappearing changes live-area height exactly as the queued rows already do.

**Status bar.** `statusbar.rs`: `working · esc to interrupt · shift+tab mode` becomes
`esc to interrupt · shift+tab mode`; the agent-view state label `working` stays (the spinner row
is below, not a replacement for the agent's state text there).

**Redraw tick.** Reuse the 80 ms `TICK` constant in `ui/mod.rs`; the idle and turn loops fold it into their `tick` (min with the existing
ones) when `spinner::active(app)`. Extract the "min of optional waits" fold that `mod.rs`
already does inline for `LIST_TICK` into one helper used by the idle loop, the turn loop and `serve_while` (DRY). The turn loop's
`has_list` sleep first becomes an `Option<Duration>` tick so it can use the helper. `serve_while` gains a `TICK` arm,
because goal evaluation, `/compact`, the `!` shell and skill expansion run through it without the turn loop.

**Scheduled/goal turns.** `scheduled_turn`, `goal_turn` and `notification_turn` all go through `Ui::turn` (`ui/turn.rs:726`), so they
show the spinner with no extra work; the goal evaluation step is the `serve_while` case above.

## Mockups
Open in a browser (animated):
- `tasks/spinner_indicator_mockups/option-a-row-above-input.html` — chosen. Dedicated row above the input, truecolor shimmer
  and the 16-colour stepped variant. Costs one row only while running.
- `tasks/spinner_indicator_mockups/option-b-transcript-tail.html` — rejected (see Trade-offs).
- `tasks/spinner_indicator_mockups/spinner-styles.html` — ten glyph styles (braille, star bloom, scanner, blocks, orbit,
  breathing dot, ripple, chevron, braille snake, quadrant), each with its own colour animation and an ASCII fallback. Orthogonal
  to the layout options above; breathing dot chosen (see Decisions).
- `tasks/spinner_indicator_mockups/option-c-prompt-glyph.html` — rejected (see Trade-offs).

## Implementation Steps
1. `theme.rs`: `Glyphs.spinner` frames (Unicode and ASCII); `Theme.spinner` on every preset.
2. `spinner.rs` (new, register in `lib.rs`): `active`, `lines`, `glyph`, `glyph_style`, `shimmer_style`, `TurnTokens`, elapsed text.
3. `app.rs`: `spinner_frame`, turn elapsed, `turn_started`, `turn_tokens`; one setter for `running` that sets/clears `turn_started` and resets `turn_tokens`, used at every site
   (`ui/turn.rs`, `ui/goal.rs`, `ui/commands/context.rs`, `ui/commands/workspace.rs`).
4. `view.rs`: spinner row in `draw` and `chrome`, and its count in `agent_list_lines`, `todo_lines`, `body_rows`, `approval_height`.
5. `statusbar.rs`: drop `working` from the running hint.
6. `lib.rs`, `ui/mod.rs` and `ui/turn.rs`: add a fixed `Instant` field to `struct Ui` (`lib.rs:224`, built at `lib.rs:313` and `ui/tests.rs:37`); set `spinner_frame` and the turn's elapsed `Duration` in `Ui::draw`; feed `app.turn_tokens` from `Ui::on_event` (`ui/turn.rs:~855`); `TICK` via the shared fold helper in the idle loop, turn loop and `serve_while`.
7. `examples/live_area.rs`: a mode that sets `running`, steps `spinner_frame` across a long tool call, resizes, then clears `running`.
8. Docs, CHANGELOG, master-plan Decisions lines.

## Files to Modify
- `rust/fontaine-tui/src/theme.rs`, `spinner.rs` (new), `lib.rs`, `app.rs`, `view.rs`, `statusbar.rs`
- `rust/fontaine-tui/src/ui/mod.rs`, `ui/turn.rs`, `ui/goal.rs`, `ui/commands/context.rs`, `ui/commands/workspace.rs`, `ui/tests.rs`
- `rust/fontaine-tui/examples/live_area.rs`
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
- Master plan `## Decisions` → TUI, dated lines appended at the end of the subsection: `esc to interrupt` stays in the status bar; no spinner in screen-reader mode; breathing-dot glyph instead of Claude Code's star glyphs.

## Testing Strategy
Unit tests (all pure):
- `shimmer_style`: RGB sweep is `theme.spinner` at the centre index and `mode_manual` at the far end; wraps; named colour yields
  DIM/normal/BOLD steps; zero-length label does not panic.
- `TurnTokens`: the estimate grows with deltas (chars/4, rounds down); `Usage` replaces it with the reported `output_tokens`;
  a missing report keeps the estimate; sums across several calls in one turn; reset on a new turn; the row hides the clause at 0.
- `glyph_style`: the RGB hue advances with the frame and wraps after a full turn; named-colour mode cycles the deduped accent fields of `dark-ansi` and `light-ansi` and wraps.
- `glyph`: every frame of every set is exactly one terminal column wide (ratatui's `Line::width()`, already a dependency, so the label never shifts); cycles through every frame and wraps; ASCII set used under `FONTAINE_ASCII_GLYPHS`.
- `active`/`lines`: running → one row; idle → none; hidden while an approval, questions, unavailable or server overlay is open and shown with a picker open; agent view follows
  the row's status; width truncation.
- `statusbar`: running hint has `esc to interrupt` and no `working`; refusal/attach-notice precedence unchanged.
- `view::chrome`/`height`: includes the spinner row exactly when active (screen budgeting stays exact).
- `view` at small height with an expanded todo list while spinning: the budget stays exact.
- Existing `screen.rs` bottom-row invariants re-run with `running = true`.
- Tick selection helper: spinner tick wins over `LIST_TICK`/`IDLE_POLL` only while active.

Real tmux (per `tasks/design/testing.md`): the row appearing and disappearing resizes the live area,
which is a layout-bug class; reproduce a run through a long tool call in tmux with the new `examples/live_area.rs` mode, including a resize while spinning, and assert no stale live rows reach scrollback.

## Manual Verification
1. `cargo run -p fontaine`, start a turn that runs `sleep 20`. Expect the glyph spinning and the label
   shimmering above the box, `esc to interrupt` in the status bar, row gone on completion. Not automated: the
   animation itself is only judged by eye.
2. Repeat with `theme = "dark-daltonized"` (RGB gradient) and `FONTAINE_ASCII_GLYPHS=1` (`.oO0Oo`).
3. In tmux, run the `examples/live_area.rs` spinner mode through a long tool call, resize the window while it spins, and
   check no stale live rows reach scrollback.

## Open Questions
- Label wording: `Working…` (mockup) vs. a rotating verb list like Claude Code's. Plan assumes the fixed `Working…`.
