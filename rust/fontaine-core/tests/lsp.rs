//! Language servers in configuration, in plugins, and in a runtime: who may define one, how
//! extension conflicts resolve, and when the `lsp` tool and the diagnostics stage exist.

mod common;

use std::ffi::OsString;

use common::runtime::build;
use common::{World, write};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::plugins::{self, PluginContext, build_plugin, checkout_dir};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, Url};
use rig_core::test_utils::MockStreamEvent;
use serde_json::json;

const SERVER: &str = "[lsp.servers.ra]\ncommand = \"no-such-server\"\nextension_to_language = { \".rs\" = \"rust\" }\n";

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn context(project: &std::path::Path) -> PluginContext<'_> {
    PluginContext {
        project_root: project,
        env: &no_env,
    }
}

fn plugin_root(w: &World, name: &str, manifest_extra: &str) -> std::path::PathBuf {
    let root = checkout_dir(&w.config, "team", name);
    std::fs::create_dir_all(&root).unwrap();
    write(
        &root.join(".claude-plugin/plugin.json"),
        &format!(r#"{{"name": "{name}"{manifest_extra}}}"#),
    );
    root
}

#[test]
fn a_user_server_loads_and_defaults_apply() {
    let w = World::new();
    w.user_file(SERVER);
    let config = w.load().unwrap();
    let table = config.lsp_table();
    assert_eq!(table.servers.len(), 1);
    assert_eq!(table.servers[0].0, "ra");
    assert_eq!(table.claims.get(".rs").map(String::as_str), Some("ra"));
    assert!(table.warnings.is_empty());
}

#[test]
fn an_invalid_server_is_a_config_error_naming_it() {
    for (body, needle) in [
        (
            "command = \"x\"\nextension_to_language = {}\n",
            "at least one extension",
        ),
        (
            "command = \"x\"\nextension_to_language = { \"rs\" = \"rust\" }\n",
            "must start with `.`",
        ),
        (
            "command = \"\"\nextension_to_language = { \".rs\" = \"rust\" }\n",
            "`command` must not be empty",
        ),
        (
            "command = \"x\"\nextension_to_language = { \".rs\" = \"rust\" }\nbogus = 1\n",
            "bogus",
        ),
        (
            "command = \"x\"\nextension_to_language = { \".rs\" = \"rust\" }\nrequest_timeout_ms = 0\n",
            "greater than zero",
        ),
    ] {
        let w = World::new();
        w.user_file(&format!("[lsp.servers.ra]\n{body}"));
        let error = w.load().unwrap_err().to_string();
        assert!(error.contains(needle), "{needle}: {error}");
        assert!(
            error.contains("lsp.servers.ra") || error.contains("bogus"),
            "{error}"
        );
    }
    let w = World::new();
    w.user_file(
        "[lsp.servers.\"bad name\"]\ncommand = \"x\"\nextension_to_language = { \".rs\" = \"rust\" }\n",
    );
    assert!(w.load().unwrap_err().to_string().contains("server names"));
}

#[test]
fn only_the_user_file_and_cli_settings_define_a_server() {
    let w = World::new();
    w.project_file(SERVER);
    assert!(w.load().unwrap_err().to_string().contains("lsp"));
    let w = World::new();
    w.local_file(SERVER);
    assert!(w.load().unwrap_err().to_string().contains("lsp"));
    let w = World::new();
    let cli = w.cli_file(SERVER);
    let config = w.load_with_settings(&cli).unwrap();
    assert_eq!(config.lsp_table().servers.len(), 1);
}

#[test]
fn plugin_servers_map_camel_case_fields_and_take_composed_names() {
    let w = World::new();
    let root = plugin_root(&w, "p", "");
    write(
        &root.join(".lsp.json"),
        r#"{"go": {
            "command": "${CLAUDE_PLUGIN_ROOT}/bin/gopls", "args": ["serve"],
            "extensionToLanguage": {".go": "go"}, "transport": "socket",
            "env": {"GOFLAGS": "-mod=mod"}, "initializationOptions": {"a": 1},
            "settings": {"gopls": {"b": 2}}, "startupTimeout": 5000, "shutdownTimeout": 900,
            "requestTimeout": 7000, "restartOnCrash": false, "maxRestarts": 1, "diagnostics": false
        }}"#,
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    assert_eq!(build.summary.lsp_servers, 1);
    let (name, server) = &build.lsp_servers[0];
    assert_eq!(name, "plugin:p:go");
    assert_eq!(server.command, format!("{}/bin/gopls", root.display()));
    assert_eq!(server.args.as_deref(), Some(&["serve".to_string()][..]));
    assert_eq!(server.extension_to_language[".go"], "go");
    assert_eq!(server.env.as_ref().unwrap()["GOFLAGS"], "-mod=mod");
    assert_eq!(server.initialization_options, Some(json!({"a": 1})));
    assert_eq!(server.settings, Some(json!({"gopls": {"b": 2}})));
    assert_eq!(
        (
            server.startup_timeout_ms,
            server.shutdown_timeout_ms,
            server.request_timeout_ms,
            server.restart_on_crash,
            server.max_restarts,
            server.diagnostics
        ),
        (
            Some(5000),
            Some(900),
            Some(7000),
            Some(false),
            Some(1),
            Some(false)
        )
    );
    assert!(build.warnings.is_empty(), "{:?}", build.warnings);
}

#[test]
fn the_manifest_key_takes_a_map_a_path_or_a_list_and_later_names_win() {
    let w = World::new();
    let one = |command: &str, ext: &str| {
        format!(r#"{{"srv": {{"command": "{command}", "extensionToLanguage": {{"{ext}": "x"}}}}}}"#)
    };
    // Inline map over `.lsp.json`.
    let root = plugin_root(
        &w,
        "p",
        &format!(r#", "lspServers": {}"#, one("inline", ".b")),
    );
    write(&root.join(".lsp.json"), &one("file", ".a"));
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    assert_eq!(build.lsp_servers.len(), 1);
    assert_eq!(build.lsp_servers[0].1.command, "inline");
    assert!(
        build.lsp_servers[0]
            .1
            .extension_to_language
            .contains_key(".b")
    );

    // A path, and a list of paths and a map: in order, the last of a name wins.
    let root = plugin_root(&w, "q", r#", "lspServers": ["a.json", "b.json"]"#);
    write(&root.join("a.json"), &one("first", ".a"));
    write(&root.join("b.json"), &one("second", ".a"));
    let build = build_plugin(&root, "q", &context(&w.work)).unwrap();
    assert_eq!(build.lsp_servers.len(), 1);
    assert_eq!(build.lsp_servers[0].1.command, "second");
    assert_eq!(build.lsp_servers[0].0, "plugin:q:srv");

    let root = plugin_root(&w, "r", r#", "lspServers": "../outside.json""#);
    write(&root.join("../outside.json"), &one("x", ".a"));
    let error = build_plugin(&root, "r", &context(&w.work)).unwrap_err();
    assert!(error.to_string().contains("outside the plugin"), "{error}");
}

#[test]
fn an_unknown_key_skips_that_whole_file_with_a_warning_and_workspace_folder_is_ignored() {
    let w = World::new();
    let root = plugin_root(&w, "p", r#", "lspServers": "ok.json""#);
    write(
        &root.join(".lsp.json"),
        r#"{"good": {"command": "g", "extensionToLanguage": {".g": "g"}},
            "bad": {"command": "b", "extensionToLanguage": {".b": "b"}, "mystery": 1}}"#,
    );
    write(
        &root.join("ok.json"),
        r#"{"kept": {"command": "k", "extensionToLanguage": {".k": "k"}, "workspaceFolder": "/elsewhere"}}"#,
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let names: Vec<&str> = build.lsp_servers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["plugin:p:kept"]);
    assert!(
        build
            .warnings
            .iter()
            .any(|w| w.contains(".lsp.json is skipped") && w.contains("mystery")),
        "{:?}",
        build.warnings
    );
    assert!(
        build
            .warnings
            .iter()
            .any(|w| w.contains("`workspaceFolder`, which Fontaine ignores")),
        "{:?}",
        build.warnings
    );
    // No warning about the key itself any more.
    assert!(
        !build
            .warnings
            .iter()
            .any(|w| w.contains("does not use") && w.contains("lspServers"))
    );
}

#[test]
fn the_first_server_to_name_an_extension_keeps_it_and_a_later_one_is_told() {
    let w = World::new();
    w.user_file(&format!(
        "{SERVER}[lsp.servers.zeta]\ncommand = \"z\"\nextension_to_language = {{ \".rs\" = \"rust\", \".toml\" = \"toml\" }}\n"
    ));
    let root = plugin_root(&w, "p", "");
    write(
        &root.join(".lsp.json"),
        r#"{"ra": {"command": "r", "extensionToLanguage": {".rs": "rust", ".md": "md"}}}"#,
    );
    let mut config = w.load().unwrap();
    let contributions = plugins::PluginContributions {
        lsp_servers: build_plugin(&root, "p", &context(&w.work))
            .unwrap()
            .lsp_servers,
        ..Default::default()
    };
    config.apply_plugins(&contributions).unwrap();
    let table = config.lsp_table();
    let names: Vec<&str> = table.servers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["ra", "zeta", "plugin:p:ra"]);
    assert_eq!(table.claims[".rs"], "ra");
    assert_eq!(table.claims[".toml"], "zeta");
    assert_eq!(table.claims[".md"], "plugin:p:ra");
    assert_eq!(
        table.warnings,
        [
            "LSP server \"zeta\" is not used for .rs files",
            "LSP server \"plugin:p:ra\" is not used for .rs files"
        ]
    );
    // Re-applying replaces the plugin's servers rather than adding to them.
    config.apply_plugins(&Default::default()).unwrap();
    assert_eq!(config.lsp_table().servers.len(), 2);
}

#[tokio::test]
async fn the_tool_exists_only_with_a_server_configured() {
    let w = World::new();
    let plain = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    assert!(!plain.runtime.tool_names().contains(&"lsp".to_string()));

    let w = World::new();
    w.user_file(SERVER);
    let built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    assert!(built.runtime.tool_names().contains(&"lsp".to_string()));
}

fn as_root() -> rig_core::tool::ToolContext {
    use fontaine_tools::gate::{AgentId, ROOT_AGENT};
    let mut context = rig_core::tool::ToolContext::new();
    context
        .insert(AgentId(ROOT_AGENT.to_string()))
        .expect("the agent id serializes");
    context
}

fn error_at(line: u32, message: &str) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(line, 0), Position::new(line, 3)),
        severity: Some(DiagnosticSeverity::ERROR),
        message: message.to_string(),
        ..Default::default()
    }
}

async fn read_a(runtime: &fontaine_core::agent::AgentRuntime) -> String {
    let handle = runtime.tool_server().clone();
    handle
        .toolset()
        .execute("read", r#"{"path": "a.rs"}"#, &mut as_root())
        .await
        .into_result()
        .map(|o| o.render())
        .map_err(|e| e.model_output().render())
        .unwrap()
}

#[tokio::test]
async fn late_diagnostics_stay_out_of_a_notification_prompt_and_ride_the_next_real_result() {
    let w = World::new();
    w.user_file(SERVER);
    std::fs::write(w.work.join("a.rs"), "fn a() {}\n").unwrap();
    let built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    let runtime = &built.runtime;

    let uri = Url::from_file_path(w.work.join("a.rs")).unwrap();
    let store = runtime.lsp().store();
    store.begin_sync(&uri, "root", 1);
    assert!(
        store
            .wait(&uri, std::time::Duration::from_millis(5))
            .await
            .is_none()
    );
    store.publish(
        "ra",
        uri.clone(),
        Some(1),
        vec![error_at(0, "borrow of moved value")],
    );

    runtime
        .state
        .agents()
        .notify_report("root", "agent-7", "Done.".to_string());
    let prompt = runtime.notification_prompt().await.unwrap();
    assert!(prompt.contains("Done."), "{prompt}");
    assert!(!prompt.contains("Late diagnostics"), "{prompt}");

    let result = read_a(runtime).await;
    assert!(result.contains("Late diagnostics (ra):"), "{result}");
    assert!(
        result.contains("a.rs:1:1 error: borrow of moved value"),
        "{result}"
    );
    assert!(!read_a(runtime).await.contains("Late diagnostics"));
}

#[tokio::test]
async fn clearing_the_conversation_forgets_pending_diagnostics() {
    let w = World::new();
    w.user_file(SERVER);
    std::fs::write(w.work.join("a.rs"), "fn a() {}\n").unwrap();
    let built = build(&w, Manual, Vec::<Vec<MockStreamEvent>>::new(), true);
    let runtime = &built.runtime;
    let uri = Url::from_file_path(w.work.join("a.rs")).unwrap();
    let store = runtime.lsp().store();
    store.begin_sync(&uri, "root", 1);
    let _ = store.wait(&uri, std::time::Duration::from_millis(5)).await;
    store.publish("ra", uri, Some(1), vec![error_at(0, "stale")]);
    runtime.clear_conversation().await.unwrap();
    assert!(!read_a(runtime).await.contains("Late diagnostics"));
}

#[test]
fn a_server_naming_an_extension_in_two_cases_does_not_conflict_with_itself() {
    let w = World::new();
    w.user_file(
        "[lsp.servers.ra]\ncommand = \"x\"\nextension_to_language = { \".rs\" = \"rust\", \".RS\" = \"rust\" }\n",
    );
    let table = w.load().unwrap().lsp_table();
    assert_eq!(table.claims[".rs"], "ra");
    assert!(table.warnings.is_empty(), "{:?}", table.warnings);
}
