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

impl Double {
    pub fn with_tools(tools: Vec<Tool>) -> Double {
        let double = Double::default();
        *double.tools.lock().unwrap() = tools;
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
}

impl ServerHandler for Double {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::LATEST)
        .with_server_info(Implementation::new("double", "0.1.0"))
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
