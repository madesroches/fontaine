//! The provider registry through the settings schema.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::World;
use fontaine_core::credentials::{OAuthTokens, StoredCredential};
use fontaine_core::providers::ConnectContext;

use fontaine_core::config::{AccountKind, Column, parse_settings};
use fontaine_core::providers;

fn parse(text: &str) -> Result<fontaine_core::config::Settings, String> {
    parse_settings(text, Path::new("user.toml"), Column::User).map_err(|e| e.to_string())
}

#[test]
fn every_registered_name_round_trips_through_the_kind() {
    for name in providers::names() {
        // A kind whose login and endpoint belong to an external program has no URL to set.
        let endpoint = if name == "claude-subscription" {
            ""
        } else {
            "base_url = \"http://localhost:1/v1\"\n"
        };
        let settings = parse(&format!("[accounts.x]\nkind = \"{name}\"\n{endpoint}"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let kind = settings.accounts.unwrap()["x"].kind;
        assert_eq!(kind.as_str(), name);
        assert_eq!(kind, AccountKind::named(name));
        assert_eq!(kind.provider().name(), name);
    }
}

#[test]
fn an_unknown_kind_lists_every_registered_name() {
    let err = parse("[accounts.x]\nkind = \"mystery\"\n").unwrap_err();
    assert!(err.contains("mystery"), "{err}");
    for name in providers::names() {
        assert!(err.contains(name), "{err}");
    }
}

#[test]
fn opencode_go_refuses_api_key_env_and_points_at_the_sign_in() {
    let err =
        parse("[accounts.go]\nkind = \"opencode-go\"\napi_key_env = \"GO_KEY\"\n").unwrap_err();
    assert!(err.contains("accounts.go.api_key_env"), "{err}");
    assert!(err.contains("/login"), "{err}");
}

#[test]
fn wires_belong_to_opencode_go_only() {
    let ok = parse(
        "[accounts.go]\nkind = \"opencode-go\"\n\n[accounts.go.wires]\n\"kimi-k3\" = \"messages\"\n",
    )
    .unwrap();
    let wires = ok.accounts.unwrap()["go"].wires.clone().unwrap();
    assert_eq!(wires["kimi-k3"], fontaine_core::config::Wire::Messages);

    for kind in [
        "anthropic",
        "openai",
        "gemini",
        "openai-compatible",
        "kimi",
        "ollama",
        "openrouter",
    ] {
        let err = parse(&format!(
            "[accounts.x]\nkind = \"{kind}\"\nbase_url = \"http://localhost:1\"\n\n[accounts.x.wires]\n\"m\" = \"chat\"\n"
        ))
        .unwrap_err();
        assert!(err.contains("accounts.x.wires"), "{kind}: {err}");
    }
    assert!(
        parse(
            "[accounts.go]\nkind = \"opencode-go\"\n\n[accounts.go.wires]\nm = \"carrier-pigeon\"\n"
        )
        .is_err()
    );
}

#[test]
fn prompt_caching_keys_belong_to_the_kinds_that_accept_them() {
    for kind in ["anthropic", "opencode-go", "openrouter"] {
        let ok = parse(&format!(
            "[accounts.x]\nkind = \"{kind}\"\nprompt_caching = true\ncache_ttl = \"1h\"\n"
        ))
        .unwrap_or_else(|e| panic!("{kind}: {e}"));
        let account = ok.accounts.unwrap()["x"].clone();
        assert_eq!(account.prompt_caching, Some(true));
        assert_eq!(
            account.cache_ttl,
            Some(fontaine_core::config::CacheTtl::OneHour)
        );
    }
    for kind in ["openai", "gemini", "openai-compatible", "kimi", "ollama"] {
        for key in ["prompt_caching = false", "cache_ttl = \"5m\""] {
            let err = parse(&format!(
                "[accounts.x]\nkind = \"{kind}\"\nbase_url = \"http://localhost:1\"\n{key}\n"
            ))
            .unwrap_err();
            let name = key.split(' ').next().unwrap();
            assert!(err.contains(&format!("accounts.x.{name}")), "{kind}: {err}");
        }
    }
    assert!(parse("[accounts.x]\nkind = \"anthropic\"\ncache_ttl = \"2h\"\n").is_err());
}

#[test]
fn prompt_caching_defaults_per_kind() {
    for (kind, default) in [
        ("anthropic", true),
        ("opencode-go", false),
        ("openrouter", true),
        ("openai", false),
        ("gemini", false),
        ("openai-compatible", false),
        ("kimi", false),
        ("ollama", false),
    ] {
        let provider = providers::find(kind).unwrap();
        assert_eq!(provider.default_prompt_caching(), default, "{kind}");
        assert_eq!(
            provider.accepts_prompt_caching(),
            matches!(kind, "anthropic" | "opencode-go" | "openrouter"),
            "{kind}"
        );
    }
}

#[test]
fn project_scope_cannot_set_prompt_caching() {
    let err = parse_settings(
        "[accounts.x]\nkind = \"anthropic\"\nprompt_caching = false\n",
        Path::new("project.toml"),
        Column::Project,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("accounts"), "{err}");
}

#[test]
fn openai_compatible_still_needs_a_base_url() {
    let err = parse("[accounts.x]\nkind = \"openai-compatible\"\n").unwrap_err();
    assert!(err.contains("accounts.x.base_url"), "{err}");
}

#[test]
fn kimi_needs_no_base_url_and_always_carries_its_quota() {
    let w = World::new();
    w.user_file("[accounts.k]\nkind = \"kimi\"\n");
    let config = w.load().unwrap();
    let network = config.loaded_network(Some("k")).unwrap();
    let client = providers::find("kimi")
        .unwrap()
        .connect(ConnectContext {
            account_name: "k",
            account: &config.accounts["k"],
            credential: None,
            data_dir: &w.data,
            session_id: None,
            network: &network,
            env: std::sync::Arc::new(|_| None),
            warn: providers::warn::Warn::process(),
        })
        .expect("the kind connects");
    assert!(config.accounts["k"].base_url.is_none());
    assert!(client.quota_source().is_some());
}

#[test]
fn project_scope_cannot_define_accounts() {
    let err = parse_settings(
        "[accounts.go]\nkind = \"opencode-go\"\nbase_url = \"http://evil.test\"\n",
        Path::new("project.toml"),
        Column::Project,
    );
    assert!(err.is_err());
}

#[test]
fn model_options_belong_to_ollama_only() {
    let ok =
        parse("[accounts.l]\nkind = \"ollama\"\ncontext_tokens = 32768\nthink = false\n").unwrap();
    let account = &ok.accounts.unwrap()["l"];
    assert_eq!(account.context_tokens, Some(32768));
    assert_eq!(account.think, Some(false));
    assert_eq!(account.ollama_context_tokens(), 32768);

    let default = parse("[accounts.l]\nkind = \"ollama\"\n").unwrap();
    assert_eq!(
        default.accounts.unwrap()["l"].ollama_context_tokens(),
        fontaine_core::config::DEFAULT_OLLAMA_CONTEXT_TOKENS
    );
    assert_eq!(fontaine_core::config::DEFAULT_OLLAMA_CONTEXT_TOKENS, 16384);

    for key in ["context_tokens = 8192", "think = false"] {
        for kind in [
            "anthropic",
            "openai",
            "gemini",
            "openai-compatible",
            "kimi",
            "opencode-go",
            "openrouter",
        ] {
            let err = parse(&format!(
                "[accounts.x]\nkind = \"{kind}\"\nbase_url = \"http://localhost:1\"\n{key}\n"
            ))
            .unwrap_err();
            let name = key.split(' ').next().unwrap();
            assert!(err.contains(&format!("accounts.x.{name}")), "{kind}: {err}");
            assert!(err.contains("not used by kind"), "{kind}: {err}");
        }
    }
}

#[test]
fn quota_belongs_to_openai_compatible_only() {
    let ok = parse(
        "[accounts.k]\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:1\"\nquota = \"kimi\"\n",
    )
    .unwrap();
    assert_eq!(
        ok.accounts.unwrap()["k"].quota,
        Some(fontaine_core::config::QuotaKind::Kimi)
    );
    for kind in [
        "anthropic",
        "openai",
        "gemini",
        "kimi",
        "ollama",
        "opencode-go",
        "openrouter",
    ] {
        let err = parse(&format!(
            "[accounts.x]\nkind = \"{kind}\"\nbase_url = \"http://localhost:1\"\nquota = \"kimi\"\n"
        ))
        .unwrap_err();
        assert!(err.contains("accounts.x.quota"), "{kind}: {err}");
        assert!(err.contains("not used by kind"), "{kind}: {err}");
    }
    let err = parse(
        "[accounts.k]\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:1\"\nquota = \"other\"\n",
    )
    .unwrap_err();
    assert!(err.contains("quota") || err.contains("other"), "{err}");
}

#[test]
fn a_tiny_context_window_is_refused() {
    let err = parse("[accounts.l]\nkind = \"ollama\"\ncontext_tokens = 512\n").unwrap_err();
    assert!(err.contains("accounts.l.context_tokens"), "{err}");
    assert!(parse("[accounts.l]\nkind = \"ollama\"\ncontext_tokens = 2048\n").is_ok());
}

#[test]
fn claude_subscription_takes_its_two_paths_and_refuses_credentials_and_endpoints() {
    let abs = if cfg!(windows) {
        "C:\\\\claude"
    } else {
        "/usr/bin/claude"
    };
    let ok = parse(&format!(
        "[accounts.c]\nkind = \"claude-subscription\"\ncli_path = \"{abs}\"\nclaude_config_dir = \"{abs}\"\nmodels = [\"sonnet\"]\n"
    ))
    .unwrap();
    let account = ok.accounts.unwrap()["c"].clone();
    assert!(account.cli_path.is_some() && account.claude_config_dir.is_some());
    for (line, key) in [
        ("api_key_env = \"K\"", "api_key_env"),
        ("base_url = \"http://x\"", "base_url"),
        ("https_proxy = \"http://p:1\"", "https_proxy"),
        ("cli_path = \"relative\"", "cli_path"),
    ] {
        let err = parse(&format!(
            "[accounts.c]\nkind = \"claude-subscription\"\n{line}\n"
        ))
        .unwrap_err();
        assert!(err.contains(&format!("accounts.c.{key}")), "{line}: {err}");
    }
    // The paths exist for this kind only.
    let err = parse("[accounts.o]\nkind = \"ollama\"\nclaude_config_dir = \"/x\"\n").unwrap_err();
    assert!(err.contains("accounts.o.claude_config_dir"), "{err}");
}

/// The vendor whose catalog an account's ids are looked up in, and an unlisted id derives facts
/// from. Only a kind with no vendor of its own (an arbitrary gateway) has none.
#[test]
fn every_kind_but_the_arbitrary_gateway_names_a_catalog_vendor() {
    let expected = |name: &str| match name {
        "anthropic" | "claude-subscription" => Some("anthropic"),
        "openai" => Some("openai"),
        "gemini" => Some("gcp.gemini"),
        "ollama" => Some("ollama"),
        "opencode-go" => Some("opencode-go"),
        "openrouter" => Some("openrouter"),
        "openai-compatible" => None,
        "kimi" => None,
        other => panic!("say which catalog vendor `{other}` looks its ids up under"),
    };
    for name in providers::names() {
        let w = World::new();
        let endpoint = if name == "claude-subscription" {
            ""
        } else {
            "base_url = \"http://localhost:1/v1\"\n"
        };
        w.user_file(&format!("[accounts.x]\nkind = \"{name}\"\n{endpoint}"));
        let config = w.load().unwrap();
        let network = config.loaded_network(Some("x")).unwrap();
        let credential = Some(if name == "opencode-go" {
            StoredCredential::OAuth(OAuthTokens {
                access: "a".into(),
                refresh: "r".into(),
                expires_at: u64::MAX,
                extra: BTreeMap::new(),
            })
        } else {
            StoredCredential::ApiKey("k".into())
        });
        let client = providers::find(name)
            .unwrap()
            .connect(ConnectContext {
                account_name: "x",
                account: &config.accounts["x"],
                credential,
                data_dir: &w.data,
                session_id: None,
                network: &network,
                env: std::sync::Arc::new(|_| None),
                warn: providers::warn::Warn::process(),
            })
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            client.catalog_vendor().map(|vendor| vendor.vendor()),
            expected(name),
            "{name}"
        );
    }
}
