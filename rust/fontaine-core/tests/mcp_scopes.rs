//! MCP servers from the local file and the repository's `.mcp.json`: where they load as, what
//! wins a name, and which of them wait for approval.

mod common;

use common::World;
use fontaine_core::agent::mcp_plan;
use fontaine_core::config::Slot;

fn local_file(w: &World, body: &str) {
    let root = toml::Value::String(w.work.to_string_lossy().into_owned()).to_string();
    w.config_file(
        "mcp-local.toml",
        &format!("[projects.{root}.servers.{body}"),
    );
}

#[test]
fn a_repository_mcp_json_loads_as_project_scope_and_needs_approval() {
    let w = World::new();
    w.mcp_json(r#"{"mcpServers":{"repo":{"type":"stdio","command":"srv","args":["${A}"]}}}"#);
    let config = w.load().unwrap();
    assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    assert_eq!(config.mcp_origins["repo"].slot, Slot::Project);
    assert!(!config.mcp_origins["repo"].user_trust);
    assert!(config.mcp_servers["repo"].expand);
    // The text stays raw in the configuration.
    assert_eq!(
        config.mcp_servers["repo"].args,
        Some(vec!["${A}".to_string()])
    );
    let plan = mcp_plan(&config, &w.work, &w.data, true, true);
    assert_eq!(plan.len(), 1);
    assert!(plan[0].needs_approval && plan[0].project);
    assert!(
        plan[0].description.contains("environment: A"),
        "{}",
        plan[0].description
    );
    // Without workspace trust it is not planned at all.
    assert!(mcp_plan(&config, &w.work, &w.data, false, true).is_empty());
}

#[test]
fn the_settings_toml_wins_a_collision_with_the_mcp_json_and_it_warns() {
    let w = World::new();
    w.mcp_json(r#"{"mcpServers":{"both":{"command":"from-json"}}}"#);
    w.project_file("[mcp.servers.both]\ncommand = \"from-toml\"\n");
    let config = w.load().unwrap();
    assert_eq!(
        config.mcp_servers["both"].command.as_deref(),
        Some("from-toml")
    );
    assert!(!config.mcp_servers["both"].expand);
    assert_eq!(config.warnings.len(), 1, "{:?}", config.warnings);
    assert!(
        config.warnings[0].contains("`both`"),
        "{:?}",
        config.warnings
    );
}

#[test]
fn a_local_entry_for_this_root_loads_without_approval_even_when_project_executables_are_off() {
    let w = World::new();
    local_file(&w, "mine]\ncommand = \"srv\"\n");
    let config = w.load().unwrap();
    assert_eq!(config.mcp_origins["mine"].slot, Slot::Local);
    assert!(config.mcp_origins["mine"].user_trust);
    assert!(!config.mcp_origins["mine"].is_project());
    for executables in [true, false] {
        let plan = mcp_plan(&config, &w.work, &w.data, executables, true);
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].needs_approval && !plan[0].project);
    }
    let slots: Vec<_> = config.scopes.iter().map(|s| s.slot.label()).collect();
    assert_eq!(
        slots,
        ["managed", "local", "project-local", "project", "user"]
    );
}

#[test]
fn an_entry_for_another_root_is_ignored() {
    let w = World::new();
    w.config_file(
        "mcp-local.toml",
        "[projects.\"/somewhere/else\".servers.mine]\ncommand = \"srv\"\n",
    );
    let config = w.load().unwrap();
    assert!(config.mcp_servers.is_empty());
    assert!(config.warnings.is_empty());
}

#[test]
fn local_beats_project_and_user_by_name_and_the_cli_layer_beats_local() {
    let w = World::new();
    w.user_file("[mcp.servers.n]\ncommand = \"user\"\n");
    w.mcp_json(r#"{"mcpServers":{"n":{"command":"json"}}}"#);
    let config = w.load().unwrap();
    assert_eq!(config.mcp_servers["n"].command.as_deref(), Some("json"));
    w.project_file("[mcp.servers.n]\ncommand = \"project\"\n");
    let config = w.load().unwrap();
    assert_eq!(config.mcp_servers["n"].command.as_deref(), Some("project"));
    local_file(&w, "n]\ncommand = \"local\"\n");
    let config = w.load().unwrap();
    assert_eq!(config.mcp_servers["n"].command.as_deref(), Some("local"));
    assert_eq!(config.mcp_origins["n"].slot, Slot::Local);
    let cli = w.cli_file("[mcp.servers.n]\ncommand = \"cli\"\n");
    let config = w.load_with_settings(&cli).unwrap();
    assert_eq!(config.mcp_servers["n"].command.as_deref(), Some("cli"));
}

#[test]
fn malformed_files_and_entries_warn_and_are_skipped() {
    let w = World::new();
    w.mcp_json(r#"{"mcpServers":{"good":{"command":"x"},"bad__name":{"command":"x"},"web":{"type":"sse","url":"https://h"}}}"#);
    let root = toml::Value::String(w.work.to_string_lossy().into_owned()).to_string();
    w.config_file(
        "mcp-local.toml",
        &format!(
            "[projects.{root}.servers.ok]\ncommand = \"x\"\n[projects.{root}.servers.both]\ncommand = \"x\"\nurl = \"u\"\n"
        ),
    );
    let config = w.load().unwrap();
    let names: Vec<_> = config.mcp_servers.keys().cloned().collect();
    assert_eq!(names, ["good", "ok"]);
    assert_eq!(config.warnings.len(), 3, "{:?}", config.warnings);

    w.mcp_json("{nope");
    w.config_file("mcp-local.toml", "= broken");
    let config = w.load().unwrap();
    assert!(config.mcp_servers.is_empty());
    assert_eq!(config.warnings.len(), 2, "{:?}", config.warnings);
}

mod start {
    use std::ffi::OsString;
    use std::path::Path;

    use fontaine_core::agent::{EnvLookup, server_digest, server_spec};
    use fontaine_core::config::Config;
    use fontaine_core::tools::scrubbed_env;
    use fontaine_tools::mcp::Transport;

    use super::World;

    const SETTINGS: &str = r#"
[accounts.work]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9"
api_key_env = "WORK_KEY"

[telemetry]
api_key_env = "TELEMETRY_KEY"

[guardian]
scanner_hf_token_env = "HF_SCANNER_TOKEN"

[websearch]
backend = "tavily"
api_key_env = "SEARCH_KEY"
"#;

    fn lookup(pairs: &[(&str, &str)]) -> EnvLookup {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        std::sync::Arc::new(move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        })
    }

    fn world_with(json: &str) -> (World, Config) {
        let w = World::new();
        w.user_file(SETTINGS);
        w.mcp_json(json);
        let search_key = |name: &str| (name == "SEARCH_KEY").then(|| OsString::from("x"));
        let config = w.load_from(&w.work, None, &search_key).unwrap();
        (w, config)
    }

    fn build(
        config: &Config,
        env: &EnvLookup,
        process_names: &[&str],
    ) -> Result<Option<fontaine_tools::mcp::ServerSpec>, String> {
        let scrubbed = scrubbed_env(config, process_names.iter().map(OsString::from));
        server_spec(config, "s", Path::new("/work"), env, &scrubbed)
    }

    #[test]
    fn references_expand_into_command_args_env_and_url() {
        let (_w, config) = world_with(
            r#"{"mcpServers":{"s":{"command":"${BIN}","args":["--a","${A:-dflt}"],"env":{"K":"v-${V}"}}}}"#,
        );
        let vars = lookup(&[("BIN", "/bin/srv"), ("V", "1")]);
        let built = build(&config, &vars, &[]).unwrap().unwrap();
        let Transport::Stdio {
            command, args, env, ..
        } = built.transport
        else {
            panic!("a command is stdio");
        };
        assert_eq!(command, "/bin/srv");
        assert_eq!(args, ["--a", "dflt"]);
        assert!(env.iter().any(|(k, v)| k == "K" && v == "v-1"));

        let (_w, config) =
            world_with(r#"{"mcpServers":{"s":{"type":"http","url":"https://${HOST}/mcp"}}}"#);
        let built = build(&config, &lookup(&[("HOST", "h.example")]), &[])
            .unwrap()
            .unwrap();
        let Transport::Http { url } = built.transport else {
            panic!("a url is http");
        };
        assert_eq!(url, "https://h.example/mcp");
    }

    #[test]
    fn an_unset_variable_without_a_default_fails_naming_it() {
        let (_w, config) = world_with(r#"{"mcpServers":{"s":{"command":"${NOPE}"}}}"#);
        let error = build(&config, &lookup(&[]), &[]).unwrap_err();
        assert!(
            error.contains("${NOPE}") && error.contains("`s`"),
            "{error}"
        );
    }

    #[test]
    fn a_credential_variable_is_never_expanded() {
        for (variable, process_names) in [
            ("WORK_KEY", vec![]),
            ("TELEMETRY_KEY", vec![]),
            ("HF_SCANNER_TOKEN", vec![]),
            ("MICROMEGAS_INGESTION_KEY", vec!["MICROMEGAS_INGESTION_KEY"]),
            (
                "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
                vec!["OTEL_EXPORTER_OTLP_TRACES_HEADERS"],
            ),
            ("SEARCH_KEY", vec![]),
            ("ANTHROPIC_API_KEY", vec![]),
        ] {
            let (_w, config) = world_with(&format!(
                r#"{{"mcpServers":{{"s":{{"type":"http","url":"https://x/${{{variable}:-fallback}}"}}}}}}"#
            ));
            let vars = lookup(&[(variable, "secret")]);
            let error = build(&config, &vars, &process_names).unwrap_err();
            assert!(error.contains(variable), "{variable}: {error}");
            assert!(!error.contains("secret"), "{variable}: {error}");
        }
    }

    #[test]
    fn the_anthropic_key_is_refused_under_a_config_with_no_account_key_variable() {
        let w = World::new();
        w.mcp_json(r#"{"mcpServers":{"s":{"command":"x","env":{"K":"${ANTHROPIC_API_KEY}"}}}}"#);
        let config = w.load().unwrap();
        let error = build(&config, &lookup(&[("ANTHROPIC_API_KEY", "k")]), &[]).unwrap_err();
        assert!(error.contains("ANTHROPIC_API_KEY"), "{error}");
    }

    #[test]
    fn the_project_dir_is_the_given_root_whatever_the_process_environment_says() {
        let (_w, config) =
            world_with(r#"{"mcpServers":{"s":{"command":"${CLAUDE_PROJECT_DIR}/bin"}}}"#);
        let vars = lookup(&[("CLAUDE_PROJECT_DIR", "/from/a/parent/claude")]);
        let built = build(&config, &vars, &[]).unwrap().unwrap();
        let Transport::Stdio { command, .. } = built.transport else {
            panic!("a command is stdio");
        };
        assert_eq!(command, "/work/bin");
    }

    #[test]
    fn toml_definitions_are_not_expanded() {
        let w = World::new();
        w.user_file("[mcp.servers.s]\ncommand = \"${LITERAL}\"\n");
        let config = w.load().unwrap();
        let built = build(&config, &lookup(&[("LITERAL", "x")]), &[])
            .unwrap()
            .unwrap();
        let Transport::Stdio { command, .. } = built.transport else {
            panic!("a command is stdio");
        };
        assert_eq!(command, "${LITERAL}");
    }

    #[test]
    fn the_digest_follows_the_reference_not_the_variable_value() {
        let (_w, config) =
            world_with(r#"{"mcpServers":{"s":{"command":"x","args":["${TOKEN}"]}}}"#);
        let server = &config.mcp_servers["s"];
        // The configuration holds only the raw text, so a rotated value cannot move the digest.
        assert_eq!(server_digest("s", server), server_digest("s", server));
        let (_w2, changed) =
            world_with(r#"{"mcpServers":{"s":{"command":"x","args":["${OTHER}"]}}}"#);
        assert_ne!(
            server_digest("s", server),
            server_digest("s", &changed.mcp_servers["s"])
        );
    }
}
