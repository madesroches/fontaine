//! An embedded session: the headless run loop on a task of its own, fed prompts through a
//! channel and read back as the `headless-v1` event stream.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::Stream;
use futures::future::BoxFuture;
use rig_core::schemars::Schema;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

use crate::assemble::{
    self, ConfigInputs, Environment, HeadlessStart, PrepareParams, SessionParams, SetupParams,
};
use crate::core::afk_report::AfkReport;
use crate::core::agent::{AgentRuntime, AgentSetup, EnvLookup};
#[cfg(test)]
use crate::core::config::AccountKind;
use crate::core::config::{Config, MatcherKind, PermissionMode};
use crate::core::dirs::{self, Dirs};
use crate::core::notice::Notice;
use crate::core::permissions::{Answer, ApprovalRequest, Approver, HumanChannel};
use crate::core::providers::claude_subscription::PROVIDER_NAME as CLAUDE_SUBSCRIPTION;
use crate::core::scratch::Scratch;
use crate::core::turn::SessionEvent;
use crate::core::usage_pause::PauseEvent;
use crate::core::user_hooks::CallbackHook;
use crate::error::EmbedError;
use crate::events::{self, Event};
use crate::run::{
    Cancel, EventSink, InputFormat, InputSource, Options, OutputFormat, PauseObserver, ResultInfo,
    run_session,
};
use crate::tools::host::{self, HostTool};
use crate::tools::lock::lock;

/// The approver of a session built without one: denies every prompt and routes notices through
/// `warn`, so the only stderr write is the trait's documented default `warn`.
struct NoApprover;

impl Approver for NoApprover {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn notice(&self, message: &str) {
        self.warn(message);
    }
}

static NO_SEARCH_WARNED: AtomicBool = AtomicBool::new(false);

static HOST_TOOL_NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

/// Leaks a host tool name once per process, however many sessions are built with it.
fn intern(name: &str) -> &'static str {
    let mut names = lock(&HOST_TOOL_NAMES, "the host tool name table");
    let names = names.get_or_insert_with(HashSet::new);
    match names.get(name) {
        Some(known) => known,
        None => {
            let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
            names.insert(leaked);
            leaked
        }
    }
}

/// Which stored session an embedded session starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionRef {
    /// The latest session of this project; a fresh start when there is none.
    Continue,
    /// A session id or name.
    Id(String),
}

/// Describes an embedded session. Nothing here can turn the permission floor or the guardian off:
/// the builder has no such setting, and project settings still load as tighten-only.
#[must_use]
pub struct SessionBuilder {
    project_dir: PathBuf,
    dirs: Option<(PathBuf, PathBuf)>,
    env: Option<EnvLookup>,
    mode: Option<PermissionMode>,
    model: Option<String>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    json_schema: Option<Value>,
    max_turns: Option<usize>,
    max_budget_usd: Option<f64>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    builtin_tools: Option<Vec<String>>,
    include_partial_messages: bool,
    add_dirs: Vec<PathBuf>,
    resume: Option<SessionRef>,
    fork: bool,
    approver: Option<Arc<dyn Approver>>,
    hooks: Vec<CallbackHook>,
    tools: Vec<Arc<dyn HostTool>>,
}

/// Refuses a session whose settings configure a `claude-subscription` account, referenced or not:
/// the `agent` tool's `model:` argument and agent types can reach any account, and that provider
/// re-executes the host binary with `--internal-claude-inventory`, which only `fontaine` handles.
fn refuse_claude_subscription(config: &Config) -> Result<(), EmbedError> {
    for (name, account) in &config.accounts {
        if account.kind.as_str() == CLAUDE_SUBSCRIPTION {
            return Err(EmbedError::config(format!(
                "account `{name}` is a claude-subscription account; that provider needs the host binary to implement the internal inventory mode (`--internal-claude-inventory`) and is not supported for embedding"
            )));
        }
    }
    Ok(())
}

/// What the builder's arguments came to once they are checked.
struct Checked {
    schema: Option<Schema>,
    host_tools: Vec<(&'static str, Arc<dyn HostTool>)>,
}

impl SessionBuilder {
    pub fn new(project_dir: impl Into<PathBuf>) -> SessionBuilder {
        SessionBuilder {
            project_dir: project_dir.into(),
            dirs: None,
            env: None,
            mode: None,
            model: None,
            system_prompt: None,
            append_system_prompt: None,
            json_schema: None,
            max_turns: None,
            max_budget_usd: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            builtin_tools: None,
            include_partial_messages: false,
            add_dirs: Vec::new(),
            resume: None,
            fork: false,
            approver: None,
            hooks: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// The config and data directories. Without them the platform's (or the `FONTAINE_CONFIG_DIR`
    /// and `FONTAINE_DATA_DIR` overrides) are used.
    pub fn dirs(mut self, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        self.dirs = Some((config_dir, data_dir));
        self
    }

    /// Where environment variables are looked up. The default is the process environment. The
    /// names Fontaine scrubs from tool subprocesses always come from the real process
    /// environment, because a lookup cannot list names.
    pub fn env(mut self, env: EnvLookup) -> Self {
        self.env = Some(env);
        self
    }

    /// The permission mode to start in; by default `[permissions] default_mode`, then `manual`.
    pub fn mode(mut self, mode: PermissionMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// A model reference or role name, which wins over the settings.
    pub fn model(mut self, reference: &str) -> Self {
        self.model = Some(reference.to_string());
        self
    }

    pub fn system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    pub fn append_system_prompt(mut self, prompt: String) -> Self {
        self.append_system_prompt = Some(prompt);
        self
    }

    /// A JSON Schema object the final answer must follow; it arrives as `structured_output`.
    pub fn json_schema(mut self, schema: Value) -> Self {
        self.json_schema = Some(schema);
        self
    }

    pub fn max_turns(mut self, turns: usize) -> Self {
        self.max_turns = Some(turns);
        self
    }

    /// Stops a turn once the session's spend passes this. The main model needs a known price.
    pub fn max_budget_usd(mut self, usd: f64) -> Self {
        self.max_budget_usd = Some(usd);
        self
    }

    /// Allow rules at the command-line scope, which sit below the permission floor.
    pub fn allowed_tools(mut self, rules: Vec<String>) -> Self {
        self.allowed_tools = rules;
        self
    }

    pub fn disallowed_tools(mut self, rules: Vec<String>) -> Self {
        self.disallowed_tools = rules;
        self
    }

    /// The tools the root agent is built with. The list must also name every host tool to keep.
    pub fn builtin_tools(mut self, names: Vec<String>) -> Self {
        self.builtin_tools = Some(names);
        self
    }

    /// Whether `assistant_delta` events are emitted as the answer streams.
    pub fn include_partial_messages(mut self, on: bool) -> Self {
        self.include_partial_messages = on;
        self
    }

    pub fn add_dir(mut self, dir: PathBuf) -> Self {
        self.add_dirs.push(dir);
        self
    }

    pub fn resume(mut self, session: SessionRef) -> Self {
        self.resume = Some(session);
        self
    }

    /// Copies the resumed session instead of reopening it.
    pub fn fork(mut self) -> Self {
        self.fork = true;
        self
    }

    /// Answers the prompts a person would answer. It is asked only after the floor, the hooks,
    /// the rules, the mode and the guardian, and is never offered "always allow". Without one,
    /// every would-prompt is a denial.
    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// A hook the host runs for tool events. It can only tighten a decision.
    pub fn hook(mut self, hook: CallbackHook) -> Self {
        self.hooks.push(hook);
        self
    }

    /// A tool the model can call. Every call is gated as an arbitrary-risk tool.
    pub fn tool(mut self, tool: impl HostTool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    fn check(&self) -> Result<Checked, EmbedError> {
        if self.max_turns == Some(0) {
            return Err(EmbedError::config("max_turns must be at least 1"));
        }
        if let Some(budget) = self.max_budget_usd
            && !(budget.is_finite() && budget >= 0.0)
        {
            return Err(EmbedError::config(
                "max_budget_usd must be a finite number, zero or more",
            ));
        }
        let schema = match &self.json_schema {
            None => None,
            Some(value) => {
                let not_a_schema =
                    || EmbedError::config("json_schema must be a JSON Schema object");
                if !value.is_object() {
                    return Err(not_a_schema());
                }
                Some(Schema::try_from(value.clone()).map_err(|_| not_a_schema())?)
            }
        };
        if self.fork && self.resume.is_none() {
            return Err(EmbedError::config("fork needs resume"));
        }
        let mut names: Vec<&str> = Vec::new();
        for tool in &self.tools {
            let name = tool.name();
            if let Some(why) = host::name_refusal(name) {
                return Err(EmbedError::config(why));
            }
            if names.contains(&name) {
                return Err(EmbedError::config(format!(
                    "the host tool name `{name}` is used twice"
                )));
            }
            names.push(name);
        }
        for rule in self.allowed_tools.iter().chain(&self.disallowed_tools) {
            let tool = rule.split('(').next().unwrap_or(rule).trim();
            if names.contains(&tool) {
                return Err(EmbedError::config(format!(
                    "`{rule}` names a host tool, which permission rules cannot address; tighten it with a hook or the approver"
                )));
            }
        }
        for hook in &self.hooks {
            if hook.event.matcher_kind() != MatcherKind::Tool {
                continue;
            }
            let Some(matcher) = hook.matcher.as_deref() else {
                continue;
            };
            let tool = matcher.split('(').next().unwrap_or(matcher).trim();
            if names.contains(&tool) {
                return Err(EmbedError::config(format!(
                    "the hook matcher `{matcher}` names a host tool, which matchers cannot address; match every call and filter in the callback"
                )));
            }
        }
        // Past every check, so a refused builder leaks nothing. The setup keeps these names for
        // plugin reloads, so a name is leaked once per tool and never again.
        let host_tools = self
            .tools
            .iter()
            .map(|tool| (intern(tool.name()), tool.clone()))
            .collect();
        Ok(Checked { schema, host_tools })
    }

    fn session_params(&self) -> SessionParams {
        SessionParams {
            id: None,
            name: None,
            resume: match &self.resume {
                Some(SessionRef::Id(id)) => Some(id.clone()),
                _ => None,
            },
            continue_session: matches!(self.resume, Some(SessionRef::Continue)),
            fork: self.fork,
        }
    }

    /// Everything the runtime's setup takes from the host.
    fn wiring(&self, host_tools: Vec<(&'static str, Arc<dyn HostTool>)>) -> Wiring {
        Wiring {
            approver: self
                .approver
                .clone()
                .unwrap_or_else(|| Arc::new(NoApprover) as Arc<dyn Approver>),
            human_channel: human_channel(self.approver.is_some()),
            hooks: self.hooks.clone(),
            host_tools,
            root_tools: self.builtin_tools.clone(),
        }
    }

    /// Builds the session: loads the settings, opens the session, resolves the models and starts
    /// the run loop. Warnings found on the way go to the approver's `warn`.
    pub async fn build(self) -> Result<Session, EmbedError> {
        let checked = self.check()?;
        let env = match &self.env {
            Some(env) => Environment {
                cwd: self.project_dir.clone(),
                env: env.clone(),
                env_names: std::env::vars_os().map(|(name, _)| name).collect(),
            },
            None => Environment::from_process(self.project_dir.clone()),
        };
        let dirs = match &self.dirs {
            Some((config, data)) => Dirs {
                config: config.clone(),
                data: data.clone(),
            },
            None => dirs::resolve(&*env.env, ::dirs::config_dir(), ::dirs::data_dir())
                .map_err(EmbedError::config_from)?,
        };
        let mut config = assemble::load_config(
            &env,
            &dirs,
            &ConfigInputs {
                settings_file: None,
                allow: &self.allowed_tools,
                deny: &self.disallowed_tools,
                afk_temperature_ceiling: None,
            },
        )
        .map_err(EmbedError::config_from)?;
        refuse_claude_subscription(&config)?;
        let requested = assemble::requested_mode(self.mode, &config, true);
        let wiring = self.wiring(checked.host_tools);
        let approver = wiring.approver.clone();

        let mut warnings: Vec<Notice> = Vec::new();
        let (project_executables, untrusted) =
            assemble::headless_project_executables(&config, &dirs.data);
        warnings.extend(untrusted.map(|line| Notice::warning(line, None)));
        let prepared = assemble::prepare_session(
            &PrepareParams {
                env: &env,
                dirs: &dirs,
                add_dirs: &self.add_dirs,
                session: &self.session_params(),
                mode: assemble::session_mode(requested),
                project_executables,
                record_resume: true,
            },
            &mut config,
            &mut warnings,
        );
        let (mut prep, scratch) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                for warning in &warnings {
                    approver.warn(&warning.to_string());
                }
                return Err(e);
            }
        };
        for warning in std::mem::take(&mut prep.warnings) {
            approver.warn(&warning.to_string());
        }

        let scanner_notice: assemble::NoticeFn = {
            let approver = approver.clone();
            Arc::new(move |line| approver.warn(line))
        };
        let started = assemble::start_headless(
            HeadlessStart {
                config: &config,
                dirs: &dirs,
                env: &env.env,
                model_override: self.model.as_deref(),
                setup: SetupParams {
                    max_budget_usd: self.max_budget_usd,
                    system_prompt: self.system_prompt.clone(),
                    append_system_prompt: self.append_system_prompt.clone(),
                    json_schema: checked.schema.clone(),
                    headless: true,
                    afk: requested == PermissionMode::Afk,
                    warn_no_search: !NO_SEARCH_WARNED.swap(true, Ordering::Relaxed),
                    ..SetupParams::default()
                },
                scanner_notice: Some(scanner_notice),
                budget_name: "max_budget_usd",
            },
            prep,
            |setup| wiring.apply(setup),
        )
        .await?;
        let runtime = started.runtime;
        if let Some((account, login)) = started.guardian_login
            && matches!(
                runtime.start_mode(),
                PermissionMode::Auto | PermissionMode::Afk
            )
        {
            return Err(EmbedError::credentials(format!(
                "the guardian's account `{account}` is not signed in; run `{}`, then retry",
                login.display
            )));
        }

        let id = runtime.session.id.clone();
        let cancel = Cancel::default();
        let abandoned = Arc::new(AtomicBool::new(false));
        let (prompts, inbox) = mpsc::unbounded_channel();
        let (outbox, events) = mpsc::unbounded_channel();
        let afk = Arc::new(Mutex::new(None));
        let input = ChannelInput {
            rx: inbox,
            cancel: cancel.clone(),
            abandoned: abandoned.clone(),
        };
        let sink = ChannelSink {
            tx: outbox,
            include_partial_messages: self.include_partial_messages,
            approver,
            afk: afk.clone(),
        };
        let options = Options {
            prompt: None,
            input_format: InputFormat::StreamJson,
            // The stdout tail of `run_session` belongs to `fontaine -p`.
            output_format: OutputFormat::StreamJson,
            include_partial_messages: self.include_partial_messages,
            max_turns: self.max_turns,
            max_budget_usd: self.max_budget_usd,
            on_usage_limit: None,
            max_usage_wait: None,
            json_schema: checked.schema,
        };
        let data_dir = dirs.data.clone();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let mut runtime = runtime;
                let mut sink = sink;
                let code = run_session(
                    options,
                    &mut runtime,
                    &data_dir,
                    None,
                    input,
                    &mut sink,
                    &cancel,
                )
                .await;
                (runtime, code)
            }
        });
        Ok(Session {
            id,
            input: Some(prompts),
            feed: Feed::new(events),
            task: Some(task),
            exited: None,
            cancel,
            abandoned,
            afk,
            scratch,
        })
    }
}

/// `Absent` backs a session with no approver: a would-prompt is a denial. `Present` means a
/// person at a terminal, which an embedded session never has.
fn human_channel(has_approver: bool) -> HumanChannel {
    if has_approver {
        HumanChannel::Delegate
    } else {
        HumanChannel::Absent
    }
}

/// What the host hands the runtime's setup.
struct Wiring {
    approver: Arc<dyn Approver>,
    human_channel: HumanChannel,
    hooks: Vec<CallbackHook>,
    host_tools: Vec<(&'static str, Arc<dyn HostTool>)>,
    root_tools: Option<Vec<String>>,
}

impl Wiring {
    fn apply(self, setup: &mut AgentSetup) {
        setup.approver = self.approver;
        setup.human_channel = self.human_channel;
        setup.callback_hooks.extend(self.hooks);
        setup.host_tools = self.host_tools;
        setup.root_tools = self.root_tools;
        setup.confirm_project_servers = false;
        setup.scheduling = false;
    }
}

/// Where the run's prompts come from: the messages `Session::send` pushes.
struct ChannelInput {
    rx: mpsc::UnboundedReceiver<String>,
    cancel: Cancel,
    /// Set when the session is dropped without `close`: prompts still queued must not run.
    abandoned: Arc<AtomicBool>,
}

impl InputSource for ChannelInput {
    fn is_exhausted(&self) -> bool {
        false
    }

    async fn next(&mut self, _runtime: &mut AgentRuntime) -> Result<Option<String>, String> {
        let message = self.rx.recv().await;
        // An interrupt is never cleared by the loop itself, so each new message starts clean.
        // Reset first: a drop that lands after this still sets `abandoned` before it triggers.
        if message.is_some() {
            self.cancel.reset();
        }
        if self.abandoned.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(message)
    }

    fn pending(&self) -> bool {
        !self.rx.is_empty()
    }

    fn long_lived(&self) -> bool {
        true
    }
}

/// Carries the run's events to the session's turns, and its lines to the approver.
struct ChannelSink {
    tx: mpsc::UnboundedSender<Event>,
    include_partial_messages: bool,
    approver: Arc<dyn Approver>,
    afk: Arc<Mutex<Option<AfkReport>>>,
}

impl ChannelSink {
    fn send(&self, event: Event) {
        // The session dropped its end: nobody is reading.
        let _ = self.tx.send(event);
    }
}

impl EventSink for ChannelSink {
    fn notice(&mut self, message: &str) {
        self.approver.warn(message);
    }

    fn session_start(&mut self, runtime: &AgentRuntime, data_dir: &Path) {
        self.send(events::init_event(runtime, data_dir));
    }

    fn event(&mut self, session_id: &str, event: &SessionEvent) {
        if let Some(event) = events::session_event(session_id, event, self.include_partial_messages)
        {
            self.send(event);
        }
    }

    fn goal_evaluation(
        &mut self,
        session_id: &str,
        judgment: Option<crate::core::goal::Judgment>,
        reason: &str,
        evaluations: u32,
    ) {
        self.send(events::goal_event(
            session_id,
            judgment,
            reason,
            evaluations,
        ));
    }

    fn wants_account_usage(&self) -> bool {
        true
    }

    fn result(&mut self, info: &ResultInfo<'_>) {
        self.send(events::result_event(
            info.session_id,
            info.outcome,
            info.usage,
            info.goal,
            info.account_usage,
            info.afk,
            info.usage_limit,
        ));
    }

    fn afk_report(&mut self, _session_id: &str, report: &AfkReport) {
        *lock(&self.afk, "the AFK report slot") = Some(report.clone());
    }

    fn usage_observer(&self, session_id: &str) -> Option<PauseObserver> {
        let tx = self.tx.clone();
        let id = session_id.to_string();
        Some(Arc::new(move |pause: &PauseEvent| {
            let _ = tx.send(events::usage_event(&id, pause));
        }))
    }
}

/// The run's events in order, shared by the turns one after another.
struct Feed {
    events: mpsc::UnboundedReceiver<Event>,
    /// Turns dropped before their `result`: the rest of their events is skipped.
    stale: usize,
}

impl Feed {
    fn new(events: mpsc::UnboundedReceiver<Event>) -> Feed {
        Feed { events, stale: 0 }
    }

    /// The next event that belongs to a live turn. `Ready(None)` is the end of the stream: the
    /// run is over.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        loop {
            match self.events.poll_recv(cx) {
                Poll::Ready(Some(event)) => {
                    if self.stale > 0 {
                        if matches!(event, Event::Result(_)) {
                            self.stale -= 1;
                        }
                        continue;
                    }
                    return Poll::Ready(Some(event));
                }
                other => return other,
            }
        }
    }
}

/// How the run's task ended.
enum Exit {
    Done(Box<AgentRuntime>, u8),
    /// The task panicked; the error is handed out once.
    Panicked(Option<JoinError>),
}

/// A running embedded session. Dropping it without [`Session::close`] cancels the run, closes the
/// input and lets the task end on its own; `SessionEnd` hooks fire only through `close`.
pub struct Session {
    id: String,
    input: Option<mpsc::UnboundedSender<String>>,
    feed: Feed,
    task: Option<JoinHandle<(AgentRuntime, u8)>>,
    exited: Option<Exit>,
    cancel: Cancel,
    abandoned: Arc<AtomicBool>,
    afk: Arc<Mutex<Option<AfkReport>>>,
    // After `exited`, so the runtime is gone before the scratch directory is removed.
    scratch: Option<Scratch>,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sends a prompt. The returned stream yields the turn's events and ends after its `result`.
    /// The first turn begins with `init`, and events emitted between turns (a usage pause or
    /// resume) arrive on the next turn. A turn dropped before its `result` has the rest of its
    /// events skipped, so the next one starts at its own first event.
    pub fn send(&mut self, prompt: impl Into<String>) -> Turn<'_> {
        let pushed = self
            .input
            .as_ref()
            .is_some_and(|input| input.send(prompt.into()).is_ok());
        let state = if pushed {
            TurnState::Live
        } else {
            TurnState::Failed(Some(self.ended_error()))
        };
        Turn {
            session: self,
            state,
        }
    }

    /// Stops the turn that is running, which ends with an error `result`. The next turn runs
    /// normally.
    pub fn interrupt_handle(&self) -> InterruptHandle {
        InterruptHandle(self.cancel.clone())
    }

    /// Ends the session: closes the input, waits for the run to finish what it holds, fires the
    /// `SessionEnd` hooks and removes the scratch directory. Returns the AFK report the run
    /// assessed once its last turn ended, if AFK was on. An error when the run ended with a
    /// failure.
    pub async fn close(mut self) -> Result<Option<AfkReport>, EmbedError> {
        self.input.take();
        let exit = match self.exited.take() {
            Some(exit) => exit,
            None => match self.task.take() {
                Some(task) => Exit::from(task.await),
                None => Exit::Panicked(None),
            },
        };
        let result = match exit {
            Exit::Done(runtime, code) => {
                runtime.fire_session_end("other").await;
                drop(runtime);
                if code == 0 {
                    Ok(())
                } else {
                    Err(EmbedError::runtime_with_code(
                        format!("the session's run ended with exit code {code}"),
                        code,
                    ))
                }
            }
            Exit::Panicked(Some(e)) => Err(EmbedError::runtime_from(e)),
            Exit::Panicked(None) => Err(EmbedError::runtime("the session's task panicked")),
        };
        self.scratch.take();
        let report = lock(&self.afk, "the AFK report slot").take();
        match result {
            Ok(()) => Ok(report),
            Err(e) => Err(e.with_afk_report(report)),
        }
    }

    fn ended_error(&mut self) -> EmbedError {
        if self.exited.is_none()
            && self.task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.task.take()
            && let Some(joined) = futures::FutureExt::now_or_never(task)
        {
            self.exited = Some(Exit::from(joined));
        }
        match &mut self.exited {
            Some(Exit::Done(_, code)) => EmbedError::runtime_with_code(
                format!("the session's run ended with exit code {code}"),
                *code,
            ),
            Some(Exit::Panicked(e)) => match e.take() {
                Some(e) => EmbedError::runtime_from(e),
                None => EmbedError::runtime("the session's task panicked"),
            },
            None => EmbedError::runtime("the session's run has ended"),
        }
    }
}

impl Exit {
    fn from(joined: Result<(AgentRuntime, u8), JoinError>) -> Exit {
        match joined {
            Ok((runtime, code)) => Exit::Done(Box::new(runtime), code),
            Err(e) => Exit::Panicked(Some(e)),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::SeqCst);
        self.cancel.trigger();
        self.input.take();
        let Some(task) = self.task.take() else {
            return;
        };
        let scratch = self.scratch.take();
        match tokio::runtime::Handle::try_current() {
            // The directory outlives the run: the task and the guard are released together.
            Ok(handle) => {
                handle.spawn(async move {
                    drop(task.await);
                    drop(scratch);
                });
            }
            Err(_) => {
                task.abort();
                drop(scratch);
            }
        }
    }
}

enum TurnState {
    Live,
    Failed(Option<EmbedError>),
    Done,
}

/// The events of one prompt, ending after its `result`. An `Err` item means the run itself ended.
pub struct Turn<'a> {
    session: &'a mut Session,
    state: TurnState,
}

impl Stream for Turn<'_> {
    type Item = Result<Event, EmbedError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match &mut this.state {
            TurnState::Done => return Poll::Ready(None),
            TurnState::Failed(error) => {
                let error = error.take();
                this.state = TurnState::Done;
                return Poll::Ready(error.map(Err));
            }
            TurnState::Live => {}
        }
        let session = &mut *this.session;
        loop {
            match session.feed.poll(cx) {
                Poll::Ready(Some(event)) => {
                    if matches!(event, Event::Result(_)) {
                        this.state = TurnState::Done;
                    }
                    return Poll::Ready(Some(Ok(event)));
                }
                Poll::Ready(None) => break,
                Poll::Pending => {
                    if session.exited.is_some() {
                        break;
                    }
                    let Some(task) = &mut session.task else {
                        break;
                    };
                    match Pin::new(task).poll(cx) {
                        Poll::Ready(joined) => {
                            session.task = None;
                            session.exited = Some(Exit::from(joined));
                            // What the run emitted before it ended is still in the channel.
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }
            }
        }
        this.state = TurnState::Done;
        Poll::Ready(Some(Err(session.ended_error())))
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if matches!(self.state, TurnState::Live) {
            self.session.feed.stale += 1;
        }
    }
}

/// Stops the running turn from another task.
#[derive(Clone)]
pub struct InterruptHandle(Cancel);

impl InterruptHandle {
    /// The turn ends with an error `result`, as in Claude Agent SDK mode. With no turn running
    /// this does nothing: the next prompt starts clean.
    pub fn interrupt(&self) {
        self.0.trigger();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use rig_core::tool::ToolExecutionError;

    struct Named(&'static str);

    impl HostTool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a tool"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<String, ToolExecutionError>> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    fn builder() -> SessionBuilder {
        SessionBuilder::new("/does/not/exist")
    }

    fn refusal(builder: SessionBuilder) -> String {
        match builder.check() {
            Err(EmbedError::Config(detail)) => detail.message().to_string(),
            Err(other) => panic!("not a config error: {other:?}"),
            Ok(_) => panic!("the builder was accepted"),
        }
    }

    #[test]
    fn an_unreferenced_claude_subscription_account_is_refused() {
        let mut config = Config::defaults(PathBuf::from("."));
        assert!(refuse_claude_subscription(&config).is_ok());
        config.accounts.insert(
            "sub".to_string(),
            crate::core::config::Account::new(AccountKind::named(CLAUDE_SUBSCRIPTION), None),
        );
        assert!(matches!(
            refuse_claude_subscription(&config),
            Err(EmbedError::Config(_))
        ));
    }

    #[test]
    fn a_plain_builder_is_accepted() {
        assert!(builder().check().is_ok());
    }

    #[test]
    fn max_turns_must_be_at_least_one() {
        assert!(refusal(builder().max_turns(0)).contains("max_turns"));
        assert!(builder().max_turns(1).check().is_ok());
    }

    #[test]
    fn the_budget_must_be_finite_and_not_negative() {
        for bad in [f64::NAN, f64::INFINITY, -0.5] {
            assert!(refusal(builder().max_budget_usd(bad)).contains("max_budget_usd"));
        }
        assert!(builder().max_budget_usd(0.0).check().is_ok());
        assert!(builder().max_budget_usd(2.5).check().is_ok());
    }

    #[test]
    fn the_json_schema_must_be_an_object() {
        for bad in [serde_json::json!([1]), serde_json::json!("x"), Value::Null] {
            assert!(refusal(builder().json_schema(bad)).contains("json_schema"));
        }
        let ok = builder().json_schema(serde_json::json!({"type": "object"}));
        assert!(ok.check().is_ok());
    }

    #[test]
    fn fork_needs_a_session_to_copy() {
        assert!(refusal(builder().fork()).contains("fork"));
        assert!(
            builder()
                .resume(SessionRef::Continue)
                .fork()
                .check()
                .is_ok()
        );
    }

    #[test]
    fn host_tool_names_are_checked() {
        for bad in ["", "shell", "mcp__a__b", "has space", &"a".repeat(65)] {
            // The name is leaked only for a builder that passes, and these do not.
            let name: &'static str = Box::leak(bad.to_string().into_boxed_str());
            let message = refusal(builder().tool(Named(name)));
            assert!(message.contains("host tool"), "{bad:?}: {message}");
        }
        assert!(builder().tool(Named("my_tool")).check().is_ok());
    }

    #[test]
    fn a_host_tool_name_twice_is_refused() {
        let message = refusal(builder().tool(Named("dup")).tool(Named("dup")));
        assert!(message.contains("twice"), "{message}");
    }

    #[test]
    fn rules_cannot_name_a_host_tool() {
        for rule in ["mine", "mine(anything)", " mine "] {
            let allowed = builder()
                .tool(Named("mine"))
                .allowed_tools(vec![rule.to_string()]);
            assert!(refusal(allowed).contains("host tool"), "{rule}");
            let denied = builder()
                .tool(Named("mine"))
                .disallowed_tools(vec![rule.to_string()]);
            assert!(refusal(denied).contains("host tool"), "{rule}");
        }
        let fine = builder()
            .tool(Named("mine"))
            .allowed_tools(vec!["read".to_string()]);
        assert!(fine.check().is_ok());
    }

    #[test]
    fn a_hook_matcher_cannot_name_a_host_tool() {
        use crate::core::config::HookEvent;
        use crate::core::permissions::HookError;
        use crate::core::user_hooks::HookTransport;
        struct Quiet;
        impl HookTransport for Quiet {
            fn call<'a>(
                &'a self,
                _: &'a str,
                _: &'a Value,
            ) -> BoxFuture<'a, Result<Value, HookError>> {
                Box::pin(async { Ok(Value::Null) })
            }
        }
        let hook = |matcher: &str| CallbackHook {
            event: HookEvent::PreToolUse,
            matcher: Some(matcher.to_string()),
            callback_id: "h".into(),
            timeout: None,
            fails_closed: true,
            transport: Arc::new(Quiet),
        };
        let message = refusal(builder().tool(Named("mine")).hook(hook("mine(x)")));
        assert!(message.contains("host tool"), "{message}");
        assert!(
            builder()
                .tool(Named("mine"))
                .hook(hook("read"))
                .check()
                .is_ok()
        );
    }

    #[test]
    fn no_approver_is_absent_and_an_approver_is_delegate() {
        assert_eq!(human_channel(false), HumanChannel::Absent);
        assert_eq!(human_channel(true), HumanChannel::Delegate);
        assert_eq!(
            builder().wiring(Vec::new()).human_channel,
            HumanChannel::Absent
        );
        let with = builder().approver(Arc::new(NoApprover));
        assert_eq!(
            with.wiring(Vec::new()).human_channel,
            HumanChannel::Delegate
        );
    }

    #[test]
    fn hooks_and_the_builtin_list_reach_the_wiring() {
        use crate::core::config::HookEvent;
        use crate::core::permissions::HookError;
        use crate::core::user_hooks::HookTransport;
        struct Quiet;
        impl HookTransport for Quiet {
            fn call<'a>(
                &'a self,
                _: &'a str,
                _: &'a Value,
            ) -> BoxFuture<'a, Result<Value, HookError>> {
                Box::pin(async { Ok(Value::Null) })
            }
        }
        let hook = CallbackHook {
            event: HookEvent::PreToolUse,
            matcher: None,
            callback_id: "h".into(),
            timeout: None,
            fails_closed: true,
            transport: Arc::new(Quiet),
        };
        let wiring = builder()
            .hook(hook)
            .builtin_tools(vec!["read".into()])
            .wiring(Vec::new());
        assert_eq!(wiring.hooks.len(), 1);
        assert_eq!(wiring.hooks[0].callback_id, "h");
        assert_eq!(wiring.root_tools, Some(vec!["read".to_string()]));
    }

    #[test]
    fn resume_maps_to_the_session_the_assembly_opens() {
        let fresh = builder().session_params();
        assert!(fresh.resume.is_none() && !fresh.continue_session && !fresh.fork);
        let cont = builder().resume(SessionRef::Continue).session_params();
        assert!(cont.continue_session && cont.resume.is_none());
        let by_id = builder()
            .resume(SessionRef::Id("abc".into()))
            .fork()
            .session_params();
        assert_eq!(by_id.resume.as_deref(), Some("abc"));
        assert!(by_id.fork && !by_id.continue_session);
    }

    #[test]
    fn modes_map_as_the_cli_does() {
        use crate::core::config::Config;
        let config = Config::defaults(PathBuf::from("."));
        assert_eq!(
            assemble::requested_mode(None, &config, true),
            PermissionMode::Manual
        );
        let afk = assemble::requested_mode(Some(PermissionMode::Afk), &config, true);
        assert_eq!(afk, PermissionMode::Afk);
        assert_eq!(assemble::session_mode(afk), PermissionMode::Auto);
    }

    fn line(kind: &str, text: &str) -> Event {
        let session_id = "s".to_string();
        match kind {
            "result" => Event::Result(events::Line {
                payload: events::ResultEvent {
                    subtype: "success",
                    is_error: false,
                    result: text.into(),
                    error: None,
                    num_turns: 1,
                    duration_ms: 0,
                    total_cost_usd: None,
                    usage: {
                        let zero = events::BucketUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: None,
                        };
                        events::UsageByRole {
                            main: zero.clone(),
                            guardian: zero.clone(),
                            subagents: zero.clone(),
                            goal: zero,
                        }
                    },
                    denials: Vec::new(),
                    structured_output: None,
                    goal: None,
                    afk: None,
                    account_usage: Vec::new(),
                    usage_limit: None,
                },
                r#type: "result",
                schema_version: 1,
                session_id,
            }),
            "usage_paused" => Event::UsagePaused(events::Line {
                payload: events::UsagePaused {
                    account: text.into(),
                    role: "main",
                    reset_at: None,
                },
                r#type: "usage_paused",
                schema_version: 1,
                session_id,
            }),
            _ => Event::Assistant(events::Line {
                payload: events::Text { text: text.into() },
                r#type: "assistant",
                schema_version: 1,
                session_id,
            }),
        }
    }

    async fn next(feed: &mut Feed) -> Option<Event> {
        std::future::poll_fn(|cx| feed.poll(cx)).await
    }

    /// The events a turn would yield: up to and including its `result`.
    async fn turn(feed: &mut Feed) -> Vec<String> {
        let mut seen = Vec::new();
        while let Some(event) = next(feed).await {
            let done = matches!(event, Event::Result(_));
            seen.push(match &event {
                Event::Assistant(l) => format!("assistant:{}", l.payload.text),
                Event::UsagePaused(l) => format!("paused:{}", l.payload.account),
                other => other.kind().to_string(),
            });
            if done {
                break;
            }
        }
        seen
    }

    #[tokio::test]
    async fn events_between_turns_arrive_on_the_next_turn() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut feed = Feed::new(rx);
        for event in [
            line("assistant", "one"),
            line("result", "one"),
            line("usage_paused", "acct"),
            line("assistant", "two"),
            line("result", "two"),
        ] {
            tx.send(event).unwrap();
        }
        assert_eq!(turn(&mut feed).await, ["assistant:one", "result"]);
        assert_eq!(
            turn(&mut feed).await,
            ["paused:acct", "assistant:two", "result"]
        );
    }

    #[tokio::test]
    async fn a_turn_dropped_early_leaves_the_next_one_at_its_own_first_event() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut feed = Feed::new(rx);
        tx.send(line("assistant", "a1")).unwrap();
        assert!(matches!(next(&mut feed).await, Some(Event::Assistant(_))));
        // The first turn is dropped here, with `a2` and its `result` still to come.
        feed.stale += 1;
        for event in [
            line("assistant", "a2"),
            line("result", "r1"),
            line("assistant", "b1"),
            line("result", "r2"),
        ] {
            tx.send(event).unwrap();
        }
        assert_eq!(turn(&mut feed).await, ["assistant:b1", "result"]);
    }

    #[tokio::test]
    async fn the_end_of_the_run_ends_the_feed() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut feed = Feed::new(rx);
        tx.send(line("assistant", "last")).unwrap();
        drop(tx);
        assert!(next(&mut feed).await.is_some());
        assert!(next(&mut feed).await.is_none());
    }

    #[tokio::test]
    async fn build_refuses_a_bad_builder_before_touching_anything() {
        let error = builder().max_turns(0).build().await.err().expect("refused");
        assert!(matches!(error, EmbedError::Config(_)));
        assert_eq!(error.exit_code(), 2);
    }
}
