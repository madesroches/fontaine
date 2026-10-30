//! A scripted language server for tests: LSP over stdio with canned answers, steered by
//! environment variables. It is a real child process, so spawn, environment, crash and shutdown
//! paths run for real.
//!
//! - `FAKE_LSP_EXIT_AT_START=<code>`: exit with that code before answering `initialize`.
//! - `FAKE_LSP_CRASH_AFTER=<n>`: exit with code 3 on the n-th request after `initialize`.
//! - `FAKE_LSP_HANG=1`: never answer a request after `initialize`.
//! - `FAKE_LSP_IGNORE_SHUTDOWN=1`: ignore `shutdown` and `exit`.
//! - `FAKE_LSP_PUBLISH_DELAY_MS=<n>`: publish diagnostics n ms after each open, change or save.
//!   A line holding `ERROR:` or `WARN:` becomes an error or warning with the rest as its message.
//! - `FAKE_LSP_UNVERSIONED=1`: publish without a document version.
//! - `FAKE_LSP_ENCODING=utf-16`: answer with that position encoding instead of `utf-32`.
//! - `FAKE_LSP_EMPTY=1`: answer every query with nothing.
//! - `FAKE_LSP_LOG=<path>`: append one line per received message, `method uri` (the process id
//!   for `initialize`).
//! - `hover` answers with the names of the environment variables the server was started with.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn number(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, message: &Value) {
    let body = message.to_string();
    let mut out = out.lock().expect("the stdout lock is not poisoned");
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("stdout is writable");
    out.flush().expect("stdout flushes");
}

fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn log(message: &Value) {
    let Some(path) = std::env::var_os("FAKE_LSP_LOG") else {
        return;
    };
    let method = message["method"].as_str().unwrap_or("");
    let params = &message["params"];
    let uri = if method == "initialize" {
        std::process::id().to_string()
    } else {
        params["textDocument"]["uri"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let mut line = format!("{method} {uri}\n");
    if method == "textDocument/didOpen" {
        let language = params["textDocument"]["languageId"].as_str().unwrap_or("");
        line.push_str(&format!("languageId {language}\n"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn range(line: u32, from: u32, to: u32) -> Value {
    json!({"start": {"line": line, "character": from}, "end": {"line": line, "character": to}})
}

fn item(name: &str, uri: &str, line: u32) -> Value {
    json!({
        "name": name, "kind": 12, "uri": uri,
        "range": range(line, 0, 20), "selectionRange": range(line, 4, 10)
    })
}

fn diagnostics_of(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for (marker, severity) in [("ERROR:", 1), ("WARN:", 2)] {
            if let Some(at) = line.find(marker) {
                out.push(json!({
                    "range": range(index as u32, at as u32, (at + marker.len()) as u32),
                    "severity": severity,
                    "code": if severity == 1 { "E0001" } else { "W0001" },
                    "message": line[at + marker.len()..].trim(),
                }));
            }
        }
    }
    out
}

fn answer(method: &str, params: &Value, root: &str) -> Value {
    if flag("FAKE_LSP_EMPTY") {
        return Value::Null;
    }
    let uri = params["textDocument"]["uri"]
        .as_str()
        .or_else(|| params["item"]["uri"].as_str())
        .unwrap_or("file:///none");
    match method {
        "textDocument/definition" => json!({"uri": uri, "range": range(5, 4, 10)}),
        "textDocument/implementation" => json!([{"uri": uri, "range": range(8, 0, 4)}]),
        "textDocument/references" => json!([
            {"uri": uri, "range": range(0, 0, 3)},
            {"uri": uri, "range": range(2, 3, 6)},
        ]),
        "textDocument/hover" => {
            let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
            let sorted: BTreeMap<_, _> = names.iter().map(|n| (n.clone(), ())).collect();
            let names: Vec<_> = sorted.keys().cloned().collect();
            json!({"contents": {"kind": "markdown", "value": format!("env: {}", names.join(","))}})
        }
        "textDocument/documentSymbol" => json!([{
            "name": "main", "kind": 12,
            "range": range(0, 0, 9), "selectionRange": range(0, 3, 7),
            "children": [{
                "name": "inner", "kind": 13,
                "range": range(1, 0, 9), "selectionRange": range(1, 4, 9)
            }]
        }]),
        "workspace/symbol" => {
            let query = params["query"].as_str().unwrap_or("");
            json!([{
                "name": format!("{query}Symbol"), "kind": 23,
                "location": {"uri": format!("{}/found.rs", root.trim_end_matches('/')), "range": range(3, 0, 5)}
            }])
        }
        "textDocument/prepareCallHierarchy" => json!([item("callee", uri, 4)]),
        "callHierarchy/incomingCalls" => json!([
            {"from": item("caller", uri, 10), "fromRanges": [range(10, 2, 8), range(11, 2, 8)]}
        ]),
        "callHierarchy/outgoingCalls" => json!([
            {"to": item("leaf", uri, 20), "fromRanges": [range(4, 2, 8)]}
        ]),
        _ => Value::Null,
    }
}

fn publish_later(out: &Out, uri: String, version: Option<i64>, text: String) {
    let delay = number("FAKE_LSP_PUBLISH_DELAY_MS");
    let (out, unversioned) = (out.clone(), flag("FAKE_LSP_UNVERSIONED"));
    std::thread::spawn(move || {
        if let Some(delay) = delay.filter(|d| *d > 0) {
            std::thread::sleep(std::time::Duration::from_millis(delay));
        }
        let mut params = json!({"uri": uri, "diagnostics": diagnostics_of(&text)});
        if let (Some(version), false) = (version, unversioned) {
            params["version"] = json!(version);
        }
        send(
            &out,
            &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}),
        );
    });
}

pub fn main() {
    if let Some(code) = number("FAKE_LSP_EXIT_AT_START") {
        eprintln!("fake_lsp: refusing to start");
        std::process::exit(code as i32);
    }
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut input = std::io::BufReader::new(std::io::stdin());
    let mut root = String::from("file:///fake");
    let mut requests = 0u64;
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    while let Some(message) = read_message(&mut input) {
        log(&message);
        let method = message["method"].as_str().unwrap_or("").to_string();
        let params = message["params"].clone();
        let id = message
            .get("id")
            .filter(|_| message.get("method").is_some())
            .cloned();
        match (method.as_str(), id) {
            ("initialize", Some(id)) => {
                root = params["rootUri"]
                    .as_str()
                    .unwrap_or("file:///fake")
                    .to_string();
                let offered = params["capabilities"]["general"]["positionEncodings"].to_string();
                let encoding = std::env::var("FAKE_LSP_ENCODING").unwrap_or_else(|_| {
                    if offered.contains("utf-32") {
                        "utf-32"
                    } else {
                        "utf-16"
                    }
                    .to_string()
                });
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": {
                        "capabilities": {
                            "positionEncoding": encoding,
                            "textDocumentSync": 1,
                            "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}}
                        },
                        "serverInfo": {"name": "fake_lsp"}
                    }}),
                );
                // A client has to take these in its stride.
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "method": "window/logMessage",
                            "params": {"type": 3, "message": "fake_lsp up"}}),
                );
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": 9000, "method": "window/workDoneProgress/create",
                            "params": {"token": "t"}}),
                );
            }
            ("shutdown", Some(id)) => {
                if !flag("FAKE_LSP_IGNORE_SHUTDOWN") {
                    send(&out, &json!({"jsonrpc": "2.0", "id": id, "result": null}));
                }
            }
            ("exit", None) => {
                if !flag("FAKE_LSP_IGNORE_SHUTDOWN") {
                    std::process::exit(0);
                }
            }
            ("textDocument/didOpen", None) => {
                let doc = &params["textDocument"];
                let (uri, text) = (
                    doc["uri"].as_str().unwrap_or("").to_string(),
                    doc["text"].as_str().unwrap_or("").to_string(),
                );
                texts.insert(uri.clone(), text.clone());
                publish_later(&out, uri, doc["version"].as_i64(), text);
            }
            ("textDocument/didChange", None) => {
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or("").to_string();
                let text = params["contentChanges"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                texts.insert(uri.clone(), text.clone());
                publish_later(&out, uri, doc["version"].as_i64(), text);
            }
            (_, Some(id)) => {
                requests += 1;
                if number("FAKE_LSP_CRASH_AFTER").is_some_and(|n| requests >= n) {
                    eprintln!("fake_lsp: crashing");
                    std::process::exit(3);
                }
                if flag("FAKE_LSP_HANG") {
                    continue;
                }
                send(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": answer(&method, &params, &root)}),
                );
            }
            _ => {}
        }
    }
}
