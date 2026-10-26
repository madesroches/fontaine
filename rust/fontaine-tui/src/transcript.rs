//! The conversation as shown: user messages, streaming assistant text, a card per tool call
//! with its outcome, and notices.

use fontaine_core::audit::Decision;
use fontaine_core::notice::{Notice, Severity};
use fontaine_core::preview::{DiffPreview, display_text};
use fontaine_core::turn::SessionEvent;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::diff::diff_rows;
use crate::input::wrap;
use crate::theme::{Glyphs, Theme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Done,
    Failed(String),
    Denied(String),
    Interrupted,
}

/// Most bytes of one call's result a transcript keeps, so a huge `cat` cannot grow memory.
pub const OUTPUT_KEEP_BYTES: usize = 64 * 1024;
/// Wrapped rows of a call a card shows before folding the rest.
const CALL_ROWS: usize = 3;
/// Wrapped rows of output a card shows, the last ones.
pub(crate) const OUTPUT_ROWS: usize = 10;
/// Lines of a diff a card shows.
const DIFF_ROWS: usize = 20;

/// What a tool call returned, safe to paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: String,
    /// Only the head and the tail of the result were kept.
    pub clipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
    },
    Tool {
        id: String,
        tool: String,
        summary: String,
        status: ToolStatus,
        /// Counts the session's calls from 1; what `/expand` takes. A settled follow-up keeps
        /// the ordinal of the row it follows.
        ordinal: usize,
        /// The whole call: every line of the command, path or URL.
        call: String,
        output: Option<ToolOutput>,
        diff: Option<DiffPreview>,
    },
    /// A call printed in full by `/expand`; always wraps an `Entry::Tool`.
    Expanded(Box<Entry>),
    Notice {
        severity: Severity,
        headline: String,
        hint: Option<String>,
    },
    Error(String),
    /// A `!` command the human ran. `output` already ends with the line saying how it ended.
    Shell {
        command: String,
        output: String,
        exit_code: Option<i32>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// Entries fully written to scrollback.
    committed: usize,
    /// Chars of `entries[committed]`'s text already written, when it is a streaming reply.
    committed_chars: usize,
    /// Calls seen this session; `/clear` does not reset it, so an ordinal never names two calls.
    calls: usize,
    /// Entries that `/clear` removed before they reached scrollback, with the chars of each
    /// already written. The screen writes them out before anything newer so the conversation
    /// that was on display is not lost from the terminal.
    retired: Vec<(Entry, usize)>,
    pub glyphs: Glyphs,
}

const SUMMARY_KEYS: &[&str] = &["command", "path", "file_path", "url", "pattern", "query"];

fn summarize(tool: &str, args: &Value) -> String {
    if tool == "todo_write"
        && let Some(items) = args.get("todos").and_then(Value::as_array)
    {
        let done = items
            .iter()
            .filter(|item| item.get("status").and_then(Value::as_str) == Some("completed"))
            .count();
        return format!("{} tasks, {done} done", items.len());
    }
    let named = SUMMARY_KEYS
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str));
    let text = named.map_or_else(|| args.to_string(), str::to_string);
    first_line(&text)
}

/// The call whole, where `summarize` keeps its first line.
fn call_text(tool: &str, args: &Value) -> String {
    if tool == "todo_write" && args.get("todos").is_some() {
        return summarize(tool, args);
    }
    let named = SUMMARY_KEYS
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str));
    let text = named.map_or_else(|| args.to_string(), str::to_string);
    display_text(text.trim())
}

/// The result of a call as the card keeps it: capped, then made safe to paint. Calls whose
/// result is not worth a block get none.
fn keep_output(tool: &str, text: &str) -> Option<ToolOutput> {
    if matches!(tool, "todo_write" | "read") || text.trim().is_empty() {
        return None;
    }
    let (kept, clipped) = if text.len() <= OUTPUT_KEEP_BYTES {
        (text.to_string(), false)
    } else {
        let mut head = OUTPUT_KEEP_BYTES / 2;
        while !text.is_char_boundary(head) {
            head -= 1;
        }
        let mut tail = text.len() - OUTPUT_KEEP_BYTES / 2;
        while !text.is_char_boundary(tail) {
            tail += 1;
        }
        (format!("{}\n{}", &text[..head], &text[tail..]), true)
    };
    Some(ToolOutput {
        text: display_text(kept.trim_end_matches('\n')),
        clipped,
    })
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

impl Transcript {
    pub fn user(&mut self, text: String) {
        self.finish_streaming();
        self.entries.push(Entry::User(text));
    }

    /// Empties the transcript after `/clear`. What was already written stays in the terminal's
    /// scrollback; what was held back moves to `retired` for the screen to write.
    pub fn clear(&mut self) {
        let skip = self.committed_chars;
        let held = self.entries.drain(self.committed..);
        for (i, entry) in held.enumerate() {
            self.retired.push((entry, if i == 0 { skip } else { 0 }));
        }
        self.entries.clear();
        self.committed = 0;
        self.committed_chars = 0;
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.push_notice(Notice::info(text));
    }

    pub fn warning(&mut self, headline: impl Into<String>, hint: Option<String>) {
        self.push_notice(Notice::warning(headline, hint));
    }

    pub fn push_notice(&mut self, notice: Notice) {
        self.finish_streaming();
        self.entries.push(Entry::Notice {
            severity: notice.severity,
            headline: notice.headline,
            hint: notice.hint,
        });
    }

    pub fn shell(&mut self, command: String, output: String, exit_code: Option<i32>) {
        self.finish_streaming();
        self.entries.push(Entry::Shell {
            command,
            output,
            exit_code,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.finish_streaming();
        self.entries.push(Entry::Error(text.into()));
    }

    fn finish_streaming(&mut self) {
        if let Some(Entry::Assistant { streaming, .. }) = self.entries.last_mut() {
            *streaming = false;
        }
    }

    fn find_tool(&self, id: &str, tool: &str) -> Option<usize> {
        let by_id = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::Tool { id: i, .. } if i == id));
        by_id.or_else(|| {
            self.entries.iter().rposition(|e| {
                matches!(e, Entry::Tool { tool: t, status: ToolStatus::Pending, .. } if t == tool)
            })
        })
    }

    fn set_status(&mut self, at: Option<usize>, new: ToolStatus) {
        if let Some(i) = at
            && let Entry::Tool { status, .. } = &self.entries[i]
            && !matches!(status, ToolStatus::Denied(_))
        {
            self.settle(i, new);
        }
    }

    /// Records `new` on call `i`. A call already written to scrollback as pending can no longer
    /// be redrawn there, so its outcome is appended as a follow-up line, which later lookups by
    /// id find first.
    fn settle(&mut self, i: usize, new: ToolStatus) {
        let Entry::Tool { status, .. } = &mut self.entries[i] else {
            return;
        };
        let written = i < self.committed && *status == ToolStatus::Pending;
        *status = new;
        if written {
            let follow_up = self.entries[i].clone();
            self.finish_streaming();
            self.entries.push(follow_up);
        }
    }

    pub fn on_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::AssistantDelta(delta) => {
                if let Some(Entry::Assistant {
                    text,
                    streaming: true,
                }) = self.entries.last_mut()
                {
                    text.push_str(&delta);
                } else {
                    self.entries.push(Entry::Assistant {
                        text: delta,
                        streaming: true,
                    });
                }
            }
            SessionEvent::Assistant(full) => {
                if matches!(
                    self.entries.last(),
                    Some(Entry::Assistant {
                        streaming: true,
                        ..
                    })
                ) {
                    self.finish_streaming();
                } else {
                    self.entries.push(Entry::Assistant {
                        text: full,
                        streaming: false,
                    });
                }
            }
            SessionEvent::ToolCall { id, tool, args } => {
                self.finish_streaming();
                self.calls += 1;
                self.entries.push(Entry::Tool {
                    id,
                    summary: summarize(&tool, &args),
                    call: call_text(&tool, &args),
                    tool,
                    status: ToolStatus::Pending,
                    ordinal: self.calls,
                    output: None,
                    diff: None,
                });
            }
            SessionEvent::ToolPreview { id, preview } => {
                // By id only: a subagent's call on the same feed must not take it.
                let at = self
                    .entries
                    .iter()
                    .rposition(|e| matches!(e, Entry::Tool { id: i, .. } if *i == id));
                if let Some(i) = at
                    && let Entry::Tool { diff, .. } = &mut self.entries[i]
                {
                    *diff = Some(*preview);
                }
            }
            SessionEvent::Decision(record) => {
                if record.decision == Decision::Deny {
                    let at =
                        self.find_tool(record.tool_use_id.as_deref().unwrap_or(""), &record.tool);
                    let why = record
                        .explanation
                        .or(record.rule)
                        .unwrap_or_else(|| "denied".to_string());
                    if let Some(i) = at {
                        self.settle(i, ToolStatus::Denied(first_line(&why)));
                    }
                }
            }
            SessionEvent::ToolResult {
                id,
                tool,
                success,
                text,
            } => {
                let at = self.find_tool(&id, &tool);
                if let Some(i) = at
                    && let Entry::Tool { output, status, .. } = &mut self.entries[i]
                    && !matches!(status, ToolStatus::Denied(_))
                {
                    *output = keep_output(&tool, &text);
                }
                let new = if success {
                    ToolStatus::Done
                } else {
                    ToolStatus::Failed(first_line(&text))
                };
                self.set_status(at, new);
            }
            SessionEvent::Usage { .. } => {}
        }
    }

    /// The guardian refused call `id`: shown with its rule label and its own explanation, not the
    /// words the agent is told.
    pub fn guardian_denied(&mut self, id: &str, tool: &str, rule: &str, explanation: &str) {
        let at = self.find_tool(id, tool);
        if let Some(i) = at {
            self.settle(
                i,
                ToolStatus::Denied(first_line(&format!("[{rule}] {explanation}"))),
            );
        }
    }

    /// Calls that never produced a result because the turn was cut short.
    pub fn interrupt_pending(&mut self) {
        self.finish_streaming();
        let pending: Vec<usize> = (0..self.entries.len())
            .filter(|&i| {
                matches!(&self.entries[i], Entry::Tool { status, .. } if *status == ToolStatus::Pending)
            })
            .collect();
        for i in pending {
            self.settle(i, ToolStatus::Interrupted);
        }
    }

    /// Prints call `ordinal` (the latest by default) in full as new lines. The last entry with
    /// that ordinal wins, so a settled follow-up is chosen over its stale pending row.
    pub fn expand(&mut self, ordinal: Option<usize>) -> Result<(), String> {
        let at = self.entries.iter().rposition(
            |e| matches!(e, Entry::Tool { ordinal: o, .. } if ordinal.is_none_or(|n| n == *o)),
        );
        let Some(i) = at else {
            return Err(match ordinal {
                Some(n) => format!(
                    "there is no tool call number {n} to expand; numbers count from the start of the session and the calls /clear removed are gone"
                ),
                None => "there is no tool call to expand".to_string(),
            });
        };
        let call = self.entries[i].clone();
        self.finish_streaming();
        self.entries.push(Entry::Expanded(Box::new(call)));
        Ok(())
    }

    /// The `n`th latest non-empty assistant entry (1 is the latest). A reply split by a tool
    /// call is two entries, so each counts on its own.
    pub fn assistant_reply(&self, n: usize) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .filter_map(|entry| match entry {
                Entry::Assistant { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
                _ => None,
            })
            .nth(n.checked_sub(1)?)
    }

    #[cfg(test)]
    pub fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for entry in &self.entries {
            entry_lines(entry, width, 0, theme, self.glyphs, &mut out);
        }
        out
    }

    /// Advances the commit cursor over what has settled and returns it. A streaming reply is
    /// cut by `cut`, keeping its tail back, and nothing behind an unsettled entry is committed
    /// before it. Every renderer settles through here.
    pub(crate) fn commit_entries(&mut self, cut: Cut) -> Vec<Settled<'_>> {
        let mut found = Vec::new();
        while let Some(entry) = self.entries.get(self.committed) {
            if let Entry::Assistant {
                text,
                streaming: true,
            } = entry
            {
                let end = cut.end(text, self.committed_chars);
                if end > self.committed_chars {
                    found.push((self.committed, self.committed_chars, Some(end)));
                    self.committed_chars = end;
                }
                break;
            }
            if !is_settled(entry) {
                break;
            }
            found.push((self.committed, self.committed_chars, None));
            self.committed += 1;
            self.committed_chars = 0;
        }
        found
            .into_iter()
            .map(|(i, skip, end)| Settled {
                entry: &self.entries[i],
                skip,
                end,
            })
            .collect()
    }

    /// The lines that have settled since the last call, for the terminal's scrollback. A
    /// streaming reply is committed a row at a time, keeping its last row live.
    pub fn commit(&mut self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        let glyphs = self.glyphs;
        for settled in self.commit_entries(Cut::Rows(width)) {
            match (settled.entry, settled.end) {
                (Entry::Assistant { text, .. }, Some(_)) => {
                    let mut rows = assistant_rows(text, settled.skip, width);
                    rows.pop();
                    out.extend(rows.into_iter().map(|(line, _)| line));
                }
                (entry, _) => entry_lines(entry, width, settled.skip, theme, glyphs, &mut out),
            }
        }
        out
    }

    /// What is not in scrollback yet.
    pub fn live_lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (i, entry) in self.entries.iter().enumerate().skip(self.committed) {
            let skip = if i == self.committed {
                self.committed_chars
            } else {
                0
            };
            entry_lines(entry, width, skip, theme, self.glyphs, &mut out);
        }
        out
    }

    /// The rows `/clear` retired, for the terminal's scrollback.
    pub fn take_retired(&mut self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (entry, skip) in std::mem::take(&mut self.retired) {
            entry_lines(&entry, width, skip, theme, self.glyphs, &mut out);
        }
        out
    }

    /// Commits the oldest rows that fit `budget`, which is how many rows the live area is over
    /// by; nothing is committed while the transcript fits. Entries commit whole, so the last
    /// one may release more than asked. Commit is oldest-first, so a pending call is released
    /// only once nothing older is left and the live area is still over budget; its outcome then
    /// reaches scrollback as a follow-up line. A streaming reply that would keep the live area
    /// over budget is released too, so the tail the screen shows never hides rows that are not
    /// in scrollback. A streaming reply is cut at a row, keeping its last row live.
    pub fn commit_within(
        &mut self,
        width: usize,
        theme: &Theme,
        mut budget: usize,
    ) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        while budget > 0 {
            let Some(entry) = self.entries.get(self.committed) else {
                break;
            };
            if let Entry::Assistant {
                text,
                streaming: true,
            } = entry
            {
                let rows = Cut::Budget {
                    width,
                    rows: budget,
                }
                .rows(text, self.committed_chars);
                if let Some((_, end)) = rows.last() {
                    self.committed_chars = *end;
                    out.extend(rows.into_iter().map(|(line, _)| line));
                }
                break;
            }
            let before = out.len();
            entry_lines(
                entry,
                width,
                self.committed_chars,
                theme,
                self.glyphs,
                &mut out,
            );
            budget = budget.saturating_sub(out.len() - before);
            self.committed += 1;
            self.committed_chars = 0;
        }
        out
    }

    /// Everything not in scrollback yet, whether settled or not.
    pub fn commit_all(&mut self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = self.take_retired(width, theme);
        out.extend(self.live_lines(width, theme));
        self.committed = self.entries.len();
        self.committed_chars = 0;
        out
    }
}

/// Where a streaming reply is cut when it is committed.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Cut {
    /// Whole wrapped rows at this width, keeping the last row live.
    Rows(usize),
    /// Whole lines, up to the last `\n`; the terminal does the wrapping.
    Lines,
    /// At most this many whole wrapped rows at this width, keeping the last row live.
    Budget { width: usize, rows: usize },
}

impl Cut {
    /// The rows a budgeted cut releases.
    fn rows(self, text: &str, from: usize) -> Vec<(Line<'static>, usize)> {
        let Cut::Budget { width, rows } = self else {
            return Vec::new();
        };
        let mut all = assistant_rows(text, from, width);
        all.pop();
        all.truncate(rows);
        all
    }

    /// The char offset in `text` that a commit starting at `from` runs to.
    fn end(self, text: &str, from: usize) -> usize {
        match self {
            Cut::Rows(width) => {
                let mut rows = assistant_rows(text, from, width);
                rows.pop();
                rows.last().map_or(from, |(_, end)| *end)
            }
            Cut::Budget { .. } => self.rows(text, from).last().map_or(from, |(_, end)| *end),
            Cut::Lines => text
                .chars()
                .skip(from)
                .enumerate()
                .filter(|(_, c)| *c == '\n')
                .last()
                .map_or(from, |(i, _)| from + i + 1),
        }
    }
}

/// A piece of the transcript that has settled.
pub(crate) struct Settled<'a> {
    pub entry: &'a Entry,
    /// Chars of the entry's text written before this piece.
    pub skip: usize,
    /// For a piece of a streaming reply, the char offset it runs to; `None` is the rest of the
    /// entry.
    pub end: Option<usize>,
}

fn is_settled(entry: &Entry) -> bool {
    !matches!(
        entry,
        Entry::Assistant {
            streaming: true,
            ..
        } | Entry::Tool {
            status: ToolStatus::Pending,
            ..
        }
    )
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn prefixed(text: &str, width: usize, first: &str, style: Style, out: &mut Vec<Line<'static>>) {
    prefixed_body(text, width, first, style, Style::default(), out);
}

/// `prefixed` with the wrapped text itself styled, not just the prefix.
fn prefixed_body(
    text: &str,
    width: usize,
    first: &str,
    style: Style,
    body_style: Style,
    out: &mut Vec<Line<'static>>,
) {
    let indent = " ".repeat(first.chars().count());
    let body = width.saturating_sub(indent.chars().count());
    let mut head = true;
    for line in text.split('\n') {
        for piece in wrap(line, body) {
            let prefix = if head { first } else { indent.as_str() };
            head = false;
            out.push(row(prefix, style, body_style, piece));
        }
    }
}

fn row(prefix: &str, style: Style, body_style: Style, piece: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(prefix.to_string(), style),
        Span::styled(piece, body_style),
    ])
}

/// The rows of assistant text from char offset `from`, each with the char offset just past it
/// (including the `'\n'` that ends its line). Offsets are what survive a change of width.
fn assistant_rows(text: &str, from: usize, width: usize) -> Vec<(Line<'static>, usize)> {
    let rest: String = text.chars().skip(from).collect();
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut offset = from;
    let mut lines = rest.split('\n').peekable();
    while let Some(line) = lines.next() {
        let pieces = wrap(line, width);
        let last = pieces.len() - 1;
        for (i, piece) in pieces.into_iter().enumerate() {
            offset += piece.chars().count();
            if i == last && lines.peek().is_some() {
                offset += 1;
            }
            rows.push((row("", Style::default(), Style::default(), piece), offset));
        }
    }
    rows
}

/// `skip` is the chars of an assistant reply already written to scrollback.
fn entry_lines(
    entry: &Entry,
    width: usize,
    skip: usize,
    theme: &Theme,
    glyphs: Glyphs,
    out: &mut Vec<Line<'static>>,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    match entry {
        Entry::User(text) => {
            let bold = Style::default().fg(theme.user).add_modifier(Modifier::BOLD);
            prefixed(text, width, "> ", bold, out);
            out.push(Line::default());
        }
        Entry::Assistant { text, .. } => {
            let rest: String = text.chars().skip(skip).collect();
            prefixed(&rest, width, "", Style::default(), out);
        }
        Entry::Tool { .. } => tool_lines(entry, width, false, theme, glyphs, out),
        Entry::Expanded(call) => {
            tool_lines(call, width, true, theme, glyphs, out);
            out.push(Line::default());
        }
        Entry::Notice {
            severity: Severity::Info,
            headline,
            ..
        } => prefixed(headline, width, glyphs.info, dim, out),
        Entry::Notice { headline, hint, .. } => {
            warning_lines(headline, hint.as_deref(), width, theme, glyphs, out)
        }
        Entry::Error(text) => prefixed(text, width, "! ", Style::default().fg(theme.error), out),
        Entry::Shell {
            command, output, ..
        } => {
            prefixed(&format!("! {command}"), width, "", dim, out);
            for line in output.trim_end().split('\n') {
                for piece in wrap(line, width) {
                    out.push(Line::styled(piece, dim));
                }
            }
            out.push(Line::default());
        }
    }
}

/// The last `rows` lines of `text` and how many came before them.
pub(crate) fn tail_lines(text: &str, rows: usize) -> (Vec<&str>, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let skipped = lines.len().saturating_sub(rows);
    (lines[skipped..].to_vec(), skipped)
}

/// A tool call: its outcome row and, once it has settled, the whole call, its diff and the end of
/// its output. `full` is the `/expand` form, which folds nothing.
fn tool_lines(
    entry: &Entry,
    width: usize,
    full: bool,
    theme: &Theme,
    glyphs: Glyphs,
    out: &mut Vec<Line<'static>>,
) {
    let Entry::Tool {
        tool,
        summary,
        status,
        ordinal,
        call,
        output,
        diff,
        ..
    } = entry
    else {
        return;
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let settled = matches!(status, ToolStatus::Done | ToolStatus::Failed(_));
    let card = full || settled;
    let (mark, style, detail) = match status {
        ToolStatus::Pending => ("…", dim, String::new()),
        ToolStatus::Done => ("✓", Style::default().fg(theme.success), String::new()),
        ToolStatus::Failed(why) => (
            "✗",
            Style::default().fg(theme.error),
            format!(" failed: {why}"),
        ),
        ToolStatus::Denied(why) => (
            "✗",
            Style::default().fg(theme.error),
            format!(" denied: {why}"),
        ),
        ToolStatus::Interrupted => ("−", dim, " interrupted".to_string()),
    };
    let wide = card
        && (call.contains('\n')
            || call.chars().count() + tool.chars().count() + 3 > width.saturating_sub(2));
    let line = truncate(
        &if wide {
            format!("• {tool}")
        } else {
            format!("• {tool} {summary}")
        },
        width.saturating_sub(2),
    );
    let mut cut = false;
    let mut call_rows = |out: &mut Vec<Line<'static>>| {
        if !wide {
            return;
        }
        let rows: Vec<String> = call
            .split('\n')
            .flat_map(|l| wrap(l, width.saturating_sub(4).max(1)))
            .collect();
        let shown = if full {
            rows.len()
        } else {
            rows.len().min(CALL_ROWS)
        };
        for row in &rows[..shown] {
            out.push(Line::raw(format!("    {row}")));
        }
        if shown < rows.len() {
            cut = true;
            out.push(Line::styled(
                format!("    … (+{} rows)", rows.len() - shown),
                dim,
            ));
        }
    };
    if let ToolStatus::Denied(why) = status {
        // A denial is the part the human has to read, so it gets its own wrapped lines.
        out.push(Line::from(vec![
            Span::styled(line, dim),
            Span::styled(format!(" {mark}"), style),
        ]));
        call_rows(out);
        prefixed(&format!("denied: {why}"), width, "    ", style, out);
        return;
    }
    let detail = truncate(&detail, width.saturating_sub(line.chars().count() + 3));
    out.push(Line::from(vec![
        Span::styled(line, dim),
        Span::styled(format!(" {mark}"), style),
        Span::styled(detail, style),
    ]));
    call_rows(out);
    if !card {
        return;
    }
    // A diff says more than the tool's one-line confirmation, which a failure still needs.
    let show_diff = diff
        .as_ref()
        .filter(|_| !matches!(status, ToolStatus::Failed(_)));
    if let Some(preview) = show_diff {
        let mut rows = Vec::new();
        let limit = if full { None } else { Some(DIFF_ROWS) };
        cut |= diff_rows(
            preview,
            width.saturating_sub(2).max(1),
            theme,
            limit,
            "",
            &mut rows,
        );
        for mut row in rows {
            row.spans.insert(0, Span::raw("  "));
            out.push(row);
        }
    }
    if let Some(output) = output.as_ref().filter(|_| show_diff.is_none()) {
        let gutter = glyphs.gutter;
        let body = width.saturating_sub(2 + gutter.chars().count()).max(1);
        let rows: Vec<String> = output
            .text
            .split('\n')
            .flat_map(|l| wrap(l, body))
            .collect();
        let skipped = if full {
            0
        } else {
            rows.len().saturating_sub(OUTPUT_ROWS)
        };
        let mut push = |text: &str| {
            out.push(Line::from(vec![
                Span::styled(format!("  {gutter}"), dim),
                Span::styled(text.to_string(), dim),
            ]));
        };
        if skipped > 0 {
            cut = true;
            push(&format!("… {skipped} earlier rows"));
        }
        for row in &rows[skipped..] {
            push(row);
        }
        if output.clipped {
            if full {
                push(&format!(
                    "… clipped: only the first and last {} KiB of the output are kept",
                    OUTPUT_KEEP_BYTES / 2048
                ));
            } else {
                cut = true;
            }
        }
    }
    if cut {
        out.push(Line::styled(format!("  expand: /expand {ordinal}"), dim));
    }
}

fn warning_lines(
    headline: &str,
    hint: Option<&str>,
    width: usize,
    theme: &Theme,
    glyphs: Glyphs,
    out: &mut Vec<Line<'static>>,
) {
    let style = Style::default().fg(theme.warning);
    let indent = " ".repeat(glyphs.warning.chars().count());
    let body = width.saturating_sub(indent.chars().count());
    prefixed_body(headline, width, glyphs.warning, style, style, out);
    let Some(hint) = hint else {
        return;
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // Backticks pair up across the whole hint, so a span that wraps stays bold on both rows.
    let mut in_ticks = false;
    for line in hint.split('\n') {
        for piece in wrap(line, body) {
            let mut spans = vec![Span::styled(indent.clone(), dim)];
            spans.extend(hint_spans(&piece, &mut in_ticks, dim, bold));
            out.push(Line::from(spans));
        }
    }
}

/// Splits one wrapped row of a hint into dim text and bold actionable spans: what sits between
/// backticks (the backticks included) and `/command` words. `in_ticks` carries across rows.
fn hint_spans(piece: &str, in_ticks: &mut bool, dim: Style, bold: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_bold = *in_ticks;
    let flush = |run: &mut String, run_bold: bool, spans: &mut Vec<Span<'static>>| {
        if !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(run),
                if run_bold { bold } else { dim },
            ));
        }
    };
    let chars: Vec<char> = piece.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let command = !*in_ticks
            && c == '/'
            && (i == 0 || chars[i - 1].is_whitespace())
            && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
        if c == '`' {
            if *in_ticks {
                run.push(c);
                *in_ticks = false;
                flush(&mut run, true, &mut spans);
                run_bold = false;
            } else {
                flush(&mut run, run_bold, &mut spans);
                *in_ticks = true;
                run_bold = true;
                run.push(c);
            }
            i += 1;
        } else if command {
            flush(&mut run, run_bold, &mut spans);
            let end = chars[i + 1..]
                .iter()
                .position(|n| !(n.is_alphanumeric() || *n == '-' || *n == '_'))
                .map_or(chars.len(), |p| i + 1 + p);
            spans.push(Span::styled(chars[i..end].iter().collect::<String>(), bold));
            i = end;
        } else {
            run.push(c);
            i += 1;
        }
    }
    flush(&mut run, run_bold, &mut spans);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn delta(t: &mut Transcript, s: &str) {
        t.on_event(SessionEvent::AssistantDelta(s.into()));
    }

    #[test]
    fn a_task_list_call_is_summarized_by_its_counts() {
        let args = serde_json::json!({"todos": [
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "pending"},
        ]});
        assert_eq!(summarize("todo_write", &args), "2 tasks, 1 done");
        assert_eq!(summarize("read", &serde_json::json!({"path": "x"})), "x");
    }

    #[test]
    fn assistant_reply_counts_from_the_latest() {
        let mut t = Transcript::default();
        t.user("q".into());
        t.on_event(SessionEvent::Assistant("first".into()));
        t.user("q2".into());
        t.on_event(SessionEvent::Assistant("  ".into()));
        t.notice("note");
        t.on_event(SessionEvent::Assistant("second".into()));
        assert_eq!(t.assistant_reply(1), Some("second"));
        assert_eq!(t.assistant_reply(2), Some("first"));
        assert_eq!(t.assistant_reply(3), None);
        assert_eq!(t.assistant_reply(0), None);
    }

    #[test]
    fn a_tool_call_splits_a_turn_into_two_replies() {
        let mut t = Transcript::default();
        delta(&mut t, "before");
        call(&mut t, "c1", "shell");
        result(&mut t, "c1", "shell");
        delta(&mut t, "after");
        assert_eq!(t.assistant_reply(1), Some("after"));
        assert_eq!(t.assistant_reply(2), Some("before"));
    }

    fn call(t: &mut Transcript, id: &str, tool: &str) {
        t.on_event(SessionEvent::ToolCall {
            id: id.into(),
            tool: tool.into(),
            args: serde_json::json!({ "command": id }),
        });
    }

    fn result(t: &mut Transcript, id: &str, tool: &str) {
        t.on_event(SessionEvent::ToolResult {
            id: id.into(),
            tool: tool.into(),
            success: true,
            text: String::new(),
        });
    }

    #[test]
    fn a_notice_mid_reply_does_not_block_later_commits() {
        let mut t = Transcript::default();
        delta(&mut t, "first");
        t.notice("heads up");
        delta(&mut t, "second");
        t.interrupt_pending();
        let out = text(&t.commit(40, &theme()));
        assert_eq!(out, vec!["first", "· heads up", "second"]);
        assert!(t.live_lines(40, &theme()).is_empty());
    }

    /// Feeds `full` in chunks of `chunk` chars, committing after every chunk.
    fn streamed(full: &str, chunk: usize, width: usize) -> Vec<String> {
        let mut t = Transcript::default();
        t.user("q".into());
        let chars: Vec<char> = full.chars().collect();
        let mut out = Vec::new();
        out.extend(text(&t.commit(width, &theme())));
        for piece in chars.chunks(chunk) {
            delta(&mut t, &piece.iter().collect::<String>());
            out.extend(text(&t.commit(width, &theme())));
        }
        t.on_event(SessionEvent::Assistant(full.into()));
        out.extend(text(&t.commit(width, &theme())));
        assert!(t.live_lines(width, &theme()).is_empty());
        let mut whole = Transcript::default();
        whole.user("q".into());
        whole.on_event(SessionEvent::Assistant(full.into()));
        assert_eq!(
            out,
            text(&whole.lines(width, &theme())),
            "chunk {chunk} width {width}"
        );
        out
    }

    #[test]
    fn streamed_commits_equal_the_whole_transcript() {
        let samples = [
            "hello world, this is a reply",
            "ab\ncd\n\nef\n",
            "abcd\nefgh\nij",
            "\n\n",
            "xxxxxxxxxxxxxxxxxxxx",
            "",
        ];
        for sample in samples {
            for chunk in 1..=7 {
                for width in [1, 2, 4, 5, 8, 40] {
                    streamed(sample, chunk, width);
                }
            }
        }
    }

    #[test]
    fn a_width_change_mid_stream_drops_and_repeats_nothing() {
        let full = "alpha beta\ngamma delta epsilon\n\nzeta";
        let widths = [3, 7, 5, 11, 2, 9];
        let mut t = Transcript::default();
        let mut out = Vec::new();
        let chars: Vec<char> = full.chars().collect();
        for (i, piece) in chars.chunks(4).enumerate() {
            delta(&mut t, &piece.iter().collect::<String>());
            out.extend(text(&t.commit(widths[i % widths.len()], &theme())));
        }
        t.on_event(SessionEvent::Assistant(full.into()));
        out.extend(text(&t.commit(6, &theme())));
        assert_eq!(out.concat(), full.replace('\n', ""));
    }

    #[test]
    fn a_shell_entry_shows_the_command_and_its_output() {
        let mut t = Transcript::default();
        t.shell("echo hi".into(), "hi\n[exit 0]".into(), Some(0));
        assert_eq!(
            text(&t.commit(40, &theme())),
            vec!["! echo hi", "hi", "[exit 0]", ""]
        );
        assert!(t.live_lines(40, &theme()).is_empty());
    }

    #[test]
    fn a_pending_call_holds_back_what_follows_it() {
        let mut t = Transcript::default();
        t.user("go".into());
        call(&mut t, "a", "shell");
        t.notice("after");
        assert_eq!(text(&t.commit(40, &theme())), vec!["> go", ""]);
        assert_eq!(t.live_lines(40, &theme()).len(), 2);
        result(&mut t, "a", "shell");
        assert_eq!(
            text(&t.commit(40, &theme())),
            vec!["• shell a ✓", "· after"]
        );
        assert!(t.live_lines(40, &theme()).is_empty());
    }

    #[test]
    fn live_lines_hold_the_partial_row_only() {
        let mut t = Transcript::default();
        delta(&mut t, "abcdef");
        assert!(t.commit(4, &theme()).len() == 1);
        assert_eq!(text(&t.live_lines(4, &theme())), vec!["ef"]);
        delta(&mut t, "gh");
        assert!(t.commit(4, &theme()).is_empty());
        assert_eq!(text(&t.live_lines(4, &theme())), vec!["efgh"]);
        delta(&mut t, "i");
        assert_eq!(text(&t.commit(4, &theme())), vec!["efgh"]);
        assert_eq!(text(&t.live_lines(4, &theme())), vec!["i"]);
    }

    #[test]
    fn a_committed_call_is_still_found_by_id() {
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        result(&mut t, "a", "shell");
        assert_eq!(t.commit(40, &theme()).len(), 1);
        call(&mut t, "b", "shell");
        t.guardian_denied("a", "shell", "r", "why");
        result(&mut t, "a", "shell");
        assert!(matches!(
            &t.entries[1],
            Entry::Tool {
                status: ToolStatus::Pending,
                ..
            }
        ));
        assert!(matches!(
            &t.entries[0],
            Entry::Tool {
                status: ToolStatus::Denied(_),
                ..
            }
        ));
    }

    #[test]
    fn clearing_empties_the_transcript_and_restarts_the_reply_count() {
        let mut t = Transcript::default();
        t.user("q".into());
        t.on_event(SessionEvent::Assistant("old".into()));
        assert!(!t.commit(80, &Theme::default()).is_empty());
        t.clear();
        assert!(t.entries.is_empty());
        assert_eq!(t.assistant_reply(1), None);
        t.user("q2".into());
        t.on_event(SessionEvent::Assistant("fresh".into()));
        assert_eq!(t.assistant_reply(1), Some("fresh"));
        assert!(
            !t.commit(80, &Theme::default()).is_empty(),
            "committing restarts with the new conversation"
        );
    }

    fn span_fg(line: &Line<'static>, containing: &str) -> Option<ratatui::style::Color> {
        line.spans
            .iter()
            .find(|s| s.content.contains(containing))
            .and_then(|s| s.style.fg)
    }

    #[test]
    fn the_default_theme_colours_tools_and_errors_as_the_interface_always_has() {
        use ratatui::style::Color;
        let mut t = Transcript::default();
        t.user("q".into());
        call(&mut t, "a", "shell");
        result(&mut t, "a", "shell");
        call(&mut t, "b", "shell");
        t.on_event(SessionEvent::ToolResult {
            id: "b".into(),
            tool: "shell".into(),
            success: false,
            text: "boom".into(),
        });
        t.error("bad");
        let lines = t.lines(40, &Theme::default());
        assert_eq!(span_fg(&lines[0], "> "), Some(Color::Cyan));
        assert_eq!(span_fg(&lines[2], "✓"), Some(Color::Green));
        assert_eq!(span_fg(&lines[3], "✗"), Some(Color::Red));
        assert_eq!(span_fg(&lines[4], "boom"), None);
        assert_eq!(span_fg(&lines[5], "!"), Some(Color::Red));
    }

    #[test]
    fn another_theme_changes_the_colours() {
        use fontaine_core::config::ThemeName;
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        result(&mut t, "a", "shell");
        let theme = Theme::preset(ThemeName::LightDaltonized);
        let lines = t.lines(40, &theme);
        assert_eq!(span_fg(&lines[0], "✓"), Some(theme.success));
    }

    #[test]
    fn a_streaming_reply_settles_by_line_when_cut_at_newlines() {
        let mut t = Transcript::default();
        delta(&mut t, "one\ntw");
        let first = t.commit_entries(Cut::Lines);
        assert_eq!(first.len(), 1);
        assert_eq!((first[0].skip, first[0].end), (0, Some(4)));
        assert!(t.commit_entries(Cut::Lines).is_empty());
        delta(&mut t, "o\n");
        let second = t.commit_entries(Cut::Lines);
        assert_eq!((second[0].skip, second[0].end), (4, Some(8)));
        t.on_event(SessionEvent::Assistant("one\ntwo\n".into()));
        let rest = t.commit_entries(Cut::Lines);
        assert_eq!((rest[0].skip, rest[0].end), (8, None));
    }

    fn bold_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn warned(width: usize, glyphs: Glyphs, theme: &Theme) -> Vec<Line<'static>> {
        let mut t = Transcript {
            glyphs,
            ..Transcript::default()
        };
        t.warning(
            "Guardian is running on your main model",
            Some("Set `[guardian] model` or run /guardian-model now".into()),
        );
        t.lines(width, theme)
    }

    #[test]
    fn a_warning_has_its_own_glyph_and_colour_and_its_hint_sits_under_the_headline() {
        use ratatui::style::Color;
        let lines = warned(80, Glyphs::UNICODE, &theme());
        assert_eq!(
            text(&lines),
            vec![
                "⚠ Guardian is running on your main model",
                "  Set `[guardian] model` or run /guardian-model now"
            ]
        );
        assert_eq!(span_fg(&lines[0], "⚠"), Some(Color::Yellow));
        assert_eq!(span_fg(&lines[0], "Guardian"), Some(Color::Yellow));
        assert_eq!(span_fg(&lines[1], "Set"), None);
        let mut info = Transcript::default();
        info.notice("fyi");
        let info = info.lines(80, &theme());
        assert_eq!(text(&info), vec!["· fyi"]);
        assert_eq!(span_fg(&info[0], "fyi"), None);
    }

    #[test]
    fn the_warning_follows_the_theme() {
        use fontaine_core::config::ThemeName;
        let theme = Theme::preset(ThemeName::LightAnsi);
        assert_ne!(theme.warning, Theme::default().warning);
        let lines = warned(80, Glyphs::UNICODE, &theme);
        assert_eq!(span_fg(&lines[0], "⚠"), Some(theme.warning));
        assert_eq!(span_fg(&lines[0], "Guardian"), Some(theme.warning));
    }

    #[test]
    fn backticked_spans_and_commands_in_a_hint_are_bold() {
        let lines = warned(80, Glyphs::UNICODE, &theme());
        assert_eq!(bold_text(&lines[1]), "`[guardian] model`/guardian-model");
        assert_eq!(bold_text(&lines[0]), "");
    }

    #[test]
    fn a_hint_wraps_under_the_headline_and_a_span_crossing_a_row_stays_bold() {
        let lines = warned(24, Glyphs::UNICODE, &theme());
        let rows = text(&lines);
        assert!(rows.len() > 3, "{rows:?}");
        assert!(rows.iter().all(|r| r.chars().count() <= 24), "{rows:?}");
        assert!(rows[1..].iter().all(|r| r.starts_with("  ")), "{rows:?}");
        let tick_row = rows.iter().position(|r| r.contains('`')).unwrap();
        let bold_rows: Vec<usize> = (1..lines.len())
            .filter(|i| !bold_text(&lines[*i]).is_empty())
            .collect();
        assert!(bold_rows.len() >= 2, "{rows:?}");
        let span: String = bold_rows
            .iter()
            .take_while(|i| **i <= tick_row + 1)
            .map(|i| bold_text(&lines[*i]))
            .collect();
        assert!(span.contains("[guardian]"), "{span}");
        let all: String = (1..lines.len()).map(|i| bold_text(&lines[i])).collect();
        assert!(all.starts_with("`[guardian] model`/guardian-mode"), "{all}");
    }

    #[test]
    fn ascii_glyphs_replace_both_marks() {
        let lines = warned(80, Glyphs::ASCII, &theme());
        assert!(text(&lines)[0].starts_with("* Guardian"));
        let mut t = Transcript {
            glyphs: Glyphs::ASCII,
            ..Transcript::default()
        };
        t.notice("fyi");
        assert_eq!(text(&t.lines(80, &theme())), vec!["- fyi"]);
        assert_eq!(Glyphs::from_env(None), Glyphs::UNICODE);
        assert_eq!(Glyphs::from_env(Some("")), Glyphs::UNICODE);
        assert_eq!(Glyphs::from_env(Some("0")), Glyphs::UNICODE);
        assert_eq!(Glyphs::from_env(Some("1")), Glyphs::ASCII);
    }

    #[test]
    fn a_budgeted_commit_releases_nothing_while_the_transcript_fits() {
        let mut t = Transcript::default();
        t.notice("a");
        t.notice("b");
        assert!(t.commit_within(40, &theme(), 0).is_empty());
        assert_eq!(t.live_lines(40, &theme()).len(), 2);
    }

    #[test]
    fn a_budgeted_commit_takes_whole_oldest_entries() {
        let mut t = Transcript::default();
        t.user("q".into());
        t.notice("b");
        t.notice("c");
        // The user entry is two rows; one row of budget still releases all of it.
        assert_eq!(text(&t.commit_within(40, &theme(), 1)), vec!["> q", ""]);
        assert_eq!(text(&t.live_lines(40, &theme())), vec!["· b", "· c"]);
        assert_eq!(text(&t.commit_within(40, &theme(), 2)), vec!["· b", "· c"]);
    }

    #[test]
    fn the_stream_budget_counts_rows_older_entries_already_used() {
        let mut t = Transcript::default();
        t.notice("old");
        delta(&mut t, "abcdefghijklmnopqrstuvwx");
        let out = text(&t.commit_within(8, &theme(), 2));
        assert_eq!(out, vec!["· old", "abcdefgh"]);
        assert_eq!(
            text(&t.live_lines(8, &theme())),
            vec!["ijklmnop", "qrstuvwx"]
        );
    }

    #[test]
    fn a_stream_cut_by_a_row_budget_advances_by_at_most_that_many_rows() {
        let mut t = Transcript::default();
        delta(&mut t, "abcdefghijkl");
        let first = t.commit_within(4, &theme(), 1);
        assert_eq!(text(&first), vec!["abcd"]);
        let second = t.commit_within(4, &theme(), 5);
        assert_eq!(text(&second), vec!["efgh"]);
        assert_eq!(text(&t.live_lines(4, &theme())), vec!["ijkl"]);
        let end = Cut::Budget { width: 4, rows: 1 }.end("abcdefghijkl", 8);
        assert_eq!(end, 8, "the last row stays live");
    }

    #[test]
    fn a_pending_call_is_released_when_the_budget_demands_it() {
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        t.notice("after");
        assert_eq!(text(&t.commit_within(40, &theme(), 1)), vec!["• shell a …"]);
        assert_eq!(text(&t.live_lines(40, &theme())), vec!["· after"]);
    }

    #[test]
    fn a_pending_call_stays_pinned_while_older_rows_cover_the_budget() {
        let mut t = Transcript::default();
        t.notice("old");
        call(&mut t, "a", "shell");
        assert_eq!(text(&t.commit_within(40, &theme(), 1)), vec!["· old"]);
        assert_eq!(text(&t.live_lines(40, &theme())), vec!["• shell a …"]);
    }

    #[test]
    fn a_call_written_as_pending_gets_a_follow_up_line_when_it_settles() {
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        call(&mut t, "b", "shell");
        call(&mut t, "c", "shell");
        assert_eq!(t.commit_within(40, &theme(), 3).len(), 3);
        t.guardian_denied("a", "shell", "Rule", "no way");
        t.on_event(SessionEvent::ToolResult {
            id: "b".into(),
            tool: "shell".into(),
            success: false,
            text: "boom".into(),
        });
        t.interrupt_pending();
        let live = text(&t.live_lines(60, &theme())).join("\n");
        assert!(live.contains("denied: [Rule] no way"), "{live}");
        assert!(live.contains("failed: boom"), "{live}");
        assert!(live.contains("interrupted"), "{live}");
    }

    #[test]
    fn clearing_retires_what_was_not_committed_with_a_streaming_offset() {
        let mut t = Transcript::default();
        t.notice("old");
        delta(&mut t, "abcdefghijklmnop");
        assert_eq!(t.commit_within(8, &theme(), 2).len(), 2);
        delta(&mut t, "qrst");
        t.clear();
        t.notice("new");
        assert_eq!(text(&t.take_retired(8, &theme())), vec!["ijklmnop", "qrst"]);
        assert!(t.take_retired(8, &theme()).is_empty());
        assert_eq!(text(&t.live_lines(8, &theme())), vec!["· new"]);
    }

    #[test]
    fn commit_all_writes_unsettled_entries_too() {
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        delta(&mut t, "abcdefghijklmnopqrstuvwx");
        assert_eq!(
            text(&t.commit_all(12, &theme())),
            vec!["• shell a …", "abcdefghijkl", "mnopqrstuvwx"]
        );
        assert!(t.live_lines(12, &theme()).is_empty());
    }

    fn call_with(t: &mut Transcript, id: &str, tool: &str, args: Value) {
        t.on_event(SessionEvent::ToolCall {
            id: id.into(),
            tool: tool.into(),
            args,
        });
    }

    fn result_with(t: &mut Transcript, id: &str, tool: &str, success: bool, body: &str) {
        t.on_event(SessionEvent::ToolResult {
            id: id.into(),
            tool: tool.into(),
            success,
            text: body.into(),
        });
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("row {i}\n")).collect()
    }

    fn preview(path: &str, lines: usize, hidden: usize) -> Box<DiffPreview> {
        use fontaine_core::preview::{DiffKind, DiffLine};
        Box::new(DiffPreview {
            path: path.into(),
            added: lines,
            removed: 0,
            lines: (1..=lines)
                .map(|i| DiffLine {
                    kind: DiffKind::Added,
                    old_no: None,
                    new_no: Some(i),
                    text: format!("new {i}"),
                })
                .collect(),
            hidden,
        })
    }

    #[test]
    fn a_short_one_line_call_without_output_renders_as_it_always_did() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "ls"}));
        result_with(&mut t, "a", "shell", true, "");
        assert_eq!(text(&t.lines(40, &theme())), vec!["• shell ls ✓"]);
    }

    #[test]
    fn a_long_command_folds_to_three_rows_and_counts_the_rest() {
        let mut t = Transcript::default();
        let command: Vec<String> = (1..=12).map(|i| format!("echo {i}")).collect();
        call_with(
            &mut t,
            "a",
            "shell",
            serde_json::json!({"command": command.join("\n")}),
        );
        result_with(&mut t, "a", "shell", true, "");
        assert_eq!(
            text(&t.lines(40, &theme())),
            vec![
                "• shell ✓",
                "    echo 1",
                "    echo 2",
                "    echo 3",
                "    … (+9 rows)",
                "  expand: /expand 1",
            ]
        );
    }

    #[test]
    fn output_shows_its_last_ten_rows_behind_a_gutter() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        result_with(&mut t, "a", "shell", true, &numbered(30));
        let rows = text(&t.lines(40, &theme()));
        assert_eq!(rows[0], "• shell go ✓");
        assert_eq!(rows[1], "  │ … 20 earlier rows");
        assert_eq!(rows[2], "  │ row 21");
        assert_eq!(rows[11], "  │ row 30");
        assert_eq!(rows[12], "  expand: /expand 1");
        assert_eq!(rows.len(), 13);
    }

    #[test]
    fn output_that_fits_has_no_expand_footer() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        result_with(&mut t, "a", "shell", true, "one\ntwo\n");
        assert_eq!(
            text(&t.lines(40, &theme())),
            vec!["• shell go ✓", "  │ one", "  │ two"]
        );
    }

    #[test]
    fn a_failed_call_keeps_its_suffix_and_its_output() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        result_with(&mut t, "a", "shell", false, "boom\ndetail");
        assert_eq!(
            text(&t.lines(40, &theme())),
            vec!["• shell go ✗ failed: boom", "  │ boom", "  │ detail"]
        );
    }

    #[test]
    fn read_and_task_list_calls_get_no_output_block() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "read", serde_json::json!({"path": "f"}));
        result_with(&mut t, "a", "read", true, "file body");
        call_with(&mut t, "b", "todo_write", serde_json::json!({"todos": []}));
        result_with(&mut t, "b", "todo_write", true, "ok");
        assert_eq!(
            text(&t.lines(40, &theme())),
            vec!["• read f ✓", "• todo_write 0 tasks, 0 done ✓"]
        );
    }

    #[test]
    fn output_is_made_safe_before_it_is_kept() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
        result_with(
            &mut t,
            "a",
            "shell",
            true,
            &format!("\x1b[31mred\x07\n\tx\nkey {key}\n50%\r100%"),
        );
        let shown = text(&t.lines(60, &theme())).join("\n");
        assert!(
            !shown.contains('\x1b') && !shown.contains('\x07'),
            "{shown:?}"
        );
        assert!(
            !shown.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{shown}"
        );
        assert!(shown.contains("│     x"), "{shown}");
        assert!(
            shown.contains("│ 100%") && !shown.contains("50%"),
            "{shown}"
        );
        let Entry::Tool { call, .. } = &t.entries[0] else {
            panic!("a tool entry");
        };
        assert_eq!(call, "go");
    }

    #[test]
    fn a_call_is_made_safe_too() {
        let mut t = Transcript::default();
        call_with(
            &mut t,
            "a",
            "shell",
            serde_json::json!({"command": "echo \x1b[2J\nok"}),
        );
        let Entry::Tool { call, .. } = &t.entries[0] else {
            panic!("a tool entry");
        };
        assert_eq!(call, "echo  [2J\nok");
    }

    #[test]
    fn output_over_the_cap_is_clipped_with_the_flag() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        let body = format!("head\n{}tail\n", "x\n".repeat(OUTPUT_KEEP_BYTES));
        result_with(&mut t, "a", "shell", true, &body);
        let Entry::Tool {
            output: Some(output),
            ..
        } = &t.entries[0]
        else {
            panic!("a kept output");
        };
        assert!(output.clipped);
        assert!(output.text.len() <= OUTPUT_KEEP_BYTES + 1);
        assert!(output.text.starts_with("head") && output.text.ends_with("tail"));
        let shown = text(&t.lines(40, &theme()));
        assert_eq!(shown.last().unwrap(), "  expand: /expand 1");
    }

    #[test]
    fn a_card_is_the_same_rows_committed_budgeted_or_live() {
        let build = || {
            let mut t = Transcript::default();
            call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
            result_with(&mut t, "a", "shell", true, &numbered(15));
            t
        };
        let expected = text(&build().lines(40, &theme()));
        assert_eq!(expected.len(), 13);
        assert_eq!(text(&build().commit(40, &theme())), expected);
        assert_eq!(text(&build().commit_within(40, &theme(), 1)), expected);
        assert_eq!(text(&build().live_lines(40, &theme())), expected);
    }

    #[test]
    fn a_card_released_pending_prints_once_with_its_output_and_diff() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "edit", serde_json::json!({"path": "f.rs"}));
        t.notice("after");
        assert_eq!(
            text(&t.commit_within(40, &theme(), 1)),
            vec!["• edit f.rs …"]
        );
        t.on_event(SessionEvent::ToolPreview {
            id: "a".into(),
            preview: preview("f.rs", 2, 0),
        });
        result_with(&mut t, "a", "edit", true, "edited");
        assert_eq!(
            text(&t.commit(40, &theme())),
            vec![
                "· after",
                "• edit f.rs ✓",
                "  f.rs  +2 -0",
                "  1 +new 1",
                "  2 +new 2",
            ]
        );
        assert!(t.live_lines(40, &theme()).is_empty());
    }

    #[test]
    fn a_long_diff_folds_at_twenty_lines() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "write", serde_json::json!({"path": "f.rs"}));
        t.on_event(SessionEvent::ToolPreview {
            id: "a".into(),
            preview: preview("f.rs", 25, 3),
        });
        result_with(&mut t, "a", "write", true, "wrote");
        let rows = text(&t.lines(40, &theme()));
        assert_eq!(rows.len(), 1 + 1 + 20 + 1 + 1);
        assert_eq!(rows[rows.len() - 2], "  … 8 more changed lines");
        assert_eq!(rows[rows.len() - 1], "  expand: /expand 1");
    }

    #[test]
    fn a_preview_attaches_by_id_only() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "edit", serde_json::json!({"path": "x"}));
        call_with(&mut t, "b", "edit", serde_json::json!({"path": "y"}));
        t.on_event(SessionEvent::ToolPreview {
            id: "a".into(),
            preview: preview("x", 1, 0),
        });
        t.on_event(SessionEvent::ToolPreview {
            id: "nope".into(),
            preview: preview("z", 1, 0),
        });
        let diffs: Vec<bool> = t
            .entries
            .iter()
            .map(|e| matches!(e, Entry::Tool { diff: Some(_), .. }))
            .collect();
        assert_eq!(diffs, [true, false]);
    }

    #[test]
    fn expand_prints_the_whole_call_output_and_diff_and_defaults_to_the_latest() {
        let mut t = Transcript::default();
        call_with(
            &mut t,
            "a",
            "shell",
            serde_json::json!({"command": "first"}),
        );
        result_with(&mut t, "a", "shell", true, "one");
        let command: Vec<String> = (1..=5).map(|i| format!("echo {i}")).collect();
        call_with(
            &mut t,
            "b",
            "shell",
            serde_json::json!({"command": command.join("\n")}),
        );
        result_with(&mut t, "b", "shell", true, &numbered(14));
        t.commit(40, &theme());
        t.expand(None).unwrap();
        let rows = text(&t.commit(40, &theme()));
        assert_eq!(rows[0], "• shell ✓");
        assert_eq!(
            &rows[1..6],
            [
                "    echo 1",
                "    echo 2",
                "    echo 3",
                "    echo 4",
                "    echo 5"
            ]
        );
        assert_eq!(rows[6], "  │ row 1");
        assert_eq!(rows[19], "  │ row 14");
        assert_eq!(rows[20], "");
        assert_eq!(rows.len(), 21);
        t.expand(Some(1)).unwrap();
        assert_eq!(
            text(&t.commit(40, &theme())),
            vec!["• shell first ✓", "  │ one", ""]
        );
        assert!(t.expand(Some(9)).is_err());
    }

    #[test]
    fn expand_prefers_the_settled_follow_up_and_an_ordinal_outlives_clear() {
        let mut t = Transcript::default();
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        t.notice("after");
        t.commit_within(40, &theme(), 1);
        result_with(&mut t, "a", "shell", true, "out");
        t.expand(Some(1)).unwrap();
        let Some(Entry::Expanded(inner)) = t.entries.last() else {
            panic!("an expanded entry");
        };
        assert!(matches!(
            &**inner,
            Entry::Tool {
                status: ToolStatus::Done,
                output: Some(_),
                ..
            }
        ));
        t.clear();
        assert!(t.expand(Some(1)).is_err());
        call_with(&mut t, "b", "shell", serde_json::json!({"command": "go"}));
        let Entry::Tool { ordinal, .. } = &t.entries[0] else {
            panic!("a tool entry");
        };
        assert_eq!(*ordinal, 2);
    }

    #[test]
    fn ascii_glyphs_give_the_gutter_an_ascii_form() {
        let mut t = Transcript {
            glyphs: Glyphs::ASCII,
            ..Transcript::default()
        };
        call_with(&mut t, "a", "shell", serde_json::json!({"command": "go"}));
        result_with(&mut t, "a", "shell", true, "out");
        assert_eq!(text(&t.lines(40, &theme()))[1], "  | out");
    }
}
