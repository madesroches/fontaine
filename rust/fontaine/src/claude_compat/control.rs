//! Control requests from the host: `initialize`, `interrupt`, `set_permission_mode`, `set_model`
//! and `mcp_status`. Anything else, and anything the capability matrix marks unsupported, gets an
//! error response naming the subtype.

use std::sync::{Arc, Mutex};

use fontaine_agents::registry::AgentType;
use fontaine_core::afk_report::AfkSource;
use fontaine_core::agent::{McpServerState, McpStatesReader};
use fontaine_core::config::PermissionMode;
use fontaine_core::gate::SessionState;
use fontaine_core::user_hooks::{CallbackHook, HookTransport};
use fontaine_tools::lock::lock;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

use super::cli::{CLAUDE_CODE_PROTOCOL_VERSION, State, control_row, parse_mode};
use super::hooks::{RouterHooks, build as build_hooks};
use super::router::{Command, ControlHandler, Router};
use fontaine_core::extensions::agent_from_definition;

/// What is known before the runtime exists: the command line and the configuration.
pub struct Facts {
    pub start_mode: PermissionMode,
    /// The model roles the user defined, which a host-defined agent may name.
    pub roles: Vec<String>,
    /// The MCP servers a host-defined agent may name tools of.
    pub servers: Vec<String>,
    /// The models the response to `initialize` lists.
    pub models: Vec<Value>,
    /// Non-empty prompts and schema given on the command line.
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub has_json_schema: bool,
}

/// What `initialize` registered, for the runtime that is built after it.
#[derive(Debug, Default)]
pub struct InitConfig {
    pub hooks: Vec<CallbackHook>,
    pub agents: Vec<AgentType>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub json_schema: Option<Value>,
    pub sdk_servers: Vec<String>,
}

/// What the runtime lends the controls once it exists.
#[derive(Clone)]
pub struct Attached {
    pub state: Arc<SessionState>,
    pub mcp: McpStatesReader,
}

/// What `initialize` came to, for the runtime that is built after it.
#[derive(Default)]
struct InitState {
    config: Option<InitConfig>,
    /// Why the host's `initialize` was rejected: the run must not go on without its config.
    failure: Option<String>,
    /// The runtime was built from this state, so a later `initialize` cannot apply.
    taken: bool,
}

pub struct Shared {
    pub facts: Facts,
    init: Mutex<InitState>,
    attached: watch::Sender<Option<Attached>>,
}

impl Shared {
    pub fn new(facts: Facts) -> Arc<Shared> {
        Arc::new(Shared {
            facts,
            init: Mutex::default(),
            attached: watch::channel(None).0,
        })
    }

    /// What `initialize` registered; empty when none came, an error when it was rejected.
    pub fn take_init(&self) -> Result<InitConfig, String> {
        let mut state = lock(&self.init, "initialize config");
        state.taken = true;
        match state.failure.clone() {
            Some(why) => Err(why),
            None => Ok(state.config.take().unwrap_or_default()),
        }
    }

    pub fn attach(&self, attached: Attached) {
        self.attached.send_replace(Some(attached));
    }

    async fn attached(&self) -> Attached {
        let mut rx = self.attached.subscribe();
        loop {
            if let Some(attached) = rx.borrow_and_update().clone() {
                return attached;
            }
            if rx.changed().await.is_err() {
                // The sender lives as long as the controls do.
                unreachable!("the shared state outlives its controls");
            }
        }
    }
}

pub struct Control {
    pub shared: Arc<Shared>,
}

/// A value that is the SDK's default for a field, which Fontaine need not honor.
fn is_default(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Number(_) => false,
    }
}

/// The fields of `initialize` Fontaine cannot honor when they are set to anything but a default.
const UNSUPPORTED_FIELDS: [&str; 10] = [
    "excludeDynamicSections",
    "systemPromptSnapshot",
    "skills",
    "forwardSubagentText",
    "toolAliases",
    "planModeInstructions",
    "title",
    "promptSuggestions",
    "plugins",
    "managedSettings",
];

/// The TS SDK's system prompt: a string, or a list whose non-empty entries are joined by a blank
/// line. A prompt with nothing in it is absent.
fn prompt_text(value: &Value) -> Result<Option<String>, String> {
    let text = match value {
        Value::Null => return Ok(None),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().ok_or("a prompt entry is not text"))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return Err("a prompt must be text or a list of text".to_string()),
    };
    Ok(super::cli::nonblank(Some(text)))
}

/// Reads `initialize` into what the runtime is built with.
pub fn read_initialize(
    request: &Value,
    facts: &Facts,
    transport: &Arc<dyn HookTransport>,
) -> Result<InitConfig, String> {
    for field in UNSUPPORTED_FIELDS {
        if let Some(value) = request.get(field)
            && !is_default(value)
        {
            return Err(format!(
                "`initialize.{field}` is not supported by Fontaine (see docs/agent-sdk.md)"
            ));
        }
    }
    if let Some(value) = request.get("workspaceTrust")
        && !is_default(value)
    {
        return Err(
            "`initialize.workspaceTrust` is not supported by Fontaine (see docs/agent-sdk.md)"
                .to_string(),
        );
    }
    let mut init = InitConfig {
        system_prompt: prompt_text(request.get("systemPrompt").unwrap_or(&Value::Null))
            .map_err(|e| format!("`initialize.systemPrompt`: {e}"))?,
        append_system_prompt: prompt_text(
            request.get("appendSystemPrompt").unwrap_or(&Value::Null),
        )
        .map_err(|e| format!("`initialize.appendSystemPrompt`: {e}"))?,
        ..InitConfig::default()
    };
    if init.system_prompt.is_some() && facts.system_prompt.is_some() {
        return Err(
            "`systemPrompt` was given on the command line and in `initialize`; give it once"
                .to_string(),
        );
    }
    if init.append_system_prompt.is_some() && facts.append_system_prompt.is_some() {
        return Err(
            "`appendSystemPrompt` was given on the command line and in `initialize`; give it once"
                .to_string(),
        );
    }
    if let Some(schema) = request.get("jsonSchema").filter(|s| !is_default(s)) {
        if !schema.is_object() {
            return Err("`initialize.jsonSchema` must be a JSON Schema object".to_string());
        }
        if facts.has_json_schema {
            return Err(
                "`jsonSchema` was given on the command line and in `initialize`; give it once"
                    .to_string(),
            );
        }
        init.json_schema = Some(schema.clone());
    }
    for key in [
        "sdkMcpServers",
        "sdkMcpServerConfigs",
        "sdkMcpServerManifests",
    ] {
        let names: Vec<String> = match request.get(key) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            Some(Value::Object(map)) => map.keys().cloned().collect(),
            Some(_) => return Err(format!("`initialize.{key}` has an unexpected shape")),
        };
        for name in names {
            if !init.sdk_servers.contains(&name) {
                init.sdk_servers.push(name);
            }
        }
    }
    if let Some(hooks) = request.get("hooks").filter(|h| !is_default(h)) {
        init.hooks = build_hooks(hooks, transport)?;
    }
    if let Some(agents) = request.get("agents").filter(|a| !is_default(a)) {
        let agents = agents
            .as_object()
            .ok_or("`initialize.agents` must be an object keyed by agent name")?;
        let mut servers = facts.servers.clone();
        servers.extend(init.sdk_servers.iter().cloned());
        let is_role = |name: &str| facts.roles.iter().any(|role| role == name);
        for (name, definition) in agents {
            init.agents
                .push(agent_from_definition(name, definition, &servers, &is_role)?);
        }
    }
    Ok(init)
}

fn mode_name(mode: PermissionMode) -> &'static str {
    mode.as_str()
}

impl Control {
    async fn serve(&self, router: &Arc<Router>, request: &Value) -> Result<Value, String> {
        let subtype = request
            .get("subtype")
            .and_then(Value::as_str)
            .ok_or("the control request has no `subtype`")?;
        match control_row(subtype).map(|row| row.state) {
            None => return Err(format!("the control request `{subtype}` is not known")),
            Some(State::Unsupported(why)) => {
                return Err(format!(
                    "the control request `{subtype}` is not supported: {why}"
                ));
            }
            Some(_) => {}
        }
        match subtype {
            "initialize" => self.initialize(router, request),
            "interrupt" => {
                router.interrupt().await;
                Ok(json!({ "still_queued": [] }))
            }
            "set_permission_mode" => {
                let text = request
                    .get("mode")
                    .and_then(Value::as_str)
                    .ok_or("`set_permission_mode` has no `mode`")?;
                let mode = parse_mode(text)?;
                // AFK is entered only by the user's own flag or key press.
                if mode == PermissionMode::Afk {
                    return Err(
                        "AFK mode can only be entered at startup with `--permission-mode afk`"
                            .to_string(),
                    );
                }
                let attached = self.shared.attached().await;
                attached
                    .state
                    .set_mode(mode, AfkSource::Sdk)
                    .map_err(|refusal| refusal.to_string())?;
                Ok(json!({ "mode": mode_name(mode) }))
            }
            "set_model" => {
                let model = match request.get("model") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(text)) if text.trim().is_empty() => None,
                    Some(Value::String(text)) => Some(text.clone()),
                    Some(_) => return Err("`set_model` has a `model` that is not text".to_string()),
                };
                let (reply, answer) = oneshot::channel();
                router.command(Command::SetModel { model, reply });
                answer
                    .await
                    .map_err(|_| "the run ended before the model changed".to_string())?
                    .map(|reference| json!({ "model": reference }))
            }
            "mcp_status" => {
                let attached = self.shared.attached().await;
                let servers: Vec<Value> = (attached.mcp)()
                    .into_iter()
                    .map(|(name, state)| match state {
                        McpServerState::Connected { tools, .. } => {
                            json!({ "name": name, "status": "connected", "toolCount": tools })
                        }
                        McpServerState::Reconnecting { .. } => {
                            json!({ "name": name, "status": "pending" })
                        }
                        McpServerState::Failed(error) => {
                            json!({ "name": name, "status": "failed", "error": error })
                        }
                    })
                    .collect();
                Ok(json!({ "mcpServers": servers }))
            }
            other => Err(format!("the control request `{other}` is not handled")),
        }
    }

    fn initialize(&self, router: &Arc<Router>, request: &Value) -> Result<Value, String> {
        if router.runtime_built() {
            return Err("`initialize` must come before the first user message".to_string());
        }
        {
            let state = lock(&self.shared.init, "initialize config");
            if state.taken {
                return Err("`initialize` came after the session started".to_string());
            }
            if state.config.is_some() {
                return Err("`initialize` was already handled".to_string());
            }
        }
        let transport: Arc<dyn HookTransport> = Arc::new(RouterHooks {
            router: router.clone(),
        });
        let init = read_initialize(request, &self.shared.facts, &transport).inspect_err(|why| {
            lock(&self.shared.init, "initialize config")
                .failure
                .get_or_insert_with(|| why.clone());
        })?;
        let agents: Vec<Value> = init
            .agents
            .iter()
            .map(|a| json!({ "name": a.name, "description": a.description }))
            .collect();
        {
            let mut state = lock(&self.shared.init, "initialize config");
            if state.taken {
                return Err("`initialize` came after the session started".to_string());
            }
            state.config = Some(init);
        }
        Ok(json!({
            "commands": [],
            "agents": agents,
            "output_style": "default",
            "available_output_styles": ["default"],
            "models": self.shared.facts.models,
            "account": {},
            "pid": std::process::id(),
            "claude_code_version": CLAUDE_CODE_PROTOCOL_VERSION,
            "current_permission_mode": mode_name(self.shared.facts.start_mode),
            "hook_on_failure_supported": true,
            "hooks_applied": true,
            "session_state": "idle",
            "capabilities": [],
        }))
    }
}

impl ControlHandler for Control {
    fn handle(
        &self,
        router: Arc<Router>,
        request_id: String,
        request: Value,
    ) -> BoxFuture<'static, ()> {
        let this = Control {
            shared: self.shared.clone(),
        };
        Box::pin(async move {
            let answer = this.serve(&router, &request).await;
            match answer {
                Ok(response) => router.respond_ok(&request_id, response),
                Err(why) => router.respond_err(&request_id, &why),
            }
            if request.get("subtype").and_then(Value::as_str) == Some("initialize") {
                router.initialized();
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_core::permissions::HookError;

    struct Never;
    impl HookTransport for Never {
        fn call<'a>(&'a self, _: &'a str, _: &'a Value) -> BoxFuture<'a, Result<Value, HookError>> {
            Box::pin(async { Ok(json!({})) })
        }
    }

    fn facts() -> Facts {
        Facts {
            start_mode: PermissionMode::Manual,
            roles: vec!["fast".to_string()],
            servers: Vec::new(),
            models: Vec::new(),
            system_prompt: None,
            append_system_prompt: None,
            has_json_schema: false,
        }
    }

    fn read(request: Value) -> Result<InitConfig, String> {
        read_initialize(
            &request,
            &facts(),
            &(Arc::new(Never) as Arc<dyn HookTransport>),
        )
    }

    #[test]
    fn blank_and_array_system_prompts_follow_the_documented_rules() {
        for blank in [json!([""]), json!(""), json!(["", "  "]), Value::Null] {
            assert_eq!(
                read(json!({"subtype": "initialize", "systemPrompt": blank}))
                    .unwrap()
                    .system_prompt,
                None
            );
        }
        let joined =
            read(json!({"subtype": "initialize", "systemPrompt": ["a", "", "b"]})).unwrap();
        assert_eq!(joined.system_prompt.as_deref(), Some("a\n\nb"));
        let appended =
            read(json!({"subtype": "initialize", "appendSystemPrompt": "more"})).unwrap();
        assert_eq!(appended.append_system_prompt.as_deref(), Some("more"));
    }

    #[test]
    fn a_prompt_given_twice_is_an_error_naming_it() {
        let mut with_argv = facts();
        with_argv.system_prompt = Some("from argv".into());
        let error = read_initialize(
            &json!({"subtype": "initialize", "systemPrompt": ["from initialize"]}),
            &with_argv,
            &(Arc::new(Never) as Arc<dyn HookTransport>),
        )
        .unwrap_err();
        assert!(error.contains("systemPrompt"), "{error}");
        // A blank one beside a real one is no conflict.
        assert!(
            read_initialize(
                &json!({"subtype": "initialize", "systemPrompt": [""]}),
                &with_argv,
                &(Arc::new(Never) as Arc<dyn HookTransport>),
            )
            .is_ok()
        );
    }

    #[test]
    fn fields_fontaine_cannot_honor_error_only_when_set_to_a_non_default() {
        assert!(read(json!({"subtype": "initialize", "title": "", "skills": [], "plugins": [], "excludeDynamicSections": false})).is_ok());
        for (field, value) in [
            ("excludeDynamicSections", json!(true)),
            ("skills", json!(["a"])),
            ("toolAliases", json!({"a": "b"})),
            ("title", json!("t")),
            ("plugins", json!([{"path": "x"}])),
            ("managedSettings", json!({"a": 1})),
            ("forwardSubagentText", json!(true)),
        ] {
            let error = read(json!({"subtype": "initialize", field: value})).unwrap_err();
            assert!(error.contains(field), "{error}");
        }
    }

    #[test]
    fn sdk_servers_hooks_and_agents_are_read() {
        let init = read(json!({
            "subtype": "initialize",
            "sdkMcpServers": ["calc"],
            "sdkMcpServerManifests": {"calc": {}, "other": {}},
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hookCallbackIds": ["hook_0"]}]},
            "agents": {"reviewer": {"description": "reviews", "prompt": "Review.", "tools": ["Read"], "model": "fast"}},
        }))
        .unwrap();
        assert_eq!(init.sdk_servers, ["calc", "other"]);
        assert_eq!(init.hooks.len(), 1);
        assert_eq!(init.agents.len(), 1);
        assert_eq!(init.agents[0].name, "reviewer");
    }

    #[test]
    fn a_bad_agent_or_hook_fails_initialize() {
        let error = read(json!({"subtype": "initialize", "agents": {"a": {"description": "d", "prompt": "p", "effort": "high"}}})).unwrap_err();
        assert!(error.contains("`a`") && error.contains("effort"), "{error}");
        let error = read(
            json!({"subtype": "initialize", "hooks": {"Nonsense": [{"hookCallbackIds": ["x"]}]}}),
        )
        .unwrap_err();
        assert!(error.contains("Nonsense"), "{error}");
    }

    #[test]
    fn a_rejected_initialize_stops_the_run_and_a_late_one_is_refused() {
        let shared = Shared::new(facts());
        lock(&shared.init, "initialize config")
            .failure
            .get_or_insert("bad".to_string());
        assert_eq!(shared.take_init().unwrap_err(), "bad");

        let shared = Shared::new(facts());
        assert!(shared.take_init().is_ok());
        assert!(lock(&shared.init, "initialize config").taken);
    }

    #[test]
    fn the_matrix_marks_the_unsupported_control_requests() {
        for subtype in [
            "get_context_usage",
            "rewind_files",
            "mcp_reconnect",
            "mcp_toggle",
            "stop_task",
        ] {
            assert!(
                matches!(
                    control_row(subtype).map(|r| r.state),
                    Some(State::Unsupported(_))
                ),
                "{subtype}"
            );
        }
        assert!(matches!(
            control_row("initialize").map(|r| r.state),
            Some(State::Mapped(_))
        ));
        assert!(control_row("mystery").is_none());
    }
}
