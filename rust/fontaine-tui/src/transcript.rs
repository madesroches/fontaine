//! The conversation as shown: user messages, streaming assistant text, one line per tool call
//! with its outcome, and notices.

use fontaine_core::audit::Decision;
use fontaine_core::turn::SessionEvent;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::input::wrap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Done,
    Failed(String),
    Denied(String),
    Interrupted,
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
    },
    Notice(String),
    Error(String),
}

#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// Entries fully written to scrollback.
    committed: usize,
    /// Chars of `entries[committed]`'s text already written, when it is a streaming reply.
    committed_chars: usize,
}

const SUMMARY_KEYS: &[&str] = &["command", "path", "file_path", "url", "pattern", "query"];

fn summarize(args: &Value) -> String {
    let named = SUMMARY_KEYS
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str));
    let text = named.map_or_else(|| args.to_string(), str::to_string);
    first_line(&text)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

impl Transcript {
    pub fn user(&mut self, text: String) {
        self.finish_streaming();
        self.entries.push(Entry::User(text));
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.finish_streaming();
        self.entries.push(Entry::Notice(text.into()));
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
            && let Entry::Tool { status, .. } = &mut self.entries[i]
            && !matches!(status, ToolStatus::Denied(_))
        {
            *status = new;
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
                self.entries.push(Entry::Tool {
                    id,
                    tool,
                    summary: summarize(&args),
                    status: ToolStatus::Pending,
                });
            }
            SessionEvent::Decision(record) => {
                if record.decision == Decision::Deny {
                    let at =
                        self.find_tool(record.tool_use_id.as_deref().unwrap_or(""), &record.tool);
                    let why = record
                        .explanation
                        .or(record.rule)
                        .unwrap_or_else(|| "denied".to_string());
                    if let Some(i) = at
                        && let Entry::Tool { status, .. } = &mut self.entries[i]
                    {
                        *status = ToolStatus::Denied(first_line(&why));
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
        if let Some(i) = at
            && let Entry::Tool { status, .. } = &mut self.entries[i]
        {
            *status = ToolStatus::Denied(first_line(&format!("[{rule}] {explanation}")));
        }
    }

    /// Calls that never produced a result because the turn was cut short.
    pub fn interrupt_pending(&mut self) {
        self.finish_streaming();
        for entry in &mut self.entries {
            if let Entry::Tool { status, .. } = entry
                && *status == ToolStatus::Pending
            {
                *status = ToolStatus::Interrupted;
            }
        }
    }

    #[cfg(test)]
    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for entry in &self.entries {
            entry_lines(entry, width, 0, &mut out);
        }
        out
    }

    /// The lines that have settled since the last call, for the terminal's scrollback. A
    /// streaming reply is committed a row at a time, keeping its last row live, and nothing
    /// behind an unsettled entry is committed before it.
    pub fn commit(&mut self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        while let Some(entry) = self.entries.get(self.committed) {
            if let Entry::Assistant {
                text,
                streaming: true,
            } = entry
            {
                let mut rows = assistant_rows(text, self.committed_chars, width);
                rows.pop();
                if let Some((_, end)) = rows.last() {
                    self.committed_chars = *end;
                }
                out.extend(rows.into_iter().map(|(line, _)| line));
                break;
            }
            if !is_settled(entry) {
                break;
            }
            entry_lines(entry, width, self.committed_chars, &mut out);
            self.committed += 1;
            self.committed_chars = 0;
        }
        out
    }

    /// What is not in scrollback yet.
    pub fn live_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (i, entry) in self.entries.iter().enumerate().skip(self.committed) {
            let skip = if i == self.committed {
                self.committed_chars
            } else {
                0
            };
            entry_lines(entry, width, skip, &mut out);
        }
        out
    }
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
    let indent = " ".repeat(first.chars().count());
    let body = width.saturating_sub(indent.chars().count());
    let mut head = true;
    for line in text.split('\n') {
        for piece in wrap(line, body) {
            let prefix = if head { first } else { indent.as_str() };
            head = false;
            out.push(row(prefix, style, piece));
        }
    }
}

fn row(prefix: &str, style: Style, piece: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(prefix.to_string(), style),
        Span::raw(piece),
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
            rows.push((row("", Style::default(), piece), offset));
        }
    }
    rows
}

/// `skip` is the chars of an assistant reply already written to scrollback.
fn entry_lines(entry: &Entry, width: usize, skip: usize, out: &mut Vec<Line<'static>>) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    match entry {
        Entry::User(text) => {
            let bold = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);
            prefixed(text, width, "> ", bold, out);
            out.push(Line::default());
        }
        Entry::Assistant { text, .. } => {
            let rest: String = text.chars().skip(skip).collect();
            prefixed(&rest, width, "", Style::default(), out);
        }
        Entry::Tool {
            tool,
            summary,
            status,
            ..
        } => {
            let (mark, style, detail) = match status {
                ToolStatus::Pending => ("…", dim, String::new()),
                ToolStatus::Done => ("✓", Style::default().fg(Color::Green), String::new()),
                ToolStatus::Failed(why) => (
                    "✗",
                    Style::default().fg(Color::Red),
                    format!(" failed: {why}"),
                ),
                ToolStatus::Denied(why) => (
                    "✗",
                    Style::default().fg(Color::Red),
                    format!(" denied: {why}"),
                ),
                ToolStatus::Interrupted => ("−", dim, " interrupted".to_string()),
            };
            let line = truncate(&format!("• {tool} {summary}"), width.saturating_sub(2));
            if let ToolStatus::Denied(why) = status {
                // A denial is the part the human has to read, so it gets its own wrapped lines.
                out.push(Line::from(vec![
                    Span::styled(line, dim),
                    Span::styled(format!(" {mark}"), style),
                ]));
                prefixed(&format!("denied: {why}"), width, "    ", style, out);
                return;
            }
            let detail = truncate(&detail, width.saturating_sub(line.chars().count() + 3));
            out.push(Line::from(vec![
                Span::styled(line, dim),
                Span::styled(format!(" {mark}"), style),
                Span::styled(detail, style),
            ]));
        }
        Entry::Notice(text) => prefixed(text, width, "· ", dim, out),
        Entry::Error(text) => prefixed(text, width, "! ", Style::default().fg(Color::Red), out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn delta(t: &mut Transcript, s: &str) {
        t.on_event(SessionEvent::AssistantDelta(s.into()));
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
        let out = text(&t.commit(40));
        assert_eq!(out, vec!["first", "· heads up", "second"]);
        assert!(t.live_lines(40).is_empty());
    }

    /// Feeds `full` in chunks of `chunk` chars, committing after every chunk.
    fn streamed(full: &str, chunk: usize, width: usize) -> Vec<String> {
        let mut t = Transcript::default();
        t.user("q".into());
        let chars: Vec<char> = full.chars().collect();
        let mut out = Vec::new();
        out.extend(text(&t.commit(width)));
        for piece in chars.chunks(chunk) {
            delta(&mut t, &piece.iter().collect::<String>());
            out.extend(text(&t.commit(width)));
        }
        t.on_event(SessionEvent::Assistant(full.into()));
        out.extend(text(&t.commit(width)));
        assert!(t.live_lines(width).is_empty());
        let mut whole = Transcript::default();
        whole.user("q".into());
        whole.on_event(SessionEvent::Assistant(full.into()));
        assert_eq!(
            out,
            text(&whole.lines(width)),
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
            out.extend(text(&t.commit(widths[i % widths.len()])));
        }
        t.on_event(SessionEvent::Assistant(full.into()));
        out.extend(text(&t.commit(6)));
        assert_eq!(out.concat(), full.replace('\n', ""));
    }

    #[test]
    fn a_pending_call_holds_back_what_follows_it() {
        let mut t = Transcript::default();
        t.user("go".into());
        call(&mut t, "a", "shell");
        t.notice("after");
        assert_eq!(text(&t.commit(40)), vec!["> go", ""]);
        assert_eq!(t.live_lines(40).len(), 2);
        result(&mut t, "a", "shell");
        assert_eq!(text(&t.commit(40)), vec!["• shell a ✓", "· after"]);
        assert!(t.live_lines(40).is_empty());
    }

    #[test]
    fn live_lines_hold_the_partial_row_only() {
        let mut t = Transcript::default();
        delta(&mut t, "abcdef");
        assert!(t.commit(4).len() == 1);
        assert_eq!(text(&t.live_lines(4)), vec!["ef"]);
        delta(&mut t, "gh");
        assert!(t.commit(4).is_empty());
        assert_eq!(text(&t.live_lines(4)), vec!["efgh"]);
        delta(&mut t, "i");
        assert_eq!(text(&t.commit(4)), vec!["efgh"]);
        assert_eq!(text(&t.live_lines(4)), vec!["i"]);
    }

    #[test]
    fn a_committed_call_is_still_found_by_id() {
        let mut t = Transcript::default();
        call(&mut t, "a", "shell");
        result(&mut t, "a", "shell");
        assert_eq!(t.commit(40).len(), 1);
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
}
