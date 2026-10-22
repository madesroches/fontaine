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
