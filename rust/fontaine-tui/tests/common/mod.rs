#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub mod routed;

/// A model name the catalog lists, so a routed world has a window to compact against.
pub const KNOWN_MODEL: &str = "gpt-4o-mini";

use fontaine_core::agent::AgentSetup;
use fontaine_core::config::{self, AccountKind, LoadParams, PermissionMode};
use fontaine_core::models::{
    GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models, Switcher,
};
use fontaine_core::permissions::DenyAll;
use fontaine_core::schedule::ManualClock;
use fontaine_core::session::Session;
use fontaine_tui::linear::Linear;
use fontaine_tui::screen::Screen;
use fontaine_tui::{Input, drive};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rig_core::completion::{CompletionRequest, Usage};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, MockTurn, mock_final};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

pub fn tool_turn(id: &str, name: &str, arguments: Value) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
            call_id: None,
        },
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

pub fn text_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(text.to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]
}

/// A scratch world with a workspace, a second directory to add, and the user settings file.
pub struct World {
    _tmp: TempDir,
    pub work: PathBuf,
    pub extra: PathBuf,
    pub user_config: PathBuf,
    config_dir: PathBuf,
    pub data: PathBuf,
    home: PathBuf,
    pub mock: MockCompletionModel,
    pub mode: PermissionMode,
    /// The scripted guardian, when the world has one.
    pub guardian: Option<MockCompletionModel>,
    guardian_unavailable: Option<String>,
    guardian_on_main: bool,
    /// Environment variables the switcher resolves models with.
    pub env: Vec<(String, String)>,
    /// Whether the repository's executables may start (the trust prompt was accepted).
    pub project_executables: bool,
    /// Whether the repository's MCP servers wait for the human's approval.
    pub confirm_servers: bool,
    /// The main model is resolved from the settings, against this routed endpoint.
    routed: bool,
    /// The session has no user settings file to save choices to.
    pub without_user_config: bool,
    /// Warnings gathered before the session started, as the binary hands them over.
    pub startup_warnings: Vec<fontaine_core::Notice>,
    /// Warnings the caller of `run` shows first (the ignored screen-reader setting).
    pub startup: Vec<fontaine_core::Notice>,
    /// A `--settings` file, as the command line would name one.
    pub settings_file: Option<PathBuf>,
    /// How many times the interface has been driven.
    runs: Cell<usize>,
    /// The time the session's scheduler sees, which a test moves itself.
    pub clock: Arc<ManualClock>,
}

/// The instant the scheduler's clock starts at in a test world: 2026-01-15 12:00 UTC.
pub const CLOCK_START_MS: i64 = 1_768_478_400_000;

/// The id of the session the `n`th drive run of a world uses: a session directory cannot be
/// reused, so each run needs its own.
fn session_id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// A verdict the scripted guardian answers with.
pub fn verdict(decision: &str, rule: &str, confidence: f64) -> Value {
    serde_json::json!({
        "decision": decision,
        "rule": rule,
        "explanation": format!("{decision} because of {rule}"),
        "confidence": confidence,
    })
}

impl World {
    pub fn new(turns: Vec<Vec<MockStreamEvent>>) -> World {
        World::scripted(|_| turns)
    }

    /// A world whose script depends on its directories.
    pub fn scripted(turns: impl FnOnce(&World) -> Vec<Vec<MockStreamEvent>>) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let config_dir = dir("config");
        let mut world = World {
            work: dir("work"),
            extra: dir("extra"),
            data: dir("data"),
            home: dir("home"),
            user_config: config_dir.join(config::USER_FILE),
            config_dir,
            _tmp: tmp,
            mock: MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new()),
            mode: PermissionMode::Manual,
            guardian: None,
            guardian_unavailable: None,
            guardian_on_main: false,
            env: Vec::new(),
            project_executables: true,
            confirm_servers: true,
            routed: false,
            without_user_config: false,
            startup_warnings: Vec::new(),
            startup: Vec::new(),
            settings_file: None,
            runs: Cell::new(0),
            clock: Arc::new(ManualClock::new(CLOCK_START_MS)),
        };
        world.mock = MockCompletionModel::from_stream_turns(turns(&world));
        world
    }

    /// Starts the session in `mode`.
    pub fn in_mode(mut self, mode: PermissionMode) -> World {
        self.mode = mode;
        self
    }

    /// A guardian that answers `verdicts` in order and repeats the last one.
    pub fn with_guardian(mut self, verdicts: Vec<Value>) -> World {
        let last = verdicts.last().cloned().expect("a verdict");
        let turns = verdicts
            .into_iter()
            .chain(std::iter::repeat_n(last, 50))
            .map(|v| MockTurn::tool_call("g1", "submit", v));
        self.guardian = Some(MockCompletionModel::from_turns(turns));
        self
    }

    /// A pinned guardian whose account has no credential.
    pub fn with_unavailable_guardian(mut self, reference: &str, reason: &str) -> World {
        self.guardian_unavailable = Some(format!("{reference}\n{reason}"));
        self
    }

    /// Starts the session with `text` as the file `--settings` names.
    pub fn with_cli_settings(mut self, text: &str) -> World {
        let path = self.config_dir.with_file_name("cli-settings.toml");
        std::fs::write(&path, text).unwrap();
        self.settings_file = Some(path);
        self
    }

    pub fn with_config(self, text: &str) -> World {
        std::fs::write(&self.user_config, text).unwrap();
        self
    }

    /// A world whose main model is the endpoint at `url`, which answers each conversation from
    /// its own route. `extra` is more user settings.
    pub fn routed(url: &str, extra: &str) -> World {
        let mut world = World::new(Vec::new());
        world.routed = true;
        world.with_config(&format!(
            "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{url}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n{extra}"
        ))
    }

    /// A world whose routed model is one the catalog lists, on an `openai` account, which
    /// speaks the Responses wire.
    pub fn routed_known(url: &str, extra: &str) -> World {
        let mut world = World::new(Vec::new());
        world.routed = true;
        world
            .env
            .push(("PAID_KEY".into(), "sk-test-not-a-real-key".into()));
        world.with_config(&format!(
            "[accounts.mock]\nkind = \"openai\"\nbase_url = \"{url}\"\napi_key_env = \"PAID_KEY\"\nmodels = [\"{KNOWN_MODEL}\"]\n\n[model]\nmodel = \"mock/{KNOWN_MODEL}\"\n{extra}"
        ))
    }

    /// The workspace's settings file, as a repository would carry it.
    pub fn with_project_settings(self, text: &str) -> World {
        let dir = self.work.join(".fontaine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), text).unwrap();
        self
    }

    /// The user declined to trust the workspace.
    pub fn untrusted(mut self) -> World {
        self.project_executables = false;
        self
    }

    /// Writes `text` to `relative` under the user's config directory.
    pub fn with_user_file(self, relative: &str, text: &str) -> World {
        let path = self.config_dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        self
    }

    /// The guardian is the main model, as when no validated guardian is available.
    pub fn guardian_follows_main(mut self) -> World {
        self.guardian_on_main = true;
        self
    }

    pub fn guardian_calls(&self) -> usize {
        self.guardian
            .as_ref()
            .map_or(0, MockCompletionModel::request_count)
    }

    /// The id of the latest run's session.
    pub fn session_id(&self) -> String {
        session_id(self.runs.get().max(1))
    }

    /// The latest run's ledger, as text.
    pub fn ledger_text(&self) -> String {
        std::fs::read_to_string(
            self.data
                .join("sessions")
                .join(self.session_id())
                .join("ledger")
                .join("ledger.jsonl"),
        )
        .unwrap()
    }

    /// The audit records of the latest run's session, in order.
    pub fn audit(&self) -> Vec<Value> {
        let path = self
            .data
            .join("audit")
            .join(format!("{}.jsonl", self.session_id()));
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn setup(&self) -> AgentSetup {
        let config = config::load(&LoadParams {
            launch_dir: &self.work,
            config_dir: &self.config_dir,
            data_dir: &self.data,
            settings_file: self.settings_file.as_deref(),
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        let model = ModelHandle::new(
            ModelRef::parse("test/mock").unwrap(),
            AccountKind::named("openai-compatible"),
            self.mock.clone().into(),
        );
        let lookup: std::collections::HashMap<String, std::ffi::OsString> = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.into()))
            .collect();
        let resolved = if self.routed {
            Some(
                futures::executor::block_on(fontaine_core::models::resolve(
                    &fontaine_core::models::ResolveParams {
                        config: &config,
                        data_dir: &self.data,
                        env: &|name| lookup.get(name).cloned(),
                        model_override: None,
                        session_id: None,
                    },
                ))
                .unwrap(),
            )
        } else {
            None
        };
        let guardian = if self.guardian_on_main {
            Some(GuardianModel::Ready(model.clone()))
        } else if let Some(spec) = &self.guardian_unavailable {
            let (reference, reason) = spec.split_once('\n').unwrap();
            Some(GuardianModel::Unavailable {
                reference: ModelRef::parse(reference).unwrap(),
                reason: reason.to_string(),
            })
        } else {
            self.guardian.as_ref().map(|mock| {
                GuardianModel::Ready(ModelHandle::new(
                    ModelRef::parse("judge/mock").unwrap(),
                    AccountKind::named("openai-compatible"),
                    mock.clone().into(),
                ))
            })
        };
        let env: std::collections::HashMap<String, std::ffi::OsString> = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.into()))
            .collect();
        let env: fontaine_core::agent::EnvLookup =
            Arc::new(move |name: &str| env.get(name).cloned());
        let switcher = Switcher::new(config.clone(), self.data.clone(), env.clone(), None);
        let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
        let extensions = fontaine_core::extensions::load(&fontaine_core::extensions::LoadParams {
            config_dir: &self.config_dir,
            project_root: &self.work,
            servers: &servers,
        })
        .unwrap();
        AgentSetup {
            config,
            config_dir: self.config_dir.clone(),
            data_dir: self.data.clone(),
            models: resolved.unwrap_or(Models {
                main: model,
                main_listing: Vec::new(),
                guardian,
                guardian_choice: if self.guardian_on_main {
                    GuardianChoice::MainFallback
                } else {
                    GuardianChoice::Pinned
                },
                notices: Vec::new(),
            }),
            session: Session::create(&self.data, &session_id(self.runs.get()))
                .unwrap()
                .with_clock(self.clock.clone()),
            roots: vec![self.work.clone()],
            mode: self.mode,
            scrub_env: vec![],
            git_bash: git_bash(),
            system_prompt: None,
            append_system_prompt: None,
            json_schema: None,
            headless: false,
            scheduling: true,
            approver: Arc::new(DenyAll),
            home: Some(self.home.clone()),
            user_config: (!self.without_user_config).then(|| self.user_config.clone()),
            guardian_profile: fontaine_guardian::Profile::Full,
            notices: Vec::new(),
            startup_warnings: self.startup_warnings.clone(),
            scanner: Default::default(),
            switcher: Some(switcher),
            extensions,
            project_executables: self.project_executables,
            confirm_project_servers: self.confirm_servers,
            origin: fontaine_core::agent::SessionOrigin::Fresh,
            env,
        }
    }

    /// Runs the interface on a test terminal while `script` plays the human. The interface
    /// quits when the script ends.
    pub async fn run<F, Fut>(&self, script: F)
    where
        F: FnOnce(Ui) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.runs.set(self.runs.get() + 1);
        let (keys, rx) = unbounded_channel();
        let screen = Rc::new(RefCell::new(String::new()));
        let live = Rc::new(RefCell::new(String::new()));
        let mut inline = Screen::new(TestBackend::new(120, 40), || TestBackend::new(1, 1)).unwrap();
        let (shown, shown_live) = (screen.clone(), live.clone());
        let render = move |app: &mut fontaine_tui::App| {
            if app.flush {
                inline.commit_all(app).unwrap();
            } else {
                inline.render(app).unwrap();
            }
            let area = inline.area();
            let backend = inline.backend();
            let mut rows = text_rows(backend.scrollback());
            let visible = text_rows(backend.buffer());
            rows.extend(visible.iter().cloned());
            *shown.borrow_mut() = rows.join("\n");
            let top = usize::from(area.top());
            *shown_live.borrow_mut() = visible[top..usize::from(area.bottom())].join("\n");
        };
        let copied = Rc::new(RefCell::new(Vec::new()));
        let sink = copied.clone();
        let clipboard = move |text: &str| {
            sink.borrow_mut().push(text.to_string());
            Ok(())
        };
        let ui = Ui {
            keys,
            screen,
            live,
            copied,
        };
        let (result, ()) = tokio::join!(
            drive(
                self.setup(),
                self.work.clone(),
                self.startup.clone(),
                rx,
                render,
                clipboard,
                fontaine_tui::no_suspend()
            ),
            script(ui)
        );
        result.unwrap();
    }

    /// Like [`World::run`], with the linear screen-reader renderer writing to `bytes`. The
    /// script's `screen()` is everything written so far.
    pub async fn run_linear<F, Fut>(&self, bytes: Rc<RefCell<Vec<u8>>>, script: F)
    where
        F: FnOnce(Ui) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.runs.set(self.runs.get() + 1);
        let (keys, rx) = unbounded_channel();
        let screen = Rc::new(RefCell::new(String::new()));
        let mut linear = Linear::new(SharedWriter(bytes.clone()));
        let shown = screen.clone();
        let render = move |app: &mut fontaine_tui::App| {
            linear.render(app).unwrap();
            *shown.borrow_mut() = String::from_utf8_lossy(&bytes.borrow()).into_owned();
        };
        let ui = Ui {
            keys,
            screen,
            live: Rc::default(),
            copied: Rc::default(),
        };
        let (result, ()) = tokio::join!(
            drive(
                self.setup(),
                self.work.clone(),
                self.startup.clone(),
                rx,
                render,
                |_: &str| Ok(()),
                fontaine_tui::no_suspend()
            ),
            script(ui)
        );
        result.unwrap();
    }

    /// What the scripted guardian was asked, one entry per request.
    pub fn guardian_requests(&self) -> Vec<String> {
        self.guardian.as_ref().map_or_else(Vec::new, |mock| {
            mock.requests()
                .iter()
                .map(|r: &CompletionRequest| format!("{r:?}"))
                .collect()
        })
    }

    pub fn requests(&self) -> Vec<String> {
        self.mock
            .requests()
            .iter()
            .map(|r: &CompletionRequest| format!("{r:?}"))
            .collect()
    }
}

/// A writer whose bytes the test can read while the interface owns it.
pub struct SharedWriter(pub Rc<RefCell<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn text_rows(buffer: &Buffer) -> Vec<String> {
    let width = usize::from(buffer.area.width);
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect()
}

pub struct Ui {
    keys: UnboundedSender<Input>,
    screen: Rc<RefCell<String>>,
    live: Rc<RefCell<String>>,
    copied: Rc<RefCell<Vec<String>>>,
}

impl Ui {
    pub fn press(&self, code: KeyCode) {
        self.press_with(code, KeyModifiers::NONE);
    }

    pub fn press_with(&self, code: KeyCode, modifiers: KeyModifiers) {
        let _ = self.keys.send(Input::Key(KeyEvent::new(code, modifiers)));
    }

    pub fn type_text(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c));
        }
    }

    pub fn paste(&self, text: &str) {
        let _ = self.keys.send(Input::Paste(text.to_string()));
    }

    pub fn send(&self, line: &str) {
        self.type_text(line);
        self.press(KeyCode::Enter);
    }

    pub fn screen(&self) -> String {
        self.screen.borrow().clone()
    }

    /// What `/copy` has sent to the clipboard, oldest first.
    pub fn clipboard(&self) -> Vec<String> {
        self.copied.borrow().clone()
    }

    /// Only the live area: what the next frame redraws.
    pub fn live(&self) -> String {
        self.live.borrow().clone()
    }

    pub async fn wait_until(&self, what: &str, done: impl Fn(&str) -> bool) {
        for _ in 0..1000 {
            if done(&self.screen()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "timed out waiting for {what}; the screen shows:\n{}",
            self.screen()
        );
    }

    pub async fn wait_for(&self, text: &str) {
        self.wait_until(&format!("`{text}`"), |s| s.contains(text))
            .await;
    }

    /// Waits for an approval prompt and for its answer keys to start counting.
    pub async fn wait_for_prompt(&self, text: &str) {
        self.wait_for(text).await;
        tokio::time::sleep(Duration::from_millis(450)).await;
    }
}

/// A chat-completions endpoint whose every answer is the guardian verdict `verdict`.
pub async fn verdict_endpoint(verdict: Value) -> wiremock::MockServer {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::builder().start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{"id": "g1", "type": "function",
                                "function": {"name": "submit", "arguments": verdict.to_string()}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
        })))
        .mount(&server)
        .await;
    server
}

/// Git Bash, which hooks run through on Windows; nothing elsewhere.
#[cfg(windows)]
fn git_bash() -> Option<fontaine_tools::git_bash::GitBash> {
    let probe = fontaine_tools::git_bash::Probe {
        env: &|name| std::env::var_os(name),
        is_file: &|path| path.is_file(),
    };
    fontaine_tools::git_bash::discover(&probe).ok()
}

#[cfg(not(windows))]
fn git_bash() -> Option<fontaine_tools::git_bash::GitBash> {
    None
}
