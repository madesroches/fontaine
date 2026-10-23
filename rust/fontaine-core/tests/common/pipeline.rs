#![allow(dead_code)]

//! A permission pipeline over a scratch world, with an approver that answers from a script.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fontaine_agents::tree::{AgentTree, Limits};
use fontaine_core::audit::DecisionRecord;
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::{AutoContext, SessionState, StateParams};
use fontaine_core::permissions::{
    Answer, ApprovalRequest, Approver, GuardianUnavailable, PermissionDeniedHook,
    PermissionPipeline, PipelineParams, PreToolUseHook, UnavailableChoice,
};
use fontaine_core::session::Session;
use fontaine_guardian::{Guardian, Settings};
use rig_core::test_utils::{MockCompletionModel, MockTurn};

use fontaine_tools::gate::{CallView, GateDecision, ROOT_AGENT, ToolGate};
use fontaine_tools::risk::{RiskClass, registered_risk};
use fontaine_tools::scope::Scope;
use futures::future::BoxFuture;
use serde_json::Value;

use super::World;

/// Answers prompts from a queue; an empty queue denies.
#[derive(Default)]
pub struct Scripted {
    answers: Mutex<VecDeque<Answer>>,
    unavailable: Mutex<VecDeque<UnavailableChoice>>,
    pub requests: Mutex<Vec<ApprovalRequest>>,
    pub notices: Mutex<Vec<String>>,
    pub unavailable_asked: Mutex<Vec<GuardianUnavailable>>,
}

impl Scripted {
    pub fn answer(&self, answer: Answer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    pub fn asked(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The next answer to "the guardian is unavailable"; an empty queue leaves the call denied.
    pub fn when_unavailable(&self, choice: UnavailableChoice) {
        self.unavailable.lock().unwrap().push_back(choice);
    }

    pub fn last_request(&self) -> ApprovalRequest {
        self.requests
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a prompt")
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

    fn guardian_unavailable<'a>(
        &'a self,
        info: GuardianUnavailable,
    ) -> BoxFuture<'a, UnavailableChoice> {
        Box::pin(async move {
            self.unavailable_asked.lock().unwrap().push(info);
            self.unavailable
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(UnavailableChoice::Deny)
        })
    }
}

pub struct Options {
    pub settings: String,
    pub mode: PermissionMode,
    pub headless: bool,
    pub allow_extra: Vec<String>,
    pub home: Option<PathBuf>,
    pub hooks: Vec<Arc<dyn PreToolUseHook>>,
    pub denied_hooks: Vec<Arc<dyn PermissionDeniedHook>>,
    pub always_allow_file: bool,
    pub guardian: Option<Arc<Guardian>>,
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
            denied_hooks: Vec::new(),
            always_allow_file: true,
            guardian: None,
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

    pub fn guardian(mut self, guardian: Arc<Guardian>) -> Options {
        self.guardian = Some(guardian);
        self
    }
}

pub struct Rig {
    pub world: World,
    pub agents: Arc<AgentTree>,
    pub state: Arc<SessionState>,
    pub pipeline: Arc<PermissionPipeline>,
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
        let agents = Arc::new(AgentTree::new(Limits::default()));
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
            guardian: options
                .guardian
                .clone()
                .unwrap_or_else(fontaine_core::gate::no_guardian),
            auto: AutoContext::default(),
            agents: agents.clone(),
            reveal_mcp_names: false,
        }));
        let approver = Arc::new(Scripted::default());
        let user_config = world.config.join("config.toml");
        let pipeline = Arc::new(PermissionPipeline::new(PipelineParams {
            state: state.clone(),
            approver: approver.clone(),
            headless: options.headless,
            hooks: options.hooks,
            denied_hooks: options.denied_hooks,
            allow_extra: options.allow_extra,
            user_config: options.always_allow_file.then(|| user_config.clone()),
        }));
        Rig {
            world,
            agents,
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
        registered_risk(tool).unwrap_or(RiskClass::Arbitrary)
    }

    /// One call through the pipeline as the root agent, with a fresh call id.
    pub async fn check(&self, tool: &str, args: Value) -> GateDecision {
        self.check_as(ROOT_AGENT, tool, args).await
    }

    /// One call through the pipeline as `agent`, with a fresh call id.
    pub async fn check_as(&self, agent: &str, tool: &str, args: Value) -> GateDecision {
        let id = format!("call-{}", self.next.fetch_add(1, Ordering::SeqCst));
        self.state.set_current_call(agent, id);
        self.pipeline
            .check(CallView {
                tool,
                risk: Rig::risk(tool),
                args: &args,
                agent,
                annotations: None,
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

/// A guardian over a scripted model that answers every adjudication with the same verdict, and
/// counts how often it was asked.
pub struct Fake {
    pub mock: MockCompletionModel,
    pub guardian: Arc<Guardian>,
}

impl Fake {
    pub fn calls(&self) -> usize {
        self.mock.request_count()
    }
}

pub fn allow_verdict() -> Value {
    verdict_json("allow", "allow", 0.95)
}

pub fn verdict_json(decision: &str, rule: &str, confidence: f64) -> Value {
    serde_json::json!({
        "decision": decision,
        "rule": rule,
        "explanation": format!("{decision}: {rule}"),
        "confidence": confidence,
    })
}

pub fn fake_guardian(verdict: Value) -> Fake {
    fake_guardian_turns(vec![verdict], Settings::default())
}

/// The guardian answers `verdicts` in order and repeats the last one.
pub fn fake_guardian_turns(verdicts: Vec<Value>, settings: Settings) -> Fake {
    let last = verdicts.last().cloned().expect("a verdict");
    let turns = verdicts
        .into_iter()
        .chain(std::iter::repeat_n(last, 200))
        .map(|v| MockTurn::tool_call("call_1", "submit", v));
    let mock = MockCompletionModel::from_turns(turns);
    let guardian = Arc::new(Guardian::new(
        mock.clone().into(),
        "test/guardian",
        settings,
    ));
    Fake { mock, guardian }
}
