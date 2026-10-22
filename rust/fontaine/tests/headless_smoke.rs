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
async fn without_the_spike_variable_no_tool_is_advertised() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("ok")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let requests = endpoint.requests().await;
    let tools = requests[0].get("tools");
    assert!(
        tools.is_none_or(|t| t.as_array().is_some_and(Vec::is_empty)),
        "{tools:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_round_trips_through_the_echo_tool() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "echo", json!({"text": "ping"})),
            Turn::text("The tool said ping"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless_with_echo(&["--output-format", "stream-json", "use the tool"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(
        kinds(&lines),
        ["init", "tool_call", "tool_result", "assistant", "result"]
    );
    assert_eq!(lines[0]["tools"], json!(["echo"]));
    assert_eq!(lines[1]["tool"], "echo");
    assert_eq!(lines[1]["tool_use_id"], "call_1");
    assert_eq!(lines[1]["args"], json!({"text": "ping"}));
    assert_eq!(lines[2]["tool_use_id"], "call_1");
    assert_eq!(lines[2]["text"], "ping");
    assert_eq!(lines[2]["success"], json!(true));
    assert_eq!(lines[3]["text"], "The tool said ping");
    assert_eq!(lines[4]["result"], "The tool said ping");
    assert_eq!(lines[4]["subtype"], "success");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].to_string().contains("\"echo\""));
    assert!(
        requests[1].to_string().contains("ping"),
        "the tool result reaches the model"
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
async fn auto_mode_is_rejected_before_the_guardian_ships() {
    let s = Sandbox::new();
    let out = s.run(&["-p", "--permission-mode", "auto", "hi"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("auto"), "{}", out.stderr);

    s.user_config("[permissions]\ndefault_mode = \"auto\"\n");
    let out = s.run(&["-p", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
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
async fn auth_is_a_subcommand_never_a_prompt() {
    let s = Sandbox::new();
    let out = s.run(&["auth", "login"]).await;
    assert_eq!(out.code, Some(2));
    assert!(
        out.stderr.contains("account") || out.stderr.contains("telemetry"),
        "{}",
        out.stderr
    );
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
async fn auth_login_stores_a_secret_read_from_stdin() {
    let s = Sandbox::new();
    s.user_config("[accounts.work]\nkind = \"anthropic\"\n");
    let out = s
        .run_with_stdin(&["auth", "login", "work"], "sk-from-stdin\n")
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!out.stdout.contains("sk-from-stdin") && !out.stderr.contains("sk-from-stdin"));
    let stored = std::fs::read_to_string(s.data.join("credentials.toml")).unwrap();
    assert!(stored.contains("[accounts.work]") && stored.contains("sk-from-stdin"));

    let out = s
        .run_with_stdin(&["auth", "login", "--telemetry"], "ingest\n")
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let stored = std::fs::read_to_string(s.data.join("credentials.toml")).unwrap();
    assert!(stored.contains("[accounts.work]") && stored.contains("[telemetry]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_login_rejects_an_unknown_account_and_an_empty_secret() {
    let s = Sandbox::new();
    s.user_config("[accounts.work]\nkind = \"anthropic\"\n");
    let out = s.run_with_stdin(&["auth", "login", "nobody"], "k\n").await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("nobody"), "{}", out.stderr);

    let out = s.run_with_stdin(&["auth", "login", "work"], "\n").await;
    assert_eq!(out.code, Some(2));
    assert!(!s.data.join("credentials.toml").exists());
}
