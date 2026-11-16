#![allow(dead_code)]

//! A scripted MCP server: a few tools, one that claims to be read-only, and a way to change the
//! tool list while it runs. Shared by the registration tests and the stdio example.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::service::{NotificationContext, Peer, RequestContext};
use rmcp::{RoleServer, ServerHandler};

/// The tool calls the server received: name and arguments.
pub type Calls = Arc<Mutex<Vec<(String, Option<JsonObject>)>>>;

#[derive(Clone, Default)]
pub struct Double {
    tools: Arc<Mutex<Vec<Tool>>>,
    resources: Arc<Mutex<Vec<Resource>>>,
    prompts: Arc<Mutex<Vec<Prompt>>>,
    peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
    pub calls: Calls,
    /// A `slow` call has started once this is notified, and ends when `finish` is.
    pub started: Arc<tokio::sync::Notify>,
    pub finish: Arc<tokio::sync::Notify>,
}

fn schema() -> Arc<JsonObject> {
    Arc::new(serde_json::Map::from_iter([(
        "type".to_string(),
        serde_json::json!("object"),
    )]))
}

pub fn echo_tool() -> Tool {
    Tool::new("echo", "Echo the arguments back", schema())
}

/// A tool that asks the person for a name, an age and a mood while it runs, and reports what it
/// got.
pub fn elicit_tool() -> Tool {
    Tool::new("elicit", "Ask the user a question", schema())
}

/// A tool that asks the person to open a link while it runs, and reports the answer.
pub fn elicit_url_tool() -> Tool {
    Tool::new("elicit_url", "Ask the user to open a link", schema())
}

/// A tool that runs until the test releases it with `Double::finish`.
pub fn slow_tool() -> Tool {
    Tool::new("slow", "Wait to be released", schema())
}

/// A tool that returns `chars` characters of text.
pub fn big_tool() -> Tool {
    Tool::new("big", "Return a long text", schema())
}

/// A tool whose result is an error of `chars` characters.
pub fn failing_tool() -> Tool {
    Tool::new("fail", "Fail with a long message", schema())
}

fn elicitation_params() -> ElicitRequestParams {
    ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: "Who are you?".to_string(),
        requested_schema: ElicitationSchema::builder()
            .required_string("name")
            .optional_integer("age", 0, 150)
            .build()
            .expect("a valid schema"),
    }
}

fn url_elicitation_params() -> ElicitRequestParams {
    ElicitRequestParams::UrlElicitationParams {
        meta: None,
        message: "Open the docs?".to_string(),
        url: "https://example.com/docs".to_string(),
        elicitation_id: "e1".to_string(),
    }
}

/// A tool that says it only reads. The harness must not believe it.
pub fn lookup_tool() -> Tool {
    Tool::new("lookup", "Look a thing up", schema()).with_annotations(
        ToolAnnotations::with_title("Lookup")
            .read_only(true)
            .destructive(false)
            .idempotent(true),
    )
}

/// A tool whose server asks that a person approve every call.
pub fn grant_access_tool() -> Tool {
    marked(unmarked_grant_access_tool())
}

pub fn unmarked_grant_access_tool() -> Tool {
    Tool::new("grant_access", "Grant access", schema())
}

pub fn marked(mut tool: Tool) -> Tool {
    let mut meta = MetaObject::new();
    meta.insert(
        "anthropic/requiresUserInteraction".to_string(),
        serde_json::json!(true),
    );
    tool.meta = Some(meta);
    tool
}

/// A resource whose content is `contents of <uri>` when read.
pub fn info_resource() -> Resource {
    Resource::new("repo://info", "repo_info")
        .with_description("Repository information")
        .with_mime_type("text/plain")
}

/// A prompt taking a required `name` and an optional `mood`.
pub fn greet_prompt() -> Prompt {
    Prompt::new(
        "greet",
        Some("Greet someone"),
        Some(vec![
            PromptArgument::new("name").with_required(true),
            PromptArgument::new("mood"),
        ]),
    )
}

/// A prompt whose message is assistant-role text: a client that only flattens user text must
/// refuse it.
pub fn assistant_prompt() -> Prompt {
    Prompt::new("assistant-note", Some("A note from the assistant"), None)
}

impl Double {
    pub fn with_tools(tools: Vec<Tool>) -> Double {
        let double = Double::default();
        *double.tools.lock().unwrap() = tools;
        double
    }

    /// A server that also lists resources and prompts.
    pub fn with_resources(
        tools: Vec<Tool>,
        resources: Vec<Resource>,
        prompts: Vec<Prompt>,
    ) -> Double {
        let double = Double::with_tools(tools);
        *double.resources.lock().unwrap() = resources;
        *double.prompts.lock().unwrap() = prompts;
        double
    }

    /// Replaces the tool list and tells the connected client.
    pub async fn replace_tools(&self, tools: Vec<Tool>) {
        *self.tools.lock().unwrap() = tools;
        let peer = self.peer.lock().unwrap().clone();
        if let Some(peer) = peer {
            let _ = peer.notify_tool_list_changed().await;
        }
    }

    /// Replaces the resource list and tells the connected client.
    pub async fn replace_resources(&self, resources: Vec<Resource>) {
        *self.resources.lock().unwrap() = resources;
        let peer = self.peer.lock().unwrap().clone();
        if let Some(peer) = peer {
            let _ = peer.notify_resource_list_changed().await;
        }
    }

    /// Replaces the prompt list and tells the connected client.
    pub async fn replace_prompts(&self, prompts: Vec<Prompt>) {
        *self.prompts.lock().unwrap() = prompts;
        let peer = self.peer.lock().unwrap().clone();
        if let Some(peer) = peer {
            let _ = peer.notify_prompt_list_changed().await;
        }
    }
}

impl ServerHandler for Double {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_resources_list_changed()
                .enable_prompts()
                .enable_prompts_list_changed()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::LATEST)
        .with_server_info(Implementation::new("double", "0.1.0"))
    }

    /// A legacy server: it does not know `server/discover`, so the client falls back to
    /// `initialize` and the older request form of elicitation.
    async fn discover(&self, _: RequestContext<RoleServer>) -> Result<DiscoverResult, ErrorData> {
        Err(ErrorData::method_not_found::<DiscoverRequestMethod>())
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            self.resources.lock().unwrap().clone(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let resources = self.resources.lock().unwrap();
        let Some(resource) = resources.iter().find(|r| r.uri == request.uri) else {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("unknown resource {}", request.uri),
                None,
            ));
        };
        let text = format!("contents of {}", resource.uri);
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(text, resource.uri.clone())])
                .into(),
        )
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(
            self.prompts.lock().unwrap().clone(),
        ))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let prompts = self.prompts.lock().unwrap();
        let Some(prompt) = prompts.iter().find(|p| p.name == request.name) else {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("unknown prompt {}", request.name),
                None,
            ));
        };
        let arguments = request.arguments.unwrap_or_default();
        let value = |name: &str| {
            arguments
                .get(name)
                .and_then(|value| value.as_str().map(str::to_string))
        };
        let messages = match prompt.name.as_str() {
            "assistant-note" => vec![PromptMessage::new_text(
                Role::Assistant,
                "an assistant-role line the command must refuse",
            )],
            "greet" => {
                let name = value("name").unwrap_or_default();
                let mood = value("mood").unwrap_or_else(|| "neutral".to_string());
                vec![
                    PromptMessage::new_text(Role::User, format!("Hello, {name}!")),
                    PromptMessage::new_text(Role::User, format!("Mood: {mood}.")),
                ]
            }
            other => vec![PromptMessage::new_text(
                Role::User,
                format!("prompt {other} says hello"),
            )],
        };
        Ok(GetPromptResult::new(messages).into())
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        *self.peer.lock().unwrap() = Some(context.peer);
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.tools.lock().unwrap().clone(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls
            .lock()
            .unwrap()
            .push((request.name.to_string(), request.arguments.clone()));
        let chars = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("chars"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let text = match request.name.as_ref() {
            "elicit" => match context.peer.create_elicitation(elicitation_params()).await {
                Ok(result) => format!(
                    "elicited: {}",
                    serde_json::json!({"action": result.action, "content": result.content})
                ),
                Err(e) => format!("elicitation failed: {e}"),
            },
            "elicit_url" => {
                match context
                    .peer
                    .create_elicitation(url_elicitation_params())
                    .await
                {
                    Ok(result) => format!(
                        "elicited: {}",
                        serde_json::json!({"action": result.action, "content": result.content})
                    ),
                    Err(e) => format!("elicitation failed: {e}"),
                }
            }
            "big" => "x".repeat(chars),
            "slow" => {
                self.started.notify_one();
                self.finish.notified().await;
                "slow: done".to_string()
            }
            "fail" => {
                return Ok(
                    CallToolResult::error(vec![ContentBlock::text("e".repeat(chars))]).into(),
                );
            }
            // Reports an environment variable, so a test can see what the server inherited.
            "env" => {
                let name = request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                std::env::var(name).unwrap_or_else(|_| "<unset>".to_string())
            }
            other => format!(
                "{other}: {}",
                serde_json::Value::Object(request.arguments.unwrap_or_default())
            ),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}
