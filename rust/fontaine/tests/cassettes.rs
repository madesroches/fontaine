//! Full runs against recorded provider traffic. Replay needs no keys; recording is a human
//! step: `RIG_PROVIDER_TEST_MODE=record cargo test -p fontaine --test cassettes -- --ignored`
//! with the provider key in `OPENAI_API_KEY`.

mod common;

use std::path::PathBuf;

use common::Sandbox;
use rig_cassette::http::ProviderCassette;
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("cassettes")
}

/// Prompt, stream, a read tool call executed on the fast path, result, answer.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "cassette not recorded"]
async fn manual_mode_reads_a_file_through_the_fast_path() {
    let cassette = ProviderCassette::start(
        &root(),
        "openai",
        "manual_read_round_trip",
        "https://api.openai.com/v1",
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"OPENAI_API_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        cassette.base_url()
    ));
    s.set_env("OPENAI_API_KEY", &cassette.api_key("OPENAI_API_KEY"));
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
#[ignore = "cassette not recorded"]
async fn auto_mode_has_the_guardian_allow_a_harmless_command() {
    let cassette = ProviderCassette::start(
        &root(),
        "openai",
        "auto_guardian_round_trip",
        "https://api.openai.com/v1",
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"OPENAI_API_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n\n[guardian]\nmodel = \"paid/gpt-4o-mini\"\n",
        cassette.base_url()
    ));
    s.set_env("OPENAI_API_KEY", &cassette.api_key("OPENAI_API_KEY"));

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
    let decision = lines
        .iter()
        .find(|l| l["type"] == "gate_decision")
        .expect("the shell call was adjudicated");
    assert_eq!(decision["tier"], "guardian");
    assert_eq!(decision["decision"], "allow");
    assert_eq!(decision["rule"], "allow");
    assert_eq!(decision["guardian_model"], "paid/gpt-4o-mini");
    assert!(s.work.join("hello.txt").exists());

    // The audit record carries the verdict.
    let session = lines[0]["session_id"].as_str().unwrap();
    let audit = std::fs::read_to_string(s.data.join("audit").join(format!("{session}.jsonl")))
        .expect("an audit log");
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
#[ignore = "cassette not recorded"]
async fn reading_an_image_sends_the_image_part_to_a_vision_model() {
    let cassette = ProviderCassette::start(
        &root(),
        "openai",
        "read_png_image_part",
        "https://api.openai.com/v1",
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"OPENAI_API_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        cassette.base_url()
    ));
    s.set_env("OPENAI_API_KEY", &cassette.api_key("OPENAI_API_KEY"));
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
    let turns = rig_cassette::http::recorded_json_turns(&root(), "openai", "read_png_image_part");
    let requests: String = turns
        .iter()
        .map(|(request, _)| request.to_string())
        .collect();
    assert!(
        requests.contains(PNG_B64),
        "the base64 image part reaches the provider"
    );
}

/// `read` of a PDF never sends the file: the recorded run's requests carry the text stub
/// and no document part.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "cassette not recorded"]
async fn reading_a_pdf_replays_the_text_stub_fallback() {
    let cassette = ProviderCassette::start(
        &root(),
        "openai",
        "read_pdf_text_stub",
        "https://api.openai.com/v1",
    )
    .await;
    let mut s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"OPENAI_API_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        cassette.base_url()
    ));
    s.set_env("OPENAI_API_KEY", &cassette.api_key("OPENAI_API_KEY"));
    std::fs::write(s.work.join("notes.pdf"), b"%PDF-1.7\nnot really a pdf").unwrap();

    let out = s
        .headless(&["--output-format", "stream-json", "Read notes.pdf."])
        .await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    cassette.finish().await;

    let turns = rig_cassette::http::recorded_json_turns(&root(), "openai", "read_pdf_text_stub");
    let requests: String = turns
        .iter()
        .map(|(request, _)| request.to_string())
        .collect();
    assert!(requests.contains("is a PDF"), "the stub names the file");
    assert!(!requests.contains("JVBER"), "no PDF bytes on the wire");
}
