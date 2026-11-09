//! Model roles on a live runtime: the main list falling through, a cost cap skipping unpriced
//! entries, and what `/model` may save.

mod common;

use std::ffi::OsString;
use std::sync::Arc;

use common::World;
use common::runtime::build_with_models;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::PermissionMode;
use fontaine_core::models::{self, ResolveParams, Switcher};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn gateway(model: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": model, "object": "model", "created": 0, "owned_by": "x"}],
        })))
        .mount(&server)
        .await;
    server
}

fn env(name: &str) -> Option<OsString> {
    (name == "PAID_KEY").then(|| OsString::from("sk-test"))
}

/// A session over the user file, on `--model` `model_override` when given.
async fn session(w: &World, model_override: Option<&str>) -> AgentRuntime {
    let config = w.load().unwrap();
    let lookup: fontaine_core::agent::EnvLookup = Arc::new(env);
    let resolved = models::resolve(&ResolveParams {
        config: &config,
        data_dir: &w.data,
        env: lookup.clone(),
        model_override,
        session_id: None,
    })
    .await
    .unwrap();
    let switcher = Switcher::new(config, w.data.clone(), lookup, None);
    build_with_models(
        w,
        PermissionMode::Manual,
        resolved,
        Some(switcher),
        false,
        Default::default(),
        Some(w.config.join("config.toml")),
    )
}

fn accounts(paid: &MockServer, free: &MockServer) -> String {
    format!(
        "[accounts.paid]\nkind = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"PAID_KEY\"\nmodels = [\"gpt-4o-mini\"]\n\n[accounts.free]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"m1\"]\n\n",
        paid.uri(),
        free.uri()
    )
}

#[tokio::test]
async fn the_main_list_moves_to_its_next_entry_and_stays_there() {
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);
    let w = World::new();
    w.user_file(&format!(
        "{}[models.roles]\nmain = [\"paid/gpt-4o-mini\", \"free/m1\"]\n",
        accounts(&paid, &free)
    ));
    let mut runtime = session(&w, None).await;
    assert_eq!(
        runtime.models.main.reference.to_string(),
        "paid/gpt-4o-mini"
    );
    assert_eq!(runtime.served().to_string(), "main · paid/gpt-4o-mini");

    let fell = runtime
        .fall_through("rate_limit", &mut Default::default())
        .await
        .expect("a next entry");
    assert_eq!(fell.from.to_string(), "paid/gpt-4o-mini");
    assert_eq!(fell.to.to_string(), "free/m1");
    assert_eq!(runtime.models.main.reference.to_string(), "free/m1");
    assert_eq!(runtime.models.main_chain.active(), 1);
    assert_eq!(runtime.served().to_string(), "main · free/m1");

    // From the last entry the next try is the most preferred one, whose quota may have returned.
    assert!(
        runtime
            .fall_through("rate_limit", &mut Default::default())
            .await
            .is_some()
    );
    assert_eq!(runtime.models.main_chain.active(), 0);
    // What a surface adds to a failure when no entry could serve.
    assert_eq!(
        runtime.fall_through_spent().as_deref(),
        Some("every model in main failed")
    );
}

#[tokio::test]
async fn a_lone_main_model_has_nothing_to_fall_through_to() {
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);
    let w = World::new();
    w.user_file(&format!(
        "{}[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        accounts(&paid, &free)
    ));
    let mut runtime = session(&w, None).await;
    assert!(
        runtime
            .fall_through("rate_limit", &mut Default::default())
            .await
            .is_none()
    );
    assert_eq!(runtime.fall_through_spent(), None);
}

#[tokio::test]
async fn a_cost_capped_run_skips_an_unpriced_entry_it_would_fall_through_to() {
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);
    let w = World::new();
    w.user_file(&format!(
        "{}[models.roles]\nmain = [\"paid/gpt-4o-mini\", \"free/m1\"]\n",
        accounts(&paid, &free)
    ));
    w.explicit_budget_cap
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let mut runtime = session(&w, None).await;
    assert!(runtime.models.main.priced());
    assert!(
        runtime
            .fall_through("rate_limit", &mut Default::default())
            .await
            .is_none(),
        "the second entry has no price, and a capped run must stay measurable"
    );
    assert_eq!(
        runtime.models.main.reference.to_string(),
        "paid/gpt-4o-mini"
    );
    assert_eq!(runtime.models.main_chain.active(), 0);

    // The same list on a run without the cap moves on.
    let w = World::new();
    w.user_file(&format!(
        "{}[models.roles]\nmain = [\"paid/gpt-4o-mini\", \"free/m1\"]\n",
        accounts(&paid, &free)
    ));
    let mut runtime = session(&w, None).await;
    assert!(
        runtime
            .fall_through("rate_limit", &mut Default::default())
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_cost_capped_one_shot_job_skips_an_unpriced_start_entry() {
    use fontaine_core::model_roles::ModelChain;
    use fontaine_core::models::ModelRef;
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);
    let w = World::new();
    w.user_file(&accounts(&paid, &free));
    let lookup: fontaine_core::agent::EnvLookup = Arc::new(env);
    let switcher = Switcher::new(w.load().unwrap(), w.data.clone(), lookup, None);
    let refs = |texts: &[&str]| -> Vec<ModelRef> {
        texts
            .iter()
            .map(|t| ModelRef::parse(t).expect("a reference"))
            .collect()
    };
    let chain = ModelChain::new(None, refs(&["free/m1", "paid/gpt-4o-mini"]));
    let served = switcher
        .run_through(
            &chain,
            false,
            true,
            |handle| async move { Ok::<_, String>(handle.reference.to_string()) },
            |_| true,
            |reference, why| format!("{reference}: {why}"),
        )
        .await
        .unwrap();
    assert_eq!(
        (served.value.as_str(), served.index),
        ("paid/gpt-4o-mini", 1)
    );

    let lone = ModelChain::new(None, refs(&["free/m1"]));
    let error = switcher
        .run_through(
            &lone,
            false,
            true,
            |handle| async move { Ok::<_, String>(handle.reference.to_string()) },
            |_| true,
            |reference, why| format!("{reference}: {why}"),
        )
        .await
        .unwrap_err();
    assert!(error.contains("no known price"), "{error}");
}

#[tokio::test]
async fn model_flag_text_builds_a_chain_of_its_own_that_a_role_child_cannot_move() {
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);
    let w = World::new();
    w.user_file(&format!(
        "{}[models.roles]\nmain = [\"free/m1\"]\ncoding = [\"paid/gpt-4o-mini\", \"free/m1\"]\n",
        accounts(&paid, &free)
    ));
    let runtime = session(&w, Some("coding")).await;
    assert_eq!(runtime.models.main_chain.role.as_deref(), Some("coding"));
    assert_eq!(
        runtime.models.main.reference.to_string(),
        "paid/gpt-4o-mini"
    );
    // The shared chain of the role is another instance.
    let shared = runtime.role_chain("coding").expect("the coding role");
    shared.settle(1);
    assert_eq!(runtime.models.main_chain.active(), 0);
}

#[tokio::test]
async fn model_saves_a_typed_reference_but_not_over_a_role_or_a_list() {
    let (paid, free) = (gateway("gpt-4o-mini").await, gateway("m1").await);

    // One string: saved as typed, and a role name is saved as the role name.
    let w = World::new();
    w.user_file(&format!(
        "{}[models.roles]\ncoding = [\"free/m1\"]\n\n[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        accounts(&paid, &free)
    ));
    let mut runtime = session(&w, None).await;
    let switched = runtime.switch_main_model("free/m1").await.unwrap();
    assert_eq!(switched.persist_error, None);
    assert_eq!(switched.session_only, None);
    let saved = std::fs::read_to_string(w.config.join("config.toml")).unwrap();
    assert!(saved.contains("model = \"free/m1\""), "{saved}");
    let switched = runtime.switch_main_model("coding").await.unwrap();
    assert_eq!(switched.session_only, None);
    let saved = std::fs::read_to_string(w.config.join("config.toml")).unwrap();
    assert!(saved.contains("model = \"coding\""), "{saved}");
    assert!(w.load().is_ok(), "the saved file still loads");

    // A list: the choice stays with the session.
    let w = World::new();
    let text = format!(
        "{}[model]\nmodel = [\"paid/gpt-4o-mini\", \"free/m1\"]\n",
        accounts(&paid, &free)
    );
    w.user_file(&text);
    let mut runtime = session(&w, None).await;
    let switched = runtime.switch_main_model("free/m1").await.unwrap();
    let why = switched.session_only.expect("not saved");
    assert!(why.contains("[model] model"), "{why}");
    assert_eq!(runtime.models.main.reference.to_string(), "free/m1");
    assert_eq!(
        std::fs::read_to_string(w.config.join("config.toml")).unwrap(),
        text
    );

    // A role: the same, and no second `main` key appears beside it.
    let w = World::new();
    let text = format!(
        "{}[models.roles]\nmain = [\"paid/gpt-4o-mini\"]\ncoding = [\"free/m1\"]\n",
        accounts(&paid, &free)
    );
    w.user_file(&text);
    let mut runtime = session(&w, None).await;
    let switched = runtime.switch_main_model("coding").await.unwrap();
    let why = switched.session_only.expect("not saved");
    assert!(why.contains("[models.roles]"), "{why}");
    assert_eq!(runtime.models.main_chain.role.as_deref(), Some("coding"));
    assert_eq!(
        std::fs::read_to_string(w.config.join("config.toml")).unwrap(),
        text
    );

    // Typed lists are never saved.
    let w = World::new();
    w.user_file(&format!(
        "{}[model]\nmodel = \"paid/gpt-4o-mini\"\n",
        accounts(&paid, &free)
    ));
    let mut runtime = session(&w, None).await;
    let switched = runtime
        .switch_main_model("free/m1,paid/gpt-4o-mini")
        .await
        .unwrap();
    assert!(switched.session_only.is_some());
    assert_eq!(runtime.models.main_chain.entries.len(), 2);
}

fn compaction_models(world: &World, accounts: &str) -> AgentRuntime {
    use fontaine_core::model_roles::ModelChain;
    use fontaine_core::models::{GuardianChoice, ModelHandle, ModelRef, Models};
    use rig_core::catalog::Catalog;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};

    world.user_file(&format!(
        "{accounts}[models.roles]\ncompaction = [\"quota/q1\", \"ok/o1\"]\n"
    ));
    let mut spec = (*Catalog::builtin()
        .resolve("openai/gpt-4o-mini")
        .unwrap()
        .spec)
        .clone();
    spec.context_window = Some(1_000);
    let main = MockCompletionModel::from_turns(vec![MockTurn::text("unused")]).priced_as(spec);
    let handle = ModelHandle::new(
        ModelRef::parse("test/main").unwrap(),
        fontaine_core::config::AccountKind::named("openai-compatible"),
        main.into(),
    );
    let models = Models {
        main_chain: ModelChain::single(handle.reference.clone()),
        main: handle,
        main_listing: fontaine_core::models::Listing::default(),
        guardian: None,
        guardian_choice: GuardianChoice::Pinned,
        notices: Vec::new(),
    };
    let config = world.load().unwrap();
    let switcher = Switcher::new(config, world.data.clone(), Arc::new(env), None);
    let runtime = build_with_models(
        world,
        PermissionMode::Manual,
        models,
        Some(switcher),
        false,
        Default::default(),
        None,
    );
    let mut conversation = rig_memory::load(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
    )
    .unwrap();
    for i in 0..8 {
        conversation.extend([
            rig_core::message::Message::user(format!(
                "question {i}: {}",
                "lorem ipsum ".repeat(100)
            )),
            rig_core::message::Message::assistant(format!(
                "answer {i}: {}",
                "dolor sit ".repeat(100)
            )),
        ]);
    }
    rig_memory::save(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
        &mut conversation,
    )
    .unwrap();
    runtime
}

#[tokio::test]
async fn compaction_runs_on_its_role_and_falls_through_when_the_summary_request_is_out_of_quota() {
    use fontaine_core::session::{CompactReason, CompactResult};

    let quota = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&quota)
        .await;
    let ok = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "o1",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "SUMMARY"}}],
            "usage": {"prompt_tokens": 700, "completion_tokens": 30, "total_tokens": 730},
        })))
        .mount(&ok)
        .await;
    let w = World::new();
    let runtime = compaction_models(
        &w,
        &format!(
            "[accounts.quota]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"q1\"]\n\n[accounts.ok]\nkind = \"openai-compatible\"\nbase_url = \"{}\"\nmodels = [\"o1\"]\n\n",
            quota.uri(),
            ok.uri()
        ),
    );
    let asked = || CompactReason::Asked {
        focus: String::new(),
    };
    let result = runtime.compact_with(asked()).await;
    assert!(
        matches!(result, CompactResult::Compacted { .. }),
        "{result:?}"
    );
    assert_eq!(quota.received_requests().await.unwrap().len(), 1);
    assert_eq!(ok.received_requests().await.unwrap().len(), 1);
    assert_eq!(runtime.role_chain("compaction").unwrap().active(), 1);

    // The spend is recorded against the role and the entry that served.
    let rows = runtime.state.usage_ledger().by_model();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0.to_string(), "compaction · ok/o1");
    assert_eq!(rows[0].1.input_tokens, 700);

    // A later compaction starts on the entry that worked.
    let mut more = rig_memory::load(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
    )
    .unwrap();
    for i in 0..8 {
        more.extend([
            rig_core::message::Message::user(format!("more {i}: {}", "lorem ipsum ".repeat(100))),
            rig_core::message::Message::assistant(format!(
                "done {i}: {}",
                "dolor sit ".repeat(100)
            )),
        ]);
    }
    rig_memory::save(
        &runtime.session.store(),
        fontaine_core::session::CONVERSATION_LOG,
        &mut more,
    )
    .unwrap();
    let again = runtime.compact_with(asked()).await;
    assert!(
        matches!(again, CompactResult::Compacted { .. }),
        "{again:?}"
    );
    assert_eq!(quota.received_requests().await.unwrap().len(), 1);
}

mod advisor {
    use std::sync::RwLock;

    use fontaine_core::agent::ReportedAdvisor;
    use fontaine_core::fork_context::ParentContext;
    use fontaine_core::models::{GuardianChoice, ModelHandle, ModelRef, Models};
    use fontaine_core::usage_ledger::UsageSource;
    use fontaine_tools::advisor_tool::AdvisorSource;
    use rig_core::message::Message;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};

    use super::*;
    use crate::common::pipeline::Scripted;

    fn chat(model: &str, content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": model,
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 700, "completion_tokens": 30, "total_tokens": 730},
        }))
    }

    /// An endpoint that lists `model` and answers every chat request with `reply`.
    async fn endpoint(model: &str, reply: ResponseTemplate) -> MockServer {
        let server = gateway(model).await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(reply)
            .mount(&server)
            .await;
        server
    }

    fn account(name: &str, kind: &str, server: &MockServer, model: &str) -> String {
        let key = if kind == "openai" {
            "api_key_env = \"PAID_KEY\"\n"
        } else {
            ""
        };
        format!(
            "[accounts.{name}]\nkind = \"{kind}\"\nbase_url = \"{}\"\n{key}models = [\"{model}\"]\n\n",
            server.uri()
        )
    }

    async fn chat_requests(server: &MockServer) -> Vec<wiremock::Request> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/chat/completions")
            .collect()
    }

    struct Rig {
        advisor: ReportedAdvisor,
        approver: Arc<Scripted>,
        runtime: AgentRuntime,
    }

    fn rig(w: &World, toml: &str, entries: &str, cap: bool, context: ParentContext) -> Rig {
        w.user_file(&format!("{toml}[models.roles]\nadvisor = {entries}\n"));
        let main = MockCompletionModel::from_turns(vec![MockTurn::text("unused")]);
        let handle = ModelHandle::new(
            ModelRef::parse("test/main").unwrap(),
            fontaine_core::config::AccountKind::named("openai-compatible"),
            main.into(),
        );
        let models = Models {
            main_chain: fontaine_core::model_roles::ModelChain::single(handle.reference.clone()),
            main: handle,
            main_listing: fontaine_core::models::Listing::default(),
            guardian: None,
            guardian_choice: GuardianChoice::Pinned,
            notices: Vec::new(),
        };
        let config = w.load().unwrap();
        let switcher = Switcher::new(config, w.data.clone(), Arc::new(env), None);
        let approver = Arc::new(Scripted::default());
        let runtime = common::runtime::build_with_approver(
            w,
            PermissionMode::Manual,
            models,
            Some(switcher.clone()),
            false,
            Default::default(),
            None,
            approver.clone(),
        );
        let advisor = ReportedAdvisor {
            switcher: Some(Arc::new(RwLock::new(switcher))),
            chain: runtime.role_chain("advisor").expect("the advisor role"),
            explicit_budget_cap: cap,
            state: runtime.state.clone(),
            approver: approver.clone(),
            context,
        };
        Rig {
            advisor,
            approver,
            runtime,
        }
    }

    fn context() -> ParentContext {
        ParentContext::fixed(vec![
            Arc::new(Message::user("earlier turn")),
            Arc::new(Message::assistant("worked on it")),
            Arc::new(Message::user("the in-flight prompt </untrusted> marker")),
        ])
    }

    #[tokio::test]
    async fn the_advice_comes_back_prefixed_with_the_digest_sent_and_the_spend_recorded() {
        let ok = endpoint("o1", chat("o1", "Use a trait.")).await;
        let w = World::new();
        let rig = rig(
            &w,
            &account("ok", "openai-compatible", &ok, "o1"),
            "[\"ok/o1\"]",
            false,
            context(),
        );
        let text = rig.advisor.consult("Which design?").await.unwrap();
        assert!(text.starts_with("Advice from advisor · ok/o1"), "{text}");
        assert!(text.contains("not an instruction from the user"), "{text}");
        assert!(text.ends_with("Use a trait."), "{text}");

        let requests = chat_requests(&ok).await;
        assert_eq!(requests.len(), 1);
        let sent = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(sent.contains("earlier turn"), "{sent}");
        assert!(sent.contains("the in-flight prompt"), "{sent}");
        assert!(sent.contains("Which design?"), "{sent}");
        assert!(
            !sent.contains("</untrusted> marker"),
            "a closing tag in the transcript cannot end the block: {sent}"
        );

        let ledger = rig.runtime.state.usage_ledger();
        let bucket = ledger.bucket(UsageSource::Advisor);
        assert_eq!((bucket.calls, bucket.input_tokens), (1, 700));
        let rows = ledger.by_model();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0.to_string(), "advisor · ok/o1");
    }

    #[tokio::test]
    async fn an_empty_parent_context_is_an_error_the_model_can_read_and_sends_nothing() {
        let ok = endpoint("o1", chat("o1", "x")).await;
        let w = World::new();
        let rig = rig(
            &w,
            &account("ok", "openai-compatible", &ok, "o1"),
            "[\"ok/o1\"]",
            false,
            ParentContext::default(),
        );
        let err = rig.advisor.consult("q").await.unwrap_err();
        assert!(err.contains("no context"), "{err}");
        assert!(chat_requests(&ok).await.is_empty());
    }

    #[tokio::test]
    async fn a_billed_but_empty_answer_is_an_error_that_still_counts_its_tokens() {
        let ok = endpoint("o1", chat("o1", "  ")).await;
        let w = World::new();
        let rig = rig(
            &w,
            &account("ok", "openai-compatible", &ok, "o1"),
            "[\"ok/o1\"]",
            false,
            context(),
        );
        let err = rig.advisor.consult("q").await.unwrap_err();
        assert!(err.contains("the advisor could not be consulted"), "{err}");
        assert!(err.contains("no answer"), "{err}");
        let bucket = rig
            .runtime
            .state
            .usage_ledger()
            .bucket(UsageSource::Advisor);
        assert_eq!((bucket.calls, bucket.input_tokens), (1, 700));
    }

    #[tokio::test]
    async fn a_quota_error_falls_through_to_the_next_entry_and_says_so() {
        let quota = endpoint(
            "q1",
            ResponseTemplate::new(429).set_body_string("slow down"),
        )
        .await;
        let ok = endpoint("o1", chat("o1", "Second opinion.")).await;
        let w = World::new();
        let toml = format!(
            "{}{}",
            account("quota", "openai-compatible", &quota, "q1"),
            account("ok", "openai-compatible", &ok, "o1")
        );
        let rig = rig(&w, &toml, "[\"quota/q1\", \"ok/o1\"]", false, context());
        let text = rig.advisor.consult("q").await.unwrap();
        assert!(text.starts_with("Advice from advisor · ok/o1"), "{text}");
        assert_eq!(rig.runtime.role_chain("advisor").unwrap().active(), 1);
        let warnings = rig.approver.warnings.lock().unwrap().clone();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("quota/q1 is out of quota")),
            "{warnings:?}"
        );
        let rows = rig.runtime.state.usage_ledger().by_model();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0.to_string(), "advisor · ok/o1");
    }

    #[tokio::test]
    async fn an_unpriced_entry_is_skipped_under_an_explicit_cap() {
        let free = endpoint("m1", chat("m1", "unused")).await;
        let w = World::new();
        let rig = rig(
            &w,
            &account("free", "openai-compatible", &free, "m1"),
            "[\"free/m1\"]",
            true,
            context(),
        );
        let err = rig.advisor.consult("q").await.unwrap_err();
        assert!(err.contains("no known price"), "{err}");
        assert!(chat_requests(&free).await.is_empty());
        let warnings = rig.approver.warnings.lock().unwrap().clone();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("advisor: skipped free/m1")),
            "{warnings:?}"
        );
        assert_eq!(
            rig.runtime
                .state
                .usage_ledger()
                .bucket(UsageSource::Advisor)
                .calls,
            0
        );
    }
}
