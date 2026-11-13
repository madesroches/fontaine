//! Servers are started with the `Auto` lifecycle: `server/discover` first, `initialize` for a
//! server that does not know it. The servers here are hand-written on a pipe because rmcp's own
//! server always answers `initialize` with the older version.

mod mcp_common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_tools::mcp::{ElicitForm, ElicitReply, Elicitor, connect_transport};
use futures::future::BoxFuture;
use mcp_common::{Gate, call, names, options, tool_server};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

struct Recording {
    forms: Mutex<Vec<ElicitForm>>,
}

impl Elicitor for Recording {
    fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
        self.forms.lock().unwrap().push(form);
        Box::pin(async {
            ElicitReply::Accept(serde_json::Map::from_iter([(
                "name".to_string(),
                json!("Ada"),
            )]))
        })
    }
}

#[derive(Clone, Copy)]
enum Lifecycle {
    /// Answers `server/discover` with 2026-07-28 and rejects `initialize`.
    DiscoverOnly,
    /// Answers `server/discover` with method-not-found and `initialize` with an older version.
    Legacy,
}

fn tool(name: &str) -> Value {
    json!({"name": name, "description": "d", "inputSchema": {"type": "object"}})
}

fn error(id: &Value, code: i64) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": "no"}}).to_string()
}

fn reply(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

struct Server {
    methods: Arc<Mutex<Vec<String>>>,
    tools: Arc<Mutex<Option<Vec<Value>>>>,
    writer: Arc<tokio::sync::Mutex<DuplexStream>>,
}

impl Server {
    fn count(&self, method: &str) -> usize {
        self.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == method)
            .count()
    }

    async fn announce_tool_change(&self) {
        let line = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        self.writer
            .lock()
            .await
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the client is listening");
    }
}

/// `tools` of `None` makes `tools/list` fail.
fn serve(
    lifecycle: Lifecycle,
    tools: Option<Vec<Value>>,
    server_read: DuplexStream,
    server_write: DuplexStream,
) -> Server {
    let server = Server {
        methods: Arc::default(),
        tools: Arc::new(Mutex::new(tools)),
        writer: Arc::new(tokio::sync::Mutex::new(server_write)),
    };
    let (methods, tools, writer) = (
        server.methods.clone(),
        server.tools.clone(),
        server.writer.clone(),
    );
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message: Value = serde_json::from_str(&line).expect("a JSON-RPC message");
            let Some(method) = message["method"].as_str() else {
                // A reply to a request this server made: the elicitation answer.
                continue;
            };
            methods.lock().unwrap().push(method.to_string());
            let Some(id) = message.get("id") else {
                continue;
            };
            let out = match (method, lifecycle) {
                ("server/discover", Lifecycle::DiscoverOnly) => reply(
                    id,
                    json!({
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {"listChanged": true}},
                        "ttlMs": 0,
                        "cacheScope": "private",
                        "_meta": {"io.modelcontextprotocol/serverInfo":
                            {"name": "hand", "version": "1"}},
                    }),
                ),
                ("server/discover", Lifecycle::Legacy) => error(id, -32601),
                ("initialize", Lifecycle::DiscoverOnly) => error(id, -32600),
                ("initialize", Lifecycle::Legacy) => reply(
                    id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {"listChanged": true}},
                        "serverInfo": {"name": "hand", "version": "1"},
                    }),
                ),
                ("tools/list", _) => match tools.lock().unwrap().clone() {
                    Some(tools) => reply(id, json!({"tools": tools})),
                    None => error(id, -32603),
                },
                ("tools/call", _) => {
                    let params = &message["params"];
                    match params["inputResponses"]["who"]["content"]["name"].as_str() {
                        None => reply(
                            id,
                            json!({
                                "resultType": "input_required",
                                "inputRequests": {"who": {
                                    "method": "elicitation/create",
                                    "params": {
                                        "message": "Who are you?",
                                        "requestedSchema": {
                                            "type": "object",
                                            "properties": {"name": {"type": "string"}},
                                            "required": ["name"],
                                        },
                                    },
                                }},
                                "requestState": "round-one",
                            }),
                        ),
                        Some(name) => reply(
                            id,
                            json!({"content": [{"type": "text", "text": format!("hello {name}")}]}),
                        ),
                    }
                }
                _ => reply(id, json!({})),
            };
            if writer
                .lock()
                .await
                .write_all(format!("{out}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    server
}

fn pipes(
    lifecycle: Lifecycle,
    tools: Option<Vec<Value>>,
) -> (Server, (DuplexStream, DuplexStream)) {
    let (c2s, sfc) = tokio::io::duplex(65536);
    let (s2c, cfs) = tokio::io::duplex(65536);
    (serve(lifecycle, tools, sfc, s2c), (cfs, c2s))
}

#[tokio::test]
async fn a_server_that_only_speaks_discover_connects_and_serves_an_input_round() {
    let (server, transport) = pipes(Lifecycle::DiscoverOnly, Some(vec![tool("ask")]));
    let elicitor = Arc::new(Recording {
        forms: Mutex::new(Vec::new()),
    });
    let mut options = options(Gate::new(true));
    options.elicitor = elicitor.clone();
    let handle = tool_server();
    let connected = connect_transport("hand", transport, handle.clone(), &options)
        .await
        .expect("connect");
    assert_eq!(names(&handle), ["mcp__hand__ask"]);
    assert_eq!(server.count("server/discover"), 1);
    assert_eq!(server.count("initialize"), 0);
    assert_eq!(server.count("tools/list"), 1);
    let out = call(&handle, "mcp__hand__ask", "{}").await.unwrap();
    assert!(out.contains("hello Ada"), "{out}");
    assert_eq!(elicitor.forms.lock().unwrap().len(), 1);
    connected.connection.close().await;
}

#[tokio::test]
async fn a_legacy_server_falls_back_to_initialize_and_registers_its_tools_once() {
    let (server, transport) = pipes(Lifecycle::Legacy, Some(vec![tool("ask"), tool("other")]));
    let handle = tool_server();
    let connected = connect_transport("old", transport, handle.clone(), &options(Gate::new(true)))
        .await
        .expect("connect");
    assert_eq!(names(&handle), ["mcp__old__ask", "mcp__old__other"]);
    assert_eq!(connected.tool_count(), 2);
    assert_eq!(server.count("server/discover"), 1);
    assert_eq!(server.count("initialize"), 1);
    assert_eq!(server.count("tools/list"), 1);
    connected.connection.close().await;
}

#[tokio::test]
async fn a_server_whose_tool_list_fails_does_not_connect() {
    let (server, transport) = pipes(Lifecycle::DiscoverOnly, None);
    let handle = tool_server();
    let options = options(Gate::new(true));
    let attempt = tokio::time::timeout(
        Duration::from_millis(500),
        connect_transport("broken", transport, handle.clone(), &options),
    )
    .await;
    assert!(attempt.is_err(), "the attempt only ends at the deadline");
    assert_eq!(server.count("tools/list"), 1);
    assert!(names(&handle).is_empty());
}

#[tokio::test]
async fn a_server_with_no_tools_connects() {
    let (_server, transport) = pipes(Lifecycle::DiscoverOnly, Some(vec![]));
    let handle = tool_server();
    let connected = connect_transport("bare", transport, handle.clone(), &options(Gate::new(true)))
        .await
        .expect("connect");
    assert!(names(&handle).is_empty());
    assert_eq!(connected.tool_count(), 0);
    connected.connection.close().await;
}

#[tokio::test]
async fn a_later_tool_list_change_still_reconciles_after_a_discover_connect() {
    let (server, transport) = pipes(Lifecycle::DiscoverOnly, Some(vec![tool("one")]));
    let handle = tool_server();
    let connected = connect_transport("live", transport, handle.clone(), &options(Gate::new(true)))
        .await
        .expect("connect");
    assert_eq!(names(&handle), ["mcp__live__one"]);
    *server.tools.lock().unwrap() = Some(vec![tool("two")]);
    server.announce_tool_change().await;
    let swapped = async {
        while names(&handle) != ["mcp__live__two"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), swapped)
        .await
        .expect("the changed list replaced the old one");
    connected.connection.close().await;
}
