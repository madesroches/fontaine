//! The screen shown before the session when no main model can be resolved: the resolution error
//! above the provider dialog, so a new user adds an account, signs in and picks a model in one
//! run. Every successful add, log-in or pick resolves the model again, and a success starts the
//! session.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fontaine_core::accounts::{self, AccountsError};
use fontaine_core::agent::EnvLookup;
use fontaine_core::config::Config;
use fontaine_core::dirs::Dirs;
use fontaine_core::models::{self, Models, ResolveParams, Switcher};
use fontaine_core::user_config;
use fontaine_tools::error_chain::chain;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::app::Input;
use crate::model_picker::{Group, ModelPicker, PickerKey, PickerPurpose};
use crate::provider_dialog::{DialogKey, Outcome, ProviderDialog, Secret};
use crate::view::draw_overlay;

/// A terminal backend whose errors can become `io::Error`s.
pub trait SetupBackend: Backend<Error: Send + Sync + 'static> {}

impl<B: Backend<Error: Send + Sync + 'static>> SetupBackend for B {}

pub struct SetupParams {
    pub config: Config,
    pub dirs: Dirs,
    pub env: EnvLookup,
    /// Why no model could be resolved, as it is shown above the dialog.
    pub error: String,
    /// `--model`, which wins over the settings until the human picks another.
    pub model_override: Option<String>,
    pub session_id: String,
    /// The user settings file accounts and the model choice are written to.
    pub user_file: PathBuf,
}

/// Where the setup went.
enum Step {
    Stay,
    Quit,
    Start(Box<(Models, Config)>),
}

struct Io<B: SetupBackend> {
    terminal: Terminal<B>,
    input: UnboundedReceiver<Input>,
}

struct Setup {
    config: Config,
    dirs: Dirs,
    env: EnvLookup,
    error: String,
    model_override: Option<String>,
    session_id: String,
    user_file: PathBuf,
    dialog: ProviderDialog,
    picker: Option<ModelPicker>,
    /// What is happening that the human waits on, such as a listing.
    status: Option<String>,
    /// Things that went wrong beside the setup itself, told once the session starts.
    warnings: Vec<String>,
}

/// Runs the setup on the real terminal. `None` is the human leaving it, in which case the caller
/// reports the original error. The returned configuration has the accounts that were added.
pub async fn run(params: SetupParams) -> io::Result<Option<(Models, Config)>> {
    let terminal = ratatui::try_init()?;
    let _ = ratatui::crossterm::execute!(io::stdout(), EnableBracketedPaste);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::restore_terminal();
        previous_hook(info);
    }));
    let (tx, input) = unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || crate::read_terminal(&tx, &stop))
    };

    let result = drive(params, terminal, input).await;

    stop.store(true, Ordering::Relaxed);
    let _ = reader.await;
    let _ = ratatui::crossterm::execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

/// The setup over any terminal and input channel, which is how the tests run it.
pub async fn drive<B: SetupBackend>(
    params: SetupParams,
    terminal: Terminal<B>,
    input: UnboundedReceiver<Input>,
) -> io::Result<Option<(Models, Config)>> {
    let rows = accounts::rows(&params.config, &params.dirs.data, &*params.env)
        .map_err(io::Error::other)?;
    let (dialog, _) = ProviderDialog::new(rows, None);
    let mut setup = Setup {
        config: params.config,
        dirs: params.dirs,
        env: params.env,
        error: params.error,
        model_override: params.model_override,
        session_id: params.session_id,
        user_file: params.user_file,
        dialog,
        picker: None,
        status: None,
        warnings: Vec::new(),
    };
    let step = setup.drive(&mut Io { terminal, input }).await?;
    Ok(match step {
        Step::Start(started) => Some(*started),
        Step::Stay | Step::Quit => None,
    })
}

impl Setup {
    fn refresh_rows(&mut self) {
        match accounts::rows(&self.config, &self.dirs.data, &*self.env) {
            Ok(rows) => {
                self.dialog.finish(Outcome::Rows(rows));
            }
            Err(e) => {
                self.dialog.finish(Outcome::Failed(chain(&e)));
            }
        }
    }

    fn redraw<B: SetupBackend>(&self, io: &mut Io<B>) -> io::Result<()> {
        io.terminal
            .draw(|frame| self.draw(frame))
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = usize::from(area.width.saturating_sub(2)).max(1);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut banner = vec![Line::styled(
            "No model is ready yet",
            bold.fg(Color::Yellow),
        )];
        banner.extend(self.error.lines().map(|l| Line::raw(l.to_string())));
        banner.push(Line::styled(
            "Add an account, sign in and pick a model; the session starts once one works.",
            dim,
        ));
        if let Some(status) = &self.status {
            banner.push(Line::styled(status.clone(), dim));
        }
        let wrapped: usize = banner
            .iter()
            .map(|l| l.width().div_ceil(usize::from(area.width).max(1)).max(1))
            .sum();
        let banner_height = u16::try_from(wrapped)
            .unwrap_or(u16::MAX)
            .min(area.height / 2);
        let [top, rest] =
            Layout::vertical([Constraint::Length(banner_height), Constraint::Min(0)]).areas(area);
        frame.render_widget(Paragraph::new(banner).wrap(Wrap { trim: false }), top);

        let rows = usize::from(rest.height).saturating_sub(4).max(1);
        let (title, body, footer) = match &self.picker {
            Some(picker) => ("model", picker.body(width, rows), picker.footer()),
            None => (
                "providers",
                self.dialog.body(width, rows),
                self.dialog.footer(),
            ),
        };
        let height = u16::try_from(body.len() + footer.len() + 2)
            .unwrap_or(u16::MAX)
            .min(rest.height);
        let [dialog, _] =
            Layout::vertical([Constraint::Length(height), Constraint::Min(0)]).areas(rest);
        draw_overlay(frame, dialog, title, body, footer);
    }

    async fn drive<B: SetupBackend>(&mut self, io: &mut Io<B>) -> io::Result<Step> {
        loop {
            self.redraw(io)?;
            let Some(event) = io.input.recv().await else {
                return Ok(Step::Quit);
            };
            match event {
                Input::Resize => {}
                Input::Paste(text) => {
                    if self.picker.is_none() {
                        self.dialog.handle_paste(&text);
                    }
                }
                Input::Key(key) => {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(Step::Quit);
                    }
                    let step = if let Some(picker) = &mut self.picker {
                        match picker.handle_key(key) {
                            PickerKey::None => Step::Stay,
                            PickerKey::Close => {
                                self.picker = None;
                                Step::Stay
                            }
                            PickerKey::Pick(_, reference) => {
                                self.picker = None;
                                self.pick(io, reference).await?
                            }
                        }
                    } else {
                        let request = self.dialog.handle_key(key);
                        self.request(io, request).await?
                    };
                    match step {
                        Step::Stay => {}
                        done => return Ok(done),
                    }
                }
            }
        }
    }

    /// Resolves the model again. A success is the end of the setup; a failure shows its error.
    async fn retry<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
    ) -> io::Result<Option<Box<(Models, Config)>>> {
        self.status = Some("checking the model…".to_string());
        self.redraw(io)?;
        let resolved = models::resolve(&ResolveParams {
            config: &self.config,
            data_dir: &self.dirs.data,
            env: &*self.env,
            model_override: self.model_override.as_deref(),
            session_id: Some(self.session_id.clone()),
        })
        .await;
        self.status = None;
        Ok(match resolved {
            Ok(mut models) => {
                models.notices.append(&mut self.warnings);
                Some(Box::new((models, self.config.clone())))
            }
            Err(e) => {
                self.error = e.to_string();
                None
            }
        })
    }

    async fn pick<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
        reference: String,
    ) -> io::Result<Step> {
        if let Err(e) = user_config::set_model(&self.user_file, &reference) {
            self.warnings.push(format!(
                "could not save {reference} as your model in {}: {e}",
                self.user_file.display()
            ));
        }
        self.config.model = Some(reference.clone());
        self.model_override = Some(reference);
        Ok(match self.retry(io).await? {
            Some(started) => Step::Start(started),
            None => Step::Stay,
        })
    }

    /// Carries out what the dialog asked for, and what its answer asks for in turn.
    async fn request<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
        mut request: DialogKey,
    ) -> io::Result<Step> {
        loop {
            let next = match request {
                DialogKey::None | DialogKey::CancelSignIn => return Ok(Step::Stay),
                DialogKey::Close => return Ok(Step::Quit),
                DialogKey::Add(new) => {
                    match accounts::add(&self.user_file, &self.dirs.data, &new, &self.config) {
                        Ok(account) => {
                            self.config.accounts.insert(new.name.clone(), account);
                            self.refresh_rows();
                            if let Some(started) = self.retry(io).await? {
                                return Ok(Step::Start(started));
                            }
                            self.dialog.finish(Outcome::Added { account: new.name })
                        }
                        Err(e) => self.dialog.finish(Outcome::Failed(chain(&e))),
                    }
                }
                DialogKey::StoreKey { account, key } => {
                    match self.store_key(io, &account, key).await? {
                        After::Ask(next) => next,
                        After::Start(started) => return Ok(Step::Start(started)),
                        After::Quit => return Ok(Step::Quit),
                    }
                }
                DialogKey::SignIn(account) => match self.sign_in(io, &account).await? {
                    After::Ask(next) => next,
                    After::Start(started) => return Ok(Step::Start(started)),
                    After::Quit => return Ok(Step::Quit),
                },
                DialogKey::LogOut(account) => match accounts::log_out(&self.dirs.data, &account) {
                    Ok(removed) => {
                        self.refresh_rows();
                        self.dialog.finish(Outcome::Done(if removed {
                            format!("logged out of `{account}`")
                        } else {
                            format!("`{account}` had no stored credential")
                        }))
                    }
                    Err(e) => self.dialog.finish(Outcome::Failed(chain(&e))),
                },
                DialogKey::Remove(account) => {
                    match accounts::remove(&self.user_file, &self.dirs.data, &account) {
                        Ok(()) => {
                            self.config.accounts.remove(&account);
                            self.refresh_rows();
                            self.dialog
                                .finish(Outcome::Done(format!("removed account `{account}`")))
                        }
                        Err(e @ AccountsError::CredentialKept { .. }) => {
                            self.config.accounts.remove(&account);
                            self.refresh_rows();
                            self.dialog.finish(Outcome::Failed(chain(&e)))
                        }
                        Err(e) => self.dialog.finish(Outcome::Failed(chain(&e))),
                    }
                }
                DialogKey::PickModel(account) => {
                    self.open_picker(io, &account).await?;
                    return Ok(Step::Stay);
                }
            };
            if next == DialogKey::None {
                return Ok(Step::Stay);
            }
            request = next;
        }
    }

    async fn store_key<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
        account: &str,
        key: Secret,
    ) -> io::Result<After> {
        let stored = accounts::store_key(&self.dirs.data, account, key.expose());
        drop(key);
        if let Err(e) = stored {
            return Ok(After::Ask(self.dialog.finish(Outcome::Failed(chain(&e)))));
        }
        let mut summary = format!("stored the key for `{account}`");
        if let Some(var) = self
            .config
            .accounts
            .get(account)
            .and_then(|a| a.api_key_env.as_deref())
        {
            summary.push_str(&format!(
                "; warning: it sets api_key_env = \"{var}\", so the stored key is ignored while the variable is set"
            ));
        }
        self.logged_in(io, account, summary).await
    }

    /// A key stored or a sign-in done: the model is resolved again, and the dialog told. The
    /// session starts here when it resolves and the human has no model to choose.
    async fn logged_in<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
        account: &str,
        summary: String,
    ) -> io::Result<After> {
        self.refresh_rows();
        if let Some(started) = self.retry(io).await? {
            return Ok(After::Start(started));
        }
        Ok(After::Ask(self.dialog.finish(Outcome::LoggedIn {
            account: account.to_string(),
            summary,
        })))
    }

    async fn sign_in<B: SetupBackend>(&mut self, io: &mut Io<B>, name: &str) -> io::Result<After> {
        let Some(account) = self.config.accounts.get(name).cloned() else {
            return Ok(After::Ask(
                self.dialog
                    .finish(Outcome::Failed(format!("unknown account `{name}`"))),
            ));
        };
        let (steps_tx, mut steps) = unbounded_channel();
        let progress = move |step| {
            let _ = steps_tx.send(step);
        };
        let data_dir = self.dirs.data.clone();
        let result = {
            let mut run = Box::pin(accounts::sign_in(name, &account, &data_dir, &progress));
            loop {
                self.redraw(io)?;
                tokio::select! {
                    biased;
                    result = &mut run => break Some(result),
                    Some(step) = steps.recv() => {
                        self.dialog.finish(Outcome::Step(step));
                    }
                    event = io.input.recv() => match event {
                        None => return Ok(After::Quit),
                        Some(Input::Key(key)) if key.kind != KeyEventKind::Release => {
                            if key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                break None;
                            }
                            if self.dialog.handle_key(key) == DialogKey::CancelSignIn {
                                break None;
                            }
                        }
                        Some(_) => {}
                    },
                }
            }
        };
        while let Ok(step) = steps.try_recv() {
            self.dialog.finish(Outcome::Step(step));
        }
        match result {
            Some(Ok(summary)) => self.logged_in(io, name, summary).await,
            Some(Err(e)) => {
                let text = match e.into_config() {
                    Ok(config) => chain(&config),
                    Err(e) => chain(&e),
                };
                Ok(After::Ask(self.dialog.finish(Outcome::Failed(text))))
            }
            None => Ok(After::Ask(DialogKey::None)),
        }
    }

    async fn open_picker<B: SetupBackend>(
        &mut self,
        io: &mut Io<B>,
        account: &str,
    ) -> io::Result<()> {
        self.status = Some(format!("listing the models of {account}…"));
        self.redraw(io)?;
        let switcher = Switcher::new(
            self.config.clone(),
            self.dirs.data.clone(),
            self.env.clone(),
            Some(self.session_id.clone()),
        );
        let listings = switcher.listings(None, Some(&[account.to_string()])).await;
        self.status = None;
        let groups: Vec<Group> = listings.iter().map(Group::from).collect();
        self.picker = Some(ModelPicker::new(
            PickerPurpose::Main,
            format!("models of {account}"),
            None,
            Vec::new(),
            groups,
        ));
        Ok(())
    }
}

/// What follows a log-in attempt.
enum After {
    /// The dialog's answer to the result, which may ask for more.
    Ask(DialogKey),
    Start(Box<(Models, Config)>),
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::config::{self, LoadParams};
    use ratatui::backend::TestBackend;

    #[test]
    fn the_error_sits_above_the_dialog() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dirs = Dirs {
            config: root.join("config"),
            data: root.join("data"),
        };
        std::fs::create_dir_all(&dirs.config).unwrap();
        let config = config::load(&LoadParams {
            launch_dir: &root,
            config_dir: &dirs.config,
            data_dir: &dirs.data,
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        let (dialog, _) = ProviderDialog::new(Vec::new(), None);
        let setup = Setup {
            config,
            dirs,
            env: Arc::new(|_| None),
            error: "no main model: set `[model] model`".to_string(),
            model_override: None,
            session_id: "s".to_string(),
            user_file: root.join("config").join("config.toml"),
            dialog,
            picker: None,
            status: None,
            warnings: Vec::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|frame| setup.draw(frame)).unwrap();
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content()
            .chunks(90)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect();
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        let error = at("no main model").expect("the error is shown");
        let dialog = at("providers").expect("the dialog is shown");
        assert!(error < dialog, "{rows:#?}");
        assert!(at("+ add account").is_some_and(|row| row > dialog));
    }
}
