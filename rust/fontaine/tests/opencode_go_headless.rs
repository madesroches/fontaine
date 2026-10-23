//! `fontaine -p` on an `opencode-go` account against a scripted gateway: the stored sign-in,
//! the chat route and the session header travel through the real binary.

mod common;

use common::{Endpoint, Sandbox, Turn, Usage, Wire};

const SESSION: &str = "0b9c6f0e-6d2a-4d35-9a6e-1f0a2b3c4d5e";

#[tokio::test(flavor = "multi_thread")]
async fn a_go_turn_carries_the_session_the_user_agent_and_the_signed_in_token() {
    let endpoint = Endpoint::start_under(
        Wire::Chat,
        vec![Turn::text("Hello from Go")],
        Usage::default(),
        "/openai/v1",
    )
    .await;
    let s = Sandbox::new();
    s.user_config(&format!(
        "[accounts.go]\nkind = \"opencode-go\"\nbase_url = \"{}\"\nmodels = [\"kimi-k3\"]\n\n[model]\nmodel = \"go/kimi-k3\"\n",
        endpoint.url()
    ));
    common::write(
        &s.data.join("credentials.toml"),
        "[accounts.go.oauth]\naccess = \"at-from-signin\"\nrefresh = \"rt\"\nexpires_at = 4102444800\n",
    );

    let out = s.headless(&["--session-id", SESSION, "say hello"]).await;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert_eq!(out.stdout, "Hello from Go\n");

    let headers = endpoint.request_headers().await;
    assert_eq!(headers.len(), 1);
    let get = |name: &str| headers[0].get(name).and_then(|v| v.to_str().ok());
    assert_eq!(get("authorization"), Some("Bearer at-from-signin"));
    assert_eq!(get("x-opencode-session"), Some(SESSION));
    assert!(
        get("user-agent").is_some_and(|ua| ua.starts_with("fontaine/")),
        "{headers:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_with_a_go_main_model_is_a_usage_error() {
    let s = Sandbox::new();
    s.user_config(
        "[accounts.go]\nkind = \"opencode-go\"\nmodels = [\"kimi-k3\"]\n\n[model]\nmodel = \"go/kimi-k3\"\n",
    );
    common::write(
        &s.data.join("credentials.toml"),
        "[accounts.go.oauth]\naccess = \"at\"\nrefresh = \"rt\"\nexpires_at = 4102444800\n",
    );
    let out = s.headless(&["--max-budget-usd", "1", "hi"]).await;
    assert_eq!(out.code, Some(2), "{}", out.stderr);
    assert!(out.stderr.contains("--max-budget-usd"), "{}", out.stderr);
}
