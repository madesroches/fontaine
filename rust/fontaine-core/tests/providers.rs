//! The provider registry through the settings schema.

use std::path::Path;

use fontaine_core::config::{AccountKind, Column, parse_settings};
use fontaine_core::providers;

fn parse(text: &str) -> Result<fontaine_core::config::Settings, String> {
    parse_settings(text, Path::new("user.toml"), Column::User).map_err(|e| e.to_string())
}

#[test]
fn every_registered_name_round_trips_through_the_kind() {
    for name in providers::names() {
        let settings = parse(&format!(
            "[accounts.x]\nkind = \"{name}\"\nbase_url = \"http://localhost:1/v1\"\n"
        ))
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
        "ollama",
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
    for kind in ["anthropic", "opencode-go"] {
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
    for kind in ["openai", "gemini", "openai-compatible", "ollama"] {
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
        ("openai", false),
        ("gemini", false),
        ("openai-compatible", false),
        ("ollama", false),
    ] {
        let provider = providers::find(kind).unwrap();
        assert_eq!(provider.default_prompt_caching(), default, "{kind}");
        assert_eq!(
            provider.accepts_prompt_caching(),
            matches!(kind, "anthropic" | "opencode-go"),
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
            "opencode-go",
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
fn a_tiny_context_window_is_refused() {
    let err = parse("[accounts.l]\nkind = \"ollama\"\ncontext_tokens = 512\n").unwrap_err();
    assert!(err.contains("accounts.l.context_tokens"), "{err}");
    assert!(parse("[accounts.l]\nkind = \"ollama\"\ncontext_tokens = 2048\n").is_ok());
}
