mod common;

use std::path::Path;

use common::{Endpoint, Sandbox, Turn, Usage, Wire};
use serde_json::{Value, json};

fn validator() -> jsonschema::Validator {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/headless-v1.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{value}\n{}", errors.join("\n"));
}

fn result_of(lines: &[Value]) -> &Value {
    lines.iter().rev().find(|l| l["type"] == "result").unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn json_output_is_one_result_object_matching_the_schema() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    assert_eq!(lines.len(), 1);
    assert_valid(&validator(), &lines[0]);
    let result = &lines[0];
    assert_eq!(result["type"], "result");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["result"], "done");
    assert_eq!(result["num_turns"], 1);
    assert_eq!(result["usage"]["main"]["input_tokens"], 1000);
    assert_eq!(result["usage"]["main"]["output_tokens"], 500);
    assert!(result["denials"].as_array().unwrap().is_empty());
    assert!(result.get("structured_output").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_json_events_match_the_schema_and_init_reports_the_session() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "echo", json!({"text": "x"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    std::fs::write(
        s.work.join("extra.toml"),
        "[permissions]\nadditional_directories = [\"/srv/shared\"]\n",
    )
    .unwrap();
    let out = s
        .headless_with_echo(&[
            "--settings",
            s.work.join("extra.toml").to_str().unwrap(),
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let v = validator();
    for line in &lines {
        assert_valid(&v, line);
    }
    let session = lines[0]["session_id"].as_str().unwrap();
    assert!(lines.iter().all(|l| l["session_id"] == session));

    let init = &lines[0];
    assert_eq!(init["type"], "init");
    assert_eq!(init["model"], "mock/scripted");
    assert_eq!(init["guardian_model"], Value::Null);
    assert_eq!(init["guardian_profile"], "full");
    assert_eq!(init["permission_mode"], "manual");
    assert_eq!(init["workspace_trust"], "untrusted");
    let roots = init["workspace_roots"].as_array().unwrap();
    assert_eq!(roots[0], s.work.display().to_string());
    assert_eq!(roots[1], "/srv/shared");
    let sources = init["config_sources"].as_array().unwrap();
    let scopes: Vec<&str> = sources
        .iter()
        .map(|s| s["scope"].as_str().unwrap())
        .collect();
    assert_eq!(
        scopes,
        ["managed", "cli", "project-local", "project", "user"]
    );
    let user = sources.iter().find(|s| s["scope"] == "user").unwrap();
    assert_eq!(user["loaded"], json!(true));
    assert!(user["path"].as_str().unwrap().ends_with("config.toml"));
    assert_eq!(result_of(&lines)["subtype"], "success");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_guardian_model_is_reported_by_init() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.mock]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"scripted\"]\n\n[accounts.keyed]\nkind = \"anthropic\"\n\n[model]\nmodel = \"mock/scripted\"\n\n[guardian]\nmodel = \"keyed/claude-haiku-5\"\nprofile = \"minimal\"\n",
        endpoint.url()
    ));
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    // The guardian's account has no credential: startup still succeeds, the guardian is
    // reported unavailable.
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let init = &out.lines()[0];
    assert_eq!(init["guardian_model"], "keyed/claude-haiku-5");
    assert_eq!(init["guardian_available"], json!(false));
    assert_eq!(init["guardian_profile"], "minimal");
    assert_valid(&validator(), init);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_turns_stops_the_run_with_its_subtype() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "echo", json!({"text": "x"})),
            Turn::text("never reached"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless_with_echo(&["--output-format", "stream-json", "--max-turns", "1", "go"])
        .await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let v = validator();
    lines.iter().for_each(|l| assert_valid(&v, l));
    let result = result_of(&lines);
    assert_eq!(result["subtype"], "error_max_turns");
    assert_eq!(result["is_error"], json!(true));

    let out = s
        .headless_with_echo(&["--output-format", "json", "--max-turns", "2", "go"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_budget_stops_a_run_whose_spend_is_exhausted() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call_1", "echo", json!({"text": "x"})),
            Turn::text("never reached"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    let out = s
        .headless_with_echo(&[
            "--output-format",
            "stream-json",
            "--max-budget-usd",
            "0.0001",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let result = result_of(&lines);
    assert_valid(&validator(), result);
    assert_eq!(result["subtype"], "error_max_budget_usd");
    // 1000 input and 500 output tokens at 0.15 and 0.60 USD per million.
    let spent = result["total_cost_usd"].as_f64().unwrap();
    assert!((spent - 0.00045).abs() < 1e-9, "{spent}");
    // The cap is checked before the tool call runs, and the model is not asked again.
    assert!(lines.iter().all(|l| l["type"] != "tool_result"));
    assert_eq!(endpoint.requests().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_budget_stops_a_stream_json_session_after_a_text_only_answer() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![Turn::text("first answer"), Turn::text("never reached")],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    let input = [
        json!({"type": "user", "message": {"role": "user", "content": "one"}}),
        json!({"type": "user", "message": {"role": "user", "content": "two"}}),
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
                "--max-budget-usd",
                "0.0001",
            ],
            &stdin,
        )
        .await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let results: Vec<&Value> = lines.iter().filter(|l| l["type"] == "result").collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["subtype"], "success");
    assert_eq!(results[1]["subtype"], "error_max_budget_usd");
    assert_eq!(endpoint.requests().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_that_is_not_exceeded_lets_the_run_finish_and_reports_its_cost() {
    let endpoint = Endpoint::start(Wire::Responses, vec![Turn::text("done")]).await;
    let s = Sandbox::priced(&endpoint.url());
    let out = s
        .headless(&["--output-format", "json", "--max-budget-usd", "1", "go"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let result = &out.lines()[0];
    assert_valid(&validator(), result);
    let spent = result["total_cost_usd"].as_f64().unwrap();
    assert!((spent - 0.00045).abs() < 1e-9, "{spent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_with_no_known_price_is_a_usage_error() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--max-budget-usd", "1", "go"]).await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("price"), "{}", out.stderr);
    assert!(endpoint.requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_json_input_drives_a_two_turn_run() {
    let endpoint = Endpoint::start_with(
        Wire::Chat,
        vec![Turn::text("first answer"), Turn::text("second answer")],
        Usage::default(),
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    let input = [
        json!({"type": "user", "message": {"role": "user", "content": "first question"}}),
        json!({"type": "user", "message": {"role": "user",
               "content": [{"type": "text", "text": "second question"}]}}),
    ];
    let v = validator();
    input.iter().for_each(|line| assert_valid(&v, line));
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
    let lines = out.lines();
    lines.iter().for_each(|l| assert_valid(&v, l));
    let results: Vec<&Value> = lines.iter().filter(|l| l["type"] == "result").collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["result"], "first answer");
    assert_eq!(results[1]["result"], "second answer");
    // One init for the session, whatever the number of turns.
    assert_eq!(lines.iter().filter(|l| l["type"] == "init").count(), 1);

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 2);
    let second = requests[1].to_string();
    assert!(second.contains("first question"), "{second}");
    assert!(second.contains("first answer"), "{second}");
    assert!(second.contains("second question"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_input_line_is_a_usage_error() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("x")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .run_with_stdin(
            &[
                "-p",
                "--permission-mode",
                "manual",
                "--input-format",
                "stream-json",
            ],
            "not json\n",
        )
        .await;
    assert_eq!(out.code, Some(2));
    assert!(out.stderr.contains("not JSON"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn json_schema_fills_structured_output() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("{\"answer\":42}")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let schema =
        r#"{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"]}"#;
    let out = s
        .headless(&["--output-format", "json", "--json-schema", schema, "go"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let result = &out.lines()[0];
    assert_valid(&validator(), result);
    assert_eq!(result["structured_output"], json!({"answer": 42}));
    let request = endpoint.requests().await.remove(0).to_string();
    assert!(
        request.contains("answer"),
        "the schema reaches the model: {request}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_json_schema_is_a_usage_error() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("x")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    for schema in ["not json", "42", "\"string\"", "true", "false"] {
        let out = s.headless(&["--json-schema", schema, "go"]).await;
        assert_eq!(out.code, Some(2), "{schema}");
    }
    assert!(endpoint.requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn piped_stdin_is_appended_to_the_prompt_argument() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("x")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .run_with_stdin(&["-p", "explain this PROMPT-ARG"], "PIPED-LINE-7731\n")
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let request = endpoint.requests().await.remove(0).to_string();
    assert!(
        request.contains("explain this PROMPT-ARG\\nPIPED-LINE-7731"),
        "{request}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn system_prompt_flags_shape_the_request() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("x")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&[
            "--system-prompt",
            "BASE-PROMPT-TEXT",
            "--append-system-prompt",
            "APPENDED-TEXT",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let request = endpoint.requests().await.remove(0).to_string();
    let base = request.find("BASE-PROMPT-TEXT").expect("base prompt");
    let appended = request.find("APPENDED-TEXT").expect("appended prompt");
    assert!(base < appended);
    assert!(
        !request.contains("You are Fontaine"),
        "the base prompt is replaced"
    );
}
