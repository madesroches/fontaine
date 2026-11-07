//! The `/bug` preview: the exact text that will leave the machine, scrollable, with the level,
//! the submission method and the destination repository.

use std::time::{Duration, Instant};

use fontaine_core::bug_report::Level;
use fontaine_core::bug_submit::{REPO, Submit};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::confirm_prompt::ANSWER_GRACE;
use crate::input::wrap;
use crate::theme::Theme;

/// Preview rows shown at once.
pub const VISIBLE_ROWS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BugAnswer {
    /// The level changed; the report must be built again at it.
    Level(Level),
    Submit {
        method: Submit,
        level: Level,
    },
    Cancel,
}

pub struct BugPrompt {
    pub(crate) id: u64,
    opened: Instant,
    title: String,
    text: String,
    level: Level,
    method: Submit,
    gh_available: bool,
    scroll: usize,
    /// Wrapped row count at the last draw: the scroll bound.
    row_count: std::cell::Cell<usize>,
}

impl BugPrompt {
    pub fn new(title: String, text: String, level: Level, gh_available: bool) -> BugPrompt {
        BugPrompt {
            id: 0,
            opened: Instant::now(),
            title,
            text,
            level,
            method: if gh_available {
                Submit::Gh
            } else {
                Submit::Url
            },
            gh_available,
            scroll: 0,
            row_count: std::cell::Cell::new(0),
        }
    }

    /// Replaces the previewed text after a level change.
    pub fn set_text(&mut self, title: String, text: String, level: Level) {
        self.title = title;
        self.text = text;
        self.level = level;
        self.scroll = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<BugAnswer> {
        self.handle_key_at(key, Instant::now())
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<BugAnswer> {
        if key.kind != KeyEventKind::Press || key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let armed = now.saturating_duration_since(self.opened) >= ANSWER_GRACE;
        match key.code {
            KeyCode::Esc => Some(BugAnswer::Cancel),
            KeyCode::Enter if armed => Some(BugAnswer::Submit {
                method: self.method,
                level: self.level,
            }),
            KeyCode::Char('m') if armed && self.level != Level::Minimal => {
                Some(BugAnswer::Level(Level::Minimal))
            }
            KeyCode::Char('f') if armed && self.level != Level::Full => {
                Some(BugAnswer::Level(Level::Full))
            }
            KeyCode::Char('g') if self.gh_available => {
                self.method = Submit::Gh;
                None
            }
            KeyCode::Char('u') => {
                self.method = Submit::Url;
                None
            }
            KeyCode::Down | KeyCode::PageDown | KeyCode::Char('j') => {
                let step = if key.code == KeyCode::PageDown {
                    VISIBLE_ROWS
                } else {
                    1
                };
                let last = self.row_count.get().saturating_sub(VISIBLE_ROWS);
                self.scroll = (self.scroll + step).min(last);
                None
            }
            KeyCode::Up | KeyCode::PageUp | KeyCode::Char('k') => {
                let step = if key.code == KeyCode::PageUp {
                    VISIBLE_ROWS
                } else {
                    1
                };
                self.scroll = self.scroll.saturating_sub(step);
                None
            }
            _ => None,
        }
    }

    fn rows(&self, width: usize) -> Vec<String> {
        self.text.lines().flat_map(|l| wrap(l, width)).collect()
    }

    pub fn body(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let method = match self.method {
            Submit::Gh => "gh issue create".to_string(),
            Submit::Url => "a prefilled URL you open yourself".to_string(),
        };
        let mut out = vec![
            Line::styled(
                format!("File this bug report to github.com/{REPO}?"),
                bold.fg(theme.heading),
            ),
            Line::raw(format!("title: {}", self.title)),
            Line::raw(format!("level: {} · via {method}", self.level.as_str())),
            Line::styled("This is everything that will be sent:", dim),
        ];
        let rows = self.rows(width);
        self.row_count.set(rows.len());
        let start = self.scroll.min(rows.len().saturating_sub(VISIBLE_ROWS));
        out.extend(
            rows.iter()
                .skip(start)
                .take(VISIBLE_ROWS)
                .map(|r| Line::raw(r.clone())),
        );
        let below = rows.len().saturating_sub(start + VISIBLE_ROWS);
        if below > 0 {
            out.push(Line::styled(
                format!("… {below} more lines (↓ scrolls)"),
                dim,
            ));
        }
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let methods = if self.gh_available {
            "g gh · u url"
        } else {
            "u url (gh is not logged in)"
        };
        vec![Line::styled(
            format!("Enter files it · Esc cancels · m minimal · f full · {methods} · ↑↓ scroll"),
            dim,
        )]
    }

    /// Time before Enter, `m` and `f` answer; a key typed ahead must not file a report.
    pub const GRACE: Duration = ANSWER_GRACE;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn prompt() -> BugPrompt {
        BugPrompt::new("t".into(), "a\nb".into(), Level::Minimal, true)
    }

    #[test]
    fn enter_files_only_after_the_grace_and_esc_always_cancels() {
        let mut p = prompt();
        let opened = p.opened;
        assert_eq!(p.handle_key_at(key(KeyCode::Enter), opened), None);
        assert_eq!(
            p.handle_key_at(key(KeyCode::Esc), opened),
            Some(BugAnswer::Cancel)
        );
        assert_eq!(
            p.handle_key_at(key(KeyCode::Enter), opened + ANSWER_GRACE),
            Some(BugAnswer::Submit {
                method: Submit::Gh,
                level: Level::Minimal
            })
        );
    }

    #[test]
    fn the_level_and_method_can_be_changed() {
        let mut p = prompt();
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(p.handle_key_at(key(KeyCode::Char('m')), later), None);
        assert_eq!(
            p.handle_key_at(key(KeyCode::Char('f')), later),
            Some(BugAnswer::Level(Level::Full))
        );
        p.handle_key_at(key(KeyCode::Char('u')), later);
        assert_eq!(
            p.handle_key_at(key(KeyCode::Enter), later),
            Some(BugAnswer::Submit {
                method: Submit::Url,
                level: Level::Minimal
            })
        );
    }

    #[test]
    fn without_gh_only_the_url_method_exists() {
        let mut p = BugPrompt::new("t".into(), "x".into(), Level::Minimal, false);
        p.handle_key_at(key(KeyCode::Char('g')), p.opened);
        let later = p.opened + ANSWER_GRACE;
        assert_eq!(
            p.handle_key_at(key(KeyCode::Enter), later),
            Some(BugAnswer::Submit {
                method: Submit::Url,
                level: Level::Minimal
            })
        );
    }

    #[test]
    fn a_long_report_scrolls() {
        let text = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut p = BugPrompt::new("t".into(), text, Level::Minimal, false);
        let theme = Theme::preset(Default::default());
        let first = p.body(80, &theme);
        assert!(first.iter().any(|l| l.to_string().contains("more lines")));
        for _ in 0..30 {
            p.handle_key_at(key(KeyCode::Down), p.opened);
        }
        let last = p.body(80, &theme);
        assert!(last.iter().any(|l| l.to_string() == "line 39"));
        for _ in 0..30 {
            p.handle_key_at(key(KeyCode::Down), p.opened);
        }
        p.handle_key_at(key(KeyCode::Up), p.opened);
        let up = p.body(80, &theme);
        assert!(up.iter().any(|l| l.to_string() == "line 38"));
    }
}
