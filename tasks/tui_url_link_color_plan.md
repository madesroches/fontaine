# TUI: Style URLs in a Distinct Color Plan

**GitHub Issue**: https://github.com/madesroches/fontaine/issues/206

## Overview
URLs in the conversation render in the default text color and vanish into a wall of text. Claude
Code draws links in a distinct color. Add a `link` token to `Theme`, and style bare `http(s)://`
URLs and markdown link text in assistant replies (including the streaming plain tail) and tool
output with it. Styling is span-level only; no escape sequences are injected into text, so
copy/yank stays plain.

## Current State
- `rust/fontaine-tui/src/theme.rs`: `Theme` has colour tokens per meaning (`user`, `success`, `error`,
  ...) with six presets in `Theme::preset`. No link token.
- `rust/fontaine-tui/src/markdown.rs`: `Inline::feed` treats `Tag::Link` as a no-op modifier
  (`Modifier::empty()`), so link text renders as plain text and the target is dropped. Bare URLs are
  `Event::Text`; pulldown-cmark does not autolink them. `<https://x>` autolinks do arrive as
  `Tag::Link`. `markdown::rows` takes `glyphs` but no theme; callers are `transcript.rs`
  (`stable_rows`, `assistant_rows`, `entry_lines`).
- `transcript.rs` `plain_rows` renders the unstable streaming tail as unstyled wrapped text;
  `tool_lines` renders tool output rows.
- Wrapping (`wrap_spans`) is per-char and keeps each char's style, so a style applied to a span
  survives wraps and hard splits of long URLs.

## Design
1. **Theme token.** Add `pub link: Color` to `Theme`, documented "URLs and markdown link text".
   Values per preset, matching Claude Code's blue-ish link convention while staying distinct from
   `user` (cyan/blue) where practical:
   - dark-ansi: `Color::LightBlue`; light-ansi: `Color::Blue`
   - dark: `rgb(120, 170, 255)`; light: `rgb(0, 70, 190)`
   - dark-daltonized: `rgb(102, 178, 255)`; light-daltonized: `rgb(0, 70, 190)` with daltonized
     presets staying clear of the orange error hue. Existing `every_preset_is_distinct` stays green.
   The link is also underlined (`Modifier::UNDERLINED`) so it does not rely on colour alone.
2. **Plumb the colour.** `markdown::rows` and its helpers take a `link: Style` (a small
   `Styles`-like parameter is not warranted; one `Style` param). `transcript.rs` passes
   `Style::default().fg(theme.link).add_modifier(UNDERLINED)`. `stable_rows`, `assistant_rows`,
   `plain_rows` and the streaming `Cut` helpers gain the same parameter; where only `glyphs` is held
   today (`Cut::rows`/`end`), thread the style next to it.
3. **Markdown links.** In `Inline`, `Tag::Link` pushes a link flag; text and code emitted inside a
   link get the link style. The target is not printed (unchanged text rendering: "links render as
   their text"), so copied text is unchanged.
4. **Bare URLs.** A single function `link_spans(text, style, link) -> Vec<Span>` in `markdown.rs`
   splits a string into plain and URL spans. A URL starts at `http://` or `https://` at a word
   boundary and runs to the next whitespace, then trailing sentence punctuation `.,;:!?` and an
   unbalanced closing `)`/`]`/`'`/`"` is trimmed back out. No regex (project rule: no regex on tool
   arguments; this is display text, but a hand scanner keeps the dependency set unchanged and is
   trivially testable). `Inline::text` calls it for `Event::Text` outside a link and outside code
   spans; inline code and fenced code blocks are not linkified.
5. **Streaming tail.** `plain_rows` runs each wrapped piece through `link_spans`. A URL split by a
   wrap boundary is detected per row (a continuation row has no `http` prefix), so a long URL that
   wraps loses colour on its continuation rows in the unstable tail only; once the block stabilises
   it is re-rendered as markdown where `wrap_spans` keeps per-char style. Accepted.
6. **Tool output.** `tool_lines` output rows pass through `link_spans` with the same style, applied
   after wrapping per row, with the same wrap-boundary caveat.
7. **Copy.** Spans carry only fg/underline; nothing is added to content, so clipboard/yank is
   unchanged. Covered by a test that `plain()` of rows equals the source text.

## Implementation Steps
1. `theme.rs`: add `link` to `Theme` and all six presets; test it is set per preset.
2. `markdown.rs`: add `link_spans`, a link flag in `Inline`, a `link: Style` parameter through
   `rows`, `render_region`, `render_block`, `Inline::rows`/`flat`, list/table helpers.
3. `transcript.rs`: compute the link style from `theme` and pass it through the assistant, stable,
   streaming and tool-output paths; linkify `plain_rows` and tool output rows.
4. Update call sites in tests and other callers of `markdown::rows`.
5. Docs.

## Files to Modify
- `rust/fontaine-tui/src/theme.rs`
- `rust/fontaine-tui/src/markdown.rs`
- `rust/fontaine-tui/src/transcript.rs`
- `docs/terminal.md` (colour themes), `docs/tui.md` (transcript rendering), `CHANGELOG.md`

## Trade-offs
- OSC 8 hyperlinks (clickable) were considered; they inject escape sequences, which the issue
  asks to avoid in clipboard and which ratatui buffers handle poorly. Out of scope.
- Using pulldown-cmark's GFM-style autolink extension is unavailable for bare URLs, hence the
  small scanner.

## Decisions
(none yet)

## Documentation
- `docs/terminal.md`: mention the link colour among the theme tokens/colours.
- `docs/tui.md`: note URLs are coloured and underlined in replies and tool output.
- `CHANGELOG.md`: entry.

## Testing Strategy
Unit tests, no live APIs:
- `theme.rs`: every preset sets a `link` colour different from `Color::Reset`; presets stay distinct.
- `markdown.rs`: `link_spans` — bare URL, URL with trailing `.`/`,`/`)`, URL inside parentheses,
  `http://` alone, no scheme, two URLs in a line, URL in inline code (not styled); markdown link text
  styled, target not printed; autolink `<https://x>`; plain text of rows equals source for each.
- Wrapping: a URL longer than the width hard-split across rows keeps the link style on every
  row in markdown rendering.
- `transcript.rs`: assistant reply, streaming tail and tool output rows carry `theme.link` fg on the
  URL span only, and a custom theme's link colour is honoured.

## Open Questions
- Should the link colour be user-overridable in config? Assumed no: themes are presets only today.
