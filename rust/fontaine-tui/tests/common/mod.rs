#![allow(dead_code)]

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use fontaine_core::agent::AgentSetup;
use fontaine_core::config::{self, AccountKind, LoadParams, PermissionMode};
use fontaine_core::models::{GuardianChoice, ModelHandle, ModelRef, Models};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::Session;
use fontaine_tui::{Input, drive, view};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rig_core::completion::{CompletionRequest, Usage};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, mock_final};
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
    data: PathBuf,
    home: PathBuf,
    pub mock: MockCompletionModel,
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
        };
        world.mock = MockCompletionModel::from_stream_turns(turns(&world));
        world
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
        AgentSetup {
            config,
            config_dir: self.config_dir.clone(),
            data_dir: self.data.clone(),
            models: Models {
                main: model,
                main_listing: Vec::new(),
                guardian: None,
                guardian_choice: GuardianChoice::MainFallback,
                notices: Vec::new(),
            },
            session: Session::create(&self.data, "00000000-0000-4000-8000-000000000001").unwrap(),
            roots: vec![self.work.clone()],
            mode: PermissionMode::Manual,
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
            switcher: None,
        }
    }

    /// Runs the interface on a test terminal while `script` plays the human. The interface
    /// quits when the script ends.
    pub async fn run<F, Fut>(&self, script: F)
    where
        F: FnOnce(Ui) -> Fut,
        Fut: Future<Output = ()>,
    {
        let (keys, rx) = unbounded_channel();
        let screen = Rc::new(RefCell::new(String::new()));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let shown = screen.clone();
        let render = move |app: &fontaine_tui::App| {
            let done = terminal.draw(|frame| view::draw(frame, app)).unwrap();
            let buffer = done.buffer;
            let width = usize::from(buffer.area.width);
            let rows: Vec<String> = buffer
                .content()
                .chunks(width)
                .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
                .collect();
            *shown.borrow_mut() = rows.join("\n");
        };
        let ui = Ui { keys, screen };
        let (result, ()) = tokio::join!(
            drive(self.setup(), self.work.clone(), rx, render),
            script(ui)
        );
        result.unwrap();
    }

    pub fn requests(&self) -> Vec<String> {
        self.mock
            .requests()
            .iter()
            .map(|r: &CompletionRequest| format!("{r:?}"))
            .collect()
    }
}

pub struct Ui {
    keys: UnboundedSender<Input>,
    screen: Rc<RefCell<String>>,
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
