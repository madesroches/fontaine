//! `fontaine -p` against the scripted endpoint, with the wrapped tool registry in the loop.

mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn tool_results(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|l| l["type"] == "tool_result")
        .collect()
}

fn result_of(lines: &[Value]) -> &Value {
    lines.iter().rev().find(|l| l["type"] == "result").unwrap()
}

fn ledger(s: &Sandbox, session: &str) -> Vec<Value> {
    let path = s
        .data
        .join("sessions")
        .join(session)
        .join("ledger")
        .join("ledger.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_that_would_prompt_come_back_as_denials_and_the_run_still_exits_zero() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "write", json!({"path": "new.txt", "content": "x"})),
            Turn::tool("c2", "shell", json!({"command": "touch shell-ran.txt"})),
            Turn::text("I could not do either"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&["--output-format", "stream-json", "make a file"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results = tool_results(&lines);
    assert_eq!(results.len(), 2);
    for result in &results {
        assert_eq!(result["success"], json!(false));
        assert!(
            result["text"].as_str().unwrap().contains("denied"),
            "{result}"
        );
    }
    assert!(!s.work.join("new.txt").exists());
    assert!(!s.work.join("shell-ran.txt").exists());

    let last = result_of(&lines);
    assert_eq!(last["subtype"], "success");
    let denials = last["denials"].as_array().unwrap();
    assert_eq!(denials.len(), 2);
    assert_eq!(denials[0]["tool"], "write");
    assert_eq!(denials[1]["tool"], "shell");
    for denial in denials {
        assert_eq!(denial["tier"], "mode");
    }
    let decisions: Vec<_> = lines
        .iter()
        .filter(|l| l["type"] == "gate_decision")
        .collect();
    assert_eq!(decisions.len(), 2);
    assert!(decisions.iter().all(|d| d["decision"] == "deny"));

    // The model is told why, as the tool result.
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 3);
    assert!(requests[1].to_string().contains("denied"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_out_of_scope_read_is_denied() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "../outside.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    std::fs::write(s.work.parent().unwrap().join("outside.txt"), "TOP-SECRET").unwrap();
    let out = s
        .headless(&["--output-format", "stream-json", "read it"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results = tool_results(&lines);
    assert_eq!(results[0]["success"], json!(false));
    let text = results[0]["text"].as_str().unwrap();
    assert!(
        text.contains("denied") && text.contains("outside the workspace roots"),
        "{text}"
    );
    assert!(!out.stdout.contains("TOP-SECRET"));
    assert!(
        !endpoint.requests().await[1]
            .to_string()
            .contains("TOP-SECRET")
    );
}

/// A sandbox with a sibling directory `extra` holding `shared.txt`, for tests that script the
/// absolute path of a file in it.
fn sandbox_with_extra_root() -> (Sandbox, std::path::PathBuf) {
    let s = Sandbox::new();
    let extra = s.work.parent().unwrap().join("extra");
    std::fs::create_dir_all(extra.join(".fontaine")).unwrap();
    std::fs::write(extra.join("shared.txt"), "SHARED-CONTENT\n").unwrap();
    (s, extra)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_in_an_add_dir_root_is_allowed() {
    let (s, extra) = sandbox_with_extra_root();
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "read",
                json!({"path": extra.join("shared.txt").to_str().unwrap()}),
            ),
            Turn::tool("c2", "read", json!({"path": "../extra/shared.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    s.point_at(&endpoint.url());
    let out = s
        .headless(&[
            "--add-dir",
            extra.to_str().unwrap(),
            "--output-format",
            "stream-json",
            "read it",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    for result in tool_results(&lines) {
        assert_eq!(result["success"], json!(true), "{result}");
        assert!(result["text"].as_str().unwrap().contains("SHARED-CONTENT"));
    }
    assert_eq!(tool_results(&lines).len(), 2);
    let roots = lines[0]["workspace_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 3);
    assert_eq!(
        roots[1],
        extra.canonicalize().unwrap().display().to_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_add_dir_the_same_read_is_denied() {
    let (s, extra) = sandbox_with_extra_root();
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "read",
                json!({"path": extra.join("shared.txt").to_str().unwrap()}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    s.point_at(&endpoint.url());
    let out = s
        .headless(&["--output-format", "stream-json", "read it"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(tool_results(&lines)[0]["success"], json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_in_an_added_root_are_not_loaded() {
    let (s, extra) = sandbox_with_extra_root();
    // If this were loaded, it would deny every `read`.
    std::fs::write(
        extra.join(".fontaine").join("settings.toml"),
        "[permissions]\ndeny = [\"read\"]\n",
    )
    .unwrap();
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool(
                "c1",
                "read",
                json!({"path": extra.join("shared.txt").to_str().unwrap()}),
            ),
            Turn::text("done"),
        ],
    )
    .await;
    s.point_at(&endpoint.url());
    let out = s
        .headless(&[
            "--add-dir",
            extra.to_str().unwrap(),
            "--output-format",
            "stream-json",
            "read it",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let sources = lines[0]["config_sources"].as_array().unwrap();
    let extra_text = extra.display().to_string();
    assert!(
        sources.iter().all(|source| !source["path"]
            .as_str()
            .is_some_and(|path| path.starts_with(&extra_text))),
        "{sources:?}"
    );
    assert_eq!(tool_results(&lines)[0]["success"], json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_system_prompt_names_the_roots_and_carries_the_instruction_files() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("FONTAINE.md", "PROJECT-INSTRUCTION-MARKER\n");
    common::write(&s.config.join("FONTAINE.md"), "USER-INSTRUCTION-MARKER\n");
    let out = s
        .headless(&["--append-system-prompt", "APPENDED-MARKER", "hi"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let request = endpoint.requests().await[0].to_string();
    for needle in [
        s.work.to_str().unwrap().replace('\\', "\\\\").as_str(),
        "USER-INSTRUCTION-MARKER",
        "PROJECT-INSTRUCTION-MARKER",
        "APPENDED-MARKER",
        "Do not retry a denied call",
    ] {
        assert!(request.contains(needle), "the request lacks `{needle}`");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hierarchy_files_an_import_and_a_rule_reach_the_system_prompt() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let above = s.work.parent().unwrap();
    common::write(&above.join("CLAUDE.md"), "ANCESTOR-MARKER\n");
    s.add_file("CLAUDE.md", "ROOT-MARKER\n@docs/imported.md\n");
    s.add_file("docs/imported.md", "IMPORTED-MARKER\n");
    s.add_file(
        ".claude/rules/always.md",
        "---\ndescription: x\n---\nRULE-MARKER\n",
    );
    s.add_file(
        ".claude/rules/scoped.md",
        "---\npaths: [\"never/**\"]\n---\nSCOPED-MARKER\n",
    );
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let request = endpoint.requests().await[0].to_string();
    for needle in [
        "ANCESTOR-MARKER",
        "ROOT-MARKER",
        "IMPORTED-MARKER",
        "RULE-MARKER",
    ] {
        assert!(request.contains(needle), "the request lacks `{needle}`");
    }
    assert!(
        !request.contains("SCOPED-MARKER"),
        "a path-scoped rule is not in the launch prompt"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_md_is_read_when_the_repository_has_no_claude_or_fontaine_file() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("AGENTS.md", "AGENTS-MARKER\n");
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let request = endpoint.requests().await[0].to_string();
    assert!(request.contains("AGENTS-MARKER"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_brings_the_nested_instruction_file_into_the_tool_result() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "sub/f.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("CLAUDE.md", "ROOT-MARKER\n");
    s.add_file("sub/CLAUDE.md", "NESTED-MARKER\n");
    s.add_file("sub/f.txt", "file body\n");
    let out = s.headless(&["read it"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].to_string().contains("NESTED-MARKER"));
    let second = requests[1].to_string();
    assert!(second.contains("file body"), "{second}");
    assert!(second.contains("NESTED-MARKER"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_conversation_continues_across_turns_through_the_session_store() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![Turn::text("first answer"), Turn::text("second answer")],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let input = [
        json!({"type": "user", "message": {"role": "user", "content": "FIRST-QUESTION"}}),
        json!({"type": "user", "message": {"role": "user", "content": "SECOND-QUESTION"}}),
    ];
    let stdin: String = input.iter().map(|l| format!("{l}\n")).collect();
    let out = s
        .run_with_stdin(
            &[
                "-p",
                "--permission-mode",
                "manual",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
            ],
            &stdin,
        )
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    let second = requests[1].to_string();
    assert!(second.contains("FIRST-QUESTION") && second.contains("first answer"));
    assert!(second.contains("SECOND-QUESTION"));

    let session = out.lines()[0]["session_id"].as_str().unwrap().to_string();
    let log = s.data.join("sessions").join(&session).join("logs");
    assert!(log.is_dir(), "the context is persisted by rig-memory");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_records_the_session_the_messages_and_the_decisions() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "read", json!({"path": "note.txt"})),
            Turn::tool("c2", "shell", json!({"command": "true"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("note.txt", "hello\n");
    s.add_file(".gitignore", "ignored.txt\n");
    s.add_file("ignored.txt", "x");
    let out = s
        .headless(&["--output-format", "stream-json", "VERBATIM user words"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let session = out.lines()[0]["session_id"].as_str().unwrap().to_string();
    let records = ledger(&s, &session);
    let kinds: Vec<&str> = records
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "session_start",
            "file_index",
            "user_message",
            "action",
            "denial"
        ]
    );
    assert_eq!(records[0]["permission_mode"], "manual");
    assert_eq!(records[0]["session_id"], session.as_str());
    assert_eq!(records[0]["workspace_trust"], "untrusted");
    let indexed = records[1]["paths"].to_string();
    assert!(
        indexed.contains("note.txt") && !indexed.contains("ignored.txt"),
        "{indexed}"
    );
    assert_eq!(records[2]["text"], "VERBATIM user words");
    assert_eq!(records[3]["tool"], "read");
    assert_eq!(records[4]["tool"], "shell");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn configured_env_reaches_the_shell_tool_and_only_its_names_reach_the_ledger() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("c1", "shell", json!({"command": "printenv PROBE_ENV"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let settings = s.work.parent().unwrap().join("env-settings.toml");
    common::write(
        &settings,
        "[env]\nPROBE_ENV = \"probe-value-7431\"\n[permissions]\nallow = [\"shell(printenv *)\"]\n",
    );
    let out = s
        .headless(&[
            "--settings",
            settings.to_str().unwrap(),
            "--output-format",
            "stream-json",
            "print it",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let results = tool_results(&lines);
    assert_eq!(results[0]["success"], json!(true), "{}", results[0]);
    assert!(
        results[0]["text"]
            .as_str()
            .unwrap()
            .contains("probe-value-7431"),
        "{}",
        results[0]
    );
    let session = lines[0]["session_id"].as_str().unwrap().to_string();
    let records = ledger(&s, &session);
    assert_eq!(records[0]["env_keys"], json!(["PROBE_ENV"]));
    let raw = serde_json::to_string(&records).unwrap();
    assert!(!raw.contains("probe-value-7431"), "{raw}");
}
