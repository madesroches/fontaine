//! The provider dialog: the configured accounts and what each authenticates with, and the
//! actions on them (add, log in, log out, remove). It is a pure state machine: keys and results
//! go in, requests for the host to carry out come out, and nothing here touches a file, the
//! network or the session. The `/login` overlay and the pre-session setup screen both host it.

use fontaine_core::accounts::{
    self, AccountRow, BaseUrlPolicy, NewAccount, SignInState, SignInStep,
};
use fontaine_core::config::AccountKind;
use fontaine_core::providers::CredentialPolicy;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

/// A secret typed or pasted into the dialog. It has no `Display` and its `Debug` is redacted, so
/// it cannot be formatted into a log line or a transcript by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Secret {
        Secret(text.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// What the dialog asks its host to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKey {
    None,
    Close,
    Add(NewAccount),
    StoreKey {
        account: String,
        key: Secret,
    },
    SignIn(String),
    CancelSignIn,
    LogOut(String),
    Remove(String),
    /// Choose a model of this account.
    PickModel(String),
}

/// What the host tells the dialog back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The accounts, freshly listed.
    Rows(Vec<AccountRow>),
    /// The account is in the settings now (its row is already in the last `Rows`); log in to it.
    Added {
        account: String,
    },
    /// A step of the running sign-in.
    Step(SignInStep),
    /// A key was stored or a sign-in finished.
    LoggedIn {
        account: String,
        summary: String,
    },
    /// A log-out or removal went through.
    Done(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    LogOut,
    Remove,
}

enum Screen {
    List,
    Kind {
        selected: usize,
    },
    Name {
        kind: AccountKind,
        input: String,
        error: Option<String>,
    },
    BaseUrl {
        kind: AccountKind,
        name: String,
        input: String,
        error: Option<String>,
    },
    Key {
        account: String,
        input: String,
        error: Option<String>,
    },
    Waiting {
        account: String,
        url: Option<String>,
        code: Option<String>,
        notes: Vec<String>,
    },
    Confirm {
        what: Confirm,
        account: String,
    },
    Message {
        text: String,
        failed: bool,
    },
}

pub struct ProviderDialog {
    rows: Vec<AccountRow>,
    /// Index into the rows, with one more for "add account".
    selected: usize,
    screen: Screen,
    /// The account the add flow created: once it is logged in, or the log-in is skipped, the
    /// host opens the model picker on it.
    adding: Option<String>,
    /// A line on the list, such as an unknown account name or a cancelled sign-in.
    notice: Option<String>,
}

fn is_text(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(c)
        }
        _ => None,
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl ProviderDialog {
    /// A dialog on the list. With `target`, it opens on that account's log-in instead, and the
    /// request that starts it comes back (a sign-in); an unknown name opens the list with an
    /// error line.
    pub fn new(rows: Vec<AccountRow>, target: Option<&str>) -> (ProviderDialog, DialogKey) {
        let mut dialog = ProviderDialog {
            rows,
            selected: 0,
            screen: Screen::List,
            adding: None,
            notice: None,
        };
        let key = match target.filter(|name| !name.is_empty()) {
            None => DialogKey::None,
            Some(name) if dialog.row(name).is_some() => dialog.login(name),
            Some(name) => {
                let known: Vec<&str> = dialog.rows.iter().map(|r| r.name.as_str()).collect();
                dialog.notice = Some(format!(
                    "unknown account `{name}`; configured accounts: {}",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
                DialogKey::None
            }
        };
        (dialog, key)
    }

    fn row(&self, name: &str) -> Option<&AccountRow> {
        self.rows.iter().find(|r| r.name == name)
    }

    /// Moves to the log-in action of `account`: a sign-in for a kind that has one, a key prompt
    /// otherwise.
    fn login(&mut self, account: &str) -> DialogKey {
        let Some(row) = self.row(account) else {
            self.screen = Screen::Message {
                text: format!("unknown account `{account}`"),
                failed: true,
            };
            return DialogKey::None;
        };
        if row.kind.provider().credential() == CredentialPolicy::SignIn {
            self.screen = Screen::Waiting {
                account: account.to_string(),
                url: None,
                code: None,
                notes: Vec::new(),
            };
            return DialogKey::SignIn(account.to_string());
        }
        self.screen = Screen::Key {
            account: account.to_string(),
            input: String::new(),
            error: None,
        };
        DialogKey::None
    }

    fn selected_account(&self) -> Option<String> {
        self.rows.get(self.selected).map(|r| r.name.clone())
    }

    fn back_to_list(&mut self) {
        self.screen = Screen::List;
        self.adding = None;
        self.selected = self.selected.min(self.rows.len());
    }

    fn key_is_optional(&self, account: &str) -> bool {
        self.row(account)
            .is_some_and(|r| r.kind.provider().credential() == CredentialPolicy::Optional)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogKey {
        if key.kind == KeyEventKind::Release {
            return DialogKey::None;
        }
        match &self.screen {
            Screen::List => self.list_key(key),
            Screen::Kind { .. } => self.kind_key(key),
            Screen::Name { .. } => self.name_key(key),
            Screen::BaseUrl { .. } => self.base_url_key(key),
            Screen::Key { .. } => self.secret_key(key),
            Screen::Waiting { .. } => {
                if key.code == KeyCode::Esc {
                    self.back_to_list();
                    self.notice = Some("sign-in cancelled".to_string());
                    return DialogKey::CancelSignIn;
                }
                DialogKey::None
            }
            Screen::Confirm { .. } => self.confirm_key(key),
            Screen::Message { .. } => {
                self.back_to_list();
                DialogKey::None
            }
        }
    }

    fn list_key(&mut self, key: KeyEvent) -> DialogKey {
        self.notice = None;
        let count = self.rows.len() + 1;
        match key.code {
            KeyCode::Esc => return DialogKey::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = (self.selected + count - 1) % count,
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1) % count,
            KeyCode::Enter => {
                return match self.selected_account() {
                    Some(account) => self.login(&account),
                    None => self.start_add(),
                };
            }
            KeyCode::Char('a') => return self.start_add(),
            KeyCode::Char('m') => {
                if let Some(account) = self.selected_account() {
                    return DialogKey::PickModel(account);
                }
            }
            KeyCode::Char(c @ ('o' | 'd')) => {
                if let Some(account) = self.selected_account() {
                    let what = if c == 'o' {
                        Confirm::LogOut
                    } else {
                        Confirm::Remove
                    };
                    self.screen = Screen::Confirm { what, account };
                }
            }
            _ => {}
        }
        DialogKey::None
    }

    fn start_add(&mut self) -> DialogKey {
        self.screen = Screen::Kind { selected: 0 };
        DialogKey::None
    }

    fn kind_key(&mut self, key: KeyEvent) -> DialogKey {
        let kinds = accounts::kinds();
        let Screen::Kind { selected } = &mut self.screen else {
            return DialogKey::None;
        };
        let count = kinds.len();
        match key.code {
            KeyCode::Esc => self.back_to_list(),
            KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + count - 1) % count,
            KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1) % count,
            KeyCode::Enter => {
                self.screen = Screen::Name {
                    kind: kinds[*selected],
                    input: String::new(),
                    error: None,
                };
            }
            _ => {}
        }
        DialogKey::None
    }

    fn name_problem(&self, name: &str) -> Option<String> {
        accounts::check_name_in(name, &|taken| self.rows.iter().any(|r| r.name == taken)).err()
    }

    fn name_key(&mut self, key: KeyEvent) -> DialogKey {
        if key.code == KeyCode::Esc {
            self.back_to_list();
            return DialogKey::None;
        }
        let Screen::Name { kind, input, error } = &mut self.screen else {
            return DialogKey::None;
        };
        let kind = *kind;
        match key.code {
            KeyCode::Backspace => {
                input.pop();
                *error = None;
            }
            KeyCode::Enter => {
                let name = input.clone();
                if let Some(problem) = self.name_problem(&name) {
                    if let Screen::Name { error, .. } = &mut self.screen {
                        *error = Some(problem);
                    }
                    return DialogKey::None;
                }
                if kind.provider().base_url_policy() == BaseUrlPolicy::Fixed {
                    return DialogKey::Add(NewAccount {
                        name,
                        kind,
                        base_url: None,
                    });
                }
                self.screen = Screen::BaseUrl {
                    kind,
                    name,
                    input: String::new(),
                    error: None,
                };
            }
            _ => {
                if let Some(c) = is_text(&key) {
                    input.push(c);
                    *error = None;
                }
            }
        }
        DialogKey::None
    }

    fn base_url_key(&mut self, key: KeyEvent) -> DialogKey {
        if key.code == KeyCode::Esc {
            self.back_to_list();
            return DialogKey::None;
        }
        let Screen::BaseUrl {
            kind,
            name,
            input,
            error,
        } = &mut self.screen
        else {
            return DialogKey::None;
        };
        match key.code {
            KeyCode::Backspace => {
                input.pop();
                *error = None;
            }
            KeyCode::Enter => {
                let url = input.trim();
                if url.is_empty() {
                    if kind.provider().base_url_policy() == BaseUrlPolicy::Required {
                        *error = Some("a base URL is required for this kind".to_string());
                        return DialogKey::None;
                    }
                    return DialogKey::Add(NewAccount {
                        name: name.clone(),
                        kind: *kind,
                        base_url: None,
                    });
                }
                return DialogKey::Add(NewAccount {
                    name: name.clone(),
                    kind: *kind,
                    base_url: Some(url.to_string()),
                });
            }
            _ => {
                if let Some(c) = is_text(&key) {
                    input.push(c);
                    *error = None;
                }
            }
        }
        DialogKey::None
    }

    fn secret_key(&mut self, key: KeyEvent) -> DialogKey {
        if key.code == KeyCode::Esc {
            self.back_to_list();
            return DialogKey::None;
        }
        let optional = match &self.screen {
            Screen::Key { account, .. } => self.key_is_optional(account),
            _ => false,
        };
        let Screen::Key {
            account,
            input,
            error,
        } = &mut self.screen
        else {
            return DialogKey::None;
        };
        match key.code {
            KeyCode::Backspace => {
                input.pop();
                *error = None;
            }
            KeyCode::Enter => {
                let account = account.clone();
                if input.is_empty() {
                    if !optional {
                        *error = Some("a key is required".to_string());
                        return DialogKey::None;
                    }
                    // Skipping the key of a keyless kind goes on as a finished log-in.
                    let adding = self.adding.take().is_some_and(|name| name == account);
                    if adding {
                        return DialogKey::PickModel(account);
                    }
                    self.back_to_list();
                    return DialogKey::None;
                }
                let key = Secret::new(std::mem::take(input));
                return DialogKey::StoreKey { account, key };
            }
            _ => {
                if let Some(c) = is_text(&key) {
                    input.push(c);
                    *error = None;
                }
            }
        }
        DialogKey::None
    }

    fn confirm_key(&mut self, key: KeyEvent) -> DialogKey {
        let Screen::Confirm { what, account } = &self.screen else {
            return DialogKey::None;
        };
        let (what, account) = (*what, account.clone());
        match key.code {
            KeyCode::Char('y') => {
                // The result decides what is shown; until then the question stays.
                match what {
                    Confirm::LogOut => DialogKey::LogOut(account),
                    Confirm::Remove => DialogKey::Remove(account),
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.back_to_list();
                DialogKey::None
            }
            _ => DialogKey::None,
        }
    }

    /// A paste goes into the text field the dialog has open and nowhere else. Line breaks are
    /// dropped, and a key is trimmed.
    pub fn handle_paste(&mut self, text: &str) {
        let single: String = text.replace(['\r', '\n'], "");
        match &mut self.screen {
            Screen::Key { input, error, .. } => {
                input.push_str(single.trim());
                *error = None;
            }
            Screen::Name { input, error, .. } | Screen::BaseUrl { input, error, .. } => {
                input.push_str(single.trim());
                *error = None;
            }
            _ => {}
        }
    }

    /// Takes in what the host did. A finished log-in in the add flow, or a new account that
    /// needs a log-in, comes back as the next request.
    pub fn finish(&mut self, outcome: Outcome) -> DialogKey {
        match outcome {
            Outcome::Rows(rows) => {
                self.rows = rows;
                self.selected = self.selected.min(self.rows.len());
            }
            Outcome::Added { account } => {
                self.adding = Some(account.clone());
                return self.login(&account);
            }
            Outcome::Step(step) => {
                if let Screen::Waiting {
                    url, code, notes, ..
                } = &mut self.screen
                {
                    match step {
                        SignInStep::Code {
                            verification_url,
                            user_code,
                        } => {
                            *url = Some(verification_url);
                            *code = Some(user_code);
                        }
                        SignInStep::Waiting => {}
                        SignInStep::Warning(text) => notes.push(text),
                    }
                }
            }
            Outcome::LoggedIn { account, summary } => {
                if self.adding.take().is_some_and(|name| name == account) {
                    return DialogKey::PickModel(account);
                }
                self.screen = Screen::Message {
                    text: summary,
                    failed: false,
                };
            }
            Outcome::Done(text) => {
                self.adding = None;
                self.selected = self.selected.min(self.rows.len());
                self.screen = Screen::Message {
                    text,
                    failed: false,
                };
            }
            Outcome::Failed(text) => self.fail(text),
        }
        DialogKey::None
    }

    fn fail(&mut self, text: String) {
        match &mut self.screen {
            Screen::Name { error, .. }
            | Screen::BaseUrl { error, .. }
            | Screen::Key { error, .. } => *error = Some(text),
            _ => {
                self.adding = None;
                self.screen = Screen::Message { text, failed: true };
            }
        }
    }

    /// The rows under the title, at most `max_rows` of them.
    pub fn body(&self, width: usize, max_rows: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let warn = Style::default().fg(Color::Yellow);
        let line = |text: String| Line::raw(clip(&text, width));
        match &self.screen {
            Screen::List => self.list_body(width, max_rows),
            Screen::Kind { selected } => {
                let mut out = vec![Line::styled("kind of account", bold)];
                for (i, kind) in accounts::kinds().into_iter().enumerate() {
                    let text = clip(&format!("  {}", kind.as_str()), width);
                    out.push(if i == *selected {
                        Line::styled(text, bold.add_modifier(Modifier::REVERSED))
                    } else {
                        Line::raw(text)
                    });
                }
                out
            }
            Screen::Name { kind, input, error } => {
                let mut out = vec![
                    line(format!("new {} account", kind.as_str())),
                    line(format!("name: {input}▏")),
                ];
                let problem = error.clone().or_else(|| {
                    (!input.is_empty())
                        .then(|| self.name_problem(input))
                        .flatten()
                });
                if let Some(problem) = problem {
                    out.push(Line::styled(clip(&problem, width), warn));
                }
                out
            }
            Screen::BaseUrl {
                kind,
                name,
                input,
                error,
            } => {
                let required = kind.provider().base_url_policy() == BaseUrlPolicy::Required;
                let mut out = vec![
                    line(format!("account `{name}` ({})", kind.as_str())),
                    line(format!("base URL: {input}▏")),
                ];
                if let Some(default) = kind.provider().default_endpoint() {
                    out.push(Line::styled(
                        clip(&format!("empty for the default, {default}"), width),
                        dim,
                    ));
                } else if !required {
                    out.push(Line::styled("empty for the default", dim));
                }
                if let Some(error) = error {
                    out.push(Line::styled(clip(error, width), warn));
                }
                out
            }
            Screen::Key {
                account,
                input,
                error,
            } => {
                let masked: String = "•".repeat(input.chars().count().min(width.saturating_sub(2)));
                let mut out = vec![
                    line(format!("API key for `{account}`")),
                    line(format!("{masked}▏")),
                ];
                if let Some(SignInState::KeyFromEnv { var } | SignInState::EnvUnset { var }) =
                    self.row(account).map(|r| &r.state)
                {
                    out.push(Line::styled(
                        clip(
                            &format!(
                                "this account sets api_key_env = \"{var}\", so a stored key is ignored while it is set"
                            ),
                            width,
                        ),
                        warn,
                    ));
                }
                if let Some(error) = error {
                    out.push(Line::styled(clip(error, width), warn));
                }
                out
            }
            Screen::Waiting {
                account,
                url,
                code,
                notes,
            } => {
                let mut out = vec![line(format!("signing in to `{account}`"))];
                match (url, code) {
                    (Some(url), Some(code)) => {
                        out.push(line(format!("open {url}")));
                        out.push(line(format!("and confirm the code {code}")));
                        out.push(Line::styled("waiting for approval…", dim));
                    }
                    _ => out.push(Line::styled("contacting the console…", dim)),
                }
                for note in notes {
                    out.push(Line::styled(clip(note, width), warn));
                }
                out
            }
            Screen::Confirm { what, account } => match what {
                Confirm::LogOut => vec![
                    line(format!("Log out of `{account}`?")),
                    Line::styled(
                        "This removes its stored credential from this machine; nothing is revoked at the provider.",
                        dim,
                    ),
                ],
                Confirm::Remove => vec![
                    line(format!("Remove account `{account}`?")),
                    Line::styled(
                        "This deletes it from your settings file and removes its stored credential.",
                        dim,
                    ),
                ],
            },
            Screen::Message { text, failed } => text
                .lines()
                .map(|l| {
                    if *failed {
                        Line::styled(clip(l, width), warn)
                    } else {
                        line(l.to_string())
                    }
                })
                .collect(),
        }
    }

    fn list_body(&self, width: usize, max_rows: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let warn = Style::default().fg(Color::Yellow);
        let name_w = self
            .rows
            .iter()
            .map(|r| r.name.chars().count())
            .max()
            .unwrap_or(0);
        let kind_w = self
            .rows
            .iter()
            .map(|r| r.kind.as_str().chars().count())
            .max()
            .unwrap_or(0);
        let endpoint_w = self
            .rows
            .iter()
            .map(|r| r.endpoint.chars().count())
            .max()
            .unwrap_or(0)
            .min(32);
        let mut lines: Vec<Line<'static>> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let marker = if i == self.selected { ">" } else { " " };
                let text = format!(
                    "{marker} {:<name_w$}  {:<kind_w$}  {:<endpoint_w$}  {}",
                    row.name,
                    row.kind.as_str(),
                    clip(&row.endpoint, endpoint_w),
                    row.state,
                );
                let text = clip(&text, width);
                if i == self.selected {
                    Line::styled(text, bold.add_modifier(Modifier::REVERSED))
                } else {
                    Line::raw(text)
                }
            })
            .collect();
        let add = self.selected == self.rows.len();
        lines.push(if add {
            Line::styled(
                clip("> + add account", width),
                bold.add_modifier(Modifier::REVERSED),
            )
        } else {
            Line::raw("  + add account")
        });
        let window = max_rows.max(1);
        let mut out = Vec::new();
        if let Some(notice) = &self.notice {
            out.push(Line::styled(clip(notice, width), warn));
        }
        let room = window.saturating_sub(out.len()).max(1);
        let start = if lines.len() > room {
            (self.selected + 1)
                .saturating_sub(room)
                .min(lines.len() - room)
        } else {
            0
        };
        out.extend(lines.into_iter().skip(start).take(room));
        out
    }

    pub fn footer(&self) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let text = match &self.screen {
            Screen::List => {
                "Enter log in · m pick model · o log out · d remove · a add · Esc close"
            }
            Screen::Kind { .. } => "↑/↓ select · Enter next · Esc cancel",
            Screen::Name { .. } => "type a name · Enter next · Esc cancel",
            Screen::BaseUrl { kind, .. } => {
                if kind.provider().base_url_policy() == BaseUrlPolicy::Required {
                    "type the URL · Enter add the account · Esc cancel"
                } else {
                    "type the URL, or Enter on empty for the default · Esc cancel"
                }
            }
            Screen::Key { account, input, .. } => {
                if input.is_empty() && self.key_is_optional(account) {
                    "paste or type the key (hidden) · Enter to skip · Esc cancel"
                } else {
                    "paste or type the key (hidden) · Enter store · Esc cancel"
                }
            }
            Screen::Waiting { .. } => "Esc cancel the sign-in",
            Screen::Confirm { .. } => "y confirm · n/Esc cancel",
            Screen::Message { .. } => "press any key",
        };
        vec![Line::styled(text, dim)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::accounts::SignInState;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn row(name: &str, kind: &str, state: SignInState) -> AccountRow {
        AccountRow {
            name: name.to_string(),
            kind: AccountKind::named(kind),
            endpoint: "default".to_string(),
            state,
        }
    }

    fn rows() -> Vec<AccountRow> {
        vec![
            row("go", "opencode-go", SignInState::NotSignedIn),
            row("local", "ollama", SignInState::NoKeyNeeded),
            row("work", "anthropic", SignInState::KeyStored),
        ]
    }

    fn dialog() -> ProviderDialog {
        ProviderDialog::new(rows(), None).0
    }

    fn press(d: &mut ProviderDialog, code: KeyCode) -> DialogKey {
        d.handle_key(key(code))
    }

    fn type_text(d: &mut ProviderDialog, text: &str) {
        for c in text.chars() {
            press(d, KeyCode::Char(c));
        }
    }

    fn select(d: &mut ProviderDialog, name: &str) {
        while d.selected_account().as_deref() != Some(name) {
            press(d, KeyCode::Down);
        }
    }

    fn text(d: &ProviderDialog) -> String {
        d.body(100, 20)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn with_new(rows_after: &str) -> Vec<AccountRow> {
        let mut all = rows();
        all.push(row(rows_after, "anthropic", SignInState::NotSignedIn));
        all
    }

    #[test]
    fn the_list_shows_each_account_and_the_add_row() {
        let d = dialog();
        let shown = text(&d);
        assert!(
            shown.contains("go") && shown.contains("opencode-go"),
            "{shown}"
        );
        assert!(shown.contains("not signed in"), "{shown}");
        assert!(shown.contains("key stored"), "{shown}");
        assert!(shown.contains("no key needed"), "{shown}");
        assert!(shown.contains("+ add account"), "{shown}");
    }

    #[test]
    fn enter_on_a_key_account_asks_for_the_key_and_stores_it() {
        let mut d = dialog();
        select(&mut d, "work");
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        type_text(&mut d, "sk-abc");
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::StoreKey {
                account: "work".into(),
                key: Secret::new("sk-abc"),
            }
        );
    }

    #[test]
    fn the_key_is_drawn_as_bullets_and_never_as_text() {
        let mut d = dialog();
        select(&mut d, "work");
        press(&mut d, KeyCode::Enter);
        type_text(&mut d, "sk-secret");
        d.handle_paste("-pasted\n");
        let shown = text(&d);
        assert!(
            shown.contains(&"•".repeat("sk-secret-pasted".len())),
            "{shown}"
        );
        assert!(
            !shown.contains("secret") && !shown.contains("pasted"),
            "{shown}"
        );
    }

    #[test]
    fn a_secret_never_formats() {
        let key = DialogKey::StoreKey {
            account: "work".into(),
            key: Secret::new("sk-abc123"),
        };
        let shown = format!("{key:?} {:?}", Secret::new("sk-abc123"));
        assert!(!shown.contains("sk-abc123"), "{shown}");
    }

    #[test]
    fn the_key_prompt_empties_once_the_key_is_sent() {
        let mut d = dialog();
        select(&mut d, "work");
        press(&mut d, KeyCode::Enter);
        type_text(&mut d, "k");
        press(&mut d, KeyCode::Enter);
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        assert!(text(&d).contains("a key is required"));
    }

    #[test]
    fn an_empty_key_is_refused_for_a_kind_that_needs_one() {
        let mut d = dialog();
        select(&mut d, "work");
        press(&mut d, KeyCode::Enter);
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        assert!(text(&d).contains("a key is required"));
        assert_eq!(press(&mut d, KeyCode::Esc), DialogKey::None);
        assert!(text(&d).contains("+ add account"));
    }

    #[test]
    fn enter_on_a_sign_in_account_starts_the_sign_in() {
        let mut d = dialog();
        select(&mut d, "go");
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::SignIn("go".into())
        );
        assert!(text(&d).contains("signing in to `go`"));
        d.finish(Outcome::Step(SignInStep::Code {
            verification_url: "https://x.test/device".into(),
            user_code: "ABCD-1234".into(),
        }));
        let shown = text(&d);
        assert!(
            shown.contains("https://x.test/device") && shown.contains("ABCD-1234"),
            "{shown}"
        );
        assert_eq!(press(&mut d, KeyCode::Esc), DialogKey::CancelSignIn);
        assert!(text(&d).contains("sign-in cancelled"));
    }

    #[test]
    fn a_sign_in_warning_is_shown_while_waiting() {
        let (mut d, key) = ProviderDialog::new(rows(), Some("go"));
        assert_eq!(key, DialogKey::SignIn("go".into()));
        d.finish(Outcome::Step(SignInStep::Warning("no email".into())));
        assert!(text(&d).contains("no email"));
    }

    #[test]
    fn opening_on_an_account_goes_straight_to_its_login() {
        let (d, key) = ProviderDialog::new(rows(), Some("work"));
        assert_eq!(key, DialogKey::None);
        assert!(text(&d).contains("API key for `work`"));
        let (d, key) = ProviderDialog::new(rows(), Some("go"));
        assert_eq!(key, DialogKey::SignIn("go".into()));
        assert!(text(&d).contains("signing in"));
    }

    #[test]
    fn an_unknown_account_opens_the_list_with_an_error_line() {
        let (d, key) = ProviderDialog::new(rows(), Some("nope"));
        assert_eq!(key, DialogKey::None);
        let shown = text(&d);
        assert!(
            shown.contains("unknown account `nope`") && shown.contains("go, local, work"),
            "{shown}"
        );
        assert!(shown.contains("+ add account"));
    }

    #[test]
    fn the_add_flow_for_a_key_kind_asks_kind_name_then_adds() {
        let mut d = dialog();
        press(&mut d, KeyCode::Char('a'));
        // Registry order: anthropic first.
        assert!(text(&d).contains("kind of account"));
        press(&mut d, KeyCode::Enter);
        type_text(&mut d, "ab");
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::Add(NewAccount {
                name: "ab".into(),
                kind: AccountKind::named("anthropic"),
                base_url: None,
            })
        );
        // The host adds it, lists again, and the dialog goes on to its key.
        d.finish(Outcome::Rows(with_new("ab")));
        assert_eq!(
            d.finish(Outcome::Added {
                account: "ab".into()
            }),
            DialogKey::None
        );
        assert!(text(&d).contains("API key for `ab`"));
        type_text(&mut d, "sk-1");
        assert!(matches!(
            press(&mut d, KeyCode::Enter),
            DialogKey::StoreKey { .. }
        ));
        assert_eq!(
            d.finish(Outcome::LoggedIn {
                account: "ab".into(),
                summary: "stored".into()
            }),
            DialogKey::PickModel("ab".into())
        );
    }

    fn pick_kind(d: &mut ProviderDialog, kind: &str) {
        press(d, KeyCode::Char('a'));
        while accounts::kinds()[match &d.screen {
            Screen::Kind { selected } => *selected,
            _ => unreachable!(),
        }]
        .as_str()
            != kind
        {
            press(d, KeyCode::Down);
        }
        press(d, KeyCode::Enter);
    }

    #[test]
    fn a_kind_that_needs_a_base_url_asks_for_it() {
        let mut d = dialog();
        pick_kind(&mut d, "openai-compatible");
        type_text(&mut d, "gw");
        press(&mut d, KeyCode::Enter);
        assert!(text(&d).contains("base URL"));
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        assert!(text(&d).contains("a base URL is required"));
        type_text(&mut d, "http://gw.test/v1");
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::Add(NewAccount {
                name: "gw".into(),
                kind: AccountKind::named("openai-compatible"),
                base_url: Some("http://gw.test/v1".into()),
            })
        );
        d.finish(Outcome::Failed("accounts.gw.base_url: not a URL".into()));
        assert!(text(&d).contains("not a URL"));
    }

    #[test]
    fn an_optional_base_url_is_skipped_with_enter() {
        let mut d = dialog();
        pick_kind(&mut d, "ollama");
        type_text(&mut d, "box");
        press(&mut d, KeyCode::Enter);
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::Add(NewAccount {
                name: "box".into(),
                kind: AccountKind::named("ollama"),
                base_url: None,
            })
        );
    }

    #[test]
    fn a_keyless_kind_skips_its_key_and_goes_on_to_the_models() {
        let mut d = dialog();
        d.finish(Outcome::Rows(vec![row(
            "box",
            "ollama",
            SignInState::NoKeyNeeded,
        )]));
        assert_eq!(
            d.finish(Outcome::Added {
                account: "box".into()
            }),
            DialogKey::None
        );
        assert!(text(&d).contains("API key for `box`"));
        assert_eq!(
            press(&mut d, KeyCode::Enter),
            DialogKey::PickModel("box".into())
        );
    }

    #[test]
    fn a_new_sign_in_account_starts_its_sign_in_and_then_picks_a_model() {
        let mut d = dialog();
        d.finish(Outcome::Rows(with_new("x")));
        d.rows[3].kind = AccountKind::named("opencode-go");
        assert_eq!(
            d.finish(Outcome::Added {
                account: "x".into()
            }),
            DialogKey::SignIn("x".into())
        );
        assert_eq!(
            d.finish(Outcome::LoggedIn {
                account: "x".into(),
                summary: "signed in".into()
            }),
            DialogKey::PickModel("x".into())
        );
    }

    #[test]
    fn a_login_outside_the_add_flow_ends_on_its_result() {
        let mut d = dialog();
        select(&mut d, "work");
        press(&mut d, KeyCode::Enter);
        type_text(&mut d, "k");
        press(&mut d, KeyCode::Enter);
        assert_eq!(
            d.finish(Outcome::LoggedIn {
                account: "work".into(),
                summary: "stored the key for `work`".into()
            }),
            DialogKey::None
        );
        assert!(text(&d).contains("stored the key for `work`"));
        press(&mut d, KeyCode::Char('x'));
        assert!(text(&d).contains("+ add account"));
    }

    #[test]
    fn names_are_checked_as_they_are_typed() {
        let mut d = dialog();
        pick_kind(&mut d, "anthropic");
        type_text(&mut d, "a b");
        assert!(text(&d).contains("letters, digits"), "{}", text(&d));
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        for _ in 0..3 {
            press(&mut d, KeyCode::Backspace);
        }
        type_text(&mut d, "work");
        assert!(text(&d).contains("already exists"));
        assert_eq!(press(&mut d, KeyCode::Enter), DialogKey::None);
        press(&mut d, KeyCode::Esc);
        assert!(text(&d).contains("+ add account"));
    }

    #[test]
    fn escape_leaves_every_add_screen_for_the_list() {
        for steps in [0, 1, 2] {
            let mut d = dialog();
            pick_kind(&mut d, "openai-compatible");
            if steps >= 1 {
                type_text(&mut d, "gw");
                press(&mut d, KeyCode::Enter);
            }
            if steps >= 2 {
                type_text(&mut d, "http");
            }
            assert_eq!(press(&mut d, KeyCode::Esc), DialogKey::None);
            assert!(matches!(d.screen, Screen::List), "step {steps}");
        }
        let mut d = dialog();
        assert_eq!(press(&mut d, KeyCode::Esc), DialogKey::Close);
    }

    #[test]
    fn log_out_and_remove_ask_first() {
        let mut d = dialog();
        select(&mut d, "work");
        press(&mut d, KeyCode::Char('o'));
        assert!(text(&d).contains("Log out of `work`"));
        assert_eq!(press(&mut d, KeyCode::Char('n')), DialogKey::None);
        assert!(text(&d).contains("+ add account"));
        press(&mut d, KeyCode::Char('o'));
        assert_eq!(
            press(&mut d, KeyCode::Char('y')),
            DialogKey::LogOut("work".into())
        );
        d.finish(Outcome::Done("logged out of `work`".into()));
        assert!(text(&d).contains("logged out of `work`"));

        let mut d = dialog();
        select(&mut d, "go");
        press(&mut d, KeyCode::Char('d'));
        assert!(text(&d).contains("Remove account `go`"));
        assert_eq!(press(&mut d, KeyCode::Esc), DialogKey::None);
        press(&mut d, KeyCode::Char('d'));
        assert_eq!(
            press(&mut d, KeyCode::Char('y')),
            DialogKey::Remove("go".into())
        );
        d.finish(Outcome::Failed(
            "the account is in use by the main model".into(),
        ));
        assert!(text(&d).contains("in use by the main model"));
    }

    #[test]
    fn m_picks_a_model_of_the_selected_account() {
        let mut d = dialog();
        select(&mut d, "local");
        assert_eq!(
            press(&mut d, KeyCode::Char('m')),
            DialogKey::PickModel("local".into())
        );
        // On the add row there is no account to pick from.
        select(&mut d, "work");
        press(&mut d, KeyCode::Down);
        assert_eq!(press(&mut d, KeyCode::Char('m')), DialogKey::None);
    }

    #[test]
    fn a_login_on_an_env_account_warns_the_stored_key_is_ignored() {
        let mut d = ProviderDialog::new(
            vec![row(
                "w",
                "anthropic",
                SignInState::KeyFromEnv {
                    var: "KEY_VAR".into(),
                },
            )],
            Some("w"),
        )
        .0;
        assert!(text(&d).contains("KEY_VAR"));
        press(&mut d, KeyCode::Esc);
    }

    #[test]
    fn a_paste_lands_in_the_open_text_field_only() {
        let mut d = dialog();
        d.handle_paste("ignored");
        pick_kind(&mut d, "anthropic");
        d.handle_paste("pasted-name\n");
        assert!(text(&d).contains("pasted-name"));
    }

    #[test]
    fn a_long_list_keeps_the_selection_in_view() {
        let many: Vec<AccountRow> = (0..30)
            .map(|i| row(&format!("a{i:02}"), "anthropic", SignInState::KeyStored))
            .collect();
        let mut d = ProviderDialog::new(many, None).0;
        for _ in 0..25 {
            press(&mut d, KeyCode::Down);
        }
        let shown: Vec<String> = d
            .body(100, 6)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(shown.len(), 6);
        assert!(shown.iter().any(|l| l.starts_with("> a25")), "{shown:?}");
    }
}
