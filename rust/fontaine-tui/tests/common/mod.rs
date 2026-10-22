#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use fontaine_core::agent::AgentSetup;
use fontaine_core::config::{self, AccountKind, LoadParams, PermissionMode};
use fontaine_core::models::{
    GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models, Switcher,
};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::Session;
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
    /// How many times the interface has been driven.
    runs: Cell<usize>,
}

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
            runs: Cell::new(0),
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

    pub fn with_config(self, text: &str) -> World {
        std::fs::write(&self.user_config, text).unwrap();
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
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        let model = ModelHandle::new(
            ModelRef::parse("test/mock").unwrap(),
            AccountKind::OpenAiCompatible,
            self.mock.clone().into(),
        );
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
                    AccountKind::OpenAiCompatible,
                    mock.clone().into(),
                ))
            })
        };
        let env: std::collections::HashMap<String, std::ffi::OsString> = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.into()))
            .collect();
        let switcher = Switcher::new(
            config.clone(),
            self.data.clone(),
            Arc::new(move |name: &str| env.get(name).cloned()),
        );
        AgentSetup {
            config,
            config_dir: self.config_dir.clone(),
            data_dir: self.data.clone(),
            models: Models {
                main: model,
                main_listing: Vec::new(),
                guardian,
                guardian_choice: if self.guardian_on_main {
                    GuardianChoice::MainFallback
                } else {
                    GuardianChoice::Pinned
                },
                notices: Vec::new(),
            },
            session: Session::create(&self.data, &session_id(self.runs.get())).unwrap(),
            roots: vec![self.work.clone()],
            mode: self.mode,
            scrub_env: vec![],
            git_bash: None,
            system_prompt: None,
            append_system_prompt: None,
            json_schema: None,
            headless: false,
            approver: Arc::new(DenyAll),
            home: Some(self.home.clone()),
            user_config: Some(self.user_config.clone()),
            guardian_profile: fontaine_guardian::Profile::Full,
            notices: Vec::new(),
            switcher: Some(switcher),
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
            inline.render(app).unwrap();
            let area = inline.area();
            let backend = inline.backend();
            let mut rows = text_rows(backend.scrollback());
            let visible = text_rows(backend.buffer());
            rows.extend(visible.iter().cloned());
            *shown.borrow_mut() = rows.join("\n");
            let top = usize::from(area.top());
            *shown_live.borrow_mut() = visible[top..usize::from(area.bottom())].join("\n");
        };
        let ui = Ui { keys, screen, live };
        let (result, ()) = tokio::join!(
            drive(self.setup(), self.work.clone(), rx, render),
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

    pub fn send(&self, line: &str) {
        self.type_text(line);
        self.press(KeyCode::Enter);
    }

    pub fn screen(&self) -> String {
        self.screen.borrow().clone()
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
