//! The inert tool inventory: a minimal MCP server over stdio that lists a request's tool schemas
//! and refuses every call. The CLI only accepts tools through MCP, but Fontaine executes tools
//! itself, through its own gate; this server exists so the model can be shown the schemas and
//! so a call fails safe if the CLI ever tried to run one.

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

/// The hidden argument that makes `fontaine` and `fontaine-evals` run as the inventory.
pub const INVENTORY_FLAG: &str = "--internal-claude-inventory";

/// Marks a tool as one the CLI must not run without a person at the keyboard, which a
/// non-interactive run denies.
const INTERACTION_MARKER: &str = "anthropic/requiresUserInteraction";

const REFUSAL: &str = "Only Fontaine executes tools; this inventory only describes them.";

/// Answers one JSON-RPC line, or `None` for a notification.
pub fn handle(tools: &[Value], line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return Some(error(Value::Null, -32700, "parse error")),
    };
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        // A response or a malformed message: nothing to answer.
        return request
            .get("id")
            .map(|id| error(id.clone(), -32600, "invalid request"));
    };
    let id = request.get("id").cloned()?;
    let reply = match method {
        "initialize" => json!({
            "protocolVersion": request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fontaine", "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => {
            let listed: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    let mut tool = tool.clone();
                    tool["_meta"] = json!({ INTERACTION_MARKER: true });
                    tool
                })
                .collect();
            json!({ "tools": listed })
        }
        "tools/call" => {
            let named = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty());
            if !named {
                return Some(error(id, -32602, "a tool call needs a name"));
            }
            json!({
                "content": [{"type": "text", "text": REFUSAL}],
                "isError": true,
            })
        }
        _ => return Some(error(id, -32601, "method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": reply}).to_string())
}

fn error(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Runs the inventory for the tool manifest at `manifest`, reading requests from stdin until it
/// closes.
pub fn serve(manifest: &Path) -> std::io::Result<()> {
    let tools: Vec<Value> = serde_json::from_slice(&std::fs::read(manifest)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&tools, &line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// The arguments of a process started in inventory mode, when they are such: the flag and the
/// manifest path.
pub fn from_args<I: IntoIterator<Item = std::ffi::OsString>>(
    args: I,
) -> Option<std::path::PathBuf> {
    let mut args = args.into_iter().skip(1);
    if args.next()? != INVENTORY_FLAG {
        return None;
    }
    args.next().map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![json!({"name": "read_file", "description": "d", "inputSchema": {"type": "object"}})]
    }

    fn call(line: &str) -> Value {
        serde_json::from_str(&handle(&tools(), line).expect("an answer")).unwrap()
    }

    #[test]
    fn it_lists_the_schemas_with_the_interaction_marker() {
        let reply = call(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let tool = &reply["result"]["tools"][0];
        assert_eq!(tool["name"], "read_file");
        assert_eq!(tool["inputSchema"], json!({"type": "object"}));
        assert_eq!(tool["_meta"][INTERACTION_MARKER], true);
    }

    #[test]
    fn every_call_is_refused() {
        let reply = call(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_file","arguments":{}}}"#,
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Only Fontaine executes tools")
        );
    }

    #[test]
    fn a_call_even_for_an_unknown_tool_is_refused_not_run() {
        let reply =
            call(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}"#);
        assert_eq!(reply["result"]["isError"], true);
    }

    #[test]
    fn malformed_input_is_answered_without_running_anything() {
        assert_eq!(call("not json")["error"]["code"], -32700);
        assert_eq!(
            call(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call"}"#)["error"]["code"],
            -32602
        );
        assert_eq!(
            call(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{}}"#)["error"]["code"],
            -32602
        );
        assert_eq!(
            call(r#"{"jsonrpc":"2.0","id":6,"method":"resources/list"}"#)["error"]["code"],
            -32601
        );
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":7}"#)["error"]["code"], -32600);
    }

    #[test]
    fn initialize_and_notifications() {
        let reply = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "fontaine");
        assert!(
            handle(
                &tools(),
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
        assert_eq!(
            call(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#)["result"],
            json!({})
        );
    }

    #[test]
    fn the_flag_selects_the_mode() {
        let args = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert_eq!(
            from_args(args(&["fontaine", INVENTORY_FLAG, "/tmp/m.json"])),
            Some(std::path::PathBuf::from("/tmp/m.json"))
        );
        assert_eq!(from_args(args(&["fontaine", "-p", "x"])), None);
        assert_eq!(from_args(args(&["fontaine", INVENTORY_FLAG])), None);
    }
}
