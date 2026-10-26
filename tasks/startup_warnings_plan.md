# Startup Warnings as Styled Conversation Entries Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/82

## Overview

Startup warnings are hard to see: the TUI shows every notice as the same dim `· ` line, and the
config-root warnings never reach the conversation because they go out through `eprintln!` before
the screen starts. This plan gives notices a severity (info / warning / error) with its own
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
  (`lib.rs:152` ignored `FONTAINE_AX_SCREEN_READER` value, `lib.rs:219` trust decision not saved).
- The linear (screen-reader) renderer labels a notice `notice:` (`linear.rs:259`).
- Theme: `fontaine-tui/src/theme.rs` has `warning`, `error`, `heading` slots but no info slot.
- Layout: `Screen::render` (`screen.rs:~165-190`) commits settled transcript lines to terminal
  scrollback via `insert_before` on every frame and sizes the inline live area to reach the
  bottom row. When the transcript is short, the committed lines sit at the top and the live area
  is mostly padding above the input (`view::draw_transcript`, `view.rs:390`, draws live lines
  from the top of the transcript area). That is the "anchored to the top" gap in the issue.

## Design

### Notice type (fontaine-core)

```text
pub enum Severity { Info, Warning, Error }
pub struct Notice { pub severity: Severity, pub headline: String, pub hint: Option<String> }
```

`Notice::info(text)`, `Notice::warning(headline, hint)` constructors; `Display`/`text()` joins
headline and hint with " — " for the plain surfaces (headless stderr, tests that match on
content). `Models::notices`, `AgentSetup.notices`, `AgentRuntime::notices()`, and the scanner
notices become `Vec<Notice>`. The guardian fallback becomes headline "Guardian is running on
your main model `kimi/kimi-for-coding`", hint "No benchmark-validated guardian model is
available on your accounts — set `[guardian] model` or run /guardian-model". The containment and
scanner notices keep their text as a headline (`Warning` for failures/fallbacks, `Info`
otherwise) with a hint only where the text already has a natural action clause.

`Approver::notice` stays `Info`, `Approver::warn` is `Warning`; `ChannelApprover` forwards the
severity: `Prompt::Notice(Severity, String)`. The trait signatures do not change (open/closed
for the other `Approver` implementors).

### Pre-TUI warnings

`prepare_session` stops printing the root warnings and returns them as `SessionPrep.warnings:
Vec<Notice>` (severity `Warning`). They flow into `AgentSetup` as a `startup_warnings` list kept
beside `scanner_notices` because, like the scanner's, they print in every mode.
`headless::announce` prints them first, in every mode, with the unchanged
`fontaine: warning: ...` text, so headless output does not change. The TUI shows them first in
the transcript, then the history/recent errors, notices and scanner notices in the existing
order. The same route carries the other non-fatal pre-TUI warnings that can reach the TUI (the
ignored screen-reader env value, the unsaved trust decision) when their emit site is before the
runtime exists, via a `Vec<Notice>` collected on `Launch`; any that cannot be carried stay on
stderr and are named in Decisions. Fatal errors keep `eprintln!` and exit.

### Rendering

- `Entry::Notice(String)` becomes `Entry::Notice { severity, headline, hint }`.
  `Transcript::notice(text)` keeps its signature (Info, no hint) so the ~25 existing call sites
  are untouched; `Transcript::warning(headline, hint)` and `Transcript::push_notice(Notice)` are
  new.
- `entry_lines` renders: Info = the existing dim `· ` style (unchanged look); Warning = `⚠ `
  in `theme.warning` with the headline in `theme.warning`; Error = `✗ ` in `theme.error`. The
  hint wraps on its own lines indented under the headline in the dim style. Spans inside
  backticks and `/command` tokens in the hint are bold, so the actionable part stands out
  independently of colour.
- Theme: no hard-coded colours. Warning and error reuse `theme.warning` / `theme.error`; no new
  slot is needed because Info keeps the dim style (decision below).
- Glyph fallback: a small `Glyphs` choice (`⚠`/`✗`/`·` vs `!`/`x`/`-`) selected once at startup by
  locale (non-UTF-8 `LC_ALL`/`LC_CTYPE`/`LANG` → ASCII) and stored on `App` next to the theme.
- Linear renderer: `notice:` for Info, `warning:` / `error:` labels for the others, headline
  and hint on separate labelled lines, so screen readers announce severity in words.

### Bottom-anchored transcript

Goal: with a short transcript, the last entry is on the row directly above the input box;
empty space is above the first entry.

Today every settled line is committed to scrollback immediately, so a short transcript is
written at the top and the live area below it is padding. Change: settled lines are committed
only when they no longer fit above the input.

- `Screen::render` computes `R`, the rows the transcript region has (terminal rows below the
  viewport top minus the rows the overlay, queue, todo panel, input box and status line need —
  `view::height` already computes `content - live` for these). `Transcript` gets a commit that
  takes this budget: whole settled entries are committed oldest-first only while the
  uncommitted rows (settled-but-held plus live) exceed `R`. A streaming reply keeps its
  row-at-a-time cut.
- Held-back settled entries are drawn as live lines, so `draw_transcript` bottom-aligns: when
  `lines.len() < area.height` it renders into the bottom `lines.len()` rows of the area.
- The viewport sizing (`wanted`/`free`) must stay consistent with what is held back, so the
  live area keeps the full height while the transcript is short instead of shrinking around
  committed lines.
- Exit: `clear_live` erases the live area, which now may hold uncommitted transcript.
  `TerminalGuard` therefore flushes everything still held back to scrollback (a
  `Screen::commit_all`) before `clear_live`, so the conversation stays in the terminal after
  quitting, as today.
- `/clear` empties the transcript and keeps scrollback as before; resizing recomputes `R` each
  frame, committing entries that stop fitting.
- The linear renderer has no live area and is unaffected.

## Implementation Steps

1. `fontaine-core`: add `Severity`/`Notice` (new `notice.rs`, exported from `lib.rs`); change
   `Models::notices`, `AgentSetup.notices`, `AgentRuntime::notices()`/`scanner_notices()` and the
   scanner stage notices (`scanner_stage.rs`) to `Notice`; add `startup_warnings`. Split the
   guardian-fallback text. Update the tests that match on notice text (`tests/models.rs`,
   `tests/scanner_model.rs`, `tests/scanner.rs`) to use `Notice` accessors.
2. `fontaine` crate: containment notice as `Notice`; `prepare_session` returns warnings in
   `SessionPrep.warnings`; collect the other non-fatal pre-TUI warnings on `Launch`; thread into
   `AgentSetup`; `headless::announce` prints `startup_warnings` first in every mode and
   `Display`s `Notice`s as before. Update `tests/scanner_headless.rs`, `parallel_agents.rs`,
   `telemetry_micromegas.rs` expectations only if text changes (it should not).
3. `fontaine-tui`: `Prompt::Notice(Severity, String)`; `ChannelApprover::{notice,warn}`; `Entry::Notice`
   variant, `Transcript::{warning,push_notice}`; `entry_lines` rendering with hint wrapping and
   bold actionable spans; `Glyphs` + locale detection; `lib.rs` startup loop pushes
   `startup_warnings`, notices and scanner notices as `Notice`s; `app.rs` test helper
   `last_notice` and `linear.rs` labels.
4. Bottom-anchoring: budgeted commit in `transcript.rs`, `Screen::render` sizing,
   `draw_transcript` bottom alignment, `Screen::commit_all` called from `TerminalGuard::drop`
   before `clear_live`.
5. Docs and changelog (see Documentation).

## Files to Modify

- `rust/fontaine-core/src/{lib.rs,models.rs,agent.rs,scanner_stage.rs}`, new `notice.rs`;
  tests under `rust/fontaine-core/tests/`
- `rust/fontaine/src/{lib.rs,headless.rs}`
- `rust/fontaine-tui/src/{lib.rs,transcript.rs,view.rs,screen.rs,linear.rs,app.rs,theme.rs
  (only if a slot proves needed),ui/mod.rs}`; `tests/tui.rs`
- `docs/tui.md`, `docs/terminal.md`, `docs/headless.md`, `CHANGELOG.md`

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
- 2026-10-25 AFK: ASCII glyph fallback is selected by locale rather than a new config key, to
  avoid a settings surface for a cosmetic switch.

## Documentation

- `docs/tui.md`: notice severities, warning glyph/colour, bottom-anchored transcript.
- `docs/terminal.md`: ASCII glyph fallback and its locale rule.
- `docs/headless.md`: confirm stderr output is unchanged and now includes the config-root
  warnings through the same path.
- `CHANGELOG.md` under Unreleased.

## Testing Strategy

All unit/`TestBackend` screen tests; nothing needs a live service.

- `Notice`: constructors, `Display` joining, guardian fallback headline/hint content
  (update `tests/models.rs`).
- Transcript rendering (`transcript.rs` tests): warning entry has `⚠ ` prefix and
  `theme.warning` colour, distinct from Info; hint wraps under the headline at narrow widths;
  backticked and `/command` spans are bold; ASCII glyph fallback; each preset theme draws the
  warning in its own `warning` colour.
- Linear renderer: `warning:` / `error:` labels.
- Bottom anchoring (`screen.rs` tests, the issue's acceptance): a transcript shorter than the
  viewport renders with its last line on the row immediately above the input box and blank rows
  above the first entry; adding entries until it overflows commits the oldest to scrollback with
  nothing lost or duplicated; a streaming reply across the boundary; resize; `commit_all`
  followed by `clear_live` leaves the full transcript in scrollback.
- Pre-TUI warnings: a `tests/tui.rs` (or `ui/tests.rs`) test that root warnings handed to the
  startup path appear as warning entries; a headless test that they still go to stderr in every
  mode.
- Regression: every existing `screen.rs` scrollback test stays green, which pins the unchanged
  overflow behaviour.

## Open Questions

None.
