mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Endpoint, Output, Sandbox, Turn, Usage, Wire};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

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

#[test]
fn the_schema_tier_enum_is_exactly_the_tier_names() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/headless-v1.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let listed: Vec<&str> = schema["$defs"]["tier"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let names: Vec<&str> = fontaine_core::tier::Tier::ALL
        .iter()
        .map(|t| t.as_str())
        .collect();
    assert_eq!(listed, names);
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
    assert_eq!(result["usage"]["goal"]["input_tokens"], 0);
    assert!(result.get("goal").is_none(), "no goal, no goal object");
    assert!(result["denials"].as_array().unwrap().is_empty());
    assert!(result.get("structured_output").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_json_events_match_the_schema_and_init_reports_the_session() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("note.txt", "x\n");
    let shared = s.work.parent().unwrap().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(
        s.work.join("extra.toml"),
        format!(
            "[permissions]\nadditional_directories = [{:?}]\n",
            shared.display().to_string()
        ),
    )
    .unwrap();
    let out = s
        .headless(&[
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
    // No guardian is configured and no validated one is listed, so the main model judges.
    assert_eq!(init["guardian_model"], "mock/scripted");
    assert_eq!(init["guardian_available"], json!(true));
    assert_eq!(init["guardian_profile"], "full");
    assert_eq!(init["suspended_allow_rules"], json!([]));
    assert_eq!(init["permission_mode"], "manual");
    assert_eq!(init["workspace_trust"], "untrusted");
    let roots = init["workspace_roots"].as_array().unwrap();
    assert_eq!(roots[0], s.work.display().to_string());
    assert_eq!(
        roots[1],
        shared.canonicalize().unwrap().display().to_string()
    );
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
    // `minimal` was configured, but nothing asserted or verified a container: `full` is in force.
    assert_eq!(init["guardian_profile"], "full");
    assert_valid(&validator(), init);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_turns_stops_the_run_with_its_subtype() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("never reached"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("note.txt", "x\n");
    let out = s
        .headless(&["--output-format", "stream-json", "--max-turns", "1", "go"])
        .await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let v = validator();
    lines.iter().for_each(|l| assert_valid(&v, l));
    let result = result_of(&lines);
    assert_eq!(result["subtype"], "error_max_turns");
    assert_eq!(result["is_error"], json!(true));

    let out = s
        .headless(&["--output-format", "json", "--max-turns", "2", "go"])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_budget_stops_a_run_whose_spend_is_exhausted() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("never reached"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    s.add_file("note.txt", "x\n");
    let out = s
        .headless(&[
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
async fn root_warnings_stay_on_stderr_when_a_later_startup_check_exits() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let missing = s.work.parent().unwrap().join("no-such-dir");
    let out = s
        .headless(&[
            "--add-dir",
            missing.to_str().unwrap(),
            "--max-budget-usd",
            "1",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    let warning = out
        .stderr
        .lines()
        .position(|l| l.starts_with("fontaine: warning:") && l.contains("--add-dir"))
        .unwrap_or_else(|| panic!("{}", out.stderr));
    let exit = out
        .stderr
        .lines()
        .position(|l| l.contains("price"))
        .unwrap_or_else(|| panic!("{}", out.stderr));
    assert!(warning < exit, "{}", out.stderr);
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
async fn a_stdin_pipe_that_stays_silent_does_not_block_the_prompt_argument() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("x")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let mut cmd = s.command(&["-p", "SILENT-PIPE-PROMPT"]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let held_open = child.stdin.take().unwrap();
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("the run blocked on stdin")
        .unwrap();
    drop(held_open);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("no stdin data received in 3s"), "{stderr}");
    let request = endpoint.requests().await.remove(0).to_string();
    assert!(request.contains("SILENT-PIPE-PROMPT"), "{request}");
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

#[tokio::test(flavor = "multi_thread")]
async fn a_nonexistent_add_dir_and_a_nonexistent_config_entry_are_skipped_with_a_warning() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let missing_flag = s.work.parent().unwrap().join("no-such-flag-dir");
    let missing_config = s.work.parent().unwrap().join("no-such-config-dir");
    std::fs::write(
        s.work.join("extra.toml"),
        format!(
            "[permissions]\nadditional_directories = [{:?}]\n",
            missing_config.display().to_string()
        ),
    )
    .unwrap();
    let out = s
        .headless(&[
            "--settings",
            s.work.join("extra.toml").to_str().unwrap(),
            "--add-dir",
            missing_flag.to_str().unwrap(),
            "--output-format",
            "stream-json",
            "go",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let init = &out.lines()[0];
    let roots = init["workspace_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 3, "{roots:?}");
    assert_eq!(roots[0], s.work.display().to_string());
    let flag_warning = out
        .stderr
        .lines()
        .find(|l| l.contains(&missing_flag.display().to_string()))
        .unwrap_or_else(|| panic!("{}", out.stderr));
    assert!(flag_warning.contains("--add-dir"), "{flag_warning}");
    let config_warning = out
        .stderr
        .lines()
        .find(|l| l.contains(&missing_config.display().to_string()))
        .unwrap_or_else(|| panic!("{}", out.stderr));
    assert!(
        config_warning.contains("additional_directories"),
        "{config_warning}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_uuid_session_id_is_a_usage_error_that_costs_no_provider_call() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("never asked")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--session-id", "not-a-uuid", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("--session-id"), "{}", out.stderr);
    assert!(endpoint.requests().await.is_empty());
    assert!(!s.data.join("sessions").join("not-a-uuid").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_id_that_is_already_in_use_exits_two_before_any_provider_call() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let id = "0b9c2a3e-9f5d-4c6e-8a11-2f3d4e5a6b7c";

    let first = s
        .headless(&["--session-id", id, "--output-format", "stream-json", "hi"])
        .await;
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    assert_eq!(first.lines()[0]["session_id"], id);
    let ledger = s
        .data
        .join("sessions")
        .join(id)
        .join("ledger")
        .join("ledger.jsonl");
    let before = std::fs::read_to_string(&ledger).unwrap();
    let calls = endpoint.requests().await.len();

    let again = s.headless(&["--session-id", id, "hi again"]).await;
    assert_eq!(again.code, Some(2), "{}", again.stderr);
    assert!(
        again
            .stderr
            .contains(&format!("session ID {id} is already in use")),
        "{}",
        again.stderr
    );
    assert_eq!(endpoint.requests().await.len(), calls, "no provider call");
    assert_eq!(std::fs::read_to_string(&ledger).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_id_is_normalized_to_its_hyphenated_form() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s
        .headless(&[
            "--session-id",
            "0B9C2A3E9F5D4C6E8A112F3D4E5A6B7C",
            "--output-format",
            "stream-json",
            "hi",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(
        out.lines()[0]["session_id"],
        "0b9c2a3e-9f5d-4c6e-8a11-2f3d4e5a6b7c"
    );
}

/// Four large prompts that together outgrow the model's window, then a short one that is
/// preceded by a summarizing call.
fn compacting_input() -> String {
    let big = "lorem ipsum dolor sit amet ".repeat(5_500);
    let mut prompts = vec![big; 4];
    prompts.push("last".to_string());
    prompts
        .iter()
        .map(|text| json!({"type": "user", "message": {"role": "user", "content": text}}))
        .map(|l| format!("{l}\n"))
        .collect()
}

async fn run_compacting(budget: Option<&str>) -> (Output, Endpoint) {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::text("a1"),
            Turn::text("a2"),
            Turn::text("a3"),
            Turn::text("a4"),
            Turn::text("the summary"),
            Turn::text("final answer"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    let mut args = vec![
        "-p",
        "--permission-mode",
        "manual",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ];
    if let Some(budget) = budget {
        args.extend(["--max-budget-usd", budget]);
    }
    let out = s.run_with_stdin(&args, &compacting_input()).await;
    (out, endpoint)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_summarizing_call_is_counted_in_the_totals() {
    let (out, endpoint) = run_compacting(None).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(endpoint.requests().await.len(), 6);
    let lines = out.lines();
    let result = result_of(&lines);
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["num_turns"], json!(6));
    let spent = result["total_cost_usd"].as_f64().unwrap();
    assert!((spent - 6.0 * 0.00045).abs() < 1e-9, "{spent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_the_summarizing_call_crosses_ends_the_run() {
    let (out, endpoint) = run_compacting(Some("0.002")).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    let lines = out.lines();
    let results: Vec<&Value> = lines.iter().filter(|l| l["type"] == "result").collect();
    assert_eq!(results.len(), 5);
    assert_eq!(results[3]["subtype"], "success");
    assert_eq!(results[4]["subtype"], "error_max_budget_usd");
    // Four answers and the summarizer ran; the last prompt's turn did not.
    assert_eq!(endpoint.requests().await.len(), 5);
    assert_eq!(results[4]["num_turns"], json!(5));
}

/// Four prompts that the model's own window leaves alone. With a small `autocompact_window`
/// the first three are summarized before the last, which still fits after the summary.
async fn run_with_window(window: Option<u64>) -> (Output, Endpoint) {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::text("a1"),
            Turn::text("a2"),
            Turn::text("a3"),
            Turn::text("the summary"),
            Turn::text("a4"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    let mut args = vec![
        "-p".to_string(),
        "--permission-mode".to_string(),
        "manual".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if let Some(window) = window {
        let file = s.work.join("window.toml");
        std::fs::write(&file, format!("[context]\nautocompact_window = {window}\n")).unwrap();
        args.extend(["--settings".to_string(), file.display().to_string()]);
    }
    let big = "lorem ipsum dolor sit amet ".repeat(100);
    let input: String = [big.as_str(), big.as_str(), big.as_str(), "last"]
        .iter()
        .map(|text| json!({"type": "user", "message": {"role": "user", "content": text}}))
        .map(|l| format!("{l}\n"))
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = s.run_with_stdin(&args, &input).await;
    (out, endpoint)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_autocompact_window_summarizes_before_the_turn() {
    let (out, endpoint) = run_with_window(None).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(
        endpoint.requests().await.len(),
        4,
        "the model's own window leaves this conversation alone"
    );

    let (out, endpoint) = run_with_window(Some(2_000)).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 5);
    assert_ne!(requests[3]["stream"], json!(true), "the summarizer call");
    assert!(requests[3].to_string().contains("<conversation>"));
    assert!(requests[4].to_string().contains("the summary"));
}

type ChildLines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

async fn event(lines: &mut ChildLines, stderr: &Arc<std::sync::Mutex<String>>) -> Value {
    let line = lines.next_line().await.unwrap().unwrap_or_else(|| {
        panic!(
            "the run ended before its result; stderr: {}",
            stderr.lock().unwrap()
        )
    });
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("`{line}`: {e}"))
}

/// The next stream-json event of `lines`, skipping everything before a message's result.
async fn result_event(lines: &mut ChildLines, stderr: &Arc<std::sync::Mutex<String>>) -> Value {
    loop {
        let event = event(lines, stderr).await;
        if event["type"] == "result" {
            return event;
        }
    }
}

async fn send_message(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut ChildLines,
    stderr: &Arc<std::sync::Mutex<String>>,
    text: &str,
) -> Value {
    use tokio::io::AsyncWriteExt;
    let line = format!(
        "{}\n",
        json!({"type": "user", "message": {"role": "user", "content": text}})
    );
    stdin.write_all(line.as_bytes()).await.unwrap();
    result_event(lines, stderr).await
}

/// A fold whose append landed but whose cut failed leaves the journal full beside a valid
/// fold mark. The pre-run fold must resolve that journal before an autocompaction rewrites
/// `main`: the mark's count check reads `main`'s message count, which the compaction
/// changes, so a fold that runs after it appends the landed turn a second time.
#[tokio::test(flavor = "multi_thread")]
async fn the_pending_fold_lands_before_autocompaction_rewrites_the_conversation() {
    let endpoint = Endpoint::start(
        Wire::Responses,
        vec![
            Turn::text("answer one"),
            Turn::text("the summary"),
            Turn::text("answer two"),
            Turn::text("the summary"),
            Turn::text("answer three"),
        ],
    )
    .await;
    let s = Sandbox::priced(&endpoint.url());
    let settings = s.work.join("window.toml");
    std::fs::write(&settings, "[context]\nautocompact_window = 200\n").unwrap();
    let settings = settings.display().to_string();
    let mut cmd = s.command(&[
        "-p",
        "--permission-mode",
        "manual",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--settings",
        &settings,
    ]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let stderr = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_sink = stderr.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = String::new();
        let _ = child_stderr.read_to_string(&mut buf).await;
        *stderr_sink.lock().unwrap() = buf;
    });

    let session = event(&mut lines, &stderr).await["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let logs = s.data.join("sessions").join(&session).join("logs");
    let big = "lorem ipsum dolor sit amet ".repeat(100);

    send_message(&mut stdin, &mut lines, &stderr, "first task").await;

    // An interrupted turn's journal holds its prompt alone, bigger than the compaction's
    // kept tail. Build it as a log of its own beside `main`: a header, then the first
    // message of the conversation reworded as the interrupted prompt. The next run's fold
    // appends it with the marker; the turn after that one reseeds the journal and a valid
    // fold mark, as a cut lost after the append would.
    let main = std::fs::read_to_string(logs.join("main.jsonl")).unwrap();
    let prompt = format!("second task {big}");
    let first_push: Value = serde_json::from_str(main.lines().nth(1).unwrap()).unwrap();
    let mut entry = first_push["entries"][0].clone();
    entry["message"]["content"][0]["text"] = json!(prompt);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let journal = format!(
        "{}\n{}\n",
        json!({"seq": 0, "n": 0, "t": now, "type": "header", "format": "rig-session/4", "log": "main.inflight", "lineage": 41u64}),
        json!({"seq": 1, "n": 1, "t": now, "type": "push", "entries": [entry]}),
    );
    std::fs::write(logs.join("main.inflight.jsonl"), &journal).unwrap();

    send_message(&mut stdin, &mut lines, &stderr, &format!("fold it {big}")).await;

    // The fold's append landed; only its cut is owed. `main` holds five messages: the
    // summary of the first exchange, the folded turn and its marker, and the turn that
    // triggered the fold.
    let journal_bytes = journal.len() as u64;
    std::fs::write(logs.join("main.inflight.jsonl"), &journal).unwrap();
    let files = s.data.join("sessions").join(&session).join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(
        files.join("main.inflight.fold"),
        json!({ "log_len": 3, "journal_bytes": journal_bytes, "appended": 2 }).to_string(),
    )
    .unwrap();

    send_message(&mut stdin, &mut lines, &stderr, "next").await;
    drop(stdin);
    let status = child.wait().await.unwrap();
    let stderr_text = stderr.lock().unwrap().clone();
    assert_eq!(status.code(), Some(0), "stderr: {stderr_text}");

    let requests = endpoint.requests().await;
    assert_eq!(requests.len(), 5, "{requests:?}");
    assert_ne!(
        requests[1]["stream"],
        json!(true),
        "the first summarizer call"
    );
    assert_ne!(
        requests[3]["stream"],
        json!(true),
        "the second summarizer call"
    );
    // The last turn reads the compacted conversation: the landed fold was cut, not folded
    // again, so neither the interrupted prompt nor its marker reaches the model twice.
    let last = requests[4].to_string();
    assert!(
        !last.contains("second task"),
        "the landed fold is not doubled by the post-compaction retry: {last}"
    );
    assert!(
        !last.contains("the session ended before this turn finished"),
        "the landed fold's marker is not doubled either: {last}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scratch_root_is_the_last_root_and_is_gone_after_the_run() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let init = &out.lines()[0];
    let roots = init["workspace_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 3);
    let scratch = Path::new(roots[2].as_str().unwrap());
    assert!(scratch.starts_with(s.scratch_parent()), "{scratch:?}");
    assert!(!scratch.exists());
    let left: Vec<_> = std::fs::read_dir(s.scratch_parent())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_memory_store_is_a_root_before_the_scratch_root_and_is_owner_only() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let init = &out.lines()[0];
    let roots = init["workspace_roots"].as_array().unwrap();
    let memory = Path::new(roots[1].as_str().unwrap());
    assert!(memory.starts_with(s.data.join("memory")), "{memory:?}");
    assert!(memory.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            memory.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_memory_off_in_the_user_file_adds_no_root_and_no_store() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let user_file = s.config.join("config.toml");
    let mut text = std::fs::read_to_string(&user_file).unwrap();
    text.push_str("\n[memory]\nauto = false\n");
    std::fs::write(&user_file, text).unwrap();
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let roots = out.lines()[0]["workspace_roots"].as_array().unwrap().len();
    assert_eq!(roots, 2);
    assert!(!s.data.join("memory").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_that_cannot_be_created_warns_and_leaves_the_roots_alone() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    std::fs::write(s.data.join("memory"), "in the way").unwrap();
    let out = s.headless(&["--output-format", "stream-json", "hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        out.stderr.contains("auto memory is off for this session"),
        "{}",
        out.stderr
    );
    assert_eq!(
        out.lines()[0]["workspace_roots"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_sweeps_the_scratch_directories_of_dead_sessions() {
    let endpoint = Endpoint::start(Wire::Chat, vec![Turn::text("done")]).await;
    let s = Sandbox::compatible(&endpoint.url());
    let parent = s.scratch_parent();
    fontaine_core::owner_only::create_dir(&parent).unwrap();
    std::fs::create_dir(parent.join("dead-session")).unwrap();
    std::fs::write(parent.join("dead-session").join("helper.sh"), "x").unwrap();
    std::fs::write(parent.join("dead-session.lock"), "").unwrap();
    let out = s.headless(&["hi"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!parent.join("dead-session").exists());
    assert!(!parent.join("dead-session.lock").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn each_model_call_emits_a_model_call_event_that_sums_to_the_ledger_totals() {
    let endpoint = Endpoint::start(
        Wire::Chat,
        vec![
            Turn::tool("call_1", "read", json!({"path": "note.txt"})),
            Turn::text("done"),
        ],
    )
    .await;
    let s = Sandbox::compatible(&endpoint.url());
    s.add_file("note.txt", "x\n");
    let out = s.headless(&["--output-format", "stream-json", "go"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let v = validator();
    for line in &lines {
        assert_valid(&v, line);
    }
    let calls: Vec<&Value> = lines.iter().filter(|l| l["type"] == "model_call").collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["calls_tool"], true);
    assert_eq!(calls[1]["calls_tool"], false);
    assert!(lines.iter().all(|l| l["schema_version"] == 1));
    let main = &result_of(&lines)["usage"]["main"];
    for field in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let sum: u64 = calls.iter().map(|c| c[field].as_u64().unwrap()).sum();
        assert_eq!(main[field].as_u64().unwrap(), sum, "{field}");
    }
}
