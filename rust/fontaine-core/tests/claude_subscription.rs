//! The `claude-subscription` account against the scripted CLI double (`claude_cli_double`, built
//! as an example of this package): configuration, preflight, discovery, request translation,
//! stream decoding, lifecycle and failure mapping. The real CLI never runs here.

mod common;

use std::path::PathBuf;

use common::World;
use common::claude_cli::{
    Claude, argv, double, main_handle, tagged_conversation, three_steps, toml_path,
};
use fontaine_core::accounts::{self, ExternalCheckError, SignInState};
use fontaine_core::agent::build_guardian;
use fontaine_core::config::AccountKind;
use fontaine_core::models::{self, GuardianModel, ModelError, Models, Switcher};
use fontaine_core::providers::claude_subscription::{
    CONFLICTING_ENV, ErrorKind, claude_error, inherited_env_names, login_command,
};
use fontaine_guardian::{
    AdjudicationRequest, Decision, EnvironmentConfig, Profile, RuleConfig, SessionFacts,
    ToolCallSummary,
};
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::message::{AssistantContent, CallId, Message, ToolName};
use serde_json::{Value, json};

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .cloned()
}

fn message(error: &ModelError) -> String {
    error.to_string()
}

fn read_tool() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new("read_file").unwrap(),
        "read a file",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
}

// ---- configuration and accounts -------------------------------------------------------------

#[test]
fn the_account_fields_are_validated() {
    let ok = |extra: &str| {
        let w = World::new();
        w.user_file(&format!(
            "[accounts.claude]\nkind = \"claude-subscription\"\n{extra}\n"
        ));
        w.load().map(|_| ()).map_err(|e| e.to_string())
    };
    assert!(ok("").is_ok());
    let abs = if cfg!(windows) {
        "C:\\\\claude"
    } else {
        "/usr/bin/claude"
    };
    assert!(
        ok(&format!(
            "cli_path = \"{abs}\"\nclaude_config_dir = \"{abs}\""
        ))
        .is_ok()
    );
    assert!(ok("models = [\"sonnet\"]").is_ok());
    for (line, key) in [
        ("api_key_env = \"K\"", "api_key_env"),
        ("base_url = \"http://x\"", "base_url"),
        ("wires = { sonnet = \"messages\" }", "wires"),
        ("context_tokens = 4096", "context_tokens"),
        ("think = false", "think"),
        ("prompt_caching = true", "prompt_caching"),
        ("cache_ttl = \"1h\"", "cache_ttl"),
        ("quota = \"kimi\"", "quota"),
        ("auth = \"bearer\"", "auth"),
        ("https_proxy = \"http://p:1\"", "https_proxy"),
        ("no_proxy = \"x\"", "no_proxy"),
        ("ca_bundle = \"/x.pem\"", "ca_bundle"),
        ("cli_path = \"claude\"", "cli_path"),
        ("claude_config_dir = \"rel\"", "claude_config_dir"),
    ] {
        let error = ok(line).unwrap_err();
        assert!(
            error.contains(&format!("accounts.claude.{key}")),
            "{line}: {error}"
        );
    }
    // The fields belong to this kind only.
    let w = World::new();
    w.user_file("[accounts.local]\nkind = \"ollama\"\ncli_path = \"/usr/bin/claude\"\n");
    assert!(
        w.load()
            .unwrap_err()
            .to_string()
            .contains("accounts.local.cli_path")
    );
}

#[test]
fn a_project_cannot_define_the_account() {
    let w = World::new();
    w.project_file("[accounts.claude]\nkind = \"claude-subscription\"\ncli_path = \"/tmp/evil\"\n");
    let loaded = w.load();
    assert!(
        loaded.is_err() || loaded.unwrap().accounts.is_empty(),
        "a repository chose an executable"
    );
}

#[test]
fn the_row_says_the_login_is_external_and_ignores_stored_keys() {
    let claude = Claude::new();
    fontaine_core::credentials::store(&claude.w.data, "claude", "stale-key").unwrap();
    let config = claude.w.load().unwrap();
    let rows = accounts::rows(&config, &claude.w.data, &*claude.env()).unwrap();
    assert_eq!(rows[0].state, SignInState::ExternalCli);
    assert!(rows[0].state.to_string().contains("external CLI"));
    assert_eq!(
        config.accounts["claude"].kind,
        AccountKind::named("claude-subscription")
    );
}

#[tokio::test]
async fn selecting_the_account_emits_no_experimental_notice() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let models = claude.resolve(None).await.unwrap();
    assert!(
        !models
            .notices
            .iter()
            .any(|n| n.headline.contains("experimental")),
        "{:?}",
        models.notices
    );
    assert!(!models.main.catalog_billed());
    assert!(!models.main.priced());
}

// ---- preflight ------------------------------------------------------------------------------

#[tokio::test]
async fn a_supported_cli_discovers_models_with_initialize_only() {
    let claude = Claude::new();
    claude.scenario(json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
        {"value": "haiku", "resolvedModel": "claude-haiku-4-5"},
        {"value": "fast", "resolvedModel": "claude-opus-5-fast", "usesUsageCredits": true},
    ]}));
    let models = claude.resolve(None).await.unwrap();
    assert_eq!(models.main_listing.models, ["sonnet", "haiku"]);
    assert_eq!(models.main.reference.model, "sonnet");
    // Only `initialize` was sent: no user frame, no model run.
    let frames = claude.frames();
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["request"]["subtype"], "initialize");
    assert!(claude.model_runs().is_empty());
    let handle = models.main;
    assert_eq!(handle.completion.id(), Some("claude-sonnet-5-5"));
    let spec = handle.completion.spec().expect("canonical facts");
    assert_eq!(spec.id, "claude-sonnet-5-5");
    assert!(spec.max_output_tokens.is_some());
    // Prompt suggestions are off on the discovery invocation.
    for invocation in claude.invocations() {
        let args = argv(&invocation);
        assert_eq!(
            flag_value(&args, "--prompt-suggestions").as_deref(),
            Some("false")
        );
        assert!(!args.contains(&"--bare".to_string()));
    }
}

#[tokio::test]
async fn a_credit_billed_model_is_refused_with_its_billing_class() {
    let claude = Claude::new();
    claude.scenario(json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
        {"value": "fast", "resolvedModel": "claude-opus-5-fast", "usesUsageCredits": true},
    ]}));
    let error = claude.resolve(Some("claude/fast")).await.unwrap_err();
    assert!(message(&error).contains("usage credits"), "{error}");
}

#[tokio::test]
async fn an_id_without_catalog_facts_is_refused() {
    let claude = Claude::new();
    claude.scenario(json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
        {"value": "future", "resolvedModel": "claude-quantum-9"},
    ]}));
    let error = claude.resolve(Some("claude/future")).await.unwrap_err();
    assert!(message(&error).contains("model facts"), "{error}");
    assert!(claude.resolve(None).await.is_ok());
}

fn derived_scenario() -> Value {
    json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"},
        {"value": "haiku", "resolvedModel": "claude-haiku-9-9",
         "supportedEffortLevels": ["low", "high", "turbo"], "supportsThinkingOff": true},
        {"value": "future", "resolvedModel": "claude-quantum-9"},
    ], "responses": [{"text": "ok"}]})
}

fn derived_notices(models: &Models) -> Vec<String> {
    models
        .notices
        .iter()
        .filter(|n| n.headline.contains("claude-haiku-9-9"))
        .map(|n| n.headline.clone())
        .collect()
}

fn with_guardian(claude: &Claude, model: &str) {
    let config = std::fs::read_to_string(claude.w.config.join("config.toml")).unwrap();
    claude
        .w
        .user_file(&format!("{config}\n[guardian]\nmodel = \"{model}\"\n"));
}

#[tokio::test]
async fn an_unlisted_model_of_a_listed_shape_resolves_with_one_notice() {
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    let models = claude.resolve(Some("claude/haiku")).await.unwrap();
    let notices = derived_notices(&models);
    assert_eq!(notices.len(), 1, "{:?}", models.notices);
    assert!(notices[0].starts_with("Model `haiku` resolves to `claude-haiku-9-9`"));
    assert_eq!(
        models.main.facts_notice.as_deref(),
        Some(notices[0].as_str())
    );

    let base = fontaine_core::providers::derived_facts::derive(
        rig_core::catalog::Catalog::builtin(),
        rig_core::providers::registry::ProviderId::catalog("anthropic").unwrap(),
        "claude-haiku-9-9",
        &Default::default(),
    )
    .unwrap();
    let limit = u64::from(base.facts.spec().unwrap().max_output_tokens.unwrap());
    models
        .main
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    let runs = claude.model_runs();
    assert_eq!(
        flag_value(&argv(&runs[0]), "--model").as_deref(),
        Some("claude-haiku-9-9")
    );
    let settings: Value = serde_json::from_str(runs[0]["settings"].as_str().unwrap()).unwrap();
    let extra: Value =
        serde_json::from_str(settings["env"]["CLAUDE_CODE_EXTRA_BODY"].as_str().unwrap()).unwrap();
    assert_eq!(extra["max_tokens"].as_u64(), Some(limit));
}

#[tokio::test]
async fn a_listed_main_model_has_no_facts_notice() {
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    let models = claude.resolve(None).await.unwrap();
    assert!(derived_notices(&models).is_empty(), "{:?}", models.notices);
    assert_eq!(models.main.facts_notice, None);
}

#[tokio::test]
async fn a_derived_guardian_warns_once_and_not_again_when_it_is_the_main_model() {
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    with_guardian(&claude, "claude/haiku");
    let models = claude.resolve(None).await.unwrap();
    assert_eq!(derived_notices(&models).len(), 1, "{:?}", models.notices);
    assert_eq!(models.main.facts_notice, None);

    let same = claude.resolve(Some("claude/haiku")).await.unwrap();
    assert_eq!(derived_notices(&same).len(), 1, "{:?}", same.notices);

    // A guardian that fell back to the derived main model shares its notice.
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    let fallback = claude.resolve(Some("claude/haiku")).await.unwrap();
    assert_eq!(
        derived_notices(&fallback).len(),
        1,
        "{:?}",
        fallback.notices
    );
}

fn runtime_over(claude: &Claude, models: Models) -> fontaine_core::agent::AgentRuntime {
    common::runtime::build_with_models(
        &claude.w,
        fontaine_core::config::PermissionMode::Manual,
        models,
        Some(claude.switcher()),
        false,
        Default::default(),
        None,
    )
}

#[tokio::test]
async fn use_guardian_and_retry_guardian_return_a_derived_guardians_notice() {
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    let models = claude.resolve(None).await.unwrap();
    let runtime = runtime_over(&claude, models);
    let switched = runtime
        .use_guardian("claude/haiku")
        .await
        .unwrap()
        .expect("a ready guardian");
    assert!(
        switched
            .facts_notice
            .as_deref()
            .is_some_and(|n| n.contains("claude-haiku-9-9")),
        "{switched:?}"
    );
    let (ready, notice) = runtime.retry_guardian().await.unwrap();
    assert!(ready);
    assert!(notice.is_some_and(|n| n.contains("claude-haiku-9-9")));

    let listed = runtime
        .use_guardian("claude/sonnet")
        .await
        .unwrap()
        .expect("a ready guardian");
    assert_eq!(listed.facts_notice, None);
    assert_eq!(runtime.retry_guardian().await.unwrap(), (true, None));
}

#[tokio::test]
async fn refused_rows_are_listed_with_their_reason_and_usable_ones_stay() {
    let claude = Claude::new();
    claude.scenario(derived_scenario());
    let listings = claude.switcher().listings(None, None).await;
    let listing = listings.iter().find(|l| l.account == "claude").unwrap();
    assert_eq!(
        listing.models.as_ref().unwrap(),
        &["sonnet".to_string(), "haiku".to_string()]
    );
    assert_eq!(listing.refused.len(), 1);
    assert_eq!(listing.refused[0].id, "future");
    assert!(listing.refused[0].reason.contains("same name pattern"));

    // The main account's listing is reused, and its refused rows survive the reuse.
    let models = claude.resolve(None).await.unwrap();
    assert_eq!(models.main_listing.refused.len(), 1);
    let reused = claude
        .switcher()
        .listings(Some((&models.main.reference, &models.main_listing)), None)
        .await;
    assert_eq!(reused[0].refused, models.main_listing.refused);
}

#[tokio::test]
async fn the_models_allowlist_narrows_refused_rows_too() {
    let claude = Claude::with_account("models = [\"haiku\"]\n");
    claude.scenario(derived_scenario());
    let listings = claude.switcher().listings(None, None).await;
    assert!(listings[0].refused.is_empty(), "{:?}", listings[0].refused);

    let claude = Claude::with_account("models = [\"haiku\", \"future\"]\n");
    claude.scenario(derived_scenario());
    let listings = claude.switcher().listings(None, None).await;
    assert_eq!(listings[0].refused.len(), 1);
    // Narrowed away, a refused row still explains itself when it is named.
    let claude = Claude::with_account("models = [\"haiku\"]\n");
    claude.scenario(derived_scenario());
    let error = claude.resolve(Some("claude/future")).await.unwrap_err();
    assert!(message(&error).contains("same name pattern"), "{error}");
}

#[tokio::test]
async fn versions_are_checked_before_anything_else() {
    for (version, ok) in [
        ("2.1.318 (Claude Code)", true),
        ("2.1.317 (Claude Code)", false),
        ("2.2.0 (Claude Code)", true),
        ("1.9.9", false),
        ("claude version unknown", false),
        ("", false),
    ] {
        let claude = Claude::new();
        claude.scenario(json!({"version": version}));
        let result = claude.resolve(None).await;
        assert_eq!(result.is_ok(), ok, "{version}: {:?}", result.err());
        if !ok {
            assert!(claude.invocations().is_empty(), "{version}");
        }
    }
    let claude = Claude::new();
    claude.scenario(json!({"version_exit": 3}));
    let error = claude.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("version"), "{error}");
    let old = Claude::new();
    old.scenario(json!({"version": "2.0.1"}));
    let error = old.resolve(None).await.unwrap_err();
    assert!(
        message(&error).contains("older than the supported minimum"),
        "{error}"
    );
}

#[tokio::test]
async fn a_missing_or_hung_cli_is_a_setup_failure() {
    let claude = Claude::with_account("");
    let config_text = std::fs::read_to_string(claude.w.config.join("config.toml")).unwrap();
    let missing = double().with_file_name("nonexistent-claude");
    let broken = config_text.replace(&toml_path(&double()), &toml_path(&missing));
    claude.w.user_file(&broken);
    let error = claude.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("could not be started"), "{error}");
    assert!(message(&error).contains("cli_path"), "{error}");

    let hung = Claude::new();
    hung.scenario(json!({"version_sleep_ms": 5000}));
    hung.set_env("FONTAINE_CLAUDE_PROBE_MS", "300");
    let started = std::time::Instant::now();
    let error = hung.resolve(None).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(message(&error).contains("--version"), "{error}");

    let slow_auth = Claude::new();
    slow_auth.scenario(json!({"auth_sleep_ms": 5000}));
    slow_auth.set_env("FONTAINE_CLAUDE_PROBE_MS", "300");
    let error = slow_auth.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("auth status"), "{error}");

    let slow_listing = Claude::new();
    slow_listing.scenario(json!({"init_hang": true}));
    slow_listing.set_env("FONTAINE_CLAUDE_PROBE_MS", "500");
    let error = slow_listing.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("list its models"), "{error}");
}

#[tokio::test]
async fn only_a_signed_in_max_or_ultra_subscription_is_accepted() {
    for (auth, expect) in [
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}),
            None,
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "Max 20x"}),
            None,
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "ultra"}),
            None,
        ),
        (json!({"loggedIn": false}), Some("not signed in")),
        (
            json!({"loggedIn": true, "authMethod": "api_key"}),
            Some("claude.ai"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "oauth_token", "subscriptionType": "max"}),
            Some("claude.ai"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "third_party"}),
            Some("claude.ai"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "pro"}),
            Some("Max or Ultra"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "team"}),
            Some("Max or Ultra"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "enterprise"}),
            Some("Max or Ultra"),
        ),
        (
            json!({"loggedIn": true, "authMethod": "claude.ai"}),
            Some("Max or Ultra"),
        ),
    ] {
        let claude = Claude::new();
        claude.scenario(json!({"auth": auth}));
        let result = claude.resolve(None).await;
        match expect {
            None => assert!(result.is_ok(), "{auth}: {:?}", result.err()),
            Some(text) => {
                let failure = result.expect_err(&auth.to_string());
                let error = message(&failure);
                assert!(error.contains(text), "{auth}: {error}");
                if text == "claude.ai" {
                    assert!(error.contains("auth login --claudeai"), "{error}");
                }
                if text == "not signed in" {
                    assert!(!error.contains("claude auth login"), "{error}");
                    assert!(failure.is_not_signed_in());
                }
                assert!(claude.frames().is_empty(), "no discovery after a refusal");
            }
        }
    }
    let claude = Claude::new();
    claude.scenario(json!({"auth_raw": "garbage", "auth_exit": 1}));
    let error = claude.resolve(None).await.unwrap_err();
    assert!(error.is_not_signed_in(), "{error}");
    assert!(!message(&error).contains("claude auth login"), "{error}");
}

#[tokio::test]
async fn every_conflicting_variable_is_named_and_no_value_is_shown() {
    for name in CONFLICTING_ENV {
        let claude = Claude::new();
        claude.scenario(json!({}));
        claude.set_env(name, "super-secret-value-123");
        let error = claude.resolve(None).await.unwrap_err();
        let text = message(&error);
        assert!(text.contains(name), "{name}: {text}");
        assert!(!text.contains("super-secret"), "{name}: {text}");
        assert!(
            claude.invocations().is_empty(),
            "{name}: no process before refusal"
        );
    }
}

#[tokio::test]
async fn a_conflict_introduced_after_connecting_is_seen_before_the_next_request() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "first"}, {"text": "second"}]}));
    let handle = main_handle(&claude).await;
    let first = handle
        .completion
        .call(CompletionRequest::new("one").max_tokens(32))
        .await
        .unwrap();
    assert_eq!(first.text(), "first");
    claude.set_env("ANTHROPIC_API_KEY", "set-later");
    let error = handle
        .completion
        .call(CompletionRequest::new("two").max_tokens(32))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ANTHROPIC_API_KEY"), "{error}");
    assert!(!error.to_string().contains("set-later"));
    claude.unset_env("ANTHROPIC_API_KEY");
    let third = handle
        .completion
        .call(CompletionRequest::new("three").max_tokens(32))
        .await
        .unwrap();
    assert_eq!(third.text(), "second");
}

#[test]
fn eval_style_owned_environments_carry_every_conflict_name() {
    let names = inherited_env_names();
    for name in CONFLICTING_ENV {
        assert!(names.contains(name), "{name}");
    }
    assert!(names.contains(&"PATH"));
}

#[tokio::test]
async fn the_cli_gets_a_rebuilt_environment() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    claude.set_env("OPENAI_API_KEY", "must-not-leak");
    claude.set_env("FONTAINE_SECRET", "must-not-leak");
    claude.set_env("HTTPS_PROXY", "http://proxy.example:3128");
    let handle = main_handle(&claude).await;
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap();
    for invocation in claude.invocations() {
        let env = invocation["env"].as_object().unwrap();
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("FONTAINE_SECRET"));
        assert_eq!(env["CLAUDE_CODE_ENTRYPOINT"], "fontaine");
        assert_eq!(env["CLAUDE_CODE_MCP_ALLOWLIST_ENV"], "1");
        assert_eq!(env["HTTPS_PROXY"], "http://proxy.example:3128");
        assert_eq!(
            env["CLAUDE_CONFIG_DIR"].as_str().unwrap(),
            claude.dir.to_str().unwrap()
        );
        assert!(env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1");
        assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "false");
        assert_eq!(env["CLAUDE_CODE_MESSAGES_PROMPT_CACHE_TTL"], "5m");
        assert_eq!(
            env["GIT_CEILING_DIRECTORIES"].as_str().unwrap(),
            claude.w.data.to_str().unwrap()
        );
    }
}

#[tokio::test]
async fn two_accounts_keep_separate_cli_profiles() {
    let claude = Claude::new();
    let second = claude.w.root.join("profile-2");
    std::fs::create_dir_all(&second).unwrap();
    let text = std::fs::read_to_string(claude.w.config.join("config.toml")).unwrap();
    claude.w.user_file(&format!(
        "{text}\n[accounts.claude2]\nkind = \"claude-subscription\"\ncli_path = {}\nclaude_config_dir = {}\n",
        toml_path(&double()),
        toml_path(&second)
    ));
    claude.scenario(json!({"models": [{"value": "sonnet", "resolvedModel": "claude-sonnet-5-5"}]}));
    std::fs::write(
        second.join("double.json"),
        json!({"models": [{"value": "opus", "resolvedModel": "claude-opus-5"}]}).to_string(),
    )
    .unwrap();
    let listings = claude.switcher().listings(None, None).await;
    let of = |name: &str| {
        listings
            .iter()
            .find(|l| l.account == name)
            .unwrap()
            .models
            .clone()
            .unwrap()
    };
    assert_eq!(of("claude"), ["sonnet"]);
    assert_eq!(of("claude2"), ["opus"]);
}

#[tokio::test]
async fn explicit_models_and_cached_listings_still_discover() {
    let claude = Claude::with_account("models = [\"haiku\"]\n");
    claude.scenario(json!({}));
    let error = claude.resolve(Some("claude/sonnet")).await.unwrap_err();
    assert!(message(&error).contains("does not offer"), "{error}");
    let models = claude.resolve(Some("claude/haiku")).await.unwrap();
    assert_eq!(models.main_listing.models, ["haiku"]);
    assert!(
        claude
            .frames()
            .iter()
            .any(|f| f["request"]["subtype"] == "initialize"),
        "the allowlist narrowed a completed discovery"
    );
    // A guardian on the same account discovers too, even with the main listing at hand.
    let before = claude.frames().len();
    let guardian = claude
        .switcher()
        .guardian(
            "claude/haiku",
            Some((&models.main.reference, &models.main_listing.models)),
        )
        .await
        .unwrap();
    assert!(matches!(guardian, GuardianModel::Ready(_)));
    assert!(
        claude.frames().len() > before,
        "the guardian was discovered"
    );
    if let GuardianModel::Ready(handle) = guardian {
        assert_eq!(handle.completion.id(), Some("claude-haiku-4-5"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_cli_that_cannot_be_executed_shows_the_os_reason() {
    use std::os::unix::fs::PermissionsExt;
    let claude = Claude::new();
    let plain = claude.w.root.join("not-executable");
    std::fs::write(&plain, "x").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let config_text = std::fs::read_to_string(claude.w.config.join("config.toml")).unwrap();
    claude
        .w
        .user_file(&config_text.replace(&toml_path(&double()), &toml_path(&plain)));
    let error = claude.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("could not be started"), "{error}");
    assert!(message(&error).contains("ermission denied"), "{error}");
}

#[tokio::test]
async fn the_picker_lists_the_allowlist_and_a_pinned_guardian_outside_it_is_refused() {
    let claude = Claude::with_account("models = [\"haiku\"]\n");
    claude.scenario(json!({}));
    let listings = claude.switcher().listings(None, None).await;
    let listing = listings.iter().find(|l| l.account == "claude").unwrap();
    assert_eq!(listing.models.as_ref().unwrap(), &["haiku".to_string()]);

    let main = claude.resolve(Some("claude/haiku")).await.unwrap();
    let from = Some((&main.main.reference, main.main_listing.models.as_slice()));
    let offered = claude.switcher().guardian("claude/haiku", from).await;
    assert!(matches!(offered, Ok(GuardianModel::Ready(_))));
    let refused = claude.switcher().guardian("claude/sonnet", from).await;
    assert!(
        refused.is_err() || matches!(refused, Ok(GuardianModel::Unavailable { .. })),
        "a model outside `models` must not be a ready guardian"
    );
}

#[tokio::test]
async fn a_failed_discovery_makes_the_guardian_unavailable_not_allowed() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    claude.set_env("ANTHROPIC_API_KEY", "x");
    let guardian = claude
        .switcher()
        .guardian("claude/sonnet", None)
        .await
        .unwrap();
    let GuardianModel::Unavailable { reason, .. } = guardian else {
        panic!("a guardian that cannot be discovered must be unavailable");
    };
    assert!(reason.contains("ANTHROPIC_API_KEY"), "{reason}");
}

#[tokio::test]
async fn a_signed_out_guardian_account_is_marked_so_and_never_allows() {
    let claude = Claude::new();
    claude.scenario(json!({"auth": {"loggedIn": false}}));
    let guardian = claude
        .switcher()
        .guardian("claude/sonnet", None)
        .await
        .unwrap();
    let GuardianModel::Unavailable {
        reference,
        reason,
        not_signed_in,
    } = &guardian
    else {
        panic!("a signed-out account cannot be a ready guardian");
    };
    assert!(not_signed_in);
    assert_eq!(reference.account, "claude");
    assert_eq!(reason, "the guardian's account `claude` is not signed in");
    // Another failure of the same account stays plain unavailable.
    claude.scenario(json!({}));
    claude.set_env("ANTHROPIC_API_KEY", "x");
    let guardian = claude
        .switcher()
        .guardian("claude/sonnet", None)
        .await
        .unwrap();
    assert!(matches!(
        guardian,
        GuardianModel::Unavailable {
            not_signed_in: false,
            ..
        }
    ));
}

// ---- request translation --------------------------------------------------------------------

#[tokio::test]
async fn aliases_resolve_to_canonical_ids_and_the_request_is_translated() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "hello there", "usage": {"input_tokens": 21, "output_tokens": 8}}]}));
    let handle = main_handle(&claude).await;
    assert_eq!(handle.reference.model, "sonnet");
    assert_eq!(handle.completion.id(), Some("claude-sonnet-5-5"));
    let call = ToolName::new("read_file").unwrap();
    let request = CompletionRequest::new(Message::tool_result(
        CallId::from_wire("toolu_1"),
        call.clone(),
        "file contents",
    ))
    .preamble("You are a test harness.")
    .messages([
        Message::user("read the file"),
        Message::Assistant(rig_core::message::AssistantMessage::new(vec![
            AssistantContent::tool_call("toolu_1", call, json!({"path": "a.txt"})),
        ])),
    ])
    .tools([read_tool()]);
    let response = handle.completion.call(request).await.unwrap();
    assert_eq!(response.text(), "hello there");
    assert_eq!(response.usage.input_tokens, Some(21));
    assert_eq!(response.usage.output_tokens, Some(8));

    let runs = claude.model_runs();
    assert_eq!(runs.len(), 1);
    let args = argv(&runs[0]);
    assert_eq!(
        flag_value(&args, "--model").as_deref(),
        Some("claude-sonnet-5-5")
    );
    for (flag, value) in [
        ("--input-format", "stream-json"),
        ("--output-format", "stream-json"),
        ("--prompt-suggestions", "false"),
        ("--tools", ""),
        ("--setting-sources", ""),
        ("--max-turns", "1"),
        ("--permission-mode", "dontAsk"),
        ("--permission-prompts", "none"),
    ] {
        assert_eq!(flag_value(&args, flag).as_deref(), Some(value), "{flag}");
    }
    for flag in [
        "-p",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--system-prompt-file",
        "--settings",
        "--mcp-config",
        "--session-id",
    ] {
        assert!(args.contains(&flag.to_string()), "{flag}");
    }
    assert!(!args.contains(&"--bare".to_string()));
    assert!(!args.contains(&"--resume".to_string()));
    assert_eq!(runs[0]["system"], "You are a test harness.");
    let settings: Value = serde_json::from_str(runs[0]["settings"].as_str().unwrap()).unwrap();
    let extra: Value =
        serde_json::from_str(settings["env"]["CLAUDE_CODE_EXTRA_BODY"].as_str().unwrap()).unwrap();
    assert_eq!(extra["tools"][0]["name"], "mcp__fontaine__read_file");
    assert!(
        extra["max_tokens"].as_u64().unwrap() > 0,
        "the catalog default applies"
    );
    let manifest: Value = serde_json::from_str(runs[0]["tools"].as_str().unwrap()).unwrap();
    assert_eq!(manifest[0]["name"], "read_file");
    // The mcp config names the inert inventory, by this executable plus the hidden flag.
    let mcp: Value = serde_json::from_str(runs[0]["mcp"].as_str().unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["fontaine"]["args"][0],
        "--internal-claude-inventory"
    );
    // No credential, endpoint or identity reaches the CLI's settings.
    let all = runs[0].to_string();
    assert!(!all.contains("x-api-key") && !all.contains("user-agent"));

    let frames: Vec<Value> = claude
        .frames()
        .into_iter()
        .filter(|f| f["type"] != "control_request")
        .collect();
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert_eq!(frames[0]["shouldQuery"], false);
    // The first block of a replayed user message is the CLI's own reminder, repeated.
    assert!(
        frames[0]["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("<system-reminder>\n")
    );
    assert_eq!(frames[0]["message"]["content"][1]["text"], "read the file");
    assert_eq!(frames[1]["type"], "assistant");
    assert_eq!(
        frames[1]["message"]["content"][0]["name"],
        "mcp__fontaine__read_file"
    );
    assert!(frames[2].get("shouldQuery").is_none());
    assert_eq!(frames[2]["message"]["content"][0]["type"], "tool_result");
}

/// A conversation of `users` plain user messages, each answered by an assistant message but the
/// last.
fn conversation(users: usize) -> CompletionRequest {
    tagged_conversation("", users)
}

fn has_reminder(frame: &Value) -> bool {
    frame.to_string().contains("system-reminder")
}

/// The blocks that carry a cache marker, as (frame, block) positions.
fn markers(frames: &[Value]) -> Vec<(usize, usize)> {
    let mut at = Vec::new();
    for (frame, f) in frames.iter().enumerate() {
        for (block, b) in f["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if b.get("cache_control").is_some() {
                at.push((frame, block));
            }
        }
    }
    at
}

#[tokio::test]
async fn the_native_layout_replays_the_reminder_and_marks_only_the_final_frame() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "ok"}]}));
    let handle = main_handle(&claude).await;
    handle.completion.call(conversation(3)).await.unwrap();

    let frames = claude.run_frames(1);
    assert_eq!(frames.len(), 5, "{frames:?}");
    for at in [0, 2] {
        assert!(
            frames[at]["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("user@example.com"),
            "{at}"
        );
    }
    assert!(!has_reminder(&frames[1]) && !has_reminder(&frames[3]));
    assert!(!has_reminder(&frames[4]));
    assert_eq!(markers(&frames), [(4, 0)]);
}

#[tokio::test]
async fn an_older_cli_or_an_account_without_an_email_keeps_the_earlier_layout() {
    for scenario in [
        json!({"claude_code_version": "2.1.318", "responses": [{"text": "ok"}]}),
        json!({"claude_code_version": null, "responses": [{"text": "ok"}]}),
        json!({"account": {"subscriptionType": "max"}, "responses": [{"text": "ok"}]}),
        json!({"account": null, "responses": [{"text": "ok"}]}),
    ] {
        let claude = Claude::new();
        claude.scenario(scenario.clone());
        let handle = main_handle(&claude).await;
        handle.completion.call(conversation(3)).await.unwrap();

        let frames = claude.run_frames(1);
        assert_eq!(frames.len(), 5, "{scenario}");
        assert!(frames.iter().all(|f| !has_reminder(f)), "{scenario}");
        assert_eq!(markers(&frames), [(2, 0)], "{scenario}");
    }
}

#[tokio::test]
async fn a_replay_the_cache_did_not_read_back_turns_the_layout_off_for_that_version() {
    let claude = three_steps("2.1.400", 100).await;
    assert!(has_reminder(&claude.run_frames(2)[0]));
    assert_eq!(markers(&claude.run_frames(2)), [(4, 0)]);
    let third = claude.run_frames(3);
    assert!(third.iter().all(|f| !has_reminder(f)));
    assert_eq!(markers(&third), [(4, 0)]);
}

#[tokio::test]
async fn a_replay_the_cache_read_back_keeps_the_layout() {
    let claude = three_steps("2.1.401", 500).await;
    let third = claude.run_frames(3);
    assert!(has_reminder(&third[0]));
    assert_eq!(markers(&third), [(6, 0)]);
}

#[tokio::test]
async fn an_explicit_max_tokens_is_kept_and_unset_uses_the_catalog_default() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "a"}, {"text": "b"}]}));
    let handle = main_handle(&claude).await;
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(77))
        .await
        .unwrap();
    handle
        .completion
        .call(CompletionRequest::new("hi"))
        .await
        .unwrap();
    let runs = claude.model_runs();
    let tokens = |run: &Value| -> u64 {
        let settings: Value = serde_json::from_str(run["settings"].as_str().unwrap()).unwrap();
        let extra: Value =
            serde_json::from_str(settings["env"]["CLAUDE_CODE_EXTRA_BODY"].as_str().unwrap())
                .unwrap();
        extra["max_tokens"].as_u64().unwrap()
    };
    assert_eq!(tokens(&runs[0]), 77);
    assert!(tokens(&runs[1]) > 77);
}

#[tokio::test]
async fn session_ids_are_stable_per_conversation_and_separate_per_caller() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "x"}]}));
    let handle = main_handle(&claude).await;
    let request = || {
        CompletionRequest::new("same task")
            .preamble("same system")
            .max_tokens(8)
    };
    handle.completion.call(request()).await.unwrap();
    handle.completion.call(request()).await.unwrap();
    handle
        .for_agent("agent-1")
        .completion
        .call(request())
        .await
        .unwrap();
    handle
        .for_agent("agent-2")
        .completion
        .call(request())
        .await
        .unwrap();
    handle
        .guardian_scoped()
        .completion
        .call(request())
        .await
        .unwrap();
    // A second Fontaine session with the identical task.
    let other = Switcher::new(
        claude.w.load().unwrap(),
        claude.w.data.clone(),
        claude.env(),
        Some("session-2".to_string()),
    );
    let (other_handle, _) = other.main("claude/sonnet").await.unwrap();
    other_handle.completion.call(request()).await.unwrap();
    let ids: Vec<String> = claude
        .model_runs()
        .iter()
        .map(|run| flag_value(&argv(run), "--session-id").unwrap())
        .collect();
    assert_eq!(ids.len(), 6);
    assert_eq!(ids[0], ids[1], "the same conversation keeps its id");
    let distinct: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        5,
        "root, two agents, guardian and the other session: {ids:?}"
    );
}

#[tokio::test]
async fn a_signed_thinking_block_and_a_tool_call_decode_with_their_names_restored() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{
        "thinking": {"thinking": "let me look", "signature": "sig-abc"},
        "text": "reading",
        "tool_use": {"name": "mcp__fontaine__read_file", "id": "toolu_9", "input": {"path": "README.md"}},
        "denial_echo": true,
        "usage": {"input_tokens": 30, "output_tokens": 12},
        "total_cost_usd": 9.99,
    }]}));
    let handle = main_handle(&claude).await;
    let response = handle
        .completion
        .call(
            CompletionRequest::new("go")
                .max_tokens(64)
                .tools([read_tool()]),
        )
        .await
        .unwrap();
    let calls: Vec<_> = response.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name.as_str(), "read_file");
    assert_eq!(
        Value::Object(calls[0].function.arguments.clone()),
        json!({"path": "README.md"})
    );
    assert_eq!(response.text(), "reading");
    assert_eq!(response.reasoning(), "let me look");
    assert_eq!(response.usage.output_tokens, Some(12));
    // The CLI's list-price `total_cost_usd` is never taken for a cost. (rig's own catalog
    // estimate is dropped where usage is counted, because the account is unpriced.)
    assert!(response.usage.cost.is_none_or(|cost| cost.total < 1.0));
    assert!(!handle.catalog_billed());
}

fn verdict() -> Value {
    json!({"decision": "allow", "rule": "allow", "explanation": "ordinary", "confidence": 0.95})
}

async fn guardian_on(claude: &Claude, alias: &str) -> (fontaine_guardian::Adjudication, Value) {
    let models = claude
        .resolve(Some(&format!("claude/{alias}")))
        .await
        .unwrap();
    let config = claude.w.load().unwrap();
    let guardian = build_guardian(
        Some(&GuardianModel::Ready(models.main.guardian_scoped())),
        &config,
        Profile::Full,
    );
    let result = guardian.adjudicate(&guardian_request()).await;
    let runs = claude.model_runs();
    let settings: Value = serde_json::from_str(runs[0]["settings"].as_str().unwrap()).unwrap();
    let extra: Value =
        serde_json::from_str(settings["env"]["CLAUDE_CODE_EXTRA_BODY"].as_str().unwrap()).unwrap();
    (result, extra)
}

#[tokio::test]
async fn a_native_structured_verdict_is_decoded_and_its_parameters_reach_the_cli() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": verdict().to_string()}]}));
    let (result, extra) = guardian_on(&claude, "sonnet").await;
    assert_eq!(result.fail_closed, None, "{}", result.explanation);
    assert_eq!(result.decision, Decision::Allow);
    assert_eq!(extra["output_config"]["format"]["type"], "json_schema");
    // The thinking parameter, its binding and the beta header that carries it all arrive.
    assert!(extra["thinking"].is_object());
    let run = &claude.model_runs()[0];
    let settings: Value = serde_json::from_str(run["settings"].as_str().unwrap()).unwrap();
    assert!(
        settings["env"]["ANTHROPIC_BETAS"]
            .as_str()
            .unwrap()
            .contains("thinking-binding")
    );
}

#[tokio::test]
async fn a_forced_tool_verdict_is_decoded_through_the_aliased_tool() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{
        "tool_use": {"name": "$FIRST_TOOL", "id": "toolu_g", "input": verdict()},
        "denial_echo": true,
    }]}));
    // A model whose catalog entry has no native output takes a forced tool call.
    let (result, extra) = guardian_on(&claude, "haiku").await;
    assert_eq!(result.fail_closed, None, "{}", result.explanation);
    assert_eq!(result.decision, Decision::Allow);
    let defined = extra["tools"][0]["name"].as_str().unwrap();
    assert!(defined.starts_with("mcp__fontaine__"), "{defined}");
    // The forced choice names exactly the aliased tool the definitions carry.
    let choice = &extra["tool_choice"];
    assert!(
        choice["name"].as_str().is_none_or(|name| name == defined),
        "{choice}"
    );
}

#[tokio::test]
async fn a_guardian_that_gets_a_provider_failure_fails_closed() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"events": ["not json"]}]}));
    let handle = main_handle(&claude).await;
    let config = claude.w.load().unwrap();
    let guardian = build_guardian(
        Some(&GuardianModel::Ready(handle.guardian_scoped())),
        &config,
        Profile::Full,
    );
    let result = guardian.adjudicate(&guardian_request()).await;
    assert_ne!(result.decision, Decision::Allow);
    assert!(result.fail_closed.is_some());
}

fn guardian_request() -> AdjudicationRequest {
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
async fn unsupported_requests_are_refused_without_starting_a_model_process() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let handle = main_handle(&claude).await;
    let request = CompletionRequest::new("a")
        .max_tokens(8)
        .additional_params(json!({"service_tier": "auto"}));
    let error = handle.completion.call(request).await.unwrap_err();
    assert!(error.to_string().contains("service_tier"), "{error}");
    assert!(!error.is_retryable());
    assert!(claude.model_runs().is_empty());
}

#[tokio::test]
async fn compaction_style_requests_carry_the_canonical_model() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "summary"}]}));
    let handle = main_handle(&claude).await;
    assert_eq!(handle.reference.to_string(), "claude/sonnet");
    handle
        .completion
        .call(
            CompletionRequest::new("compact this")
                .preamble("summarize")
                .max_tokens(100),
        )
        .await
        .unwrap();
    let runs = claude.model_runs();
    assert_eq!(
        flag_value(&argv(&runs[0]), "--model").as_deref(),
        Some("claude-sonnet-5-5")
    );
}

// ---- failures, lifecycle ----------------------------------------------------------------------

async fn call_error(claude: &Claude, script: Value) -> rig_core::error::ProviderError {
    claude.scenario(json!({"responses": [script]}));
    let handle = main_handle(claude).await;
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap_err()
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    }
}

#[tokio::test]
async fn a_hang_after_the_result_is_killed_at_the_boundary() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "done", "hang_after_result": true}]}));
    let handle = main_handle(&claude).await;
    let started = std::time::Instant::now();
    let response = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap();
    assert_eq!(response.text(), "done");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[tokio::test]
async fn a_max_tokens_stop_is_returned_truncated_and_the_recovery_never_runs() {
    let claude = Claude::new();
    let pidfile = claude.w.root.join("descendant.pid");
    claude.scenario(json!({"responses": [{
        "descendant": pidfile, "text": "cut off", "stop_reason": "max_tokens",
        "hang_after_result": true,
        // The CLI's output-limit recovery: a synthetic user turn, then another message.
        "after_stop": [
            {"type": "user", "message": {"role": "user", "content": "Output token limit hit."}},
            {"type": "stream_event", "parent_tool_use_id": null,
             "event": {"type": "message_start", "message": {"model": "claude-sonnet-5-5"}}},
        ],
    }]}));
    let handle = main_handle(&claude).await;
    let response = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap();
    assert_eq!(response.text(), "cut off");
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..100 {
        if !pid_alive(pid) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!pid_alive(pid), "the descendant outlived the cut response");
}

#[tokio::test]
async fn a_silent_model_process_is_a_retryable_timeout() {
    let claude = Claude::new();
    claude.set_env("FONTAINE_CLAUDE_READ_IDLE_MS", "400");
    let error = call_error(&claude, json!({"hang_before_output": true})).await;
    assert!(error.is_retryable(), "{error}");
    assert!(error.to_string().contains("no output"), "{error}");
}

#[tokio::test]
async fn an_early_exit_is_truncated_and_retryable_unless_it_is_an_auth_failure() {
    let claude = Claude::new();
    let error = call_error(&claude, json!({"drop_after": 3, "exit_code": 1})).await;
    assert!(error.is_retryable(), "{error}");
    let claude = Claude::new();
    let error = call_error(
        &claude,
        json!({"drop_after": 0, "exit_code": 1, "stderr": "Error: not logged in"}),
    )
    .await;
    assert!(!error.is_retryable(), "{error}");
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Auth));
    assert!(
        !error.to_string().contains("not logged in")
            || error.to_string().contains("claude auth login")
    );
}

#[tokio::test]
async fn native_failures_do_not_echo_raw_output_and_limits_are_not_retried() {
    let claude = Claude::new();
    let error = call_error(
        &claude,
        json!({"events": [
            {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": [],
             "mcp_servers": [{"name": "fontaine", "status": "connected"}], "plugins": [], "skills": []},
            {"type": "result", "subtype": "error_during_execution", "is_error": true,
             "api_error_status": 429, "result": "RAW-SECRET-TEXT"},
        ]}),
    )
    .await;
    assert!(!error.is_retryable());
    assert!(!error.to_string().contains("RAW-SECRET"), "{error}");
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Limit));
    assert!(error.to_string().contains("claude.ai/settings/usage"));

    let claude = Claude::new();
    let error = call_error(
        &claude,
        json!({"events": [
            {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": [],
             "mcp_servers": [{"name": "fontaine", "status": "connected"}], "plugins": [], "skills": []},
            {"type": "result", "subtype": "error_during_execution", "is_error": true,
             "api_error_status": 529},
        ]}),
    )
    .await;
    assert!(error.is_retryable(), "{error}");
}

#[tokio::test]
async fn protocol_violations_are_refused_and_never_retried() {
    for script in [
        json!({"events": ["not json"]}),
        json!({"text": "x", "after_stop": [{"type": "prompt_suggestion", "suggestion": "more"}]}),
        json!({"text": "x", "before": [{"type": "system", "subtype": "api_retry"}]}),
        json!({"text": "x", "result_subtype": "error_max_turns"}),
    ] {
        let claude = Claude::new();
        let error = call_error(&claude, script.clone()).await;
        assert!(!error.is_retryable(), "{script}: {error}");
    }
}

async fn refused_before_any_query(claude: &Claude, handle: &models::ModelHandle) -> ErrorKind {
    let error = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(8))
        .await
        .unwrap_err();
    assert!(!error.is_retryable(), "{error}");
    assert!(
        claude.query_frames().is_empty(),
        "a query frame was written: {:?}",
        claude.frames()
    );
    claude_error(&error).expect("a Claude CLI error").kind
}

#[tokio::test]
async fn a_failed_check_is_refused_before_any_query_frame() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    claude.set_env("FONTAINE_CLAUDE_PROBE_MS", "1500");
    let handle = main_handle(&claude).await;

    claude.scenario(json!({"models": [
        {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "usesUsageCredits": true},
    ]}));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::UsageCredits
    );

    claude.scenario(json!({"models": [{"value": "sonnet", "resolvedModel": "claude-opus-5"}]}));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::Catalog
    );

    claude.scenario(json!({"models": [{"value": "haiku", "resolvedModel": "claude-haiku-4-5"}]}));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::Catalog
    );

    claude.scenario(json!({"auth": {"loggedIn": false}}));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::NotSignedIn
    );

    claude.scenario(json!({"init_hang": true}));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::SetupTimeout
    );
}

#[tokio::test]
async fn a_login_lost_mid_session_names_the_command_that_signs_in() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let handle = main_handle(&claude).await;
    claude.scenario(json!({"auth": {"loggedIn": false}}));
    let error = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(8))
        .await
        .unwrap_err();
    assert_eq!(
        claude_error(&error).map(|e| e.kind),
        Some(ErrorKind::NotSignedIn)
    );
    let text = error.to_string();
    assert!(
        text.contains("CLAUDE_CONFIG_DIR=") && text.contains("auth login --claudeai"),
        "{text}"
    );
}

#[test]
fn the_login_command_follows_the_account_and_the_environment() {
    let none: models::SharedEnv = std::sync::Arc::new(|_| None);
    let bare = login_command(None, None, &none);
    assert_eq!(bare.display, "claude auth login --claudeai");
    assert!(bare.env.is_empty());

    let from_env: models::SharedEnv = std::sync::Arc::new(|name| {
        (name == "CLAUDE_CONFIG_DIR").then(|| std::ffi::OsString::from("/profile"))
    });
    let inherited = login_command(None, None, &from_env);
    assert_eq!(
        inherited.display,
        "CLAUDE_CONFIG_DIR=/profile claude auth login --claudeai"
    );
    // The account's own directory wins over the environment's.
    let own = login_command(
        Some(std::path::Path::new("/opt/claude")),
        Some(std::path::Path::new("/own")),
        &from_env,
    );
    assert_eq!(
        own.display,
        "CLAUDE_CONFIG_DIR=/own /opt/claude auth login --claudeai"
    );
    assert_eq!(own.program, std::path::PathBuf::from("/opt/claude"));
}

#[tokio::test]
async fn a_signed_out_login_outranks_a_credit_billed_row() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let handle = main_handle(&claude).await;
    claude.scenario(json!({
        "auth": {"loggedIn": false},
        "models": [{"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "usesUsageCredits": true}],
    }));
    assert_eq!(
        refused_before_any_query(&claude, &handle).await,
        ErrorKind::NotSignedIn
    );
}

#[tokio::test]
async fn a_request_lists_models_on_its_own_process() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "a"}, {"text": "b"}]}));
    let handle = main_handle(&claude).await;
    for prompt in ["one", "two"] {
        handle
            .completion
            .call(CompletionRequest::new(prompt).max_tokens(8))
            .await
            .unwrap();
    }
    // Discovery's listing process, then exactly one process per request.
    assert_eq!(claude.invocations().len(), 3);
    assert_eq!(claude.model_runs().len(), 2);
    let mut expecting_initialize = false;
    for entry in claude.log() {
        if entry["kind"] == "invocation" {
            expecting_initialize = argv(&entry).contains(&"--model".to_string());
        } else if expecting_initialize {
            assert_eq!(entry["value"]["type"], "control_request", "{entry}");
            assert_eq!(entry["value"]["request"]["subtype"], "initialize");
            expecting_initialize = false;
        }
    }
}

#[tokio::test]
async fn every_process_runs_in_one_stable_empty_directory() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let handle = main_handle(&claude).await;
    let request = || CompletionRequest::new("hi").max_tokens(8);
    for _ in 0..2 {
        handle.completion.call(request()).await.unwrap();
        handle
            .guardian_scoped()
            .completion
            .call(request())
            .await
            .unwrap();
        handle
            .for_agent("agent-1")
            .completion
            .call(request())
            .await
            .unwrap();
    }
    let workdir = claude.w.data.join("claude-cli-cwd");
    let canonical = std::fs::canonicalize(&workdir).unwrap();
    let invocations = claude.invocations();
    assert_eq!(invocations.len(), 7);
    for invocation in &invocations {
        let cwd = std::fs::canonicalize(invocation["cwd"].as_str().unwrap()).unwrap();
        assert_eq!(cwd, canonical);
        let args = argv(invocation);
        let settings = PathBuf::from(flag_value(&args, "--settings").unwrap());
        let files = settings.parent().unwrap();
        assert!(
            files != workdir && files != canonical,
            "the per-launch files live elsewhere"
        );
    }
    assert_eq!(std::fs::read_dir(&workdir).unwrap().count(), 0);

    // An instruction file refuses the launch before any process starts.
    std::fs::write(workdir.join("notes.txt"), "x").unwrap();
    handle.completion.call(request()).await.unwrap();
    std::fs::write(workdir.join("CLAUDE.md"), "x").unwrap();
    let started = claude.invocations().len();
    let error = handle.completion.call(request()).await.unwrap_err();
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Cli));
    assert!(error.to_string().contains("claude-cli-cwd"), "{error}");
    assert_eq!(claude.invocations().len(), started);

    // Case-insensitive filesystems resolve a differently cased name to the same file.
    std::fs::remove_file(workdir.join("CLAUDE.md")).unwrap();
    std::fs::create_dir(workdir.join(".Claude")).unwrap();
    let error = handle.completion.call(request()).await.unwrap_err();
    assert_eq!(claude_error(&error).map(|e| e.kind), Some(ErrorKind::Cli));
    assert_eq!(claude.invocations().len(), started);
}

#[tokio::test]
async fn a_non_empty_directory_fails_connect() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    let workdir = claude.w.data.join("claude-cli-cwd");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::write(workdir.join("stray"), "x").unwrap();
    let error = claude.resolve(None).await.unwrap_err();
    assert!(message(&error).contains("claude-cli-cwd"), "{error}");
    assert!(message(&error).contains("not empty"), "{error}");
}

#[tokio::test]
async fn dropping_a_stream_kills_the_process_tree_and_removes_the_directory() {
    use futures::StreamExt;
    let claude = Claude::new();
    let pidfile = claude.w.root.join("descendant.pid");
    claude.scenario(json!({"responses": [{
        "descendant": pidfile, "text": "begin", "hang_after_result": true,
        // Stay silent after a start so the stream is mid-flight when dropped.
        "events": [
            {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": [],
             "mcp_servers": [{"name": "fontaine", "status": "connected"}], "plugins": [], "skills": []},
            {"type": "stream_event", "parent_tool_use_id": null,
             "event": {"type": "message_start", "message": {"model": "claude-sonnet-5-5", "usage": {"input_tokens": 1}}}},
        ],
    }]}));
    let handle = main_handle(&claude).await;
    let mut stream = handle
        .completion
        .stream(CompletionRequest::new("hi").max_tokens(16))
        .unwrap();
    // Read until something arrives, then drop mid-flight.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await;
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid_alive(pid));
    let run = &claude.model_runs()[0];
    let settings = PathBuf::from(flag_value(&argv(run), "--settings").unwrap());
    let cwd = settings.parent().unwrap().to_path_buf();
    assert!(cwd.exists());
    drop(stream);
    for _ in 0..100 {
        if !pid_alive(pid) && !cwd.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!pid_alive(pid), "the descendant outlived the stream");
    assert!(!cwd.exists(), "the temporary directory was not removed");
}

#[tokio::test]
async fn output_arriving_in_odd_chunks_with_crlf_decodes() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "chunked reply", "chunked": true}]}));
    let handle = main_handle(&claude).await;
    let response = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap();
    assert_eq!(response.text(), "chunked reply");
}

#[tokio::test]
async fn unexpected_init_customization_is_refused() {
    for extra in [
        json!({"plugins": [{"name": "foo", "path": "/p", "source": "foo@market"}]}),
        json!({"skills": ["foo"]}),
        json!({"tools": ["Bash"]}),
        json!({"mcp_servers": [{"name": "fontaine", "status": "failed"}]}),
        json!({"model": "claude-opus-5"}),
    ] {
        let claude = Claude::new();
        claude.scenario(json!({"init_extra": extra, "responses": [{"text": "x"}]}));
        let handle = main_handle(&claude).await;
        let error = handle
            .completion
            .call(CompletionRequest::new("hi").max_tokens(8))
            .await
            .unwrap_err();
        assert!(!error.is_retryable(), "{extra}: {error}");
        if extra.get("plugins").is_some() {
            assert!(error.to_string().contains("foo@market"), "{error}");
        }
    }
}

#[tokio::test]
async fn builtin_cli_plugins_are_accepted() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"text": "hello"}]}));
    let handle = main_handle(&claude).await;
    let response = handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(16))
        .await
        .unwrap();
    assert_eq!(response.text(), "hello");
}

#[tokio::test]
async fn a_stale_stored_credential_is_never_passed_to_the_cli() {
    let claude = Claude::new();
    claude.scenario(json!({}));
    fontaine_core::credentials::store(&claude.w.data, "claude", "sk-stale-key").unwrap();
    let handle = main_handle(&claude).await;
    handle
        .completion
        .call(CompletionRequest::new("hi").max_tokens(8))
        .await
        .unwrap();
    assert!(
        !claude
            .log()
            .iter()
            .any(|e| e.to_string().contains("sk-stale-key"))
    );
}

#[tokio::test]
async fn the_external_check_reports_the_login_instruction() {
    let claude = Claude::new();
    claude.scenario(json!({"auth": {"loggedIn": false}}));
    let config = claude.w.load().unwrap();
    let network = config.loaded_network(Some("claude")).unwrap();
    let error = accounts::external_check(
        "claude",
        &config.accounts["claude"],
        &claude.w.data,
        &network,
        claude.env(),
    )
    .await
    .unwrap_err();
    let ExternalCheckError::NotSignedIn { account, login, .. } = &error else {
        panic!("a signed-out CLI is its own failure: {error}");
    };
    assert_eq!(account, "claude");
    assert_eq!(login.program, double());
    assert_eq!(
        login.args,
        ["auth", "login", "--claudeai"].map(std::ffi::OsString::from)
    );
    assert_eq!(
        login.env,
        [(
            std::ffi::OsString::from("CLAUDE_CONFIG_DIR"),
            claude.dir.clone().into_os_string()
        )]
    );
    assert!(
        login.display.contains("CLAUDE_CONFIG_DIR=")
            && login.display.ends_with("auth login --claudeai"),
        "{}",
        login.display
    );
    assert!(!error.to_string().contains("login"), "{error}");
    claude.scenario(json!({}));
    let summary = accounts::external_check(
        "claude",
        &config.accounts["claude"],
        &claude.w.data,
        &network,
        claude.env(),
    )
    .await
    .unwrap();
    assert!(summary.contains("3 model"), "{summary}");
    assert!(accounts::external_log_out_summary("claude").contains("claude auth logout"));
}

fn search_events(extra: Vec<Value>) -> Vec<Value> {
    let stream =
        |event: Value| json!({"type": "stream_event", "parent_tool_use_id": null, "event": event});
    let mut events = vec![
        json!({"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": [],
               "mcp_servers": [{"name": "fontaine", "status": "connected"}], "plugins": [], "skills": []}),
        stream(
            json!({"type": "message_start", "message": {"model": "claude-sonnet-5-5",
                      "usage": {"input_tokens": 9, "output_tokens": 1}}}),
        ),
        stream(
            json!({"type": "content_block_start", "index": 0, "content_block":
                      {"type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search", "input": {}}}),
        ),
        stream(json!({"type": "content_block_delta", "index": 0, "delta":
                      {"type": "input_json_delta", "partial_json": "{\"query\": \"rust edition\"}"}})),
        stream(json!({"type": "content_block_stop", "index": 0})),
        stream(
            json!({"type": "content_block_start", "index": 1, "content_block":
                      {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01", "content": [
                       {"type": "web_search_result", "url": "https://example.com/a", "title": "A",
                        "encrypted_content": "EqgfCioIAxgC", "page_age": null}]}}),
        ),
        stream(json!({"type": "content_block_stop", "index": 1})),
        stream(
            json!({"type": "content_block_start", "index": 2, "content_block":
                      {"type": "text", "text": "", "citations": []}}),
        ),
        stream(json!({"type": "content_block_delta", "index": 2, "delta":
                      {"type": "text_delta", "text": "It shipped."}})),
        stream(json!({"type": "content_block_delta", "index": 2, "delta":
                      {"type": "citations_delta", "citation": {
                       "type": "web_search_result_location", "url": "https://example.com/a",
                       "title": "A", "cited_text": "shipped", "encrypted_index": "Eo8B"}}})),
        stream(json!({"type": "content_block_stop", "index": 2})),
    ];
    events.extend(extra);
    events.extend([
        stream(
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                      "usage": {"output_tokens": 12}}),
        ),
        stream(json!({"type": "message_stop"})),
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
               "result": "", "usage": {"input_tokens": 9, "output_tokens": 12}}),
    ]);
    events
}

#[tokio::test]
async fn the_search_entry_reaches_the_cli_and_a_search_response_decodes() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"events": search_events(Vec::new())}]}));
    let handle = main_handle(&claude).await;
    let entry = json!({"type": "web_search_20250305", "name": "web_search"});
    let response = handle
        .completion
        .call(
            CompletionRequest::new("what shipped?")
                .max_tokens(64)
                .tools([read_tool()])
                .additional_params(json!({"tools": [entry.clone()]})),
        )
        .await
        .unwrap();
    assert_eq!(response.text(), "It shipped.");
    let content: Vec<_> = response.choice.iter().collect();
    let [
        AssistantContent::Opaque(call),
        AssistantContent::Opaque(result),
        AssistantContent::Text(text),
    ] = content.as_slice()
    else {
        panic!("a server call, its result and the text: {content:?}");
    };
    assert_eq!(call.kind(), Some("server_tool_use"));
    assert_eq!(call.item["id"], "srvtoolu_01");
    assert_eq!(result.kind(), Some("web_search_tool_result"));
    assert_eq!(
        result.item["content"][0]["encrypted_content"],
        "EqgfCioIAxgC"
    );
    let [citation] = text.citations() else {
        panic!("one citation: {:?}", text.citations());
    };
    assert_eq!(citation.sources[0].cited_text.as_deref(), Some("shipped"));

    let run = &claude.model_runs()[0];
    let settings: Value = serde_json::from_str(run["settings"].as_str().unwrap()).unwrap();
    let extra: Value =
        serde_json::from_str(settings["env"]["CLAUDE_CODE_EXTRA_BODY"].as_str().unwrap()).unwrap();
    assert_eq!(extra["tools"][0]["name"], "mcp__fontaine__read_file");
    assert_eq!(extra["tools"][1], entry);
    let manifest: Value = serde_json::from_str(run["tools"].as_str().unwrap()).unwrap();
    assert_eq!(manifest.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_search_the_request_did_not_offer_is_refused() {
    let claude = Claude::new();
    claude.scenario(json!({"responses": [{"events": search_events(Vec::new())}]}));
    let handle = main_handle(&claude).await;
    let error = handle
        .completion
        .call(CompletionRequest::new("go").max_tokens(64))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("native tool or hosted content"),
        "{error}"
    );
}
