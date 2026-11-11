//! MCP resources and prompts: listings pulled per capability, `list_changed` refresh, and the
//! two gated built-in tools (`list_mcp_resources`, `read_mcp_resource`) with output capping.

mod common;
mod mcp_common;

use std::sync::{Arc, Mutex};

use common::{AllowAll, Dirs, call, env_with, registry, scope, text, tool};
use fontaine_tools::builtins::BuiltinEnv;
use fontaine_tools::mcp::output::OutputLimits;
use fontaine_tools::mcp::{
    Listings, McpResourceQueries, PromptArgument, PromptSummary, ResourceSummary,
    connect_transport, pull_listings,
};
use futures::future::BoxFuture;
use mcp_common::server::{Double, assistant_prompt, echo_tool, greet_prompt, info_resource};
use mcp_common::{Gate, options, tool_server};
use rmcp::model::{
    GetPromptRequestParams, PromptMessage, ReadResourceRequestParams, ResourceContents, Role,
    ServerCapabilities, ServerConfig,
};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::json;

/// A scripted server that declares tools only: the common MCP case, where both listings must
/// stay empty without a single request failing.
#[derive(Clone)]
struct ToolsOnly;

impl ServerHandler for ToolsOnly {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

async fn serve(double: Double) -> fontaine_tools::mcp::Connected {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let running = double.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    connect_transport(
        "double",
        (cfs, c2s),
        tool_server(),
        &options(Gate::new(true)),
    )
    .await
    .expect("connect")
}

#[tokio::test]
async fn listings_follow_the_declared_capabilities() {
    let connected = serve(Double::with_resources(
        vec![echo_tool()],
        vec![info_resource()],
        vec![greet_prompt(), assistant_prompt()],
    ))
    .await;
    let listings = pull_listings(&connected.connection, &Listings::default()).await;
    assert_eq!(
        listings.resources,
        vec![ResourceSummary {
            uri: "repo://info".to_string(),
            name: "repo_info".to_string(),
            description: Some("Repository information".to_string()),
            mime_type: Some("text/plain".to_string()),
        }]
    );
    assert_eq!(
        listings.prompts,
        vec![
            PromptSummary {
                name: "greet".to_string(),
                description: Some("Greet someone".to_string()),
                arguments: vec![
                    PromptArgument {
                        name: "name".to_string(),
                        required: true,
                    },
                    PromptArgument {
                        name: "mood".to_string(),
                        required: false,
                    },
                ],
            },
            PromptSummary {
                name: "assistant-note".to_string(),
                description: Some("A note from the assistant".to_string()),
                arguments: Vec::new(),
            },
        ]
    );
}

#[tokio::test]
async fn a_tools_only_server_lists_nothing() {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let running = ToolsOnly.serve((sfc, s2c)).await.expect("server start");
        let _ = running.waiting().await;
    });
    let connected = connect_transport(
        "plain",
        (cfs, c2s),
        tool_server(),
        &options(Gate::new(true)),
    )
    .await
    .expect("connect");
    let listings = pull_listings(&connected.connection, &Listings::default()).await;
    assert!(listings.is_empty());
}

/// A server whose prompt listing fails while its resource listing answers.
#[derive(Clone)]
struct BrokenPrompts {
    resources: Arc<Mutex<Vec<rmcp::model::Resource>>>,
}

impl ServerHandler for BrokenPrompts {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::model::ErrorData> {
        Ok(rmcp::model::ListResourcesResult::with_all_items(
            self.resources.lock().unwrap().clone(),
        ))
    }

    async fn list_prompts(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListPromptsResult, rmcp::model::ErrorData> {
        Err(rmcp::model::ErrorData::new(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "the prompt list is broken",
            None,
        ))
    }
}

#[tokio::test]
async fn a_failed_prompt_listing_keeps_the_previous_prompts_and_still_updates_resources() {
    let (c2s, sfc) = tokio::io::duplex(8192);
    let (s2c, cfs) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let running = BrokenPrompts {
            resources: Arc::new(Mutex::new(vec![info_resource()])),
        }
        .serve((sfc, s2c))
        .await
        .expect("server start");
        let _ = running.waiting().await;
    });
    let connected = connect_transport(
        "broken",
        (cfs, c2s),
        tool_server(),
        &options(Gate::new(true)),
    )
    .await
    .expect("connect");
    let previous = Listings {
        resources: Vec::new(),
        prompts: vec![PromptSummary {
            name: "greet".to_string(),
            description: Some("Greet someone".to_string()),
            arguments: vec![PromptArgument {
                name: "name".to_string(),
                required: true,
            }],
        }],
    };
    let pulled = pull_listings(&connected.connection, &previous).await;
    assert_eq!(pulled.resources.len(), 1, "the resource listing landed");
    assert_eq!(
        pulled.prompts, previous.prompts,
        "the failed prompt listing kept its value"
    );

    // The same independence holds through a connection's refresh.
    connected.refresh_listings().await;
    assert_eq!(connected.listings().resources.len(), 1);
    assert!(connected.listings().prompts.is_empty());
}

#[tokio::test]
async fn a_list_changed_notification_refreshes_the_listings() {
    let double = Double::with_resources(
        vec![echo_tool()],
        vec![info_resource()],
        vec![greet_prompt()],
    );
    let connected = serve(double.clone()).await;
    connected.refresh_listings().await;
    assert_eq!(connected.listings().resources.len(), 1);

    // The resource read works through the single-round peer request.
    let response = connected
        .connection
        .peer()
        .read_resource_once(ReadResourceRequestParams::new("repo://info"))
        .await;
    let text = match response.expect("a read") {
        rmcp::model::ReadResourceResponse::Complete(result) => match &result.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected a complete read, got {other:?}"),
    };
    assert_eq!(text, "contents of repo://info");

    // The server drops its resource and announces the change; the transport tap forwards the
    // notification and the refresh replaces what is stored.
    double.replace_resources(Vec::new()).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            connected.listings_changed().notified().await;
            connected.refresh_listings().await;
            if connected.listings().resources.is_empty() {
                break;
            }
        }
    })
    .await
    .expect("the refresh lands");
    assert!(connected.listings().resources.is_empty());
    assert_eq!(connected.listings().prompts.len(), 1);
}

/// One scripted read: `server`, `uri`, and what the read answers.
type ScriptedRead = (String, String, Result<Vec<ResourceContents>, String>);

/// Scripted answers for the built-in tools.
struct Fake {
    listings: Vec<(String, Listings)>,
    reads: Mutex<Vec<ScriptedRead>>,
}

impl McpResourceQueries for Fake {
    fn connected(&self) -> Vec<(String, Listings)> {
        self.listings.clone()
    }

    fn read_resource<'a>(
        &'a self,
        server: &'a str,
        uri: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ResourceContents>, String>> {
        let found = self
            .reads
            .lock()
            .unwrap()
            .iter()
            .find(|(s, u, _)| s == server && u == uri)
            .map(|(_, _, r)| r.clone());
        Box::pin(async move {
            found.unwrap_or_else(|| Err(format!("no resource `{uri}` on `{server}`")))
        })
    }
}

fn fake_env(d: &Dirs, fake: Fake) -> BuiltinEnv {
    let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    env.mcp_resources = Arc::new(fake);
    env
}

#[tokio::test]
async fn list_mcp_resources_renders_every_listed_resource_and_the_empty_case() {
    let d = Dirs::new();
    let env = fake_env(
        &d,
        Fake {
            listings: Vec::new(),
            reads: Mutex::new(Vec::new()),
        },
    );
    let tools = registry(&env);
    let out = call(&tools, "list_mcp_resources", json!({})).await.unwrap();
    assert_eq!(text(&out), "no connected MCP server declared resources");

    let listings = vec![(
        "repo".to_string(),
        Listings {
            resources: vec![
                ResourceSummary {
                    uri: "repo://info".to_string(),
                    name: "repo_info".to_string(),
                    description: Some("Repository information".to_string()),
                    mime_type: Some("text/plain".to_string()),
                },
                ResourceSummary {
                    uri: "repo://raw".to_string(),
                    name: String::new(),
                    description: None,
                    mime_type: None,
                },
            ],
            prompts: Vec::new(),
        },
    )];
    let env = fake_env(
        &d,
        Fake {
            listings,
            reads: Mutex::new(Vec::new()),
        },
    );
    let tools = registry(&env);
    let out = call(&tools, "list_mcp_resources", json!({})).await.unwrap();
    assert_eq!(
        text(&out),
        "repo: repo://info — repo_info · Repository information (text/plain)\nrepo: repo://raw"
    );
}

#[tokio::test]
async fn read_mcp_resource_renders_text_and_blobs_and_names_a_bad_target() {
    let d = Dirs::new();
    let reads = Mutex::new(vec![
        (
            "repo".to_string(),
            "repo://info".to_string(),
            Ok(vec![ResourceContents::text("the info text", "repo://info")]),
        ),
        (
            "repo".to_string(),
            "repo://bin".to_string(),
            Ok(vec![
                ResourceContents::blob("QUJD", "repo://bin")
                    .with_mime_type("application/octet-stream"),
            ]),
        ),
        (
            "repo".to_string(),
            "repo://one".to_string(),
            Ok(vec![
                ResourceContents::blob("QQ==", "repo://one")
                    .with_mime_type("application/octet-stream"),
            ]),
        ),
        (
            "repo".to_string(),
            "repo://two".to_string(),
            Ok(vec![
                ResourceContents::blob("QUI=", "repo://two")
                    .with_mime_type("application/octet-stream"),
            ]),
        ),
        (
            "repo".to_string(),
            "repo://gone".to_string(),
            Err("no resource `repo://gone` on `repo`".to_string()),
        ),
    ]);
    let env = fake_env(
        &d,
        Fake {
            listings: Vec::new(),
            reads,
        },
    );
    let tools = registry(&env);
    let out = call(
        &tools,
        "read_mcp_resource",
        json!({"server": "repo", "uri": "repo://info"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&out), "the info text\n");

    let out = call(
        &tools,
        "read_mcp_resource",
        json!({"server": "repo", "uri": "repo://bin"}),
    )
    .await
    .unwrap();
    assert_eq!(
        text(&out),
        "[blob at repo://bin: application/octet-stream, 3 bytes, base64]\n"
    );

    // The size accounts for base64 padding: `QQ==` is one byte, `QUI=` two.
    for (uri, size) in [("repo://one", 1), ("repo://two", 2)] {
        let out = call(
            &tools,
            "read_mcp_resource",
            json!({"server": "repo", "uri": uri}),
        )
        .await
        .unwrap();
        assert_eq!(
            text(&out),
            format!("[blob at {uri}: application/octet-stream, {size} bytes, base64]\n")
        );
    }

    let error = call(
        &tools,
        "read_mcp_resource",
        json!({"server": "repo", "uri": "repo://gone"}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("repo://gone"), "{error}");
}

#[tokio::test]
async fn an_oversized_read_spills_like_any_mcp_result() {
    let d = Dirs::new();
    let spill = d.dir("spill");
    std::fs::create_dir_all(&spill).unwrap();
    let big = "x".repeat(60_000);
    let reads = Mutex::new(vec![(
        "repo".to_string(),
        "repo://big".to_string(),
        Ok(vec![ResourceContents::text(big, "repo://big")]),
    )]);
    let mut env = fake_env(
        &d,
        Fake {
            listings: Vec::new(),
            reads,
        },
    );
    // The default cap is 25k tokens (~100k chars): lower the token limit so the read spills.
    env.mcp_limits = Arc::new(OutputLimits::new(
        1_000,
        Some(fontaine_tools::mcp::output::Spill(spill.clone())),
    ));
    let tools = registry(&env);
    let out = call(
        &tools,
        "read_mcp_resource",
        json!({"server": "repo", "uri": "repo://big"}),
    )
    .await
    .unwrap();
    let shown = text(&out);
    assert!(shown.starts_with("Output too large ("), "{shown}");
    assert!(shown.contains("saved to"), "{shown}");
    let saved = std::fs::read_dir(&spill)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        format!("{}\n", "x".repeat(60_000))
    );
}

#[tokio::test]
async fn an_oversized_listing_spills_like_any_mcp_result() {
    let d = Dirs::new();
    let spill = d.dir("spill");
    let big = "x".repeat(60_000);
    let listings = vec![(
        "repo".to_string(),
        Listings {
            resources: vec![ResourceSummary {
                uri: format!("repo://{big}"),
                name: String::new(),
                description: None,
                mime_type: None,
            }],
            prompts: Vec::new(),
        },
    )];
    let mut env = fake_env(
        &d,
        Fake {
            listings,
            reads: Mutex::new(Vec::new()),
        },
    );
    env.mcp_limits = Arc::new(OutputLimits::new(
        1_000,
        Some(fontaine_tools::mcp::output::Spill(spill.clone())),
    ));
    let tools = registry(&env);
    let out = call(&tools, "list_mcp_resources", json!({})).await.unwrap();
    let shown = text(&out);
    assert!(shown.starts_with("Output too large ("), "{shown}");
    assert!(shown.contains("saved to"), "{shown}");
    let saved = std::fs::read_dir(&spill)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(saved).unwrap(),
        format!("repo: repo://{big}")
    );
}

#[tokio::test]
async fn both_tools_are_registered_arbitrary_and_wrapped() {
    let d = Dirs::new();
    let env = fake_env(
        &d,
        Fake {
            listings: Vec::new(),
            reads: Mutex::new(Vec::new()),
        },
    );
    let tools = registry(&env);
    for name in ["list_mcp_resources", "read_mcp_resource"] {
        let registered = tool(&tools, name);
        assert_eq!(registered.risk, fontaine_tools::risk::RiskClass::Arbitrary);
        assert_eq!(
            fontaine_tools::descriptor::find(name).map(|row| row.risk),
            Some(registered.risk)
        );
        assert_eq!(
            fontaine_tools::descriptor::find_claude(if name == "list_mcp_resources" {
                "ListMcpResources"
            } else {
                "ReadMcpResource"
            })
            .map(|row| row.name),
            Some(name)
        );
    }
}

#[tokio::test]
async fn the_guard_result_pipeline_scans_a_resource_read() {
    use fontaine_tools::result_pipeline::{ResultPipeline, ResultStage, StageKind, ToolOutcome};

    let d = Dirs::new();
    let runs = Arc::new(Mutex::new(Vec::new()));
    struct Marker(Arc<Mutex<Vec<String>>>);
    impl ResultStage for Marker {
        fn process<'a>(
            &'a self,
            view: fontaine_tools::gate::CallView<'a>,
            outcome: ToolOutcome,
        ) -> BoxFuture<'a, ToolOutcome> {
            let runs = self.0.clone();
            Box::pin(async move {
                if let Ok(output) = &outcome {
                    runs.lock()
                        .unwrap()
                        .push(format!("{}:{}", view.tool, output.render()));
                }
                outcome
            })
        }
    }
    let runs2 = runs.clone();
    let mut env = fake_env(
        &d,
        Fake {
            listings: Vec::new(),
            reads: Mutex::new(Vec::new()),
        },
    );
    env.guard.results =
        Arc::new(ResultPipeline::new().with_stage(StageKind::Scanner, Arc::new(Marker(runs2))));
    let reads = Mutex::new(vec![(
        "repo".to_string(),
        "repo://info".to_string(),
        Ok(vec![ResourceContents::text("scan me", "repo://info")]),
    )]);
    env.mcp_resources = Arc::new(Fake {
        listings: Vec::new(),
        reads,
    });
    let tools = registry(&env);
    let _ = call(
        &tools,
        "read_mcp_resource",
        json!({"server": "repo", "uri": "repo://info"}),
    )
    .await;
    assert_eq!(
        runs.lock().unwrap().as_slice(),
        ["read_mcp_resource:scan me\n"]
    );
}

// The prompt half of the listings: the single-round fetch a prompt command drives.
#[tokio::test]
async fn get_prompt_once_flattens_user_text_and_marks_other_roles() {
    let connected = serve(Double::with_resources(
        vec![],
        vec![],
        vec![greet_prompt(), assistant_prompt()],
    ))
    .await;
    let response = connected
        .connection
        .peer()
        .get_prompt_once({
            let mut params = GetPromptRequestParams::new("greet");
            params.arguments = Some(serde_json::Map::from_iter([(
                "name".to_string(),
                json!("Ada"),
            )]));
            params
        })
        .await
        .expect("a prompt");
    let messages = match response {
        rmcp::model::GetPromptResponse::Complete(result) => result.messages,
        other => panic!("expected a complete prompt, got {other:?}"),
    };
    let texts: Vec<String> = messages
        .iter()
        .map(
            |message: &PromptMessage| match (&message.role, &message.content) {
                (Role::User, rmcp::model::ContentBlock::Text(text)) => text.text.clone(),
                (role, block) => panic!("unexpected {role:?} {block:?}"),
            },
        )
        .collect();
    assert_eq!(texts, ["Hello, Ada!", "Mood: neutral."]);

    let response = connected
        .connection
        .peer()
        .get_prompt_once(GetPromptRequestParams::new("assistant-note"))
        .await
        .expect("a prompt");
    let messages = match response {
        rmcp::model::GetPromptResponse::Complete(result) => result.messages,
        other => panic!("expected a complete prompt, got {other:?}"),
    };
    assert!(matches!(messages[0].role, Role::Assistant));
}
