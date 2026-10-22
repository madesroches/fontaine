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
        self.entries.push(Entry::User(text));
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::Notice(text.into()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
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

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for entry in &self.entries {
            entry_lines(entry, width, &mut out);
        }
        out
    }
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
            out.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::raw(piece),
            ]));
        }
    }
}

fn entry_lines(entry: &Entry, width: usize, out: &mut Vec<Line<'static>>) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    match entry {
        Entry::User(text) => {
            let bold = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);
            prefixed(text, width, "> ", bold, out);
            out.push(Line::default());
        }
        Entry::Assistant { text, .. } => prefixed(text, width, "", Style::default(), out),
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
