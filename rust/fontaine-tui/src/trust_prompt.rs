//! The workspace trust prompt, shown before anything the repository defines may run. Trusting
//! is written to the trust store by the caller; declining holds for this session only.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::accessibility::Source;
use crate::linear::{OverlayEcho, flatten_all};
use crate::server_prompt::ANSWER_GRACE;
use crate::theme::Theme;

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
    opened: Instant,
    theme: Theme,
}

impl TrustPrompt {
    pub fn new(root: &Path, summary: &Summary, theme: Theme) -> TrustPrompt {
        TrustPrompt {
            root: root.to_path_buf(),
            summary: summary.clone(),
            selected: Choice::DontTrust,
            opened: Instant::now(),
            theme,
        }
    }

    pub fn selected(&self) -> Choice {
        self.selected
    }

    /// Handles a key; the choice comes back once the human has made one. Nothing is chosen
    /// by default: Enter acts on the highlighted answer, which starts on "don't trust".
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Choice> {
        self.handle_key_at(key, Instant::now())
    }

    /// Keys typed ahead must not answer a prompt that appears under them, and trusting
    /// persists: answer keys wait out the grace period and ignore auto-repeat. Ctrl-C
    /// declines at any time.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> Option<Choice> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(key.code, KeyCode::Char('c')) && control {
            return Some(Choice::DontTrust);
        }
        let answering = matches!(
            key.code,
            KeyCode::Char('y' | 't' | 'n' | 'd') | KeyCode::Esc | KeyCode::Enter
        );
        if answering
            && (key.kind != KeyEventKind::Press
                || now.saturating_duration_since(self.opened) < ANSWER_GRACE)
        {
            return None;
        }
        match key.code {
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

    pub(crate) fn lines(&self) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut out = vec![
            Line::styled(
                "Do you trust the files in this folder?",
                bold.fg(self.theme.heading),
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

/// Shows the prompt on the real terminal and waits for the answer. With `screen_reader` it is
/// printed as lines instead of drawn.
pub fn ask(
    root: &Path,
    summary: &Summary,
    theme: Theme,
    screen_reader: Option<Source>,
) -> io::Result<Choice> {
    let mut prompt = TrustPrompt::new(root, summary, theme);
    if screen_reader.is_some() {
        return ask_linear(&mut prompt);
    }
    let mut terminal = ratatui::try_init()?;
    let result = (|| {
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

fn ask_linear(prompt: &mut TrustPrompt) -> io::Result<Choice> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
        previous_hook(info);
    }));
    let result = ask_with(prompt, &mut io::stdout(), next_key, Instant::now);
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    result
}

/// Blocks until the terminal reports a key.
fn next_key() -> io::Result<KeyEvent> {
    loop {
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            return Ok(key);
        }
    }
}

/// The prompt as plain lines on `out`, announcing again whatever changes as keys arrive, until
/// a key answers. `keys` and `clock` are what the terminal and the time are in a real run.
fn ask_with(
    prompt: &mut TrustPrompt,
    out: &mut impl Write,
    mut keys: impl FnMut() -> io::Result<KeyEvent>,
    clock: impl Fn() -> Instant,
) -> io::Result<Choice> {
    let mut echo = OverlayEcho::default();
    loop {
        let shown = echo.update("workspace trust", None, flatten_all(&prompt.lines()));
        for line in shown.lines {
            write!(out, "{line}\r\n")?;
        }
        out.flush()?;
        if let Some(choice) = prompt.handle_key_at(keys()?, clock()) {
            return Ok(choice);
        }
    }
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
    fn typed_ahead_keys_do_not_answer() {
        let mut prompt = TrustPrompt::new(Path::new("/work/repo"), &summary(), Theme::default());
        let opened = prompt.opened;
        let y = key(KeyCode::Char('y'));
        assert_eq!(prompt.handle_key_at(y, opened), None);
        assert_eq!(prompt.handle_key_at(key(KeyCode::Enter), opened), None);
        let later = opened + ANSWER_GRACE;
        let mut repeat = y;
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(prompt.handle_key_at(repeat, later), None);
        assert_eq!(prompt.handle_key_at(y, later), Some(Choice::Trust));
    }

    #[test]
    fn nothing_is_trusted_until_the_human_says_so() {
        let mut prompt = TrustPrompt::new(Path::new("/work/repo"), &summary(), Theme::default());
        prompt.opened -= ANSWER_GRACE;
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
        let prompt = TrustPrompt::new(Path::new("/work/repo"), &summary(), Theme::default());
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

    #[test]
    fn the_linear_prompt_prints_its_lines_and_answers_once_the_grace_period_is_over() {
        let mut prompt = TrustPrompt::new(Path::new("/work/repo"), &summary(), Theme::default());
        let opened = prompt.opened;
        let mut script = vec![
            key(KeyCode::Char('y')),
            key(KeyCode::Down),
            key(KeyCode::Char('y')),
        ]
        .into_iter();
        let ticks = std::cell::Cell::new(0);
        let clock = || {
            ticks.set(ticks.get() + 1);
            // The first key arrives inside the grace period, the rest after it.
            if ticks.get() == 1 {
                opened
            } else {
                opened + ANSWER_GRACE
            }
        };
        let mut out = Vec::new();
        let choice = ask_with(
            &mut prompt,
            &mut out,
            || script.next().ok_or_else(|| io::Error::other("out of keys")),
            clock,
        )
        .unwrap();
        assert_eq!(choice, Choice::Trust);
        let out = String::from_utf8(out).unwrap();
        for needle in [
            "workspace trust:\r\n",
            "Do you trust the files in this folder?\r\n",
            "hooks: 2",
            "selected: [n] do not trust it (asked again next time)\r\n",
            "selected: [y] trust this folder\r\n",
        ] {
            assert!(out.contains(needle), "{needle} missing from {out:?}");
        }
        assert_eq!(out.matches("workspace trust:").count(), 1, "{out:?}");
        assert!(out.chars().all(|c| !('\u{2500}'..='\u{257f}').contains(&c)));
        assert!(out.replace("\r\n", "").find('\n').is_none());
    }
}
