#![allow(dead_code)]

//! A permission pipeline over a scratch world, with an approver that answers from a script.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

use fontaine_tools::ask_user::{Question, QuestionReply};
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
    pub warnings: Mutex<Vec<String>>,
    pub unavailable_asked: Mutex<Vec<GuardianUnavailable>>,
    replies: Mutex<VecDeque<QuestionReply>>,
    pub questions: Mutex<Vec<Vec<Question>>>,
    /// While set, a prompt records its request and is never answered.
    pub hang: AtomicBool,
}

impl Scripted {
    pub fn answer(&self, answer: Answer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    /// The next reply to the model's questions; an empty queue reports nobody can answer.
    pub fn reply(&self, reply: QuestionReply) {
        self.replies.lock().unwrap().push_back(reply);
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
            if self.hang.load(Ordering::SeqCst) {
                futures::future::pending::<()>().await;
            }
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Answer::Deny { feedback: None })
        })
    }

    fn ask_user<'a>(
        &'a self,
        _agent: &'a str,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, QuestionReply> {
        Box::pin(async move {
            self.questions.lock().unwrap().push(questions);
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(QuestionReply::Unavailable)
        })
    }

    fn notice(&self, message: &str) {
        self.notices.lock().unwrap().push(message.to_string());
    }

    fn warn(&self, message: &str) {
        self.warnings.lock().unwrap().push(message.to_string());
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
    /// Replaces the scripted approver the pipeline and session state are built with.
    pub approver: Option<Arc<dyn Approver>>,
    /// Gives the rig a session scratch root beside the launch directory.
    pub scratch: bool,
    /// Gives the rig an auto-memory root under the data directory, as a session with
    /// `[memory] auto` on has.
    pub memory: bool,
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
            approver: None,
            scratch: false,
            memory: false,
        }
    }

    pub fn scratch(mut self) -> Options {
        self.scratch = true;
        self
    }

    pub fn memory(mut self) -> Options {
        self.memory = true;
        self
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
    pub scratch: Option<PathBuf>,
    pub memory: Option<PathBuf>,
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
        let scratch = options.scratch.then(|| {
            let dir = world.root.join("scratch");
            std::fs::create_dir_all(&dir).unwrap();
            dir
        });
        let memory = options.memory.then(|| {
            let dir = world.data.join("memory").join("project-1");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::canonicalize(dir).unwrap()
        });
        let scope = Arc::new(Scope::new(
            config.workspace.clone(),
            scratch.iter().chain(&memory).cloned(),
        ));
        let agents = Arc::new(AgentTree::new(Limits::default()));
        let scripted = Arc::new(Scripted::default());
        let gate_approver: Arc<dyn Approver> =
            options.approver.clone().unwrap_or_else(|| scripted.clone());
        let state = Arc::new(SessionState::new(StateParams {
            session_id: session.id.clone(),
            mode: options.mode,
            scope,
            deny: config.permissions.deny.clone(),
            ask: config.permissions.ask.clone(),
            allow: config.permissions.allow.clone(),
            ledger: session.ledger.clone(),
            audit: session.audit.clone(),
            approver: gate_approver.clone(),
            home: options.home.clone(),
            protected_dirs: vec![world.config.clone(), world.data.clone()],
            scratch: scratch.clone(),
            memory_root: memory.clone(),
            guardian: options
                .guardian
                .clone()
                .unwrap_or_else(fontaine_core::gate::no_guardian),
            auto: AutoContext::default(),
            agents: agents.clone(),
            reveal_mcp_names: false,
            auto_edits: fontaine_core::gate::AutoEdits {
                tracked: config.permissions.auto_tracked_edits,
                scratch: config.permissions.auto_scratch_writes,
            },
        }));
        let user_config = world.config.join("config.toml");
        let pipeline = Arc::new(PermissionPipeline::new(PipelineParams {
            state: state.clone(),
            approver: gate_approver,
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
            approver: scripted,
            session,
            user_config,
            scratch,
            memory,
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
        self.check_flagged(agent, tool, args, false).await
    }

    /// One root-agent call to an MCP tool its server marked as needing approval every time.
    pub async fn check_marked(&self, tool: &str, args: Value) -> GateDecision {
        self.check_flagged(ROOT_AGENT, tool, args, true).await
    }

    async fn check_flagged(
        &self,
        agent: &str,
        tool: &str,
        args: Value,
        requires_user_interaction: bool,
    ) -> GateDecision {
        let id = format!("call-{}", self.next.fetch_add(1, Ordering::SeqCst));
        self.state.set_current_call(agent, id);
        self.pipeline
            .check(CallView {
                tool,
                risk: Rig::risk(tool),
                args: &args,
                agent,
                annotations: None,
                requires_user_interaction,
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

/// A guardian whose model answers after `delay`, with the same verdict every time. `asked`
/// counts requests received; `completed` counts answers the client was still waiting for when
/// they were sent, so a dropped adjudication leaves it at zero.
pub struct Delayed {
    pub guardian: Arc<Guardian>,
    pub asked: Arc<AtomicUsize>,
    pub completed: Arc<AtomicUsize>,
}

pub async fn delayed_guardian(verdict: Value, delay: std::time::Duration) -> Delayed {
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::wire::{Auth, Dialect};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let asked = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let body = serde_json::json!({
        "id": "chatcmpl-delayed",
        "object": "chat.completion",
        "created": 1,
        "model": "delayed",
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "submit", "arguments": verdict.to_string()}}],
        }}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    })
    .to_string();
    let (asked_by, completed_by) = (asked.clone(), completed.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (asked, completed, body) = (asked_by.clone(), completed_by.clone(), body.clone());
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65536];
                let mut seen = 0;
                loop {
                    let n = stream.read(&mut buffer[seen..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    seen += n;
                    let head = String::from_utf8_lossy(&buffer[..seen]).to_string();
                    if let Some(end) = head.find("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if seen >= end + 4 + length {
                            break;
                        }
                    }
                }
                asked.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                // A client that gave up closed its end while the answer was pending.
                let mut probe = [0u8; 1];
                let waiting = tokio::time::timeout(
                    std::time::Duration::from_millis(20),
                    stream.read(&mut probe),
                )
                .await
                .is_err();
                if !waiting {
                    return;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                if stream.write_all(response.as_bytes()).await.is_ok() {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    let client = OpenAIConfig::with_key(&Dialect::gateway("openai-compatible", "", ""), "key")
        .with_auth(Auth::OptionalBearer)
        .with_base_url(&url)
        .client();
    let guardian = Arc::new(Guardian::new(
        client.completion("delayed").erase(),
        "test/delayed",
        Settings::default(),
    ));
    Delayed {
        guardian,
        asked,
        completed,
    }
}
