//! The workspace trust prompt, shown before anything the repository defines may run. Trusting
//! is written to the trust store by the caller; declining holds for this session only.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// What the workspace holds that would run, for the prompt to name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Hooks the repository's settings define.
    pub hooks: usize,
    /// MCP servers the repository's settings define.
    pub servers: Vec<String>,
    /// The repository carries skills, whose commands run when they are used.
    pub skills: bool,
    /// The settings files found under `.fontaine/`.
    pub settings_files: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Trust,
    DontTrust,
}

pub struct TrustPrompt {
    root: PathBuf,
    summary: Summary,
    selected: Choice,
}

impl TrustPrompt {
    pub fn new(root: &Path, summary: &Summary) -> TrustPrompt {
        TrustPrompt {
            root: root.to_path_buf(),
            summary: summary.clone(),
            selected: Choice::DontTrust,
        }
    }

    pub fn selected(&self) -> Choice {
        self.selected
    }

    /// Handles a key; the choice comes back once the human has made one. Nothing is chosen
    /// by default: Enter acts on the highlighted answer, which starts on "don't trust".
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Choice> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if control => Some(Choice::DontTrust),
            KeyCode::Char('y' | 't') if !control => Some(Choice::Trust),
            KeyCode::Char('n' | 'd') | KeyCode::Esc if !control => Some(Choice::DontTrust),
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Char('j' | 'k') => {
                self.selected = match self.selected {
                    Choice::Trust => Choice::DontTrust,
                    Choice::DontTrust => Choice::Trust,
                };
                None
            }
            KeyCode::Enter => Some(self.selected),
            _ => None,
        }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = vec![
            Line::styled(
                "Do you trust the files in this folder?",
                bold.fg(Color::Yellow),
            ),
            Line::raw(self.root.display().to_string()),
            Line::raw(""),
            Line::raw(
                "Fontaine can run what this folder defines: hooks, MCP servers and the commands in skills. If you trust it, they start; if you do not, the session runs without them. Whatever you choose, none of them can approve a tool call for you.",
            ),
            Line::raw(""),
        ];
        let s = &self.summary;
        if s.hooks == 0 && s.servers.is_empty() && !s.skills && s.settings_files.is_empty() {
            out.push(Line::styled(
                "This folder defines nothing that would run.",
                dim,
            ));
        }
        if s.hooks > 0 {
            out.push(Line::raw(format!("  hooks: {}", s.hooks)));
        }
        if !s.servers.is_empty() {
            out.push(Line::raw(format!(
                "  MCP servers: {}",
                s.servers.join(", ")
            )));
        }
        if s.skills {
            out.push(Line::raw("  skills (their commands run when used)"));
        }
        if !s.settings_files.is_empty() {
            out.push(Line::raw(format!(
                "  settings: {}",
                s.settings_files.join(", ")
            )));
        }
        out.push(Line::raw(""));
        for (choice, key, label) in [
            (Choice::Trust, 'y', "trust this folder"),
            (
                Choice::DontTrust,
                'n',
                "do not trust it (asked again next time)",
            ),
        ] {
            let marker = if self.selected == choice { ">" } else { " " };
            let style = if self.selected == choice {
                bold.add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.push(Line::styled(format!("{marker} [{key}] {label}"), style));
        }
        out
    }

    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let [_, middle, _] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(self.lines().len() as u16 + 6),
            Constraint::Min(0),
        ])
        .areas(area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" workspace trust ");
        let inner = block.inner(middle);
        frame.render_widget(block, middle);
        frame.render_widget(
            Paragraph::new(self.lines()).wrap(Wrap { trim: false }),
            inner,
        );
    }
}

/// Shows the prompt on the real terminal and waits for the answer.
pub fn ask(root: &Path, summary: &Summary) -> io::Result<Choice> {
    let mut terminal = ratatui::try_init()?;
    let result = (|| {
        let mut prompt = TrustPrompt::new(root, summary);
        loop {
            terminal.draw(|frame| prompt.draw(frame))?;
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            if let Event::Key(key) = event::read()?
                && let Some(choice) = prompt.handle_key(key)
            {
                return Ok(choice);
            }
        }
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn summary() -> Summary {
        Summary {
            hooks: 2,
            servers: vec!["github".into()],
            skills: true,
            settings_files: vec![".fontaine/settings.toml".into()],
        }
    }

    #[test]
    fn nothing_is_trusted_until_the_human_says_so() {
        let mut prompt = TrustPrompt::new(Path::new("/work/repo"), &summary());
        assert_eq!(prompt.selected(), Choice::DontTrust);
        assert_eq!(
            prompt.handle_key(key(KeyCode::Enter)),
            Some(Choice::DontTrust)
        );
        assert_eq!(prompt.handle_key(key(KeyCode::Down)), None);
        assert_eq!(prompt.handle_key(key(KeyCode::Enter)), Some(Choice::Trust));
        assert_eq!(
            prompt.handle_key(key(KeyCode::Char('y'))),
            Some(Choice::Trust)
        );
        assert_eq!(
            prompt.handle_key(key(KeyCode::Esc)),
            Some(Choice::DontTrust)
        );
        assert_eq!(prompt.handle_key(key(KeyCode::Char('x'))), None);
    }

    #[test]
    fn the_prompt_names_what_would_run() {
        let prompt = TrustPrompt::new(Path::new("/work/repo"), &summary());
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| prompt.draw(f)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        for needle in [
            "trust the files",
            "hooks: 2",
            "github",
            "skills",
            "settings.toml",
        ] {
            assert!(screen.contains(needle), "{needle} missing from\n{screen}");
        }
    }
}
