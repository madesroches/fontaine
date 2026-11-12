//! Markdown in assistant replies: a bounded subset rendered as styled rows. Anything outside
//! the subset renders as its source lines, wrapped plain, exactly as an unrendered reply does,
//! so the renderer can only ever add formatting, never lose text.

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd, TextMergeWithOffset,
};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::hyperlink::{self, Kinds, Ref};
use crate::theme::Glyphs;

type Row = Line<'static>;
type Cells = Vec<Span<'static>>;

/// Narrowest a table column is shrunk to.
const COLUMN_FLOOR: usize = 4;
/// Joins the cells of a table row: the column glyph with a space on each side.
const COLUMN_GAP: usize = 3;

#[derive(Clone, Copy)]
struct Fence {
    ch: char,
    len: usize,
}

/// One source line, with where it starts.
struct SrcLine<'a> {
    text: &'a str,
    byte: usize,
    chars: usize,
    /// A line that has not received its `\n` yet may still grow.
    terminated: bool,
}

fn split_lines(text: &str) -> Vec<SrcLine<'_>> {
    let mut out = Vec::new();
    let (mut byte, mut chars) = (0, 0);
    for piece in text.split_inclusive('\n') {
        out.push(SrcLine {
            text: piece.strip_suffix('\n').unwrap_or(piece),
            byte,
            chars,
            terminated: piece.ends_with('\n'),
        });
        byte += piece.len();
        chars += piece.chars().count();
    }
    out
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Leading columns of `line` and the bytes they take (tabs count as four columns).
fn indent(line: &str) -> (usize, usize) {
    let (mut cols, mut bytes) = (0, 0);
    for c in line.chars() {
        match c {
            ' ' => cols += 1,
            '\t' => cols += 4,
            _ => break,
        }
        bytes += 1;
    }
    (cols, bytes)
}

fn fence_open(line: &str) -> Option<Fence> {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return None;
    }
    let rest = &line[bytes..];
    let ch = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    // A backtick run with a backtick after it is inline code, not a fence.
    if len < 3 || (ch == '`' && rest[len..].contains('`')) {
        return None;
    }
    Some(Fence { ch, len })
}

fn fence_closes(line: &str, fence: Fence) -> bool {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return false;
    }
    let rest = &line[bytes..];
    let run = rest.chars().take_while(|c| *c == fence.ch).count();
    run >= fence.len && is_blank(&rest[run..])
}

fn list_marker(line: &str) -> bool {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return false;
    }
    let rest = &line[bytes..];
    let after = |at: usize| matches!(rest[at..].chars().next(), None | Some(' ' | '\t'));
    match rest.chars().next() {
        Some('-' | '*' | '+') => after(1),
        Some(c) if c.is_ascii_digit() => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            matches!(rest[digits..].chars().next(), Some('.' | ')')) && after(digits + 1)
        }
        _ => false,
    }
}

enum Next {
    Boundary,
    NotBoundary,
    Unknown,
}

/// Whether a block can end before `line`, the first non-blank line after a blank one.
fn classify(line: &SrcLine<'_>) -> Next {
    let text = line.text;
    if is_blank(text) {
        return Next::Unknown;
    }
    if text.starts_with([' ', '\t']) {
        return Next::NotBoundary;
    }
    let unfinished = |known: bool| match (known, line.terminated) {
        (true, _) => Next::NotBoundary,
        (false, true) => Next::Boundary,
        (false, false) => Next::Unknown,
    };
    let Some(first) = text.chars().next() else {
        return Next::Unknown;
    };
    match first {
        '-' | '*' | '+' => match text[1..].chars().next() {
            Some(' ' | '\t') => Next::NotBoundary,
            Some(_) => Next::Boundary,
            None if line.terminated => Next::NotBoundary,
            None => Next::Unknown,
        },
        c if c.is_ascii_digit() => {
            let digits = text.chars().take_while(char::is_ascii_digit).count();
            match text[digits..].chars().next() {
                Some('.' | ')') => match text[digits + 1..].chars().next() {
                    Some(' ' | '\t') => Next::NotBoundary,
                    Some(_) => Next::Boundary,
                    None if line.terminated => Next::NotBoundary,
                    None => Next::Unknown,
                },
                Some(_) => Next::Boundary,
                None => unfinished(false),
            }
        }
        _ => Next::Boundary,
    }
}

/// Char offset up to which a streaming `text` can be rendered as markdown without a later
/// delta changing what was rendered. The rest is the plain tail.
pub(crate) fn stable_end(text: &str) -> usize {
    let lines = split_lines(text);
    // The flag records whether the fence opened outside a list item, where its close is a
    // block boundary.
    let mut fence: Option<(Fence, bool)> = None;
    let mut in_list = false;
    let mut boundary = 0;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if let Some((open, top)) = fence {
            if line.terminated && fence_closes(line.text, open) {
                fence = None;
                if top {
                    boundary = line.chars + line.text.chars().count() + 1;
                    in_list = false;
                }
            }
            i += 1;
            continue;
        }
        if !line.terminated {
            break;
        }
        if is_blank(line.text) {
            let mut next = i + 1;
            while next < lines.len() && lines[next].terminated && is_blank(lines[next].text) {
                next += 1;
            }
            let Some(after) = lines.get(next) else { break };
            match classify(after) {
                // An indented or list-marker line continues a list, but starts a new block
                // after anything else.
                Next::Boundary => {
                    boundary = after.chars;
                    in_list = false;
                }
                Next::NotBoundary if !in_list => boundary = after.chars,
                Next::NotBoundary => {}
                Next::Unknown => break,
            }
            i = next;
            continue;
        }
        if let Some(open) = fence_open(line.text) {
            fence = Some((open, !in_list));
        } else if list_marker(line.text) {
            in_list = true;
        }
        i += 1;
    }
    boundary
}

/// The fence that is still open where `from` falls, found by scanning the complete lines
/// before it.
fn open_fence_at(text: &str, from: usize) -> Option<Fence> {
    let head: String = text.chars().take(from).collect();
    let complete = head.rfind('\n').map_or(0, |i| i + 1);
    let mut open: Option<Fence> = None;
    // A fence inside a list item is not resumed as a top-level code block; its remainder
    // renders as plain source.
    let mut in_list = false;
    for line in split_lines(&head[..complete]) {
        open = match open {
            Some(fence) if fence_closes(line.text, fence) => None,
            Some(fence) => Some(fence),
            None => {
                if is_blank(line.text) {
                    None
                } else if !line.text.starts_with(' ') && !list_marker(line.text) {
                    in_list = false;
                    fence_open(line.text)
                } else if list_marker(line.text) {
                    in_list = true;
                    None
                } else if in_list {
                    None
                } else {
                    fence_open(line.text)
                }
            }
        };
    }
    open
}

/// Rendered rows of the chars `from..until` of `text`, each with the char offset (in `text`)
/// just past the block it belongs to, including the blank lines after it. A block's rows share
/// that offset, so a cursor that follows the offsets only ever stops between blocks.
pub(crate) fn rows(
    text: &str,
    from: usize,
    until: usize,
    width: usize,
    glyphs: Glyphs,
    link: Style,
) -> Vec<(Row, usize)> {
    let width = width.max(1);
    let mut out = Vec::new();
    let region: String = text
        .chars()
        .skip(from)
        .take(until.saturating_sub(from))
        .collect();
    let mut rest = region.as_str();
    let mut base = from;
    if let Some(fence) = open_fence_at(text, from) {
        let used = resume_fence(
            rest,
            base,
            from > 0 && !starts_line(text, from),
            fence,
            width,
            link,
        );
        out.extend(used.rows);
        rest = &rest[used.bytes..];
        base += used.chars;
    }
    render_region(rest, base, width, glyphs, link, &mut out);
    out
}

fn starts_line(text: &str, at: usize) -> bool {
    at == 0 || text.chars().nth(at - 1) == Some('\n')
}

struct Resumed {
    rows: Vec<(Row, usize)>,
    bytes: usize,
    chars: usize,
}

/// The rest of a fence that began before `base`: its code lines up to the closing fence, and
/// the blank lines after it.
fn resume_fence(
    rest: &str,
    base: usize,
    mid_line: bool,
    fence: Fence,
    width: usize,
    link: Style,
) -> Resumed {
    let mut code = Vec::new();
    let mut bytes = 0;
    let mut closed = false;
    for (i, piece) in rest.split_inclusive('\n').enumerate() {
        let line = piece.strip_suffix('\n').unwrap_or(piece);
        bytes += piece.len();
        if !(mid_line && i == 0) && piece.ends_with('\n') && fence_closes(line, fence) {
            closed = true;
            break;
        }
        code.extend(code_rows(line, width, link));
    }
    if closed {
        for piece in rest[bytes..].split_inclusive('\n') {
            if !piece.ends_with('\n') || !is_blank(piece) {
                break;
            }
            bytes += piece.len();
            code.push(Line::default());
        }
    }
    let chars = rest[..bytes].chars().count();
    Resumed {
        rows: code.into_iter().map(|row| (row, base + chars)).collect(),
        bytes,
        chars,
    }
}

struct Block<'a> {
    start: usize,
    end: usize,
    events: Vec<Event<'a>>,
}

/// A fresh parser per region, so a link reference definition does not resolve across streamed
/// blocks.
fn parse_blocks(src: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<Block<'_>> = None;
    // Without merging, the parser splits a text node at every special byte, which would cut
    // a URL such as `/_next/` into pieces the scanner cannot see whole.
    let events =
        TextMergeWithOffset::new(Parser::new_ext(src, Options::ENABLE_TABLES).into_offset_iter());
    for (event, range) in events {
        if depth == 0 && matches!(event, Event::Rule) {
            blocks.push(Block {
                start: range.start,
                end: range.end,
                events: vec![event],
            });
            continue;
        }
        match event {
            Event::Start(_) => {
                depth += 1;
                if depth == 1 {
                    open = Some(Block {
                        start: range.start,
                        end: range.end,
                        events: Vec::new(),
                    });
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if let Some(block) = open.as_mut() {
            block.events.push(event);
        }
        if depth == 0
            && let Some(block) = open.take()
        {
            blocks.push(block);
        }
    }
    blocks
}

struct Chunk {
    rows: Vec<Row>,
    blank: bool,
    end_line: usize,
}

fn raw_rows(lines: &[SrcLine<'_>], width: usize, link: Style) -> Vec<Row> {
    lines
        .iter()
        .flat_map(|line| link_wrap(line.text, Style::default(), link, Kinds::PROSE, width))
        .map(Line::from)
        .collect()
}

fn uncovered(
    lines: &[SrcLine<'_>],
    range: std::ops::Range<usize>,
    width: usize,
    link: Style,
) -> Vec<Chunk> {
    range
        .map(|i| {
            let blank = is_blank(lines[i].text);
            Chunk {
                rows: if blank {
                    vec![Line::default()]
                } else {
                    raw_rows(&lines[i..=i], width, link)
                },
                blank,
                end_line: i + 1,
            }
        })
        .collect()
}

fn render_region(
    src: &str,
    base: usize,
    width: usize,
    glyphs: Glyphs,
    link: Style,
    out: &mut Vec<(Row, usize)>,
) {
    let lines = split_lines(src);
    let line_of = |byte: usize| lines.partition_point(|l| l.byte <= byte).saturating_sub(1);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut next = 0;
    for block in parse_blocks(src) {
        let first = line_of(block.start).max(next);
        let trimmed = src[..block.end].trim_end().len();
        let last = if trimmed > block.start {
            line_of(trimmed - 1)
        } else {
            first
        };
        if last < first {
            continue;
        }
        chunks.extend(uncovered(&lines, next..first, width, link));
        let rows = render_block(&block.events, width, glyphs, link)
            .unwrap_or_else(|| raw_rows(&lines[first..=last], width, link));
        chunks.push(Chunk {
            rows,
            blank: false,
            end_line: last + 1,
        });
        next = last + 1;
    }
    chunks.extend(uncovered(&lines, next..lines.len(), width, link));
    let total = src.chars().count();
    let mut i = 0;
    while i < chunks.len() {
        let mut j = i + 1;
        while j < chunks.len() && chunks[j].blank {
            j += 1;
        }
        let end_line = chunks[j - 1].end_line;
        let end = base + lines.get(end_line).map_or(total, |l| l.chars);
        for chunk in &mut chunks[i..j] {
            out.extend(
                std::mem::take(&mut chunk.rows)
                    .into_iter()
                    .map(|r| (r, end)),
            );
        }
        i = j;
    }
}

/// `None` is a block this renderer does not draw.
fn render_block(
    events: &[Event<'_>],
    width: usize,
    glyphs: Glyphs,
    link: Style,
) -> Option<Vec<Row>> {
    let (first, rest) = events.split_first()?;
    let Event::Start(tag) = first else {
        return None;
    };
    let inner = &rest[..rest.len().saturating_sub(1)];
    match tag {
        Tag::Paragraph => {
            let mut inline = Inline::new(link);
            inline.feed_all(inner)?;
            Some(into_rows(inline.rows(width, Style::default())))
        }
        Tag::Heading { level, .. } => {
            let mut inline = Inline::new(link);
            inline.feed_all(inner)?;
            let mut style = Style::default().add_modifier(Modifier::BOLD);
            if matches!(level, pulldown_cmark::HeadingLevel::H1) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            Some(into_rows(inline.rows(width, style)))
        }
        Tag::CodeBlock(CodeBlockKind::Fenced(_)) => {
            let mut code = String::new();
            for event in inner {
                match event {
                    Event::Text(t) => code.push_str(t),
                    _ => return None,
                }
            }
            // The parser's code text carries the blank lines up to the block's end; `render_region`
            // draws those itself.
            let code = code.trim_end_matches('\n');
            if code.is_empty() {
                return Some(Vec::new());
            }
            Some(
                code.split('\n')
                    .flat_map(|l| code_rows(l, width, link))
                    .collect(),
            )
        }
        Tag::List(start) => {
            let mut at = 0;
            let rows = list_rows(events, &mut at, *start, width, glyphs, link)?;
            Some(into_rows(rows))
        }
        Tag::Table(aligns) => table_rows(inner, aligns, width, glyphs, link),
        _ => None,
    }
}

fn into_rows(rows: Vec<Cells>) -> Vec<Row> {
    rows.into_iter().map(Line::from).collect()
}

fn code_rows(line: &str, width: usize, link: Style) -> Vec<Row> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let pad = if width > 2 { "  " } else { "" };
    link_wrap(
        line,
        dim,
        link,
        Kinds::URL | Kinds::WHOLE,
        width - pad.len(),
    )
    .into_iter()
    .map(|piece| {
        let mut spans = vec![Span::raw(pad)];
        spans.extend(piece);
        Line::from(spans)
    })
    .collect()
}

enum Piece {
    Text(Span<'static>),
    Break,
}

/// Flattens the inline events of a block into styled spans.
struct Inline {
    pieces: Vec<Piece>,
    mods: Vec<Style>,
    /// What a link's text, and a bare URL, is drawn with.
    link: Style,
    /// The target of each link enclosing the event being fed; `None` for one that is not
    /// followed (a scheme other than http or https).
    links: Vec<Option<Ref>>,
}

impl Inline {
    fn new(link: Style) -> Self {
        Inline {
            pieces: Vec::new(),
            mods: Vec::new(),
            link,
            links: Vec::new(),
        }
    }

    fn style(&self) -> Style {
        self.mods.iter().fold(Style::default(), |s, m| s.patch(*m))
    }

    fn text(&mut self, text: &str) {
        let style = self.style();
        if let Some(target) = self.links.last() {
            let style = match target {
                Some(r) if self.link != Style::default() => hyperlink::tag(style, r.clone()),
                _ => style,
            };
            self.pieces
                .push(Piece::Text(Span::styled(text.to_string(), style)));
        } else {
            self.pieces.extend(
                link_spans(text, style, self.link, Kinds::PROSE)
                    .into_iter()
                    .map(Piece::Text),
            );
        }
    }

    /// Inline code is a link only as a whole: it holds one URL or one path and nothing else.
    fn code(&mut self, text: &str) {
        let style = self.style().add_modifier(Modifier::BOLD);
        let style = match self.links.last() {
            Some(Some(r)) if self.link != Style::default() => hyperlink::tag(style, r.clone()),
            Some(_) => style,
            None if self.link == Style::default() => style,
            None => match hyperlink::scan(text, Kinds::URL | Kinds::PATH).as_slice() {
                [(range, r)] if range.len() == text.len() => {
                    hyperlink::styled(style, self.link, r.clone())
                }
                _ => style,
            },
        };
        self.pieces
            .push(Piece::Text(Span::styled(text.to_string(), style)));
    }

    fn markup(&mut self, text: &str) {
        let span = Span::styled(text.to_string(), self.style());
        self.pieces.push(Piece::Text(span));
    }

    fn feed_all(&mut self, events: &[Event<'_>]) -> Option<()> {
        for event in events {
            if !self.feed(event)? {
                return None;
            }
        }
        Some(())
    }

    /// `Some(true)` consumed it, `Some(false)` is not an inline event, `None` is inline
    /// markup this renderer does not draw.
    fn feed(&mut self, event: &Event<'_>) -> Option<bool> {
        match event {
            Event::Text(t) => self.text(t),
            Event::Html(t) | Event::InlineHtml(t) => self.markup(t),
            Event::Code(t) => self.code(t),
            Event::SoftBreak | Event::HardBreak => self.pieces.push(Piece::Break),
            Event::Start(Tag::Strong) => self
                .mods
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Emphasis) => self
                .mods
                .push(Style::default().add_modifier(Modifier::ITALIC)),
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.mods.push(self.link);
                self.links
                    .push(is_http(dest_url).then(|| Ref::Url(dest_url.to_string())));
            }
            Event::End(TagEnd::Link) => {
                self.mods.pop();
                self.links.pop();
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis) => {
                self.mods.pop();
            }
            Event::Start(Tag::Image { .. }) | Event::FootnoteReference(_) => return None,
            _ => return Some(false),
        }
        Some(true)
    }

    /// Wrapped rows, with `extra` added to every span's style.
    fn rows(&mut self, width: usize, extra: Style) -> Vec<Cells> {
        let mut rows = Vec::new();
        let mut line: Cells = Vec::new();
        let pieces = std::mem::take(&mut self.pieces);
        for piece in pieces.into_iter().map(Some).chain([None]) {
            match piece {
                Some(Piece::Text(span)) => line.push(span.patch_style(extra)),
                Some(Piece::Break) | None => {
                    rows.extend(wrap_spans(&std::mem::take(&mut line), width));
                }
            }
        }
        rows
    }

    /// The pieces as one line, a break becoming a space (a table cell has no lines).
    fn flat(&mut self) -> Cells {
        std::mem::take(&mut self.pieces)
            .into_iter()
            .map(|p| match p {
                Piece::Text(span) => span,
                Piece::Break => Span::raw(" "),
            })
            .collect()
    }
}

fn is_http(url: &str) -> bool {
    let lower = |n: usize| {
        url.get(..n)
            .is_some_and(|p| p.eq_ignore_ascii_case(&"https://"[..n]))
    };
    lower(8) || lower(7)
}

/// Splits `text` into spans of `style`, with each reference of `kinds` tagged as a hyperlink
/// and, when its target needs no context, patched with `link`.
pub(crate) fn link_spans(
    text: &str,
    style: Style,
    link: Style,
    kinds: Kinds,
) -> Vec<Span<'static>> {
    if link == Style::default() {
        return vec![Span::styled(text.to_string(), style)];
    }
    let mut spans = Vec::new();
    let mut plain_from = 0;
    for (range, r) in hyperlink::scan(text, kinds) {
        if plain_from < range.start {
            spans.push(Span::styled(
                text[plain_from..range.start].to_string(),
                style,
            ));
        }
        spans.push(Span::styled(
            text[range.clone()].to_string(),
            hyperlink::styled(style, link, r),
        ));
        plain_from = range.end;
    }
    if plain_from < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[plain_from..].to_string(), style));
    }
    spans
}

/// Scans `text` for references first, then splits the styled spans into rows of at most `width`
/// chars exactly where `wrap` would, so every piece of a split reference keeps the full target.
pub(crate) fn link_wrap(
    text: &str,
    style: Style,
    link: Style,
    kinds: Kinds,
    width: usize,
) -> Vec<Cells> {
    chunk_spans(&link_spans(text, style, link, kinds), width)
}

/// Splits styled spans into rows of at most `width` chars, matching `wrap`'s boundaries.
pub(crate) fn chunk_spans(spans: &[Span<'static>], width: usize) -> Vec<Cells> {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    if chars.is_empty() {
        return vec![Vec::new()];
    }
    chars.chunks(width).map(|c| merge(c.to_vec())).collect()
}

/// Word-wraps styled text by char count, hard-splitting a word longer than `width`. Wide
/// glyphs count as one char.
fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Cells> {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].0 == ' ' {
            // A row begun by a wrap does not open with the space that caused it.
            let wrapped = rows.len() > 1;
            let row = rows.last_mut().expect("rows is never empty");
            if row.len() >= width {
                rows.push(Vec::new());
            } else if !row.is_empty() || !wrapped {
                row.push(chars[i]);
            }
            i += 1;
            continue;
        }
        let end = chars[i..]
            .iter()
            .position(|(c, _)| *c == ' ')
            .map_or(chars.len(), |p| i + p);
        let word = &chars[i..end];
        let row = rows.last_mut().expect("rows is never empty");
        if row.len() + word.len() > width {
            while row.last().is_some_and(|(c, _)| *c == ' ') {
                row.pop();
            }
            if !row.is_empty() {
                rows.push(Vec::new());
            }
        }
        for piece in word.chunks(width) {
            if rows.last().expect("rows is never empty").len() + piece.len() > width {
                rows.push(Vec::new());
            }
            rows.last_mut()
                .expect("rows is never empty")
                .extend_from_slice(piece);
        }
        i = end;
    }
    rows.into_iter().map(merge).collect()
}

fn merge(row: Vec<(char, Style)>) -> Cells {
    let mut spans: Cells = Vec::new();
    for (c, style) in row {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    spans
}

/// The list whose `Start` is at `events[*at]`; leaves `*at` past its `End`. Rows carry their
/// own indentation.
fn list_rows(
    events: &[Event<'_>],
    at: &mut usize,
    start: Option<u64>,
    width: usize,
    glyphs: Glyphs,
    link: Style,
) -> Option<Vec<Cells>> {
    *at += 1;
    let mut number = start.unwrap_or(1);
    let mut items: Vec<Vec<Cells>> = Vec::new();
    let mut loose = false;
    loop {
        match events.get(*at)? {
            Event::Start(Tag::Item) => {
                *at += 1;
                let marker = match start {
                    Some(_) => format!("{number}. "),
                    None => glyphs.bullet.to_string(),
                };
                number += 1;
                let (rows, was_loose) = item_rows(events, at, marker, width, glyphs, link)?;
                loose |= was_loose;
                items.push(rows);
            }
            Event::End(TagEnd::List(_)) => {
                *at += 1;
                break;
            }
            _ => return None,
        }
    }
    let mut out = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        if loose && i > 0 {
            out.push(Vec::new());
        }
        out.extend(item);
    }
    Some(out)
}

fn item_rows(
    events: &[Event<'_>],
    at: &mut usize,
    marker: String,
    width: usize,
    glyphs: Glyphs,
    link: Style,
) -> Option<(Vec<Cells>, bool)> {
    let pad = marker.chars().count();
    let (marker, pad) = if width > pad + 1 {
        (marker, pad)
    } else {
        (String::new(), 0)
    };
    let inner = width - pad;
    let mut rows: Vec<Cells> = Vec::new();
    let mut inline = Inline::new(link);
    let mut loose = false;
    loop {
        match events.get(*at)? {
            Event::End(TagEnd::Item) => {
                *at += 1;
                // A tight item's own text is still in the builder.
                if !inline.pieces.is_empty() {
                    rows.extend(inline.rows(inner, Style::default()));
                }
                break;
            }
            Event::Start(Tag::Paragraph) => {
                loose = true;
                if !rows.is_empty() {
                    rows.push(Vec::new());
                }
                *at += 1;
            }
            Event::End(TagEnd::Paragraph) => {
                rows.extend(inline.rows(inner, Style::default()));
                *at += 1;
            }
            Event::Start(Tag::List(nested)) => {
                if !inline.pieces.is_empty() {
                    rows.extend(inline.rows(inner, Style::default()));
                }
                rows.extend(list_rows(events, at, *nested, inner, glyphs, link)?);
            }
            event => {
                if !inline.feed(event)? {
                    return None;
                }
                *at += 1;
            }
        }
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    let indent = " ".repeat(pad);
    for (i, row) in rows.iter_mut().enumerate() {
        let lead = if i == 0 {
            marker.clone()
        } else {
            indent.clone()
        };
        if !lead.is_empty() && (i == 0 || !row.is_empty()) {
            row.insert(0, Span::raw(lead));
        }
    }
    Some((rows, loose))
}

fn table_rows(
    events: &[Event<'_>],
    aligns: &[Alignment],
    width: usize,
    glyphs: Glyphs,
    link: Style,
) -> Option<Vec<Row>> {
    let mut table: Vec<Vec<Cells>> = Vec::new();
    let mut row: Vec<Cells> = Vec::new();
    let mut cell = Inline::new(link);
    for event in events {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow | Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => row.push(cell.flat()),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                table.push(std::mem::take(&mut row));
            }
            event => {
                if !cell.feed(event)? {
                    return None;
                }
            }
        }
    }
    let columns = aligns.len();
    if columns == 0 || table.is_empty() {
        return None;
    }
    for r in &mut table {
        r.resize_with(columns, Vec::new);
    }
    let size = |c: &Cells| c.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let mut widths: Vec<usize> = (0..columns)
        .map(|c| table.iter().map(|r| size(&r[c])).max().unwrap_or(0).max(1))
        .collect();
    let gaps = COLUMN_GAP * (columns - 1);
    while widths.iter().sum::<usize>() + gaps > width {
        let widest = (0..columns).max_by_key(|c| widths[*c])?;
        if widths[widest] <= COLUMN_FLOOR {
            return None;
        }
        widths[widest] -= 1;
    }
    let gap = Span::raw(format!(" {} ", glyphs.col_sep));
    let mut out = Vec::new();
    for (n, r) in table.iter().enumerate() {
        let header = n == 0;
        let wrapped: Vec<Vec<Cells>> = r
            .iter()
            .zip(&widths)
            .map(|(c, w)| {
                let styled: Cells = if header {
                    c.iter()
                        .cloned()
                        .map(|s| s.patch_style(Style::default().add_modifier(Modifier::BOLD)))
                        .collect()
                } else {
                    c.clone()
                };
                wrap_spans(&styled, *w)
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..height {
            let mut spans: Cells = Vec::new();
            for c in 0..columns {
                if c > 0 {
                    spans.push(gap.clone());
                }
                let cell = wrapped[c].get(k).cloned().unwrap_or_default();
                let pad = widths[c].saturating_sub(size(&cell));
                let (left, right) = match aligns[c] {
                    Alignment::Right => (pad, 0),
                    Alignment::Center if c + 1 == columns => (pad / 2, 0),
                    Alignment::Center => (pad / 2, pad - pad / 2),
                    _ if c + 1 == columns => (0, 0),
                    _ => (0, pad),
                };
                if left > 0 {
                    spans.push(Span::raw(" ".repeat(left)));
                }
                spans.extend(cell);
                if right > 0 {
                    spans.push(Span::raw(" ".repeat(right)));
                }
            }
            out.push(Line::from(spans));
        }
        if header {
            let joint = glyphs.rule.repeat(COLUMN_GAP);
            let rule: Vec<String> = widths.iter().map(|w| glyphs.rule.repeat(*w)).collect();
            out.push(Line::from(rule.join(&joint)));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::wrap;

    fn render(text: &str, width: usize) -> Vec<Row> {
        rows(
            text,
            0,
            text.chars().count(),
            width,
            Glyphs::UNICODE,
            Style::default(),
        )
        .into_iter()
        .map(|(l, _)| l)
        .collect()
    }

    fn plain(lines: &[Row]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn shown(text: &str, width: usize) -> Vec<String> {
        plain(&render(text, width))
    }

    fn styled(line: &Row, modifier: Modifier) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(modifier))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn inline_styles_nest_and_markers_vanish() {
        let lines = render("a **bold *both*** and *it* and `code`", 80);
        assert_eq!(plain(&lines), vec!["a bold both and it and code"]);
        assert_eq!(styled(&lines[0], Modifier::BOLD), "bold bothcode");
        assert_eq!(styled(&lines[0], Modifier::ITALIC), "bothit");
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(shown("a **b and `c", 80), vec!["a **b and `c"]);
    }

    #[test]
    fn a_styled_span_keeps_its_style_across_a_wrapped_row() {
        let lines = render("**alpha beta gamma**", 8);
        assert_eq!(plain(&lines), vec!["alpha", "beta", "gamma"]);
        for line in &lines {
            assert_eq!(
                styled(line, Modifier::BOLD),
                plain(std::slice::from_ref(line))[0]
            );
        }
    }

    #[test]
    fn a_long_word_is_split_by_chars() {
        assert_eq!(shown("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(shown("ab cdefghij", 4), vec!["ab", "cdef", "ghij"]);
    }

    #[test]
    fn a_soft_break_keeps_the_source_line_break() {
        assert_eq!(shown("one\ntwo", 80), vec!["one", "two"]);
    }

    #[test]
    fn links_render_as_their_text() {
        assert_eq!(
            shown("see [the docs](http://x.y)", 80),
            vec!["see the docs"]
        );
    }

    #[test]
    fn headings_are_bold_and_h1_is_underlined() {
        let lines = render("# One\n\n## Two\n", 80);
        assert_eq!(plain(&lines), vec!["One", "", "Two"]);
        assert_eq!(styled(&lines[0], Modifier::BOLD), "One");
        assert_eq!(styled(&lines[0], Modifier::UNDERLINED), "One");
        assert_eq!(styled(&lines[2], Modifier::BOLD), "Two");
        assert_eq!(styled(&lines[2], Modifier::UNDERLINED), "");
    }

    #[test]
    fn lists_wrap_under_their_text() {
        assert_eq!(
            shown("- alpha beta gamma\n- delta\n", 10),
            vec!["• alpha", "  beta", "  gamma", "• delta"]
        );
        assert_eq!(
            shown("1. alpha beta\n2. delta\n", 9),
            vec!["1. alpha", "   beta", "2. delta"]
        );
    }

    #[test]
    fn nested_lists_indent() {
        assert_eq!(
            shown("- a\n  - b\n  - c\n- d\n", 20),
            vec!["• a", "  • b", "  • c", "• d"]
        );
    }

    #[test]
    fn a_loose_list_keeps_its_items_apart() {
        assert_eq!(shown("1. a\n\n2. b\n", 20), vec!["1. a", "", "2. b"]);
    }

    #[test]
    fn fenced_code_is_verbatim_dim_and_indented() {
        let lines = render("```rust\nlet a = **1**;\n\n  x\n```\n", 80);
        assert_eq!(plain(&lines), vec!["  let a = **1**;", "  ", "    x"]);
        assert_eq!(styled(&lines[0], Modifier::DIM), "let a = **1**;");
    }

    #[test]
    fn blank_lines_after_an_unclosed_fence_are_drawn_once() {
        assert_eq!(shown("```\na\n\n\n", 80), vec!["  a", "", ""]);
    }

    #[test]
    fn a_long_code_line_wraps() {
        assert_eq!(shown("```\nabcdefgh\n```", 6), vec!["  abcd", "  efgh"]);
    }

    #[test]
    fn unsupported_blocks_render_as_their_source() {
        for source in [
            "> quoted *text*",
            "<div>html</div>",
            "---",
            "    indented code",
            "![alt](img.png)",
        ] {
            assert_eq!(shown(source, 80), vec![source], "{source}");
        }
        assert_eq!(shown("> a\n> b", 80), vec!["> a", "> b"]);
    }

    #[test]
    fn a_list_with_an_unsupported_nested_block_falls_back_whole() {
        for source in [
            "- a\n- > quote\n",
            "- a\n  ```\n  code\n  ```\n",
            "- a\n\n  | x | y |\n  |---|---|\n  | 1 | 2 |\n",
            "- a\n\n  <div>x</div>\n",
        ] {
            let want: Vec<String> = source.lines().map(str::to_string).collect();
            assert_eq!(shown(source, 80), want, "{source}");
        }
    }

    #[test]
    fn a_table_that_fits_uses_natural_widths() {
        assert_eq!(
            shown(
                "| Name | Qty |\n|---|---|\n| apple | 3 |\n| fig | 12 |\n",
                40
            ),
            vec!["Name  │ Qty", "───────────", "apple │ 3", "fig   │ 12",]
        );
    }

    #[test]
    fn table_alignment_follows_the_separator_row() {
        let rows = shown(
            "| a | b | c |\n|:--|--:|:-:|\n| 1 | 2 | 3 |\n| longer | longer | longer |\n",
            80,
        );
        assert_eq!(rows[0], "a      │      b │   c");
        assert_eq!(rows[2], "1      │      2 │   3");
        assert_eq!(rows[3], "longer │ longer │ longer");
    }

    #[test]
    fn a_wide_table_shrinks_and_wraps_its_cells() {
        let source =
            "| Name | Description |\n|---|---|\n| x | a long description that must wrap |\n";
        let rows = shown(source, 24);
        assert!(rows.iter().all(|r| r.chars().count() <= 24), "{rows:?}");
        assert_eq!(rows[0], "Name │ Description");
        assert!(rows.len() > 4, "{rows:?}");
        let joined = rows[2..].join(" ");
        for word in ["long", "description", "wrap"] {
            assert!(joined.contains(word), "{rows:?}");
        }
    }

    #[test]
    fn a_table_whose_floors_do_not_fit_falls_back_to_source() {
        let source = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let want: Vec<String> = source.lines().map(str::to_string).collect();
        assert_eq!(
            shown(source, 6),
            want.iter().flat_map(|l| { wrap(l, 6) }).collect::<Vec<_>>()
        );
    }

    #[test]
    fn table_glyphs_follow_the_glyph_set() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let ascii = rows(
            text,
            0,
            text.chars().count(),
            40,
            Glyphs::ASCII,
            Style::default(),
        );
        let lines: Vec<Row> = ascii.into_iter().map(|(l, _)| l).collect();
        assert_eq!(plain(&lines), vec!["a | b", "-----", "1 | 2"]);
    }

    #[test]
    fn table_cells_keep_inline_markup() {
        let lines = render("| h |\n|---|\n| **b** and `c` |\n", 40);
        assert_eq!(plain(&lines)[2], "b and c");
        assert_eq!(styled(&lines[2], Modifier::BOLD), "bc");
        assert_eq!(styled(&lines[0], Modifier::BOLD), "h");
    }

    #[test]
    fn a_block_shares_one_end_offset_past_its_blank_lines() {
        let text = "one two\nthree\n\n\nnext";
        let out = rows(
            text,
            0,
            text.chars().count(),
            4,
            Glyphs::UNICODE,
            Style::default(),
        );
        let ends: Vec<usize> = out.iter().map(|(_, e)| *e).collect();
        assert!(ends[..out.len() - 1].iter().all(|e| *e == 16), "{ends:?}");
        assert_eq!(*ends.last().unwrap(), text.chars().count());
    }

    #[test]
    fn stable_end_stops_at_a_blank_line_once_the_next_block_starts() {
        assert_eq!(stable_end("one\n\ntwo"), 5);
        assert_eq!(stable_end("one\n\n"), 0);
        assert_eq!(stable_end("one\n\n  "), 0);
        assert_eq!(stable_end("one\ntwo"), 0);
        assert_eq!(stable_end("a\n\nb\n\nc"), 6);
        assert_eq!(stable_end(""), 0);
    }

    #[test]
    fn stable_end_keeps_a_list_together() {
        assert_eq!(stable_end("- a\n\n  more\n\nafter"), 13);
        assert_eq!(stable_end("- a\n\n  more"), 0);
        assert_eq!(stable_end("1. a\n\n2. b\n\nz"), 12);
        assert_eq!(stable_end("1. a\n\n2. b"), 0);
        assert_eq!(stable_end("- a\n\n  - b\n\nz"), 12);
        assert_eq!(stable_end("p\n\n-"), 0);
        assert_eq!(stable_end("p\n\n12"), 0);
        assert_eq!(stable_end("p\n\n12."), 0);
        assert_eq!(stable_end("p\n\n-x"), 3);
        assert_eq!(stable_end("p\n\n- a"), 3);
        assert_eq!(stable_end("p\n\n  more"), 3);
        assert_eq!(stable_end("| a |\n|---|\n\n- a\n\nz"), 18);
        assert_eq!(stable_end("p\n\n3x"), 3);
    }

    #[test]
    fn stable_end_does_not_split_a_fence() {
        assert_eq!(stable_end("p\n\n```\na\n\nb\n"), 3);
        assert_eq!(stable_end("p\n\n```\na\n\nb\n```\n"), 16);
        assert_eq!(stable_end("p\n\n```\na\n```x\n\nq\n"), 3);
        assert_eq!(stable_end("p\n\n````\na\n```\n\nb\n"), 3);
        assert_eq!(stable_end("p\n\n~~~\na\n```\n~~~\nz"), 17);
        for indent in ["", " ", "  ", "   "] {
            let text = format!("p\n\n{indent}```\na\n\nb\n{indent}```\nz");
            assert_eq!(stable_end(&text), text.chars().count() - 1, "{indent:?}");
        }
    }

    #[test]
    fn a_fence_inside_a_list_item_belongs_to_the_list() {
        assert_eq!(stable_end("- a\n  ```\n  x\n  ```\nz"), 0);
        assert_eq!(stable_end("- a\n  ```\n  x\n  ```\n\nz"), 21);
    }

    #[test]
    fn a_half_arrived_table_row_is_in_the_tail() {
        assert_eq!(stable_end("p\n\n| a | b |\n|---|---|\n| 1"), 3);
    }

    #[test]
    fn resuming_inside_an_open_fence_renders_the_remainder_as_code() {
        let text = "p\n\n```\nalpha\nbeta\n```\n\nafter\n";
        let whole = shown(text, 80);
        assert_eq!(whole, vec!["p", "", "  alpha", "  beta", "", "after"]);
        let len = text.chars().count();
        let render_from = |from: usize| {
            plain(
                &rows(text, from, len, 80, Glyphs::UNICODE, Style::default())
                    .into_iter()
                    .map(|(l, _)| l)
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(render_from(7), whole[2..]);
        assert_eq!(render_from(13), whole[3..]);
        let mid = render_from(9);
        assert_eq!(mid[0], "  pha");
        assert_eq!(mid[1..], whole[3..]);
    }

    #[test]
    fn resuming_inside_a_list_fence_does_not_open_a_code_block() {
        let text = "- a\n  ```\n  x\n  y\n  ```\n";
        let got = rows(
            text,
            10,
            text.chars().count(),
            80,
            Glyphs::UNICODE,
            Style::default(),
        );
        let got = plain(&got.into_iter().map(|(l, _)| l).collect::<Vec<_>>());
        assert!(got.iter().all(|l| !l.starts_with("    ")), "{got:?}");
    }

    fn link() -> Style {
        Style::default()
            .fg(ratatui::style::Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn linked_rows(text: &str, width: usize) -> Vec<Row> {
        rows(
            text,
            0,
            text.chars().count(),
            width,
            Glyphs::UNICODE,
            link(),
        )
        .into_iter()
        .map(|(l, _)| l)
        .collect()
    }

    /// The text drawn in the link style, per row.
    fn colored(lines: &[Row]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .filter(|s| s.style.fg == link().fg)
                    .map(|s| s.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// The ref of every tagged span, in order.
    fn targets(lines: &[Row]) -> Vec<Ref> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter_map(|s| hyperlink::ref_of_style(s.style))
            .collect()
    }

    #[test]
    fn link_spans_split_a_bare_url_from_its_text() {
        let spans = link_spans(
            "see https://a.io/x now",
            Style::default(),
            link(),
            Kinds::URL,
        );
        let got: Vec<(&str, bool)> = spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style.fg.is_some()))
            .collect();
        assert_eq!(
            got,
            vec![("see ", false), ("https://a.io/x", true), (" now", false)]
        );
    }

    #[test]
    fn link_spans_trim_trailing_punctuation_and_unbalanced_brackets() {
        let url = |text: &str| {
            link_spans(text, Style::default(), link(), Kinds::URL)
                .into_iter()
                .filter(|s| s.style.fg.is_some())
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(url("go to https://a.io."), vec!["https://a.io"]);
        assert_eq!(url("https://a.io, then"), vec!["https://a.io"]);
        assert_eq!(url("(see https://a.io/x)"), vec!["https://a.io/x"]);
        assert_eq!(url("(see https://a.io/x)."), vec!["https://a.io/x"]);
        assert_eq!(url("\"https://a.io\""), vec!["https://a.io"]);
        assert_eq!(url("https://a.io/F_(b)"), vec!["https://a.io/F_(b)"]);
        assert_eq!(
            url("http://a.io and https://b.io"),
            vec!["http://a.io", "https://b.io"]
        );
    }

    #[test]
    fn link_spans_ignore_what_is_not_a_url() {
        for text in ["http://", "https://.", "a.io/x", "xhttps://a.io", "http:/a"] {
            let spans = link_spans(text, Style::default(), link(), Kinds::URL);
            assert!(spans.iter().all(|s| s.style.fg.is_none()), "{text}");
            let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn a_bare_url_in_a_reply_is_colored_and_the_text_is_unchanged() {
        let text = "visit https://a.io/x, ok\n";
        let with = linked_rows(text, 40);
        assert_eq!(colored(&with), vec!["https://a.io/x"]);
        assert_eq!(plain(&with), plain(&render(text, 40)));
    }

    #[test]
    fn markdown_link_text_is_colored_and_the_target_is_not_printed() {
        let text = "a [the **docs**](https://a.io/d) b\n";
        let with = linked_rows(text, 40);
        assert_eq!(plain(&with), vec!["a the docs b"]);
        assert_eq!(colored(&with), vec!["the docs"]);
        assert_eq!(plain(&with), plain(&render(text, 40)));
    }

    #[test]
    fn an_autolink_is_colored() {
        let with = linked_rows("<https://a.io/x>\n", 40);
        assert_eq!(plain(&with), vec!["https://a.io/x"]);
        assert_eq!(colored(&with), vec!["https://a.io/x"]);
    }

    #[test]
    fn a_url_split_into_several_text_events_is_colored_whole() {
        for url in [
            "https://en.wikipedia.org/wiki/Foo_(bar)",
            "https://x.com/_next/a",
        ] {
            let text = format!("see {url}\n");
            let with = linked_rows(&text, 80);
            assert_eq!(colored(&with), vec![url], "{url}");
        }
    }

    #[test]
    fn code_is_linkified_only_as_a_whole_url_or_path() {
        let text = "`https://a.io` and `see https://a.io` and `#82`\n";
        let with = linked_rows(text, 60);
        assert_eq!(colored(&with), vec!["https://a.io"]);
        assert_eq!(targets(&with), vec![Ref::Url("https://a.io".into())]);
        let with = linked_rows("`src/main.rs:1`\n", 60);
        assert_eq!(targets(&with), vec![Ref::Path("src/main.rs".into())]);
        assert!(colored(&with).iter().all(String::is_empty));
    }

    #[test]
    fn a_fenced_row_links_a_whole_url_token_and_nothing_else() {
        let with = linked_rows("```\ncurl https://a\n```\n", 40);
        assert_eq!(targets(&with), vec![Ref::Url("https://a".into())]);
        let with = linked_rows("```\nx=https://a #82 src/a.rs\n```\n", 40);
        assert!(targets(&with).is_empty(), "{with:?}");
        let with = linked_rows("```\ncurl https://a\n", 40);
        assert_eq!(targets(&with), vec![Ref::Url("https://a".into())]);
    }

    #[test]
    fn markdown_link_text_carries_its_target() {
        let with = linked_rows("a [the **docs**](https://a.io/d) b\n", 40);
        let urls = Ref::Url("https://a.io/d".into());
        assert_eq!(targets(&with), vec![urls.clone(), urls]);
        let with = linked_rows("<https://a.io/x>\n", 40);
        assert_eq!(targets(&with), vec![Ref::Url("https://a.io/x".into())]);
        let with = linked_rows("[`code`](https://a.io/c)\n", 40);
        assert_eq!(targets(&with), vec![Ref::Url("https://a.io/c".into())]);
    }

    #[test]
    fn a_markdown_target_that_is_not_http_is_never_tagged() {
        for dest in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "docs/a.md",
            "mailto:a@b.c",
        ] {
            let with = linked_rows(&format!("[text]({dest})\n"), 40);
            assert!(targets(&with).is_empty(), "{dest}");
        }
    }

    #[test]
    fn prose_issues_and_paths_are_tagged_and_not_colored() {
        let with = linked_rows("see #82 and owner/repo#7 and src/a.rs:3\n", 60);
        assert_eq!(
            targets(&with),
            vec![
                Ref::Issue {
                    repo: None,
                    number: 82
                },
                Ref::Issue {
                    repo: Some(("owner".into(), "repo".into())),
                    number: 7
                },
                Ref::Path("src/a.rs".into()),
            ]
        );
        assert_eq!(colored(&with), vec!["owner/repo#7"]);
    }

    #[test]
    fn a_url_in_a_raw_fallback_block_is_colored() {
        let text = "> quoted https://a.io/q\n";
        let with = linked_rows(text, 40);
        assert_eq!(plain(&with), vec!["> quoted https://a.io/q"]);
        assert_eq!(colored(&with), vec!["https://a.io/q"]);
    }

    #[test]
    fn urls_in_lists_and_table_cells_are_colored() {
        let with = linked_rows("- a https://a.io/l\n", 40);
        assert_eq!(colored(&with), vec!["https://a.io/l"]);
        let with = linked_rows("| h |\n|---|\n| https://a.io/t |\n", 40);
        assert_eq!(colored(&with)[2], "https://a.io/t");
    }

    #[test]
    fn a_url_hard_split_across_rows_keeps_its_color_on_every_row() {
        let with = linked_rows("https://example.com/a/long/path\n", 10);
        assert!(with.len() > 2);
        assert!(colored(&with).iter().all(|c| !c.is_empty()), "{with:?}");
        assert_eq!(colored(&with).concat(), "https://example.com/a/long/path");
    }
}
