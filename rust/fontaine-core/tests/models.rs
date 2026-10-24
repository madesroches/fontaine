//! Config accounts to rig provider clients, one smoke test per provider wire format against a
//! `wiremock` endpoint, and the handle-level behavior around them.

mod common;

use std::collections::HashMap;
use std::ffi::OsString;

use common::World;
use fontaine_core::config::AccountKind;
use fontaine_core::models::{
    self, GuardianChoice, GuardianModel, ModelHandle, ModelRef, ResolveParams, Switcher,
};
use fontaine_guardian::models::ModelEntry;
use rig_core::completion::CompletionRequest;
use rig_core::test_utils::MockCompletionModel;
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

async fn resolve_main(world: &World, env: &dyn Fn(&str) -> Option<OsString>) -> ModelHandle {
    let config = world.load().unwrap();
    models::resolve(&ResolveParams {
        config: &config,
        data_dir: &world.data,
        env,
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap()
    .main
}

async fn say_hi(handle: &ModelHandle) -> String {
    handle
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap()
        .text()
}

#[tokio::test]
async fn anthropic_uses_the_messages_wire_and_the_env_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "key-from-env"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-4-5",
            "content": [{"type": "text", "text": "hi from anthropic"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 4},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.work]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"WORK_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"work/claude-haiku-4-5\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("WORK_KEY", "key-from-env")])).await;
    assert_eq!(handle.kind, AccountKind::named("anthropic"));
    assert_eq!(handle.reference.to_string(), "work/claude-haiku-4-5");
    assert_eq!(say_hi(&handle).await, "hi from anthropic");
}

#[tokio::test]
async fn openai_uses_the_responses_wire_and_a_bearer_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer stored-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                        "status": "completed",
                        "content": [{"type": "output_text", "text": "hi from openai",
                                     "annotations": []}]}],
            "usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai\"\nbase_url = \"{}\"\nmodels = [\"gpt-4o-mini\"]\n\n[model]\nmodel = \"main/gpt-4o-mini\"\n",
        server.uri()
    ));
    // No api_key_env: the key comes from the stored-credentials file.
    common::write(
        &w.data.join("credentials.toml"),
        "[accounts.main]\napi_key = \"stored-key\"\n",
    );
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::named("openai"));
    assert!(handle.priced(), "gpt-4o-mini has a catalog price");
    assert_eq!(say_hi(&handle).await, "hi from openai");
}

#[tokio::test]
async fn gemini_uses_generate_content_with_the_key_in_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:generateContent"))
        .and(wiremock::matchers::query_param("key", "gem-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "hi from gemini"}]},
                            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 2,
                              "totalTokenCount": 3},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.g]\nkind = \"gemini\"\nbase_url = \"{}\"\napi_key_env = \"GEM\"\nmodels = [\"gemini-2.5-flash\"]\n\n[model]\nmodel = \"g/gemini-2.5-flash\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("GEM", "gem-key")])).await;
    assert_eq!(handle.kind, AccountKind::named("gemini"));
    assert_eq!(say_hi(&handle).await, "hi from gemini");
}

#[tokio::test]
async fn ollama_uses_the_native_chat_route_without_a_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:4b", "created_at": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": "hi from ollama"},
            "done": true, "done_reason": "stop",
            "prompt_eval_count": 1, "eval_count": 2,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.local]\nkind = \"ollama\"\nbase_url = \"{}\"\nmodels = [\"qwen3:4b\"]\n\n[model]\nmodel = \"local/qwen3:4b\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::named("ollama"));
    assert_eq!(say_hi(&handle).await, "hi from ollama");
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests[0].headers.contains_key("authorization"),
        "an Ollama account with no credential source is unauthenticated"
    );
}

/// One `/api/chat` call against an Ollama account whose settings are `account_extra`, returning
/// the body the daemon received and the handle.
async fn ollama_request(model: &str, account_extra: &str) -> (serde_json::Value, ModelHandle) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": model, "created_at": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": "hi"},
            "done": true, "done_reason": "stop",
        })))
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.local]\nkind = \"ollama\"\nbase_url = \"{}\"\nmodels = [\"{model}\"]\n{account_extra}\n[model]\nmodel = \"local/{model}\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[])).await;
    say_hi(&handle).await;
    let requests = server.received_requests().await.unwrap();
    let chat = requests
        .iter()
        .find(|r| r.url.path() == "/api/chat")
        .unwrap();
    (serde_json::from_slice(&chat.body).unwrap(), handle)
}

#[tokio::test]
async fn ollama_requests_carry_the_default_window_and_refuse_truncation() {
    let (body, _) = ollama_request("qwen3:4b", "").await;
    assert_eq!(body["options"]["num_ctx"], json!(16384));
    assert_eq!(body["truncate"], json!(false));
    assert!(
        body.get("think").is_none(),
        "unset leaves the model's default: {body}"
    );
}

#[tokio::test]
async fn ollama_requests_carry_the_configured_window() {
    let (body, _) = ollama_request("qwen3:4b", "context_tokens = 32768\n").await;
    assert_eq!(body["options"]["num_ctx"], json!(32768));
    assert_eq!(body["truncate"], json!(false));
}

#[tokio::test]
async fn ollama_think_false_turns_reasoning_off() {
    let (body, _) = ollama_request("qwen3:4b", "think = false\n").await;
    assert_eq!(body["think"], json!(false));
    let (body, _) = ollama_request("qwen3:4b", "think = true\n").await;
    assert!(body.get("think").is_none(), "{body}");
}

#[tokio::test]
async fn a_catalog_listed_ollama_model_reports_the_configured_window() {
    let (_, handle) = ollama_request("gpt-oss:20b", "context_tokens = 8192\n").await;
    let spec = handle
        .completion
        .spec()
        .expect("rig's catalog lists gpt-oss:20b");
    assert_eq!(spec.context_window, Some(8192));
    let (_, handle) = ollama_request("gpt-oss:20b", "").await;
    assert_eq!(
        handle.completion.spec().unwrap().context_window,
        Some(16384)
    );
}

#[tokio::test]
async fn an_unlisted_ollama_model_has_no_spec() {
    let (_, handle) = ollama_request("my-finetune:1b", "").await;
    assert!(handle.completion.spec().is_none());
}

#[tokio::test]
async fn openai_compatible_uses_chat_completions_and_is_unpriced() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "hi from a gateway"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"openai/gpt-4o-mini\"]\n\n[model]\nmodel = \"gw/openai/gpt-4o-mini\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[])).await;
    assert_eq!(handle.kind, AccountKind::named("openai-compatible"));
    // The model id keeps its own slash: the account is whatever precedes the first one.
    assert_eq!(handle.reference.account, "gw");
    assert_eq!(handle.reference.model, "openai/gpt-4o-mini");
    assert!(
        !handle.priced(),
        "the gateway dialect is priced by its kind name, which the catalog lacks"
    );
    assert_eq!(say_hi(&handle).await, "hi from a gateway");
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests[0].headers.contains_key("authorization"),
        "no credential source, no Authorization header"
    );
}

#[tokio::test]
async fn an_openai_compatible_key_is_sent_as_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer gw-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "ok"}}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.gw]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\napi_key_env = \"GW\"\nmodels = [\"m\"]\n\n[model]\nmodel = \"gw/m\"\n",
        server.uri()
    ));
    let handle = resolve_main(&w, &env_of(&[("GW", "gw-key")])).await;
    assert_eq!(say_hi(&handle).await, "ok");
}

#[tokio::test]
async fn the_guardian_model_may_sit_on_another_account() {
    let w = World::new();
    w.user_file(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"qwen3:4b\"]\n\n[accounts.safety]\nkind = \"anthropic\"\napi_key_env = \"SAFETY_KEY\"\nmodels = [\"claude-haiku-4-5\"]\n\n[model]\nmodel = \"main/qwen3:4b\"\n\n[guardian]\nmodel = \"safety/claude-haiku-4-5\"\n",
    );
    let config = w.load().unwrap();
    let env = env_of(&[("SAFETY_KEY", "k")]);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &env,
        model_override: None,
        session_id: None,
    })
    .await
    .unwrap();
    assert_eq!(resolved.main.kind, AccountKind::named("ollama"));
    let Some(GuardianModel::Ready(guardian)) = resolved.guardian else {
        panic!("the guardian is ready: {:?}", resolved.guardian);
    };
    assert_eq!(guardian.kind, AccountKind::named("anthropic"));
    assert_eq!(guardian.reference.to_string(), "safety/claude-haiku-4-5");
}

fn entry(kind: &str, model: &str, catch: f64) -> ModelEntry {
    ModelEntry {
        kind: kind.into(),
        model: model.into(),
        catch_rate: catch,
        false_deny_rate: 0.01,
        p95_ms: 900,
        scored_on: "2026-01-01".into(),
        dataset_version: "abc".into(),
    }
}

async fn resolve_listed(
    w: &World,
    env: &dyn Fn(&str) -> Option<OsString>,
    list: &[ModelEntry],
) -> models::Models {
    let config = w.load().unwrap();
    models::resolve_with(
        &ResolveParams {
            config: &config,
            data_dir: &w.data,
            env,
            model_override: None,
            session_id: None,
        },
        list,
    )
    .await
    .unwrap()
}

const MAIN: &str = "[accounts.main]\nkind = \"ollama\"\nmodels = [\"qwen3:4b\"]\n\n[model]\nmodel = \"main/qwen3:4b\"\n";

#[tokio::test]
async fn an_empty_list_means_the_main_model_judges_with_a_one_time_notice() {
    let w = World::new();
    w.user_file(MAIN);
    for resolved in [
        resolve_listed(&w, &env_of(&[]), &[]).await,
        // The shipped list is empty until a scorecard run fills it.
        {
            let config = w.load().unwrap();
            models::resolve(&ResolveParams {
                config: &config,
                data_dir: &w.data,
                env: &env_of(&[]),
                model_override: None,
                session_id: None,
            })
            .await
            .unwrap()
        },
    ] {
        assert_eq!(resolved.guardian_choice, GuardianChoice::MainFallback);
        let Some(GuardianModel::Ready(guardian)) = resolved.guardian else {
            panic!("the main model is the guardian");
        };
        assert_eq!(guardian.reference.to_string(), "main/qwen3:4b");
        assert_eq!(resolved.notices.len(), 1);
        assert!(resolved.notices[0].contains("main/qwen3:4b"));
    }
}

#[tokio::test]
async fn a_list_with_no_match_on_any_account_falls_back_to_the_main_model() {
    let w = World::new();
    w.user_file(MAIN);
    let list = [entry("anthropic", "claude-haiku-5-5", 0.99)];
    let resolved = resolve_listed(&w, &env_of(&[]), &list).await;
    assert_eq!(resolved.guardian_choice, GuardianChoice::MainFallback);
    assert_eq!(resolved.notices.len(), 1);
}

#[tokio::test]
async fn the_top_entry_on_an_unconfigured_account_is_skipped_for_the_next() {
    let w = World::new();
    w.user_file(&format!(
        "{MAIN}\n[accounts.cloud]\nkind = \"anthropic\"\napi_key_env = \"CLOUD_KEY\"\n\n[accounts.oa]\nkind = \"openai\"\nmodels = [\"gpt-flash\"]\napi_key_env = \"OA_KEY\"\n"
    ));
    // `cloud` holds no credential in this environment, so its entry cannot be used.
    let list = [
        entry("anthropic", "claude-haiku-5-5", 0.99),
        entry("openai", "gpt-flash", 0.95),
    ];
    let resolved = resolve_listed(&w, &env_of(&[("OA_KEY", "k")]), &list).await;
    assert_eq!(resolved.guardian_choice, GuardianChoice::Default);
    assert!(resolved.notices.is_empty());
    assert_eq!(
        resolved.guardian.unwrap().reference().to_string(),
        "oa/gpt-flash"
    );
}

#[tokio::test]
async fn a_list_less_account_counts_with_a_credential_and_is_never_listed() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "{MAIN}\n[accounts.cloud]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"CLOUD_KEY\"\n",
        server.uri()
    ));
    let list = [entry("anthropic", "claude-haiku-5-5", 0.99)];
    let resolved = resolve_listed(&w, &env_of(&[("CLOUD_KEY", "k")]), &list).await;
    assert_eq!(resolved.guardian_choice, GuardianChoice::Default);
    assert_eq!(
        resolved.guardian.unwrap().reference().to_string(),
        "cloud/claude-haiku-5-5"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_main_account_is_judged_by_the_listing_already_fetched() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "claude-sonnet-4-6", "type": "model", "display_name": "s", "created_at": "2025-01-01T00:00:00Z"},
                     {"id": "claude-haiku-5-5", "type": "model", "display_name": "h", "created_at": "2025-01-01T00:00:00Z"}],
            "has_more": false, "first_id": "a", "last_id": "b"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.work]\nkind = \"anthropic\"\nbase_url = \"{}\"\napi_key_env = \"K\"\n\n[model]\nmodel = \"work/claude-sonnet-4-6\"\n",
        server.uri()
    ));
    let list = [
        entry("anthropic", "claude-unlisted", 0.99),
        entry("anthropic", "claude-haiku-5-5", 0.95),
    ];
    let resolved = resolve_listed(&w, &env_of(&[("K", "k")]), &list).await;
    assert_eq!(
        resolved.guardian.unwrap().reference().to_string(),
        "work/claude-haiku-5-5"
    );
    assert_eq!(resolved.main_listing.len(), 2);
}

#[tokio::test]
async fn a_pinned_guardian_is_never_substituted() {
    let w = World::new();
    w.user_file(&format!(
        "{MAIN}\n[accounts.cloud]\nkind = \"anthropic\"\napi_key_env = \"CLOUD_KEY\"\nmodels = [\"claude-haiku-5-5\"]\n\n[accounts.other]\nkind = \"openai\"\napi_key_env = \"OA_KEY\"\nmodels = [\"gpt-flash\"]\n\n[guardian]\nmodel = \"cloud/claude-haiku-5-5\"\n"
    ));
    // The pinned model has no credential; the list holds a model that would work.
    let list = [entry("openai", "gpt-flash", 0.99)];
    let resolved = resolve_listed(&w, &env_of(&[("OA_KEY", "k")]), &list).await;
    assert_eq!(resolved.guardian_choice, GuardianChoice::Pinned);
    assert!(resolved.notices.is_empty());
    let Some(GuardianModel::Unavailable { reference, reason }) = resolved.guardian else {
        panic!("unavailable");
    };
    assert_eq!(reference.to_string(), "cloud/claude-haiku-5-5");
    assert!(reason.contains("CLOUD_KEY"), "{reason}");
}

#[tokio::test]
async fn a_validated_model_on_an_account_without_its_credential_is_not_picked() {
    let w = World::new();
    w.user_file(&format!(
        "{MAIN}\n[accounts.nokey]\nkind = \"openai\"\napi_key_env = \"MISSING\"\nmodels = [\"gpt-flash\"]\n\n[accounts.oa]\nkind = \"openai\"\napi_key_env = \"OA_KEY\"\nmodels = [\"gpt-flash\"]\n"
    ));
    let list = [entry("openai", "gpt-flash", 0.99)];
    let resolved = resolve_listed(&w, &env_of(&[("OA_KEY", "k")]), &list).await;
    let Some(GuardianModel::Ready(handle)) = resolved.guardian else {
        panic!("ready");
    };
    assert_eq!(handle.reference.to_string(), "oa/gpt-flash");

    let resolved = resolve_listed(&w, &env_of(&[]), &list).await;
    let Some(GuardianModel::Ready(handle)) = resolved.guardian else {
        panic!("falls back to the main model");
    };
    assert_eq!(handle.reference.to_string(), "main/qwen3:4b");
}

#[tokio::test]
async fn a_stored_sign_in_is_no_credential_for_a_key_based_account() {
    let w = World::new();
    w.user_file(
        "[accounts.cloud]\nkind = \"anthropic\"\n\n[model]\nmodel = \"cloud/claude-haiku-5-5\"\n",
    );
    std::fs::write(
        w.data.join("credentials.toml"),
        "[accounts.cloud.oauth]\naccess = \"at\"\nrefresh = \"rt\"\nexpires_at = 4102444800\n",
    )
    .unwrap();
    let config = w.load().unwrap();
    let Err(err) = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: &env_of(&[]),
        model_override: None,
        session_id: None,
    })
    .await
    else {
        panic!("a sign-in is no key");
    };
    assert!(err.to_string().contains("has no credential"), "{err}");
}

#[test]
fn the_switcher_offers_the_validated_and_the_explicit_models_the_accounts_have() {
    let w = World::new();
    w.user_file(&format!(
        "{MAIN}\n[accounts.cloud]\nkind = \"anthropic\"\napi_key_env = \"CLOUD_KEY\"\nmodels = [\"claude-haiku-5-5\", \"claude-sonnet-4-6\"]\n\n[accounts.nokey]\nkind = \"openai\"\napi_key_env = \"MISSING\"\nmodels = [\"gpt-flash\"]\n"
    ));
    let switcher = Switcher::new(
        w.load().unwrap(),
        w.data.clone(),
        std::sync::Arc::new(|name: &str| (name == "CLOUD_KEY").then(|| OsString::from("k"))),
        None,
    );
    let main = ModelRef::parse("main/qwen3:4b").unwrap();
    let names: Vec<String> = switcher
        .candidates(&main, &["qwen3:4b".to_string()])
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        names,
        [
            "cloud/claude-haiku-5-5",
            "cloud/claude-sonnet-4-6",
            "main/qwen3:4b"
        ]
    );
    // An account without its credential gives an unavailable guardian, not an error.
    let nokey = switcher.guardian("nokey/gpt-flash", None).unwrap();
    assert!(matches!(nokey, GuardianModel::Unavailable { .. }));
    assert!(matches!(
        switcher.guardian("cloud/claude-haiku-5-5", None).unwrap(),
        GuardianModel::Ready(_)
    ));
    assert!(switcher.guardian("cloud/not-offered", None).is_err());

    // A guardian on the main model's account is checked against that account's listing.
    let listing = ["qwen3:4b".to_string()];
    let with_main = Some((&main, &listing[..]));
    assert!(matches!(
        switcher.guardian("main/qwen3:4b", with_main).unwrap(),
        GuardianModel::Ready(_)
    ));
    assert!(switcher.guardian("main/typo", with_main).is_err());
}

#[test]
fn model_references_split_at_the_first_slash() {
    let r = ModelRef::parse("router/anthropic/claude-sonnet-4-6").unwrap();
    assert_eq!(r.account, "router");
    assert_eq!(r.model, "anthropic/claude-sonnet-4-6");
    assert_eq!(r.to_string(), "router/anthropic/claude-sonnet-4-6");
    assert!(ModelRef::parse("noslash").is_none());
    assert!(ModelRef::parse("/model").is_none());
    assert!(ModelRef::parse("account/").is_none());
}

#[tokio::test]
async fn a_handle_over_a_scripted_model_answers_without_any_endpoint() {
    let script = MockCompletionModel::default();
    let handle = ModelHandle::new(
        ModelRef::parse("test/scripted").unwrap(),
        AccountKind::named("openai-compatible"),
        script.into(),
    );
    assert!(!handle.priced());
    assert_eq!(handle.reference.model, "scripted");
}

#[test]
fn the_shipped_model_list_parses_and_every_entry_clears_the_bar() {
    use fontaine_guardian::models::{MODELS_TOML, ModelsFile, clears_bar};
    let file: ModelsFile = toml::from_str(MODELS_TOML).expect("the shipped list parses");
    for e in &file.models {
        assert!(
            clears_bar(e.catch_rate, e.false_deny_rate, e.p95_ms),
            "{}/{}",
            e.kind,
            e.model
        );
    }
}

fn openai_models(ids: &[&str]) -> ResponseTemplate {
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "x"}))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data}))
}

fn listing_switcher(w: &World, env: &[(&str, &str)]) -> Switcher {
    let map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Switcher::new(
        w.load().unwrap(),
        w.data.clone(),
        std::sync::Arc::new(move |name: &str| map.get(name).map(OsString::from)),
        None,
    )
}

#[tokio::test]
async fn listings_use_config_and_the_main_listing_without_a_request_and_list_the_rest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(openai_models(&["zeta", "alpha"]))
        .expect(1)
        .mount(&server)
        .await;
    let w = World::new();
    // `main` and `fixed` would fail if called: nothing listens on port 9.
    w.user_file(&format!(
        "[accounts.main]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\n\n[accounts.fixed]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"one\", \"two\"]\n\n[accounts.live]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    let switcher = listing_switcher(&w, &[]);
    let main = ModelRef::parse("main/m1").unwrap();
    let listings = switcher
        .listings(Some((&main, &["m1".to_string()])), None)
        .await;
    let names: Vec<&str> = listings.iter().map(|l| l.account.as_str()).collect();
    assert_eq!(names, ["fixed", "live", "main"]);
    assert_eq!(listings[0].models, Ok(vec!["one".into(), "two".into()]));
    assert_eq!(listings[1].models, Ok(vec!["zeta".into(), "alpha".into()]));
    assert_eq!(listings[2].models, Ok(vec!["m1".into()]));

    // `only` narrows the accounts visited.
    let only = switcher
        .listings(
            Some((&main, &["m1".to_string()])),
            Some(&["fixed".to_string()]),
        )
        .await;
    assert_eq!(only.len(), 1);
}

#[tokio::test]
async fn a_failing_account_keeps_its_entry_with_the_reason() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"m1\"]\n\n[accounts.broken]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[accounts.nokey]\nkind = \"openai\"\napi_key_env = \"MISSING\"\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    let switcher = listing_switcher(&w, &[]);
    let main = ModelRef::parse("main/m1").unwrap();
    let listings = switcher
        .listings(Some((&main, &["m1".to_string()])), None)
        .await;
    let names: Vec<&str> = listings.iter().map(|l| l.account.as_str()).collect();
    assert_eq!(names, ["broken", "main", "nokey"]);
    let broken = listings[0].models.as_ref().unwrap_err();
    assert!(
        broken.contains("500") || broken.contains("boom"),
        "{broken}"
    );
    let nokey = listings[2].models.as_ref().unwrap_err();
    assert!(nokey.contains("MISSING"), "{nokey}");
}

#[tokio::test]
async fn a_slow_account_times_out_without_stopping_the_others() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(openai_models(&["late"]).set_delay(std::time::Duration::from_secs(5)))
        .mount(&server)
        .await;
    let w = World::new();
    w.user_file(&format!(
        "[accounts.main]\nkind = \"ollama\"\nmodels = [\"m1\"]\n\n[accounts.slow]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n\n[model]\nmodel = \"main/m1\"\n",
        server.uri()
    ));
    let switcher = listing_switcher(&w, &[]);
    let main = ModelRef::parse("main/m1").unwrap();
    let timeout = std::time::Duration::from_millis(200);
    let listings = switcher
        .listings_within(Some((&main, &["m1".to_string()])), None, timeout)
        .await;
    assert_eq!(
        listings[1].models,
        Err("listing timed out after 200ms".to_string())
    );
    assert!(listings[0].models.is_ok());
}
