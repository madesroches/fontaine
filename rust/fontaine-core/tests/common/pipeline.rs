#![allow(dead_code)]

//! A permission pipeline over a scratch world, with an approver that answers from a script.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fontaine_core::audit::DecisionRecord;
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::{SessionState, StateParams};
use fontaine_core::permissions::{
    Answer, ApprovalRequest, Approver, PermissionPipeline, PipelineParams, PreToolUseHook,
};
use fontaine_core::session::Session;

use fontaine_tools::gate::{CallView, GateDecision, ROOT_AGENT, ToolGate};
use fontaine_tools::risk::{RiskClass, builtin_risk};
use fontaine_tools::scope::Scope;
use futures::future::BoxFuture;
use serde_json::Value;

use super::World;

/// Answers prompts from a queue; an empty queue denies.
#[derive(Default)]
pub struct Scripted {
    answers: Mutex<VecDeque<Answer>>,
    pub requests: Mutex<Vec<ApprovalRequest>>,
    pub notices: Mutex<Vec<String>>,
}

impl Scripted {
    pub fn answer(&self, answer: Answer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    pub fn asked(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Approver for Scripted {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Answer::Deny { feedback: None })
        })
    }

    fn notice(&self, message: &str) {
        self.notices.lock().unwrap().push(message.to_string());
    }
}

pub struct Options {
    pub settings: String,
    pub mode: PermissionMode,
    pub headless: bool,
    pub allow_extra: Vec<String>,
    pub home: Option<PathBuf>,
    pub hooks: Vec<Arc<dyn PreToolUseHook>>,
    pub always_allow_file: bool,
}

impl Options {
    pub fn new(mode: PermissionMode) -> Options {
        Options {
            settings: String::new(),
            mode,
            headless: false,
            allow_extra: Vec::new(),
            home: None,
            hooks: Vec::new(),
            always_allow_file: true,
        }
    }

    pub fn settings(mut self, text: &str) -> Options {
        self.settings = text.to_string();
        self
    }

    pub fn headless(mut self) -> Options {
        self.headless = true;
        self
    }
}

pub struct Rig {
    pub world: World,
    pub state: Arc<SessionState>,
    pub pipeline: PermissionPipeline,
    pub approver: Arc<Scripted>,
    pub session: Session,
    pub user_config: PathBuf,
    next: AtomicUsize,
}

impl Rig {
    pub fn new(mode: PermissionMode) -> Rig {
        Rig::with(Options::new(mode))
    }

    pub fn with(options: Options) -> Rig {
        let world = World::new();
        world.user_file(&options.settings);
        let config = world.load().unwrap();
        let session = Session::create(&world.data, "00000000-0000-4000-8000-000000000001").unwrap();
        let scope = Arc::new(Scope::new(config.workspace.clone(), []));
        let state = Arc::new(SessionState::new(StateParams {
            session_id: session.id.clone(),
            mode: options.mode,
            scope,
            deny: config.permissions.deny.clone(),
            ask: config.permissions.ask.clone(),
            allow: config.permissions.allow.clone(),
            ledger: session.ledger.clone(),
            audit: session.audit.clone(),
            home: options.home.clone(),
            protected_dirs: vec![world.config.clone(), world.data.clone()],
        }));
        let approver = Arc::new(Scripted::default());
        let user_config = world.config.join("config.toml");
        let pipeline = PermissionPipeline::new(PipelineParams {
            state: state.clone(),
            approver: approver.clone(),
            headless: options.headless,
            hooks: options.hooks,
            allow_extra: options.allow_extra,
            user_config: options.always_allow_file.then(|| user_config.clone()),
        });
        Rig {
            world,
            state,
            pipeline,
            approver,
            session,
            user_config,
            next: AtomicUsize::new(1),
        }
    }

    pub fn work(&self) -> &std::path::Path {
        &self.world.work
    }

    pub fn risk(tool: &str) -> RiskClass {
        builtin_risk(tool).unwrap_or(RiskClass::Arbitrary)
    }

    /// One call through the pipeline as the root agent, with a fresh call id.
    pub async fn check(&self, tool: &str, args: Value) -> GateDecision {
        let id = format!("call-{}", self.next.fetch_add(1, Ordering::SeqCst));
        self.state.set_current_call(ROOT_AGENT, id);
        self.pipeline
            .check(CallView {
                tool,
                risk: Rig::risk(tool),
                args: &args,
                agent: ROOT_AGENT,
            })
            .await
    }

    pub fn audit(&self) -> Vec<DecisionRecord> {
        self.session.audit.read().unwrap()
    }

    /// The first record the last call produced.
    pub fn last_first(&self, from: usize) -> DecisionRecord {
        self.audit()[from].clone()
    }
}

pub fn allowed(decision: &GateDecision) -> bool {
    matches!(decision, GateDecision::Allow { .. })
}

pub fn denied_message(decision: &GateDecision) -> &str {
    match decision {
        GateDecision::Deny { message } => message,
        other => panic!("expected a denial, got {other:?}"),
    }
}
