//! `<account>/<model>` resolution: unknown accounts and models, discovery for accounts without
//! a `models` list, routing between accounts of one kind, and where credentials come from.

mod common;

use std::collections::HashMap;
use std::ffi::OsString;

use common::{World, write};
use fontaine_core::models::{self, GuardianModel, ModelError, Models, ResolveParams};
use rig_core::completion::CompletionRequest;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).map(OsString::from)
}

async fn resolve(
    world: &World,
    env: &dyn Fn(&str) -> Option<OsString>,
    model_override: Option<&str>,
) -> Result<Models, ModelError> {
    let config = world.load().unwrap();
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &world.data,
        env,
        model_override,
    })
    .await
}

fn message(error: &ModelError) -> String {
    error.to_string()
}

async fn models_route(server: &MockServer, status: u16, ids: &[&str]) {
    let body = json!({
        "object": "list",
        "data": ids.iter().map(|id| json!({"id": id, "object": "model", "created": 1,
                                           "owned_by": "x"})).collect::<Vec<_>>(),
    });
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn an_unknown_account_is_a_config_error_naming_the_known_ones() {
    let w = World::new();
    w.user_file(
        "[accounts.work]\nkind = \"ollama\"\nmodels = [\"m\"]\n\n[model]\nmodel = \"work/m\"\n",
    );
    let err = resolve(&w, &env_of(&[]), Some("home/m")).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("`home`"), "{err}");
    assert!(message(&err).contains("work"), "{err}");
}

#[tokio::test]
async fn an_unknown_model_in_an_explicit_list_is_a_config_error() {
    let w = World::new();
    w.user_file(
        "[accounts.work]\nkind = \"ollama\"\nmodels = [\"alpha\", \"beta\"]\n\n[model]\nmodel = \"work/gamma\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("gamma"), "{err}");
    assert!(message(&err).contains("alpha"), "{err}");
}

#[tokio::test]
async fn no_main_model_names_both_ways_to_set_one() {
    let w = World::new();
    w.user_file("[accounts.work]\nkind = \"ollama\"\n");
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("[model] model"), "{err}");
    assert!(message(&err).contains("--model"), "{err}");
}

#[tokio::test]
async fn an_account_without_models_is_listed_once_and_the_model_checked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "alpha", "object": "model", "created": 1, "owned_by": "x"},
                     {"id": "beta", "object": "model", "created": 1, "owned_by": "x"}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    // The guardian shares the account, so it is checked against the same single listing.
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"gw/alpha\"\n\n[guardian]\nmodel = \"gw/beta\"\n",
        server.uri()
    ));
    let resolved = resolve(&w, &env_of(&[]), None).await.unwrap();
    assert_eq!(resolved.main.reference.model, "alpha");
    assert!(matches!(resolved.guardian, Some(GuardianModel::Ready(_))));
}

#[tokio::test]
async fn a_listing_that_lacks_the_model_is_a_config_error() {
    let server = MockServer::start().await;
    models_route(&server, 200, &["alpha"]).await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"gw/missing\"\n",
        server.uri()
    ));
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("missing"), "{err}");
}

#[tokio::test]
async fn a_failed_listing_is_a_provider_failure() {
    for status in [500u16, 401, 403] {
        let server = MockServer::start().await;
        models_route(&server, status, &[]).await;
        let w = World::new();
        w.user_file(&format!(
            "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"gw/alpha\"\n",
            server.uri()
        ));
        let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
        assert_eq!(err.exit_code(), 1, "status {status}: {err}");
    }
}

#[tokio::test]
async fn ollama_models_are_listed_from_the_native_tags_route() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {"name": "qwen3:4b", "model": "qwen3:4b"},
                {"name": "llama3.2:latest", "model": "llama3.2:latest"},
            ],
        })))
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.local]\nkind = \"ollama\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"local/qwen3:4b\"\n",
        server.uri()
    ));
    let resolved = resolve(&w, &env_of(&[]), None).await.unwrap();
    assert_eq!(resolved.main.reference.model, "qwen3:4b");

    // A bare name means the `latest` tag.
    let resolved = resolve(&w, &env_of(&[]), Some("local/llama3.2"))
        .await
        .unwrap();
    assert_eq!(resolved.main.reference.model, "llama3.2");

    let err = resolve(&w, &env_of(&[]), Some("local/nothing"))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
}

#[tokio::test]
async fn two_accounts_of_one_kind_reach_their_own_endpoints_with_their_own_keys() {
    async fn endpoint(key: &str, answer: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", key))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "m", "type": "message", "role": "assistant", "model": "claude-haiku-4-5",
                "content": [{"type": "text", "text": answer}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1},
            })))
            .expect(1)
            .mount(&server)
            .await;
        server
    }
    let work = endpoint("work-key", "work answer").await;
    let personal = endpoint("personal-key", "personal answer").await;

    let w = World::new();
    w.user_file(&format!(
        "[accounts.work]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"WORK_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[accounts.personal]\nkind = \"anthropic\"\nbase_url = \"{}\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
        work.uri(),
        personal.uri()
    ));
    // The personal account's key comes from the stored-credentials file.
    write(
        &w.data.join("credentials.toml"),
        "[accounts.personal]\napi_key = \"personal-key\"\n",
    );
    let env = env_of(&[("WORK_KEY", "work-key")]);

    let main = resolve(&w, &env, None).await.unwrap().main;
    let other = resolve(&w, &env, Some("personal/claude-haiku-4-5"))
        .await
        .unwrap()
        .main;
    let work_text = main
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    let personal_text = other
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    assert_eq!(work_text.text(), "work answer");
    assert_eq!(personal_text.text(), "personal answer");
}

#[tokio::test]
async fn a_main_account_that_needs_a_key_and_has_none_is_a_config_error() {
    let w = World::new();
    w.user_file(
        "[accounts.work]\nkind = \"anthropic\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("fontaine auth login work"), "{err}");

    // A named variable that is unset or empty is just as missing.
    w.user_file(
        "[accounts.work]\nkind = \"anthropic\"\napi_key_env = \"WORK_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
    );
    for env in [env_of(&[]), env_of(&[("WORK_KEY", "")])] {
        let err = resolve(&w, &env, None).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(message(&err).contains("WORK_KEY"), "{err}");
    }

    // A stored key is not consulted when `api_key_env` names a source.
    write(
        &w.data.join("credentials.toml"),
        "[accounts.work]\napi_key = \"stored\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
}

#[tokio::test]
async fn a_named_variable_that_is_unset_fails_even_for_a_keyless_kind() {
    let w = World::new();
    w.user_file(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:1\"\napi_key_env = \"GW_KEY\"\nmodels = [\"m\"]\n\n[model]\nmodel = \"gw/m\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("GW_KEY"), "{err}");
    assert!(resolve(&w, &env_of(&[("GW_KEY", "k")]), None).await.is_ok());
}

#[tokio::test]
async fn a_stored_credential_satisfies_an_account_with_no_env_source() {
    let w = World::new();
    w.user_file(
        "[accounts.work]\nkind = \"anthropic\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
    );
    write(
        &w.data.join("credentials.toml"),
        "[accounts.work]\napi_key = \"stored\"\n",
    );
    assert!(resolve(&w, &env_of(&[]), None).await.is_ok());
}

#[tokio::test]
async fn a_guardian_account_with_no_models_and_no_credential_still_starts() {
    let main = MockServer::start().await;
    let guardian = MockServer::start().await;
    // Any request to the guardian's endpoint would be a listing call made with its credentials.
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&guardian)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"alpha\"]\n\n[accounts.safety]\nkind = \"anthropic\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"main/alpha\"\n\n[guardian]\nmodel = \"safety/claude-haiku-4-5\"\n",
        main.uri(),
        guardian.uri()
    ));
    let resolved = resolve(&w, &env_of(&[]), None).await.unwrap();
    let Some(GuardianModel::Unavailable { reference, reason }) = resolved.guardian else {
        panic!("the guardian is unavailable");
    };
    assert_eq!(reference.to_string(), "safety/claude-haiku-4-5");
    assert!(reason.contains("safety"), "{reason}");
}

#[tokio::test]
async fn a_guardian_model_naming_an_unknown_account_or_model_is_a_config_error() {
    let w = World::new();
    w.user_file(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"alpha\"]\n\n[model]\nmodel = \"main/alpha\"\n\n[guardian]\nmodel = \"ghost/alpha\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("ghost"), "{err}");

    w.user_file(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"alpha\"]\n\n[model]\nmodel = \"main/alpha\"\n\n[guardian]\nmodel = \"main/zeta\"\n",
    );
    let err = resolve(&w, &env_of(&[]), None).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(message(&err).contains("zeta"), "{err}");
}
