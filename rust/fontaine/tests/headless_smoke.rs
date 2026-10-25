mod common;

use common::{Endpoint, Sandbox, Turn, Wire};
use serde_json::{Value, json};

fn kinds(lines: &[Value]) -> Vec<&str> {
    lines.iter().map(|l| l["type"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_streams_to_stdout_and_exits_zero() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("Hello from the mock")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["say hello"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "Hello from the mock\n");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["stream"], json!(true));
    assert!(requests[0].to_string().contains("say hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registered_tools_are_advertised_to_the_model() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let names = [
        "read",
        "ls",
        "glob",
        "grep",
        "write",
        "edit",
        "shell",
        "webfetch",
        "agent",
        "skill",
        "send_message",
        "ask_user_question",
        "todo_write",
    ];
    assert_eq!(out.lines()[0]["tools"], json!(names));

    let requests = endpoint.requests().await;
    let advertised: Vec<&str> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    let mut advertised_sorted = advertised.clone();
    advertised_sorted.sort_unstable();
    let mut expected = names.to_vec();
    expected.sort_unstable();
    assert_eq!(advertised_sorted, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_round_trips_through_the_wrapped_read_tool() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("The tool said ping"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("note.txt", "ping\n");
    let out = s
        .headless(&["--output-format", "stream-json", "use the tool"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(
        kinds(&lines),
        [
            "init",
            "tool_call",
            "gate_decision",
            "tool_result",
            "assistant",
            "result"
        ]
    );
    assert!(
        lines[0]["tools"]
            .as_array()
            .unwrap()
            .contains(&json!("read"))
    );
    assert_eq!(lines[1]["tool"], "read");
    assert_eq!(lines[1]["tool_use_id"], "call_1");
    assert_eq!(lines[1]["args"], json!({"path": "note.txt"}));
    assert_eq!(lines[2]["tool_use_id"], "call_1");
    assert_eq!(lines[2]["tier"], "read_only_fast_path");
    assert_eq!(lines[2]["decision"], "allow");
    assert_eq!(lines[3]["tool_use_id"], "call_1");
    assert!(lines[3]["text"].as_str().unwrap().contains("ping"));
    assert_eq!(lines[3]["success"], json!(true));
    assert_eq!(lines[4]["text"], "The tool said ping");
    assert_eq!(lines[5]["result"], "The tool said ping");
    assert_eq!(lines[5]["subtype"], "success");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].to_string().contains("\"read\""));
    assert!(
        requests[1].to_string().contains("ping"),
        "the tool result reaches the model"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_list_call_succeeds_is_saved_in_the_session_and_survives_a_resume() {
    let list = json!({"todos": [
        {"content": "write the fix", "status": "in_progress"},
        {"content": "run the tests", "status": "pending"},
    ]});
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "todo_write", list.clone()),
            Turn::text("planned"),
            Turn::text("carrying on"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&["--output-format", "stream-json", "plan it"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(
        kinds(&lines),
        [
            "init",
            "tool_call",
            "gate_decision",
            "tool_result",
            "assistant",
            "result"
        ]
    );
    assert_eq!(lines[2]["tier"], "read_only_fast_path");
    assert_eq!(lines[3]["success"], json!(true));
    assert_eq!(
        lines[3]["text"],
        "Updated todo list: 2 items (1 pending, 1 in progress, 0 completed)"
    );
    let id = lines[0]["session_id"].as_str().unwrap().to_string();
    let file = s.data.join("sessions").join(&id).join("todos.json");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved, list["todos"]);

    let again = s
        .headless(&["--output-format", "stream-json", "--resume", &id, "go on"])
        .await;
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(after, saved);
    assert!(
        !again.stderr.contains("task list"),
        "no load warning: {}",
        again.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_messages_add_deltas() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("abcdef")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let deltas: Vec<&str> = lines
        .iter()
        .filter(|l| l["type"] == "assistant_delta")
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert_eq!(deltas.concat(), "abcdef");
    assert!(deltas.len() >= 2);
    assert_eq!(kinds(&lines).last(), Some(&"result"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prompt_is_read_from_stdin_when_absent() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("fine")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .run_with_stdin(&["-p", "--permission-mode", "manual"], "from stdin\n")
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        endpoint.requests().await[0]
            .to_string()
            .contains("from stdin")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_failure_exits_one() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::Fail(500)]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stdout.is_empty(), "{}", out.stdout);
    assert!(out.stderr.contains("fontaine:"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_main_model_is_a_config_error_naming_both_ways_to_set_it() {
    let s = Sandbox::new();
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("[model] model"), "{}", out.stderr);
    assert!(out.stderr.contains("--model"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_flag_overrides_the_configured_model() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\", \"other\"]\n\n[model]\nmodel = \"mock/scripted\"\n",
        endpoint.url()
    ));
    let out = s.headless(&["--model", "mock/other", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(endpoint.requests().await[0]["model"], "other");

    let out = s.headless(&["--model", "mock/missing", "hi"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("missing"), "{}", out.stderr);

    let out = s.headless(&["--model", "nobody/other", "hi"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("nobody"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_mode_is_accepted_by_flag_and_by_the_user_file() {
    let s = Sandbox::new();
    // No model is configured, so the run ends there, not at the mode.
    let out = s.run(&["-p", "--permission-mode", "auto", "hi"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("no main model"), "{}", out.stderr);

    s.user_config("[permissions]\ndefault_mode = \"auto\"\n");
    let out = s.run(&["-p", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("no main model"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headless_run_starts_in_manual_even_though_the_interactive_ui_defaults_to_auto() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.run(&["-p", "--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.lines()[0]["permission_mode"], "manual");

    // An explicit default_mode applies to both surfaces.
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[model]\nmodel = \"mock/scripted\"\n\n[permissions]\ndefault_mode = \"plan\"\n",
        endpoint.url()
    ));
    let out = s.run(&["-p", "--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.lines()[0]["permission_mode"], "plan");
}

#[tokio::test(flavor = "multi_thread")]
async fn there_is_no_bypass_mode() {
    let s = Sandbox::new();
    for value in ["bypassPermissions", "bypass", "none"] {
        let out = s.run(&["-p", "--permission-mode", value, "hi"]).await;
        assert_eq!(out.code, Some(2), "{value}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_with_stream_json_input_is_a_usage_error() {
    let s = Sandbox::new();
    let out = s
        .run(&["-p", "--input-format", "stream-json", "a prompt"])
        .await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("stream-json"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_login_is_gone_and_never_reads_a_prompt() {
    let s = Sandbox::new();
    let out = s.run(&["auth", "login"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("login"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_flag_value_is_a_usage_error() {
    let s = Sandbox::new();
    let out = s.run(&["-p", "--output-format", "xml", "hi"]).await;
    assert_eq!(out.code, Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_responses_wire_works_for_the_priced_openai_kind() {
    let endpoint = Endpoint::start(Wire::Responses, vec![Turn::text("from responses")]).await;
    let s = Sandbox::priced(&endpoint.url());
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "from responses\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_cannot_connect_names_the_cause() {
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let s = Sandbox::compatible(&format!("http://127.0.0.1:{closed}/v1"));
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(out.stderr.contains("tcp connect error"), "{}", out.stderr);
}
