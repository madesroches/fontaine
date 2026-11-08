//! The scripted `claude_cli_double` account (built as an example of this package; prebuild it when
//! running a test that uses it alone).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::World;
use fontaine_core::models::{self, ModelError, Models, ResolveParams, SharedEnv, Switcher};
use fontaine_core::providers::claude_subscription::{CONFLICTING_ENV, inherited_env_names};
use rig_core::completion::CompletionRequest;
use rig_core::message::Message;
use serde_json::{Value, json};

pub fn double() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "claude_cli_double.exe"
    } else {
        "claude_cli_double"
    };
    let path = dir.join(name);
    assert!(
        path.exists(),
        "{} is not built: run `cargo build -p fontaine-core --example claude_cli_double`",
        path.display()
    );
    path
}

/// A world with one `claude` account whose CLI is the double and whose CLI profile directory
/// holds the scenario and the log.
pub struct Claude {
    pub w: World,
    pub dir: PathBuf,
    pub extra: Arc<Mutex<HashMap<String, String>>>,
}

impl Claude {
    pub fn new() -> Claude {
        Self::with_account("")
    }

    pub fn with_account(extra_account: &str) -> Claude {
        let w = World::new();
        let dir = w.root.join("claude-profile");
        std::fs::create_dir_all(&dir).unwrap();
        w.user_file(&format!(
            "[accounts.claude]\nkind = \"claude-subscription\"\ncli_path = {}\nclaude_config_dir = {}\n{extra_account}\n[model]\nmodel = \"claude/sonnet\"\n",
            toml_path(&double()),
            toml_path(&dir),
        ));
        Claude {
            w,
            dir,
            extra: Arc::default(),
        }
    }

    pub fn scenario(&self, scenario: Value) {
        std::fs::write(self.dir.join("double.json"), scenario.to_string()).unwrap();
    }

    pub fn set_env(&self, name: &str, value: &str) {
        self.extra
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
    }

    pub fn unset_env(&self, name: &str) {
        self.extra.lock().unwrap().remove(name);
    }

    /// The process environment the CLI may need, minus anything that would conflict, plus what a
    /// test sets. Looked up live, as in production.
    pub fn env(&self) -> SharedEnv {
        let extra = self.extra.clone();
        let root = self.w.root.clone();
        Arc::new(move |name: &str| {
            if let Some(value) = extra.lock().unwrap().get(name) {
                return Some(OsString::from(value));
            }
            if CONFLICTING_ENV.contains(&name) || !inherited_env_names().contains(&name) {
                return None;
            }
            if name == "HOME" {
                return Some(root.clone().into_os_string());
            }
            std::env::var_os(name)
        })
    }

    pub async fn resolve(&self, model: Option<&str>) -> Result<Models, ModelError> {
        let config = self.w.load().unwrap();
        models::resolve(&ResolveParams {
            config: &config,
            data_dir: &self.w.data,
            env: self.env(),
            model_override: model,
            session_id: Some("session-1".to_string()),
        })
        .await
    }

    pub fn switcher(&self) -> Switcher {
        Switcher::new(
            self.w.load().unwrap(),
            self.w.data.clone(),
            self.env(),
            Some("session-1".to_string()),
        )
    }

    pub fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn invocations(&self) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|entry| entry["kind"] == "invocation")
            .collect()
    }

    pub fn frames(&self) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|entry| entry["kind"] == "frame")
            .map(|entry| entry["value"].clone())
            .collect()
    }

    /// The user frames that ask for a response: those not marked `shouldQuery: false`.
    pub fn query_frames(&self) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|f| f["type"] == "user" && f["shouldQuery"] != json!(false))
            .collect()
    }

    /// The model processes' invocations: those with a `--model` argument.
    pub fn model_runs(&self) -> Vec<Value> {
        self.invocations()
            .into_iter()
            .filter(|i| argv(i).contains(&"--model".to_string()))
            .collect()
    }
}

pub fn toml_path(path: &std::path::Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

pub fn argv(invocation: &Value) -> Vec<String> {
    invocation["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect()
}

pub async fn main_handle(claude: &Claude) -> models::ModelHandle {
    claude.resolve(None).await.unwrap().main
}

/// A `conversation` whose first message carries `tag`, so its session id (and the process-wide
/// self-check record under it) is its own.
pub fn tagged_conversation(tag: &str, users: usize) -> CompletionRequest {
    let mut history = Vec::new();
    for n in 1..users {
        let tag = if n == 1 { tag } else { "" };
        history.push(Message::user(format!("question {n}{tag}")));
        history.push(Message::assistant(format!("answer {n}")));
    }
    let tag = if users == 1 { tag } else { "" };
    CompletionRequest::new(Message::user(format!("question {users}{tag}"))).messages(history)
}

impl Claude {
    /// The frames the double read in its `run`th model process, initialize requests left out.
    pub fn run_frames(&self, run: u64) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|entry| entry["kind"] == "frame" && entry["run"] == run)
            .map(|entry| entry["value"].clone())
            .collect()
    }
}

pub fn cached(read: u64, write_1h: u64) -> Value {
    json!({"input_tokens": 1, "output_tokens": 7, "cache_read_input_tokens": read,
           "cache_creation": {"ephemeral_1h_input_tokens": write_1h}})
}

pub async fn three_steps(version: &str, second_read: u64) -> Claude {
    let claude = Claude::new();
    claude.scenario(json!({
        "claude_code_version": version,
        "responses": [
            {"text": "a", "usage": cached(0, 500)},
            {"text": "b", "usage": cached(second_read, 100)},
            {"text": "c", "usage": cached(1000, 0)},
        ],
    }));
    let handle = main_handle(&claude).await;
    for users in [2, 3, 4] {
        handle
            .completion
            .call(tagged_conversation(version, users))
            .await
            .unwrap();
    }
    claude
}
