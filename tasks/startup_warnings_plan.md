# Startup Warnings as Styled Conversation Entries Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/82

## Overview

Startup warnings are hard to see: the TUI shows every notice as the same dim `· ` line, and the
config-root warnings never reach the conversation because they go out through `eprintln!` before
the screen starts. This plan gives notices a severity (info / warning) with its own
glyph and theme colour, splits a notice into a short headline and an actionable hint, routes
every non-fatal startup warning into the transcript, and bottom-anchors the transcript so a short
one (typically just the warnings) sits directly above the prompt.

## Current State

- `Models::notices` (`rust/fontaine-core/src/models.rs:492-516`) builds the guardian-fallback
  warning as one long `String`. `rust/fontaine/src/lib.rs:563-564` appends the containment
  notice; both land in `AgentSetup.notices` → `AgentRuntime::notices()`
  (`fontaine-core/src/agent.rs:103,508`). Scanner notices travel separately
  (`scanner_notices`, `agent.rs:132-134,512`) because every surface shows them in every mode.
- The TUI pushes each one with `Transcript::notice` (`fontaine-tui/src/lib.rs:205-226`), which
  creates `Entry::Notice(String)` (`transcript.rs:35,93`), rendered as dim `· ` text
  (`transcript.rs:482`). Runtime `Approver::notice`/`warn` become `Prompt::Notice(String)`
  (`lib.rs:74,113-119`, `ui/mod.rs:107,116`).
- Headless prints notices to stderr in `announce` (`fontaine/src/headless.rs:500-525`); notices
  and the guardian-unavailable line only in auto/plan mode, scanner notices always.
- Config-root warnings: `prepare_session` does `eprintln!("fontaine: warning: ...")` for each
  `roots.warnings` (`fontaine/src/lib.rs:435-437`) before either surface starts; the TUI never
  sees them. Other pre-TUI `eprintln!`s are either fatal errors (exit) or one-off warnings
  (`lib.rs:152` ignored `FONTAINE_AX_SCREEN_READER` value; `lib.rs:219` unsaved trust decision is
  fatal and returns exit 1).
- The linear (screen-reader) renderer labels a notice `notice:` (`linear.rs:259`).
- Theme: `fontaine-tui/src/theme.rs` has `warning`, `error`, `heading` slots but no info slot.
- Layout: `Screen::render` (`screen.rs:~153-178`) commits settled transcript lines to terminal
  scrollback via `insert_before` on every frame and sizes the inline live area to reach the
  bottom row. When the transcript is short, the committed lines sit at the top and the live area
  is mostly padding above the input (`view::draw_transcript`, `view.rs:390`, draws live lines
  from the top of the transcript area). That is the "anchored to the top" gap in the issue.

## Design

### Notice type (fontaine-core)

```text
pub enum Severity { Info, Warning }
pub struct Notice { pub severity: Severity, pub headline: String, pub hint: Option<String> }
```

`Notice::info(text)`, `Notice::warning(headline, hint)` constructors; `Display`/`text()` joins
headline and hint with " — " for the plain surfaces (headless stderr, tests that match on
content). `Models::notices`, `AgentSetup.notices`, `AgentRuntime::notices()`, and the scanner
notices become `Vec<Notice>`. `fontaine-tui/src/setup.rs` keeps `warnings: Vec<String>` and
converts them to `Notice::warning` where it appends them into `models.notices` (`:423,435`). The guardian fallback becomes headline "Guardian is running on
your main model `kimi/kimi-for-coding`", hint "No benchmark-validated guardian model is
available on your accounts — set `[guardian] model` or run /guardian-model". The containment and
scanner notices keep their text as a headline (`Warning` for failures/fallbacks, `Info`
otherwise) with a hint only where the text already has a natural action clause.

`Approver::notice` stays `Info`, `Approver::warn` is `Warning`; `ChannelApprover` forwards the
severity: `Prompt::Notice(Severity, String)`. The trait signatures do not change (open/closed
for the other `Approver` implementors).

### Pre-TUI warnings

`prepare_session` stops printing the root warnings and returns them as `SessionPrep.warnings:
Vec<Notice>` (severity `Warning`). Headless prints them to stderr, with the unchanged
`fontaine: warning: ...` text, in `main` right after `prepare_session` (lib.rs:246) when
`headless.is_some()`, taking them out of `prep` (`std::mem::take`) so `run_headless` and
`agent_setup` see none; this is before `resolve_models`, the `--max-budget-usd` check and
`open_input`, so early exits keep them and headless ordering does not change. Warnings collected
inside `prepare_session` are not lost on its `Err` paths (`git_bash(host)?`, `open_session`, the
`extensions::load` error exit): each `Err` return first prints the collected warnings to stderr.
Interactive early exits before the TUI starts (e.g. the model-resolution failure in
`run_interactive`) likewise print `prep.warnings` and the screen-reader warning to stderr before
returning. The interactive path carries them on: `run_interactive` and `agent_setup` both take `SessionPrep`, and `AgentSetup`
gets a `startup_warnings` list beside `scanner_notices`. The TUI shows them first in the
transcript, then the history/recent errors, notices and scanner notices in the existing order.
`screen_reader_mode` runs before any `Launch` exists, so it returns its ignored-env-value warning
as a `Notice` passed to `run_interactive` and pushed first in the TUI. The unsaved trust decision
is fatal and stays on stderr, as do fatal errors, which keep `eprintln!` and exit.

### Rendering

- `Entry::Notice(String)` becomes `Entry::Notice { severity, headline, hint }`.
  `Transcript::notice(text)` keeps its signature (Info, no hint) so the 57 existing non-test call sites
  are untouched; `Transcript::warning(headline, hint)` and `Transcript::push_notice(Notice)` are
  new. `fontaine_tui::run` and `drive` take the extra startup `Notice`s (screen-reader warning;
  `AgentSetup.startup_warnings` carries the rest), and the `tests/tui.rs` callers are updated.
- `entry_lines` renders: Info = the existing dim `· ` style (unchanged look); Warning = `⚠ `
  in `theme.warning` with the headline in `theme.warning`. The
  hint wraps on its own lines indented under the headline in the dim style. Spans inside
  backticks and `/command` tokens in the hint are bold, so the actionable part stands out
  independently of colour. Hint wrapping is span-aware: the bold state is carried across wrapped
  rows, so a backticked span that crosses a row break stays bold on both rows.
- Theme: no hard-coded colours. Warning reuses `theme.warning`; no new
  slot is needed because Info keeps the dim style (decision below).
- Glyph fallback: a small `Glyphs` choice (`⚠`/`·` vs `!`/`-`) selected once at startup by
  locale (non-UTF-8 `LC_ALL`/`LC_CTYPE`/`LANG` → ASCII) and stored on `App` next to the theme.
- Linear renderer: `notice:` for Info, `warning:` for Warning, headline
  and hint on separate labelled lines, so screen readers announce severity in words.

### Bottom-anchored transcript

Goal: with a short transcript, the last notice is on the row directly above the input box;
empty space is above the first entry. A trailing User/Shell entry ends in a blank row, so the
"directly above the input" goal is scoped to notices.

Today every settled line is committed to scrollback immediately, so a short transcript is
written at the top and the live area below it is padding. Change: settled lines are committed
only when they no longer fit above the input.

- `Screen::render` computes `chrome`, the non-live rows (overlay, queue, todo panel, input box,
  status line), from a new `view` helper split out of `view::height`'s `content - live`. The
  helper returns the unclamped total: `view::height` clamps to `max_height`, which would make the
  budget wrong under a large overlay. The order in `render` becomes: chrome, budgeted commit,
  `view::height` (today the commit runs before the height is computed).
  `Transcript` gets a commit that takes this budget. Invariant: after each frame's commit, held
  rows + chrome <= term_h (committing c rows moves `top` down by c, and `wanted = max(content,
  free)` absorbs the remainder); whole oldest entries are committed until it holds, and an entry
  taller than the budget commits whole. `commit_entries` has no per-entry row count, so the
  budgeted commit derives it at the current width: an entry's rows are `entry_lines` at that width
  offset by `committed_chars` (`assistant_rows(text, skip, width)` for a streaming reply). The
  budget is the rows still releasable (held + chrome - term_h, if positive) and each commit
  subtracts the rows it releases, so rows consumed by older entries in the same call count against
  what a later entry or the stream may release. `commit_entries` stops at the first unsettled
  entry, so a pending tool call or stream pins later entries (accepted, as today). The streaming
  cut gets the same budget as a new `Cut` variant carrying that remaining row budget; it advances
  `committed_chars` partially, by at most that many rows. Commit is oldest-first, so once the
  stream overflows the budget the older settled entries (the warnings) commit before any stream
  row; nothing is forced out while everything still fits. `commit(width, theme)` and
  `Cut::Rows`/`Cut::Lines` stay unchanged for the linear renderer and the existing `transcript.rs`
  unit tests.
- Held-back settled entries are drawn as live lines, so `draw_transcript` bottom-aligns: when
  `lines.len() < area.height` it renders into the bottom `lines.len()` rows of the area.
- `wanted`/`free` are computed after the commit, unchanged.
- Exit: `clear_live` erases the live area, which now may hold uncommitted transcript.
  `TerminalGuard` therefore flushes everything still held back to scrollback (a
  `Screen::commit_all`) before `clear_live`, so the conversation stays in the terminal after
  quitting, as today.
- `/clear` calls `commit_all` before `transcript.clear()` (`ui/commands/context.rs:19`, which
  resets `committed`), so held entries reach scrollback instead of vanishing; scrollback is kept as
  before; resizing recomputes `R` each
  frame, committing entries that stop fitting.
- The linear renderer has no live area and is unaffected.

## Implementation Steps

1. `fontaine-core`: add `Severity`/`Notice` (new `notice.rs`, exported from `lib.rs`); change
   `Models::notices`, `AgentSetup.notices`, `AgentRuntime::notices()`/`scanner_notices()` and the
   scanner stage notices (`scanner_stage.rs`) to `Notice`; add `startup_warnings` (and update the full `AgentSetup` struct literal in
   `fontaine-core/tests/common/runtime.rs`). Split the guardian-fallback text. Update the tests that match on notice text (`tests/models.rs`,
   `tests/scanner_model.rs`, `tests/scanner.rs`) to use `Notice` accessors.
2. `fontaine` crate: containment notice as `Notice`; `prepare_session` returns warnings in
   `SessionPrep.warnings`; headless prints them in `main` right after `prepare_session` and takes them out of `prep`;
   print collected warnings on every `Err` path of `prepare_session` and on interactive early
   exits before the TUI starts; thread them from `SessionPrep` into `AgentSetup` on the
   interactive path only; `screen_reader_mode` returns its
   warning as a `Notice` passed to `run_interactive`; `headless::announce` `Display`s `Notice`s (the guardian-fallback
   stderr text changes to headline — hint; root-warning text is unchanged). Update
   `tests/scanner_headless.rs`, `parallel_agents.rs`, `telemetry_micromegas.rs` expectations that
   match the guardian-fallback text.
3. `fontaine-tui`: convert `setup.rs` `warnings` into `Notice`s where appended to
   `models.notices`; update `ui/tests.rs:106` (`Prompt::Notice` construction) and
   `fontaine-evals/src/runner.rs:731` (scanner notice printing); the full `AgentSetup` struct
   literal in `fontaine-tui/tests/common/mod.rs`; `fontaine_tui::run`/`drive` signatures and the
   `tests/tui.rs` callers; `Prompt::Notice(Severity, String)`; `ChannelApprover::{notice,warn}`; `Entry::Notice`
   variant, `Transcript::{warning,push_notice}`; `entry_lines` rendering with hint wrapping and
   bold actionable spans; `Glyphs` + locale detection; `lib.rs` startup loop pushes
   `startup_warnings`, notices and scanner notices as `Notice`s; `app.rs` test helper
   `last_notice` and `linear.rs` labels.
4. Bottom-anchoring: budgeted commit in `transcript.rs`, `Screen::render` sizing,
   `draw_transcript` bottom alignment, `Screen::commit_all` called from `TerminalGuard::drop`
   before `clear_live` and before `transcript.clear()` on `/clear`.
5. Docs and changelog (see Documentation).

## Files to Modify

- `rust/fontaine-core/src/{lib.rs,models.rs,agent.rs,scanner_stage.rs}`, new `notice.rs`;
  tests under `rust/fontaine-core/tests/` including `tests/common/runtime.rs`
- `rust/fontaine/src/{lib.rs,headless.rs}`; `rust/fontaine-evals/src/runner.rs`
- `rust/fontaine-tui/src/{lib.rs,transcript.rs,view.rs,screen.rs,linear.rs,app.rs,theme.rs
  (only if a slot proves needed),setup.rs,ui/mod.rs,ui/tests.rs,ui/commands/context.rs}`; `tests/tui.rs`, `tests/common/mod.rs`
- `docs/tui.md`, `docs/terminal.md`, `docs/headless.md`, `docs/configuration.md`, `docs/tools.md`,
  `CHANGELOG.md`

## Trade-offs

- **Severity in the type vs. parsing it from text.** A `Notice` struct carries severity and the
  headline/hint split structurally; guessing from strings would be fragile and is the kind of
  matching on prose the project avoids.
- **Keep `Approver::notice(&str)`/`warn(&str)` signatures.** Severity is implied by which method
  is called, so other implementors are untouched.
- **Info keeps the dim style** instead of a new colour: it is the existing look, adds no theme
  slot, and keeps warnings visually distinct without a second accent.
- **Hold back commits** vs. padding with blank lines above the viewport or moving the cursor:
  holding back keeps the single-writer rule (settled lines are written once) and needs no
  terminal cursor tricks; cost is that a short transcript is redrawn each frame, which is cheap.

## Decisions

- 2026-10-25 AFK: Claude Code's interactive startup notices cannot be observed through `claude -p`
  (headless prints none); matched from its documented behaviour (yellow warning line above the
  prompt). No deviation recorded beyond the dim Info style, which is kept for non-warning notices.
- 2026-10-25 AFK: The headless guardian-fallback stderr text changes from one sentence to
  "headline — hint" because the notice is now split structurally; root-warning text is unchanged.
- 2026-10-25 AFK: ASCII glyph fallback is selected by locale rather than a new config key, to
  avoid a settings surface for a cosmetic switch.

## Documentation

- `docs/tui.md`: notice severities, warning glyph/colour, bottom-anchored transcript; revise the
  top-down / scrollback / narrowing paragraphs (~lines 10-27).
- `docs/terminal.md`: update the label table (~line 157); add a new glyph/locale section for the
  ASCII fallback and its locale rule.
- `docs/headless.md`: add that config-root warnings go to stderr.
- `docs/configuration.md` (~551, ~870, ~209) and `docs/tools.md` (~87): the additional-directory
  warning (~551, tools.md ~87), the invalid `FONTAINE_AX_SCREEN_READER` "ignored with a warning"
  (~870) and the guardian-fallback notice text (~209) now appear as warnings in the conversation
  (interactive) instead of "a warning on stderr"; update the wording and the notice text.
- `CHANGELOG.md` under Unreleased.

## Testing Strategy

All unit/`TestBackend` screen tests; nothing needs a live service.

- `Notice`: constructors, `Display` joining, guardian fallback headline/hint content
  (update `tests/models.rs`).
- Transcript rendering (`transcript.rs` tests): warning entry has `⚠ ` prefix and
  `theme.warning` colour, distinct from Info; hint wraps under the headline at narrow widths;
  backticked and `/command` spans are bold, including a backticked span crossing a row break; ASCII glyph fallback; one test with a non-default
  theme draws the warning in that theme's `warning` colour.
- Linear renderer: `warning:` label.
- Bottom anchoring (`screen.rs` tests, the issue's acceptance): a transcript shorter than the
  viewport renders with its last line on the row immediately above the input box and blank rows
  above the first entry; adding entries until it overflows commits the oldest to scrollback with
  nothing lost or duplicated, ending with `commit_all` then `clear_live` leaving the full
  transcript in scrollback; a streaming reply across the boundary keeps held warnings anchored;
  a pending tool call holding an over-budget transcript; an overlay/approval growing the chrome so
  held entries commit; `/clear` commits held entries first; resize.
- Pre-TUI warnings: a `tests/tui.rs` (or `ui/tests.rs`) test that root warnings handed to the
  startup path appear as warning entries; a headless test that they still go to stderr in every
  mode, including when a later startup check exits early.
- Regression: these `screen.rs` tests are rewritten for held-back commits:
  `first_commits_fill_from_the_top`, `committed_lines_stay_contiguous`, the `late` notices test,
  `clear_live_keeps_scrollback_and_blanks_the_live_rows`, and `scripted()`'s `area.height`
  assertion. The row-budgeted stream cut is a new `Cut` variant with a partial `committed_chars`
  advance, tested separately; the remaining scrollback tests stay green.

## Open Questions

None.
