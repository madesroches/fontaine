//! A server that negotiates protocol 2026-07-28 asks for input inside the answer to `tools/call`.
//! rmcp's own server always answers `initialize` with the older version, so the server here is
//! hand-written on a pipe.

mod mcp_common;

use std::sync::{Arc, Mutex};

use fontaine_tools::mcp::{ElicitForm, ElicitReply, Elicitor, connect_transport};
use futures::future::BoxFuture;
use mcp_common::{Gate, call, options, tool_server};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

fn reply(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// Answers `initialize` with 2026-07-28, lists one tool, and asks for a name before completing
/// `tools/call`. Returns the `tools/call` params it received.
fn serve(
    server_read: tokio::io::DuplexStream,
    mut server_write: tokio::io::DuplexStream,
) -> Arc<Mutex<Vec<Value>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message: Value = serde_json::from_str(&line).expect("a JSON-RPC message");
            let (Some(method), Some(id)) = (message["method"].as_str(), message.get("id")) else {
                continue;
            };
            let out = match method {
                "initialize" => reply(
                    id,
                    json!({
                        "protocolVersion": "2026-07-28",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "hand", "version": "1"},
                    }),
                ),
                "tools/list" => reply(
                    id,
                    json!({"tools": [{
                        "name": "ask",
                        "description": "Ask for a name",
                        "inputSchema": {"type": "object"},
                    }]}),
                ),
                "tools/call" => {
                    let params = message["params"].clone();
                    seen.lock().unwrap().push(params.clone());
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
            if server_write
                .write_all(format!("{out}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    calls
}

#[tokio::test]
async fn a_2026_07_28_server_that_asks_for_input_gets_the_answer_on_a_retry() {
    let (c2s, sfc) = tokio::io::duplex(65536);
    let (s2c, cfs) = tokio::io::duplex(65536);
    let calls = serve(sfc, s2c);
    let elicitor = Arc::new(Recording {
        forms: Mutex::new(Vec::new()),
    });
    let mut options = options(Gate::new(true));
    options.elicitor = elicitor.clone();
    let handle = tool_server();
    let connected = connect_transport("hand", (cfs, c2s), handle.clone(), &options)
        .await
        .expect("connect");
    let out = call(&handle, "mcp__hand__ask", "{}").await.unwrap();
    assert!(out.contains("hello Ada"), "{out}");
    {
        let forms = elicitor.forms.lock().unwrap();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].message, "Who are you?");
    }
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1]["requestState"], json!("round-one"));
    assert_eq!(calls[1]["inputResponses"]["who"]["action"], json!("accept"));
    connected.connection.close().await;
}
