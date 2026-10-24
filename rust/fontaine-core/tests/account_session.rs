//! What the provider dialog's operations do to a running session: a credential stored or removed
//! reaches the main model and the guardian, an account added mid-session is listed, and an
//! account in use cannot be removed.

mod common;

use std::sync::Arc;

use common::World;
use common::runtime::build_with_models;
use fontaine_core::accounts::AccountsError;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::{Account, AccountKind, PermissionMode};
use fontaine_core::credentials;
use fontaine_core::models::{self, ResolveParams, Switcher};
use fontaine_guardian::{
    AdjudicationRequest, Decision, EnvironmentConfig, RuleConfig, SessionFacts, ToolCallSummary,
};
use rig_core::completion::CompletionRequest;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn gateway() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "m1", "object": "model", "created": 0, "owned_by": "x"}],
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m1",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "hello"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })))
        .mount(&server)
        .await;
    server
}

async fn session(w: &World) -> AgentRuntime {
    let config = w.load().unwrap();
    let env: fontaine_core::agent::EnvLookup = Arc::new(|_| None);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &*env,
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap();
    let switcher = Switcher::new(config, w.data.clone(), env, None);
    build_with_models(
        w,
        PermissionMode::Manual,
        resolved,
        Some(switcher),
        false,
        Default::default(),
        None,
    )
}

fn bearer_of_last_completion(requests: &[wiremock::Request]) -> Option<String> {
    requests
        .iter()
        .rev()
        .find(|r| r.url.path() == "/chat/completions")
        .and_then(|r| r.headers.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

#[tokio::test]
async fn a_key_stored_for_the_main_models_account_reaches_the_live_model() {
    let server = gateway().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    let mut runtime = session(&w).await;
    runtime
        .models
        .main
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    assert_eq!(
        bearer_of_last_completion(&server.received_requests().await.unwrap()),
        None
    );

    credentials::store(&w.data, "main", "sk-new").unwrap();
    let effect = runtime.credentials_changed("main").await;
    assert!(effect.main_rebuilt, "{effect:?}");
    assert_eq!(effect.main_error, None);

    runtime
        .models
        .main
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    assert_eq!(
        bearer_of_last_completion(&server.received_requests().await.unwrap()).as_deref(),
        Some("Bearer sk-new")
    );
}

#[tokio::test]
async fn a_credential_removed_from_the_main_account_keeps_the_live_client_and_says_so() {
    let server = gateway().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"anthropic\"\nbase_url = \"{}\"\nmodels = [\"m1\"]\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    credentials::store(&w.data, "main", "sk-old").unwrap();
    let mut runtime = session(&w).await;
    credentials::remove(&w.data, "main").unwrap();
    let effect = runtime.credentials_changed("main").await;
    assert!(!effect.main_rebuilt);
    assert!(
        effect
            .main_error
            .as_deref()
            .is_some_and(|e| e.contains("/login main")),
        "{effect:?}"
    );
    assert_eq!(runtime.models.main.reference.to_string(), "main/m1");
}

fn request() -> AdjudicationRequest {
    AdjudicationRequest {
        task_intent: "Run the tests".into(),
        recent_user_messages: vec!["Run the tests".into()],
        project_instructions: None,
        environment: EnvironmentConfig::expand(&[]),
        rules: RuleConfig::expand(&[], &[], &[]),
        tool_call: ToolCallSummary {
            tool: "shell".into(),
            args_digest: "digest".into(),
            args: json!({"command": "cargo test"}),
            destructive: false,
            annotations: None,
        },
        session_facts: SessionFacts::default(),
        hook_context: vec![],
        prior_denials: vec![],
    }
}

#[tokio::test]
async fn a_guardian_whose_key_is_removed_is_unavailable_and_denies() {
    let server = gateway().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"{uri}\"\n\n[accounts.judge]\nkind = \"anthropic\"\nbase_url = \"{uri}\"\nmodels = [\"g1\"]\n\n[model]\nmodel = \"main/m1\"\n\n[guardian]\nmodel = \"judge/g1\"\n",
        uri = server.uri()
    ));
    credentials::store(&w.data, "judge", "sk-judge").unwrap();
    let mut runtime = session(&w).await;
    assert_eq!(runtime.state.guardian().unavailable_reason(), None);

    assert!(credentials::remove(&w.data, "judge").unwrap());
    let effect = runtime.credentials_changed("judge").await;
    assert_eq!(effect.guardian_ready, Some(false));
    assert!(!effect.main_rebuilt);

    let guardian = runtime.state.guardian();
    assert!(guardian.unavailable_reason().is_some());
    let adjudication = guardian.adjudicate(&request()).await;
    assert_eq!(adjudication.decision, Decision::Deny);
    assert!(adjudication.fail_closed.is_some());

    credentials::store(&w.data, "judge", "sk-judge").unwrap();
    let effect = runtime.credentials_changed("judge").await;
    assert_eq!(effect.guardian_ready, Some(true));
    assert_eq!(runtime.state.guardian().unavailable_reason(), None);
}

fn late_account() -> Account {
    Account {
        kind: AccountKind::named("openai-compatible"),
        api_key_env: None,
        base_url: Some("http://127.0.0.1:9".to_string()),
        models: Some(vec!["z1".to_string()]),
        wires: None,
    }
}

#[tokio::test]
async fn an_account_added_mid_session_is_listed_and_a_removed_one_is_dropped() {
    let server = gateway().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    let mut runtime = session(&w).await;
    let first = runtime.model_listings(None).await.unwrap();
    assert_eq!(first.len(), 1);

    runtime.account_added("late", late_account());
    let all = runtime.model_listings(None).await.unwrap();
    let names: Vec<&str> = all.iter().map(|l| l.account.as_str()).collect();
    assert_eq!(names, ["late", "main"]);

    let only = runtime.model_listings(Some("late")).await.unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].models, Ok(vec!["z1".to_string()]));

    runtime.account_removed("late");
    assert!(!runtime.config.accounts.contains_key("late"));
    let after = runtime.model_listings(None).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].account, "main");
}

#[tokio::test]
async fn an_account_in_use_cannot_be_removed() {
    let server = gateway().await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"{uri}\"\n\n[accounts.judge]\nkind = \"openai-compatible\"\nbase_url = \"{uri}\"\nmodels = [\"g1\"]\n\n[accounts.eyes]\nkind = \"ollama\"\n\n[accounts.spare]\nkind = \"ollama\"\n\n[model]\nmodel = \"main/m1\"\n\n[guardian]\nmodel = \"judge/g1\"\n\n[guardian.screen]\nmodel = \"eyes/screen\"\n",
        uri = server.uri()
    ));
    let runtime = session(&w).await;
    assert!(matches!(
        runtime.removable("main"),
        Err(AccountsError::InUse {
            by: "the main model"
        })
    ));
    assert!(matches!(
        runtime.removable("judge"),
        Err(AccountsError::InUse { by: "the guardian" })
    ));
    assert!(matches!(
        runtime.removable("eyes"),
        Err(AccountsError::InUse { .. })
    ));
    assert!(runtime.removable("spare").is_ok());
}
