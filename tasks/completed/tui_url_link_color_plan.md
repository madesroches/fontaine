# TUI: Style URLs in a Distinct Color Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/206

## Overview
URLs in the conversation render in the default text color and vanish into a wall of text. Claude
Code draws links in a distinct color. Add a `link` token to `Theme`, and style bare `http(s)://`
URLs and markdown link text in assistant replies (including the streaming plain tail) and tool
output (including `Entry::Shell` output rows) with it. Styling is span-level only; no escape
sequences are injected into text. Blocks that fall back to raw source rows (blockquotes, HTML blocks,
indented code, too-narrow tables, lists containing those) are linkified per row. Errors, notices,
call rows and reasoning are deliberately not linkified.

## Current State
- `rust/fontaine-tui/src/theme.rs`: `Theme` has colour tokens per meaning (`user`, `success`, `error`,
  ...) with six presets in `Theme::preset`. No link token.
- `rust/fontaine-tui/src/markdown.rs`: `Inline::feed` treats `Tag::Link` as a no-op modifier
  (`Modifier::empty()`), so link text renders as plain text and the target is dropped. Bare URLs are
  `Event::Text`; pulldown-cmark does not autolink them. `<https://x>` autolinks do arrive as
  `Tag::Link`. `markdown::rows` takes `glyphs` but no theme; callers are `transcript.rs`
  (`stable_rows`, `assistant_rows`).
- `transcript.rs` `plain_rows` renders the unstable streaming tail as unstyled wrapped text;
  `tool_lines` renders tool output rows.
- Wrapping (`wrap_spans`) is per-char and keeps each char's style, so a style applied to a span
  survives wraps and hard splits of long URLs.

## Design
1. **Theme token.** Add `pub link: Color` to `Theme`, documented "URLs and markdown link text".
   Values per preset, matching Claude Code's blue-ish link convention. The link shares the blue family with
   `user` and sits close to `user`/`success` in hue, which is accepted: user rows are never linkified and the underline, not hue, sets links apart:
   - dark-ansi: `Color::LightBlue`; light-ansi: `Color::Blue`
   - dark: `rgb(120, 170, 255)`; light: `rgb(0, 70, 190)`
   - dark-daltonized: `rgb(102, 178, 255)`; light-daltonized: `rgb(0, 70, 190)` with daltonized
     presets staying clear of the orange error hue.
   The link is also underlined (`Modifier::UNDERLINED`) so it does not rely on colour alone.
2. **Plumb the colour.** `markdown::rows` and its helpers take a `link: Style` (one `Style` param).
   It lives in `Inline` as a `link: Style` field set where `Inline::default()` is built (markdown.rs
   ~464, 469, 718, 780) and folded into `style()`, so text and code inside a link get it. It is passed
   through `rows`, `render_region`, `render_block`, `list_rows`, `item_rows` and `table_rows`.
   `transcript.rs` passes `Style::default().fg(theme.link).add_modifier(UNDERLINED)`. `stable_rows`,
   `assistant_rows` and `plain_rows` gain the same parameter. Only `Cut::rows` (reached from
   `commit_within`, where a theme exists) receives the real style; `Cut::end` calls the row helpers
   with `Style::default()`, so `commit_entries` and `linear.rs` stay unchanged.
3. **Markdown links.** In `Inline`, `Tag::Link` pushes a link flag; text and code emitted inside a
   link get the link style. The target is not printed (unchanged text rendering: "links render as
   their text"), so copied text is unchanged.
4. **Bare URLs.** A single function `link_spans(text, style, link) -> Vec<Span>` in `markdown.rs`
   splits a string into plain and URL spans. A URL starts at `http://` or `https://` at a word
   boundary and runs to the next whitespace, then trailing sentence punctuation `.,;:!?` and an
   unbalanced closing `)`/`]`/`'`/`"` is trimmed back out. `Inline::text` calls it for `Event::Text`
   outside a link and outside code spans; the `Text` arm is split off from `Html | InlineHtml`, and
   HTML is not linkified; inline code and fenced code blocks are not linkified. pulldown-cmark 0.13.6
   splits text nodes at special bytes (unmatched `[` `]` `*` `!`, `_` next to punctuation, escapes,
   entities), so `parse_blocks` wraps the offset iterator in `pulldown_cmark::TextMergeWithOffset`
   and `link_spans` sees merged text. A matched emphasis pair inside a URL (`/_a_/`) still splits it.
   Blocks that fall back to raw source (`raw_rows`/`uncovered` and the
   `render_block(..).unwrap_or_else(raw_rows)` fallback) run `link_spans` over each raw row, with the
   same wrap-boundary caveat as the streaming tail.
5. **Streaming tail.** `plain_rows` runs each wrapped piece through `link_spans`. A URL split by a
   wrap boundary is detected per row (a continuation row has no `http` prefix), so a long URL that
   wraps loses colour on its continuation rows while it streams. Once the block settles the tail is
   re-rendered as markdown, where `wrap_spans` keeps per-char style, except rows already committed
   to scrollback under the row budget (`commit_within`/`Cut::Budget`), which are never re-rendered
   and keep their uncoloured continuation rows. Accepted.
6. **Tool output.** `tool_lines` output rows and `Entry::Shell` output rows (transcript.rs
   ~1016-1025) pass through `link_spans` with the same style, applied after wrapping per row, with
   the same wrap-boundary caveat. The link style is patched onto the row style, so URLs in tool
   output keep DIM and take the link colour and underline.
7. **Copy.** Spans carry only fg/underline; nothing is added to content. `/copy` uses the raw
   assistant reply source text (`ui/commands/reports.rs:121-136`), so it is unaffected. A test
   asserts the plain text of rows is identical with and without the link style, which protects mouse
   selection in scrollback.

## Implementation Steps
1. `theme.rs`: add `link` to `Theme` and all six presets.
2. `markdown.rs`: add `link_spans`, a `link: Style` field in `Inline` (folded into `style()`), a
   `link: Style` parameter through `rows`, `render_region`, `render_block`, `list_rows`, `item_rows`,
   `table_rows`; `TextMergeWithOffset` in `parse_blocks`; linkify `raw_rows` rows.
3. `transcript.rs`: compute the link style from `theme` and pass it through the assistant, stable,
   streaming and tool-output paths; linkify `plain_rows`, tool output rows and `Entry::Shell` rows.
4. Update call sites in tests and other callers of `markdown::rows`.
5. Docs.
6. `tasks/fontaine_harness_plan.md`: add the Decisions lines below (dated 2026-11-03) at the end of
   the `### TUI` subsection.

## Files to Modify
- `rust/fontaine-tui/src/theme.rs`
- `rust/fontaine-tui/src/markdown.rs`
- `rust/fontaine-tui/src/transcript.rs`
- `docs/terminal.md` (colour themes), `docs/tui.md` (transcript rendering), `CHANGELOG.md`

## Trade-offs
- OSC 8 hyperlinks (clickable) were considered; not needed for the issue and deferred.

## Decisions
- 2026-11-03: hand-written URL scanner keeps `fontaine-tui` free of a `regex` dependency.
- 2026-11-03: bare URLs are scanned because pulldown-cmark does not autolink them.
- 2026-11-03: no OSC 8 hyperlinks (Claude Code 2.1.318 emits OSC 8 around the visible text); links are coloured whether or not the terminal supports hyperlinks.
- 2026-11-03: links are underlined in addition to coloured (Claude Code: blue on light / blueBright on dark, no underline), so they do not rely on colour alone.
- 2026-11-03: link colour is not user-overridable; `ThemeName` is a closed set of six presets and custom themes are a separate issue.
- AFK: 2026-11-03: link colours stay in the blue family and may sit close to `user`/`success`/`mode_plan` (as Claude Code's blue/blueBright does); the underline, not hue, sets links apart, so no RGB changes and no "distinct from user" claim.

## Documentation
- `docs/terminal.md` `## Colours`: add a sentence that URLs and link text use an underlined blue in
  every theme.
- `docs/tui.md` (~line 370, links render "as their text"): say links are coloured and underlined, and
  bare URLs too.
- `docs/tui.md` (~line 37, tool output "dim behind a `│` gutter"): say URLs in tool and `!` shell
  output are drawn in the link colour, underlined.
- `CHANGELOG.md`: entry under Unreleased > Added > TUI.

## Testing Strategy
Unit tests, no live APIs:
- `markdown.rs`: `link_spans` — bare URL, URL with trailing `.`/`,`/`)`, URL inside parentheses,
  `http://` alone, no scheme, two URLs in a line, URL in inline code (not styled); markdown link text
  styled, target not printed; autolink `<https://x>`; plain text of rows is identical with and without
  the link style for each; `https://en.wikipedia.org/wiki/Foo_(bar)` and `https://x.com/_next/a`
  are styled whole despite split text events; a URL inside a blockquote (raw-row fallback) is styled.
- Wrapping: a URL longer than the width hard-split across rows keeps the link style on every
  row in markdown rendering.
- `transcript.rs`: assistant reply, streaming tail and tool output rows carry `theme.link` fg on the
  URL span only, and a custom theme's link colour is honoured.
