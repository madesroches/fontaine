//! Full runs against recorded provider traffic. Replay needs no keys; recording is a human
//! step, one test at a time, with the key exported into that command only:
//! `OPENROUTER_API_KEY=... RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes -- --ignored <name>`.

mod common;

use std::path::PathBuf;

use common::Sandbox;
use rig_cassette::http::{CassetteSpec, ProviderCassette};
use serde_json::{Value, json};

const UPSTREAM: &str = "https://openrouter.ai/api/v1";
const KEY_ENV: &str = "OPENROUTER_API_KEY";
const PROVIDER: &str = "openrouter";
const MODEL: &str = "openai/gpt-4o-mini";
/// gpt-4o-mini misapplied the destroy rule to a new file, so the guardian uses a model the evals measured.
const GUARDIAN_MODEL: &str = "anthropic/claude-haiku-5.5";

/// Request bodies embed the per-run sandbox path, which the cassette scrubber leaves alone
/// under `/tmp`, so bodies are matched by shape.
fn spec(scenario: &'static str) -> CassetteSpec {
    CassetteSpec::new(scenario).shape_matched()
}

fn account_config(cassette: &ProviderCassette, extra: &str) -> String {
    format!(
        "[accounts.paid]\nkind = \"openrouter\"\nbase_url = \"{}\"\napi_key_env = \"{KEY_ENV}\"\nmodels = [\"{MODEL}\", \"{GUARDIAN_MODEL}\"]\n\n[model]\nmodel = \"paid/{MODEL}\"\n{extra}",
        cassette.base_url()
    )
}

/// Every recorded request body, concatenated. Responses are SSE streams, so they are not
/// parsed as JSON.
fn recorded_requests(scenario: &str) -> String {
    rig_cassette::http::recorded_interaction_bodies(&root(), PROVIDER, scenario)
        .into_iter()
        .map(|(request, _)| request)
        .collect()
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cassettes")
}

/// Prompt, stream, a read tool call executed on the fast path, result, answer.
#[tokio::test(flavor = "multi_thread")]
async fn manual_mode_reads_a_file_through_the_fast_path() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("manual_read_round_trip"), UPSTREAM).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(&cassette, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    s.add_file("note.txt", "the magic word is plugh\n");

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read note.txt and tell me the magic word.",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("the read call was adjudicated");
    assert_eq!(decision["tier"], "read_only_fast_path");
    assert_eq!(decision["decision"], "allow");
    let result = lines.iter().find(|l| l["type"] == "tool_result").unwrap();
    assert_eq!(result["success"], json!(true));
    let last: &Value = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(last["result"].as_str().unwrap().contains("plugh"));
    cassette.finish().await;
}

/// Auto mode end to end: tool call, the guardian's verdict, the tool runs, the answer. The agent
/// and the guardian share the one recorded account.
#[tokio::test(flavor = "multi_thread")]
async fn auto_mode_has_the_guardian_allow_a_harmless_command() {
    let cassette = ProviderCassette::start(
        &root(),
        PROVIDER,
        spec("auto_guardian_round_trip"),
        UPSTREAM,
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(
        &cassette,
        &format!("\n[guardian]\nmodel = \"paid/{GUARDIAN_MODEL}\"\n"),
    ));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));

    let out = s
        .run(&[
            "-p",
            "--permission-mode",
            "auto",
            "--output-format",
            "stream-json",
            "Use the shell tool to run exactly `echo hello > hello.txt`, then say done.",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let lines = out.lines();
    let session = lines[0]["session_id"].as_str().unwrap();
    let audit = std::fs::read_to_string(s.data.join("audit").join(format!("{session}.jsonl")))
        .expect("an audit log");
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("the shell call was adjudicated");
    let why = format!(
        "gate_decision: {decision}\naudit: {audit}\nstderr: {}",
        out.stderr
    );
    assert_eq!(decision["tier"], "guardian", "{why}");
    assert_eq!(decision["decision"], "allow", "{why}");
    assert_eq!(decision["rule"], "allow", "{why}");
    assert_eq!(decision["guardian_model"], format!("paid/{GUARDIAN_MODEL}"));
    assert!(s.work.join("hello.txt").exists());

    // The audit record carries the verdict.
    let record: Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(record["tier"], "guardian");
    assert_eq!(record["guardian_verdict"], "allow");
    assert!(record["confidence"].as_f64().unwrap() > 0.0);

    let last: &Value = lines.iter().rev().find(|l| l["type"] == "result").unwrap();
    assert!(
        last["result"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("done")
    );
    cassette.finish().await;
}

/// A 1x1 PNG, as base64; the magic bytes are what `read`'s sniff recognizes.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// `read` of an image puts the base64 image part on the provider request: this replays a
/// recorded run against a vision model and inspects the recorded traffic, which unit tests
/// cannot see because they never build a provider request.
#[tokio::test(flavor = "multi_thread")]
async fn reading_an_image_sends_the_image_part_to_a_vision_model() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("read_png_image_part"), UPSTREAM).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(&cassette, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(PNG_B64)
        .unwrap();
    std::fs::write(s.work.join("pixel.png"), png).unwrap();

    let out = s
        .headless(&[
            "--output-format",
            "stream-json",
            "Read pixel.png and tell me what you see.",
        ])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    cassette.finish().await;

    // The recorded request that carried the tool result has the image part on the wire.
    let requests = recorded_requests("read_png_image_part");
    assert!(
        requests.contains(PNG_B64),
        "the base64 image part reaches the provider"
    );
}

/// `read` of a PDF never sends the file: the recorded run's requests carry the text stub
/// and no document part.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "cassette not recorded: the model called read_mcp_resource, not read"]
async fn reading_a_pdf_replays_the_text_stub_fallback() {
    let cassette =
        ProviderCassette::start(&root(), PROVIDER, spec("read_pdf_text_stub"), UPSTREAM).await;
    let mut s = Sandbox::new();
    s.user_config(&account_config(&cassette, ""));
    s.set_env(KEY_ENV, &cassette.api_key(KEY_ENV));
    std::fs::write(s.work.join("notes.pdf"), b"%PDF-1.7\nnot really a pdf").unwrap();

    let out = s
        .headless(&["--output-format", "stream-json", "Read notes.pdf."])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    cassette.finish().await;

    let requests = recorded_requests("read_pdf_text_stub");
    assert!(requests.contains("is a PDF"), "the stub names the file");
    assert!(!requests.contains("JVBER"), "no PDF bytes on the wire");
}
