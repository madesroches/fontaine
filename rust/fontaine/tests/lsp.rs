//! A language server from the user's config reaches the model as the `lsp` tool, and its
//! diagnostics reach it on `write` results, through the headless `-p` surface.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire, write};
use serde_json::{Value, json};

fn example(name: &str) -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = dir.join(file);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}

fn with_server(s: &Sandbox) {
    let path = s.config.join("config.toml");
    let base = std::fs::read_to_string(&path).unwrap();
    write(
        &path,
        &format!(
            "{base}\n[lsp.servers.fake]\ncommand = '{}'\nextension_to_language = {{ \".rs\" = \"rust\" }}\n",
            example("fake_lsp")
        ),
    );
}

fn of_type<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|l| l["type"] == kind).collect()
}

fn tools(lines: &[Value]) -> Vec<&str> {
    lines[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_lsp_tool_exists_only_with_a_server_and_answers_on_the_fast_path() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "lsp",
                json!({"operation": "goToDefinition", "path": "src/main.rs", "line": 1, "character": 4}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    with_server(&s);
    s.add_file("src/main.rs", "fn main() {}\n");
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(tools(&lines).contains(&"lsp"), "{:?}", tools(&lines));
    let decision = of_type(&lines, "gate_decision")[0];
    assert_eq!(decision["decision"], "allow", "{decision}");
    assert_eq!(decision["tier"], "read_only_fast_path", "{decision}");
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    assert_eq!(result["text"], "1 definition:\nsrc/main.rs:6:5", "{result}");

    // Without a server the tool is not offered at all.
    let plain = Sandbox::compatible(&endpoint.url());
    let out = plain
        .headless(&["--output-format", "stream-json", "go"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert!(!tools(&lines).contains(&"lsp"), "{:?}", tools(&lines));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_carries_the_diagnostics_note_through_the_pipeline() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "write",
                json!({"path": "src/new.rs", "content": "ERROR: boom\n"}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    with_server(&s);
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "write",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let result = of_type(&lines, "tool_result")[0];
    assert_eq!(result["success"], json!(true), "{result}");
    let text = result["text"].as_str().unwrap();
    assert!(text.contains("Created"), "{text}");
    assert!(
        text.contains(
            "Diagnostics (fake) for src/new.rs: 1 new — src/new.rs:1:1 error[E0001]: boom"
        ),
        "{text}"
    );
    assert!(
        text.contains("[Diagnostics from language server fake."),
        "{text}"
    );
}
