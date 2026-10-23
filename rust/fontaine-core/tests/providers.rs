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
    assert!(err.contains("fontaine auth login"), "{err}");
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
