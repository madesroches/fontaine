//! The `/resume` overlay: the sessions started in this workspace, newest first, narrowed by a
//! substring filter typed into it.

use std::time::SystemTime;

use fontaine_core::session::SessionInfo;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

/// The most rows the body shows at once.
const MAX_ROWS: usize = 14;

/// Shown for a session that has neither a name nor a message.
const NO_PROMPT: &str = "(no messages)";

/// What the human did with the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeKey {
    None,
    Close,
    /// Switch to the session with this id.
    Pick(String),
}

pub struct ResumePicker {
    sessions: Vec<SessionInfo>,
    now: SystemTime,
    query: String,
    /// Index into the visible rows.
    selected: usize,
}

/// How long ago `updated` was, at the coarseness a list needs.
fn age(updated: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(updated).unwrap_or_default().as_secs();
    match secs {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

impl ResumePicker {
    /// `sessions` are listed in the order given, which is newest first; `now` dates the ages.
    pub fn new(sessions: Vec<SessionInfo>, now: SystemTime) -> ResumePicker {
        ResumePicker {
            sessions,
            now,
            query: String::new(),
            selected: 0,
        }
    }

    fn visible(&self) -> Vec<&SessionInfo> {
        let wanted = self.query.to_lowercase();
        self.sessions
            .iter()
            .filter(|info| {
                wanted.is_empty()
                    || [
                        info.name.as_deref(),
                        info.first_prompt.as_deref(),
                        Some(info.id.as_str()),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|text| text.to_lowercase().contains(&wanted))
            })
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ResumeKey {
        if key.kind == KeyEventKind::Release {
            return ResumeKey::None;
        }
        match key.code {
            KeyCode::Esc => return ResumeKey::Close,
            KeyCode::Enter => {
                return match self.visible().get(self.selected) {
                    Some(info) => ResumeKey::Pick(info.id.clone()),
                    None => ResumeKey::None,
                };
            }
            KeyCode::Up | KeyCode::Down => {
                let n = self.visible().len();
                if n > 0 {
                    self.selected = if key.code == KeyCode::Up {
                        (self.selected + n - 1) % n
                    } else {
                        (self.selected + 1) % n
                    };
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        ResumeKey::None
    }

    /// `max_rows` bounds the rows under the header. `plain` words the branch marker and adds
    /// the id, for a renderer that reads rows out one at a time.
    pub fn body_as(&self, width: usize, max_rows: usize, plain: bool) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let clip = |text: String| -> String { text.chars().take(width).collect() };
        let mut out = vec![Line::raw(clip(
            "sessions started in this directory".to_string(),
        ))];
        let rows = self.visible();
        if rows.is_empty() {
            out.push(Line::styled(
                if !self.query.is_empty() {
                    clip(format!("no session matches \"{}\"", self.query))
                } else {
                    "no other session was started in this directory".to_string()
                },
                dim,
            ));
            return out;
        }
        let lines: Vec<Line<'static>> = rows
            .iter()
            .enumerate()
            .map(|(i, info)| {
                let label = info
                    .name
                    .as_deref()
                    .or(info.first_prompt.as_deref())
                    .unwrap_or(NO_PROMPT);
                let when = age(info.updated, self.now);
                let text = if plain {
                    let branch = if info.branched_from.is_some() {
                        ", branch"
                    } else {
                        ""
                    };
                    format!("{label} ({when}{branch}, {})", short(&info.id))
                } else {
                    let marker = if info.branched_from.is_some() {
                        "⎇"
                    } else {
                        " "
                    };
                    let room = width.saturating_sub(when.chars().count() + 6).max(10);
                    let label = if label.chars().count() > room {
                        let head: String = label.chars().take(room - 1).collect();
                        format!("{head}…")
                    } else {
                        label.to_string()
                    };
                    format!("{marker} {label}  {when}")
                };
                let style = if i == self.selected {
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::styled(clip(format!("  {text}")), style)
            })
            .collect();
        let window = max_rows.clamp(1, MAX_ROWS);
        let start = if lines.len() > window {
            self.selected
                .saturating_sub(window / 2)
                .min(lines.len() - window)
        } else {
            0
        };
        out.extend(lines.into_iter().skip(start).take(window));
        out
    }

    /// The filter as a prompt and what is typed, for a renderer that echoes typing instead of
    /// redrawing the footer.
    pub fn field(&self) -> (&'static str, String) {
        ("filter: ", self.query.clone())
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        vec![
            Line::raw(format!("filter: {}▏", self.query)),
            Line::styled(
                "type to filter · ↑/↓ select · Enter resume · Esc cancel",
                dim,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn info(id: &str, name: Option<&str>, prompt: Option<&str>, secs_ago: u64) -> SessionInfo {
        SessionInfo {
            id: id.to_string(),
            name: name.map(str::to_string),
            first_prompt: prompt.map(str::to_string),
            updated: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - secs_ago),
            primary_root: "/work".to_string(),
            branched_from: None,
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn picker() -> ResumePicker {
        let mut branch = info("cccccccc-0000", None, Some("fix the parser"), 90_000);
        branch.branched_from = Some("aaaaaaaa-0000".to_string());
        ResumePicker::new(
            vec![
                info("aaaaaaaa-0000", Some("demo"), Some("hello there"), 30),
                info("bbbbbbbb-0000", None, Some("write the docs"), 7_300),
                branch,
            ],
            now(),
        )
    }

    #[test]
    fn rows_show_the_name_or_first_prompt_the_age_and_a_branch_marker() {
        let rows = text(&picker().body_as(80, 10, false));
        assert_eq!(rows[1], "    demo  just now");
        assert_eq!(rows[2], "    write the docs  2h ago");
        assert_eq!(rows[3], "  ⎇ fix the parser  1d ago");
    }

    #[test]
    fn the_plain_form_words_the_marker_and_carries_the_id() {
        let rows = text(&picker().body_as(80, 10, true));
        assert_eq!(rows[1], "  demo (just now, aaaaaaaa)");
        assert_eq!(rows[3], "  fix the parser (1d ago, branch, cccccccc)");
    }

    #[test]
    fn typing_filters_by_name_prompt_and_id_without_regard_to_case() {
        let mut p = picker();
        for c in "DOCS".chars() {
            p.handle_key(key(KeyCode::Char(c)));
        }
        let rows = text(&p.body_as(80, 10, false));
        assert_eq!(rows.len(), 2);
        assert!(rows[1].contains("write the docs"));
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            ResumeKey::Pick("bbbbbbbb-0000".to_string())
        );
        let mut by_id = picker();
        for c in "cccc".chars() {
            by_id.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(by_id.visible().len(), 1);
        let mut by_name = picker();
        for c in "dem".chars() {
            by_name.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(by_name.visible()[0].id, "aaaaaaaa-0000");
    }

    #[test]
    fn arrows_wrap_and_enter_picks_the_selected_row() {
        let mut p = picker();
        p.handle_key(key(KeyCode::Up));
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            ResumeKey::Pick("cccccccc-0000".to_string())
        );
        p.handle_key(key(KeyCode::Down));
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            ResumeKey::Pick("aaaaaaaa-0000".to_string())
        );
        assert_eq!(p.handle_key(key(KeyCode::Esc)), ResumeKey::Close);
    }

    #[test]
    fn an_empty_list_and_an_unmatched_filter_say_so() {
        let mut p = ResumePicker::new(Vec::new(), now());
        assert!(text(&p.body_as(80, 10, false))[1].contains("no other session"));
        assert_eq!(p.handle_key(key(KeyCode::Enter)), ResumeKey::None);
        let mut q = picker();
        q.handle_key(key(KeyCode::Char('z')));
        assert!(text(&q.body_as(80, 10, false))[1].contains("no session matches \"z\""));
    }

    #[test]
    fn the_selected_row_is_reversed() {
        let lines = picker().body_as(80, 10, false);
        assert!(lines[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!lines[2].style.add_modifier.contains(Modifier::REVERSED));
    }
}
