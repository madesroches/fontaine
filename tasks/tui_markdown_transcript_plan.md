# TUI Markdown Transcript Rendering Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/328

## Overview

The TUI prints assistant replies as plain wrapped text, so model markdown shows as raw `**`, backticks
and pipe tables (a four-column table with long cells collapses into a wall of pipes). Claude Code renders
markdown in the transcript. This adds a bounded markdown renderer for `Entry::Assistant`: inline
bold/italic/code, headings, bullet/numbered lists, fenced code blocks and aligned, wrapped tables.
Everything it does not understand renders as the source text it is today. The stored text is never
changed, so `/copy`, `/export` (which builds from the core `Message::Assistant`, not `Entry.text`) and the plain
line-mode renderer give exact source.

## Current State

- `rust/fontaine-tui/src/transcript.rs`: `Entry::Assistant { text, streaming }`. `assistant_rows`
  (line ~681) wraps source lines with `input::wrap` and returns `(Line, char_offset_past_row)`.
  `entry_lines` (~702) renders a reply (settled or streaming) from a `skip` char offset.
- Streaming: `Transcript` tracks `committed_chars` into `entries[committed]`. `Cut::{Rows, Budget, Lines}`
  (~575) decide how far a streaming reply is written to scrollback; `Rows`/`Budget` use
  `assistant_rows` and always keep the last row live because it may still grow. `commit_within`
  (~520) force-releases a streaming reply that would keep the live area over budget. Offsets are what
  survive a width change (`a_width_change_mid_stream_drops_and_repeats_nothing`).
- `Cut::Lines` is used only by `rust/fontaine-tui/src/linear.rs:406` (line mode, no styling; the
  terminal wraps). It must stay raw.
- `/copy` (`ui/commands/reports.rs:84`, `assistant_reply`) reads `Entry.text`; `/export` builds from core
  `Message::Assistant` (`fontaine-core/src/export.rs`).
- Styling precedent: `hint_spans`/`warning_lines` (~928-1005) pair backticks for bold. `Theme`
  (`theme.rs`) holds colour tokens; call sites add `BOLD`/`DIM`. `Glyphs` (`theme.rs`) has Unicode and
  ASCII sets selected by `FONTAINE_ASCII_GLYPHS`.
- `pulldown-cmark` is not a dependency yet (`fontaine-core`'s instructions markdown handling does not use it).

## Design

### New module `fontaine-tui/src/markdown.rs`

Pure functions, no `Transcript` knowledge:

```rust
/// Rendered rows of `text`, each with the source char offset (relative to `text`) just past it.
pub(crate) fn rows(text: &str, width: usize, glyphs: Glyphs) -> Vec<(Line<'static>, usize)>;
/// Char offset up to which a streaming `text` can be rendered as markdown without a later
/// delta changing what was rendered. The rest is the plain tail.
pub(crate) fn stable_end(text: &str) -> usize;
```

**Parsing.** `pulldown-cmark` (pinned exactly like the rig crates) with default features off;
`Options::ENABLE_TABLES` is set at runtime. `into_offset_iter` gives source ranges, so
each top-level block knows the source char range it came from. Supported blocks: paragraph, heading,
fenced code, bullet/ordered list (nested by indent), table. Supported inline: strong, emphasis, code,
soft/hard break; link text renders as its text. **Soundness rule:** any block kind not in that list
(blockquote, HTML, rule, indented code, footnotes, images) renders as its source lines wrapped plain,
exactly as today. A list containing any unsupported nested block (blockquote, code block, table, HTML)
falls back as a whole: the entire top-level list renders as its source lines wrapped plain.

`into_offset_iter` yields byte ranges; they are converted to char offsets before use.

**Styling** uses modifiers only, no new theme tokens (works in every theme): strong `BOLD`, emphasis
`ITALIC`, inline code `BOLD` (the existing precedent for backtick spans), heading `BOLD` (h1 also
`UNDERLINED`), fenced code lines `DIM` with a two-space indent, lists with `• `/`1. ` markers (a new `bullet`
glyph: `• `, ASCII `- `), continuation lines indented under the text.

**Wrapping styled text.** `wrap_spans(Vec<Span>, width) -> Vec<Vec<Span>>` word-wraps by char count and
hard-splits over-long words by chars like `input::wrap`, keeping each span's style across rows. Wide
(CJK) glyphs count as one char, so such tables misalign; that is accepted. Used by paragraphs, list items, headings and table cells.

**Tables.** Column natural width = widest cell. If the sum plus separators exceeds `width`, shrink the
widest columns first down to a floor of 4 columns, wrapping cell text inside its column. If the floors
do not fit, the table renders as raw wrapped source lines. Header is bold, followed by a rule row; column
alignment from the separator row (`:--`, `--:`, `:-:`). Separator glyphs come from new `Glyphs`
fields (`col_sep`, `rule`: `│`/`─` and ASCII `|`/`-`).

### Row to source-offset mapping

Rendered text differs from source (markers vanish, columns re-flow), so offsets inside a rendered block
are not exact. The rule that keeps scrollback correct:

- **A block's rows are released atomically.** Every row of a block carries the block's end offset (past
  its trailing blank lines). The commit cursor therefore only ever stops on a block boundary, and a
  resume from `committed_chars` re-parses from a block start. Width changes just re-layout from there.
  The budget path (`Cut::Budget` / `commit_within`) cuts at the Nth row as today, then extends the cut
  to the end of the block containing that row, overshooting the budget as entries already do. Fenced-code
  lines are cut per row; tables, paragraphs and lists are cut as a whole block.
- **Fenced code lines are row-exact**, like plain text, so an open fence is renderable (and committable)
  line by line without waiting for its closing fence.
- **The plain tail keeps today's exact offsets.**

### Streaming

`stable_end(text)` is the end of the last blank line outside a fenced block (a closing fence must match
the opener's character and be at least as long), plus, when inside an open fence, the end of its last
complete line. A table ends at a blank line, so a half-arrived table row is always in the tail. A
non-streaming entry (`streaming: false`) is all stable.

`assistant_rows(text, from, width, streaming, glyphs)` gains `streaming: bool` (`entry_lines` takes it from the entry,
so `take_retired` and `commit_all` get `true` for an entry still streaming at `/clear`; `live_lines` goes
through `entry_lines`): rows = `markdown::rows` of the stable prefix after `from`, then plain rows (today's
code) of the tail. The live area shows all of them. The ordinary commit path (`Transcript::commit()`,
`Cut::Rows`) releases only the stable-prefix rows (block rows and row-exact fence lines) and holds the
plain tail back entirely: it stays in the live area, never committed. The old "keep the last row live"
pop no longer applies; a stable row never grows, so nothing is popped from the prefix. The cursor
therefore only stops on a block boundary or a complete fence line, and a paragraph shows raw until it
ends. Because the stable prefix is re-rendered per frame, no cache is kept.

One function returns the releasable rows for the Rows path (the stable-prefix rows, without the tail);
`commit()`, `Cut::rows` and `Cut::end` all call it, so the three sites cannot diverge.

Only the over-budget release (`Cut::Budget` / `commit_within`) may commit plain tail rows (exact
offsets), as it does today. The remainder of that paragraph is then rendered as markdown from the cut, so
a span straddling the cut shows its markers literally. This is the one visible degradation, and only
for a single paragraph taller than the live area.

Line mode (`Cut::Lines`, `linear.rs`) is untouched: it prints source lines.

## Implementation Steps

1. Add `pulldown-cmark = { version = "=0.13.6", default-features = false }` to `rust/Cargo.toml`
   `[workspace.dependencies]` and to `fontaine-tui`; `cargo deny check` (from `rust/`, where `deny.toml` lives).
2. `theme.rs`: add `bullet`/`col_sep`/`rule` (three new fields) to `Glyphs` (UNICODE and ASCII sets).
3. `markdown.rs`: `wrap_spans`, inline flattening, blocks other than tables, `stable_end`, `rows`, with
   the unsupported-block fallback.
4. `markdown.rs`: tables (widths, shrink, wrap, alignment, raw fallback).
5. `transcript.rs`: thread `streaming` and `glyphs` as parameters through `assistant_rows`, `Cut::rows` and
   `Cut::end`, updating their call sites and the existing direct test call (~1432). `entry_lines` takes
   `streaming` from the entry; `live_lines` is unchanged (it calls `entry_lines`). `commit()` (which
   calls `assistant_rows` directly), `Cut::rows` and `Cut::end` share the single Rows-path release
   function (stable-prefix rows only, tail held back, no pop); whole-block extension of the cut in
   `Cut::Budget`/`commit_within`.
6. `examples/live_area.rs`: `main()` has no argument parsing and runs one fixed script, so append markdown
   content (a streamed reply with a table and a fence) to that existing streamed script for the tmux check.
7. Docs, CHANGELOG, plan Decisions.

## Files to Modify

- `rust/Cargo.toml`, `rust/fontaine-tui/Cargo.toml`
- `rust/fontaine-tui/src/markdown.rs` (new), `lib.rs` (module), `transcript.rs`, `theme.rs`
- `rust/fontaine-tui/examples/live_area.rs`
- `docs/tui.md`, `CHANGELOG.md`, `tasks/fontaine_harness_plan.md` (Decisions, TUI)

## Trade-offs

- **Atomic block release vs exact mid-block offsets:** exact offsets cannot exist for re-flowed text;
  atomic release makes the cursor a block boundary at the cost of a block possibly exceeding a row
  budget (the existing "entries commit whole" precedent).

## Documentation

- `docs/tui.md`: new "Markdown in replies" section (supported subset, fallback, that `/copy`, `/export`
  and line mode give source). Insert it before "## Messages during a turn".
- `CHANGELOG.md` entry under `#### TUI` in `[Unreleased]` / Added; one dated line at the end of the
  master plan's `## Decisions` / `### TUI` subsection noting the markdown subset and that line mode stays
  raw, ending with `(#328)`.

## Testing Strategy

Unit tests in `markdown.rs` and `transcript.rs` (no live APIs):

- Inline: bold/italic/code/nested, unmatched markers stay literal, spans keep style across a wrapped row.
- Blocks: headings, bullet and numbered lists with wrapped continuation, fenced code (verbatim, indented,
  long line wraps), each unsupported block kind falls back to its source text; a list containing a nested blockquote, code
  block, table or HTML falls back as a whole top-level list.
- Tables: fits as natural widths; shrinks and wraps long cells; alignment markers; floors do not fit →
  raw fallback; ASCII glyph set; cells with inline markup.
- `stable_end`: blank line, fence open/closed/mismatched closers, blank line inside a fence, half-arrived
  table row, whole text when not streaming.
- Streaming: extend `streamed_commits_equal_the_whole_transcript` with a markdown sample over many chunk
  sizes and widths (commits through `Cut::Rows` equal the one-shot render); the width-change test with a
  table mid-stream drops and repeats nothing; a stable-prefix commit never pops a block row; the
  over-budget release of tail and fence rows advances by exactly the rows it emits; a tall block under a
  small budget is emitted whole, none lost.

## Manual Verification

Layout bugs in live area/scrollback must be reproduced in real tmux (`tasks/design/testing.md`,
"Reproducing terminal layout bugs"; the vt100 model cannot see how the terminal treats erases):

1. `cargo build -p fontaine-tui --example live_area`, then run it in a detached tmux as in that
   section and the appended markdown content at `-x 80 -y 12` and again at `-x 50 -y 12`.
2. `tmux capture-pane -p -S - -E -`: each table row, heading and code line appears once in history, in
   order, and no raw `**` or pipe-table source remains for supported constructs.

Not automated because the behavior under test is the terminal's handling of the escape stream.
