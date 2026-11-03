mod common;

use common::Sandbox;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn serving(ids: &[&str]) -> MockServer {
    let server = MockServer::start().await;
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "x"}))
        .collect();
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data})),
        )
        .mount(&server)
        .await;
    server
}

fn sandbox(url: &str, model: &str) -> Sandbox {
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.live]\nkind = \"openai-compatible\"\nbase_url = \"{url}\"\n\n[model]\nmodel = \"{model}\"\n"
    ));
    s
}

#[tokio::test]
async fn lists_the_account_models_and_marks_the_main_one() {
    let server = serving(&["alpha", "beta"]).await;
    let s = sandbox(&server.uri(), "live/alpha");
    let out = s.run(&["models"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        out.stdout.contains("live (openai-compatible)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("alpha [main]"), "{}", out.stdout);
    assert!(out.stdout.contains("beta\n"), "{}", out.stdout);
}

#[tokio::test]
async fn json_output_has_the_documented_shape() {
    let server = serving(&["alpha", "beta"]).await;
    let s = sandbox(&server.uri(), "live/alpha");
    let out = s.run(&["models", "live", "--format=json"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let value: Value = serde_json::from_str(&out.stdout).unwrap();
    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["account"], "live");
    assert_eq!(accounts[0]["models"][0]["id"], "alpha");
    assert_eq!(accounts[0]["models"][0]["roles"][0], "main");
    assert_eq!(accounts[0]["models"][1]["roles"], json!([]));
}

#[tokio::test]
async fn the_model_flag_moves_the_main_mark() {
    let server = serving(&["alpha", "beta"]).await;
    let s = sandbox(&server.uri(), "live/alpha");
    let out = s.run(&["--model", "live/beta", "models"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("beta [main]"), "{}", out.stdout);
}

#[tokio::test]
async fn an_unknown_account_exits_2() {
    let server = serving(&["alpha"]).await;
    let s = sandbox(&server.uri(), "live/alpha");
    let out = s.run(&["models", "nope"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr.contains("`nope`") && out.stderr.contains("live"),
        "{}",
        out.stderr
    );
}

#[tokio::test]
async fn no_accounts_exits_2() {
    let s = Sandbox::new();
    s.user_config("");
    let out = s.run(&["models"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(
        out.stderr.contains("no accounts are configured"),
        "{}",
        out.stderr
    );
}

#[tokio::test]
async fn every_account_failing_exits_1_and_prints_the_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let s = sandbox(&server.uri(), "live/alpha");
    let out = s.run(&["models"]).await;
    assert_eq!(out.code, Some(1), "{}", out.stderr);
    assert!(
        out.stdout.contains("could not list models"),
        "{}",
        out.stdout
    );
}
