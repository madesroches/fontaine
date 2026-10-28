//! Plugins: manifest, catalog and component parsing, the registry, and the tighten-only scope
//! rules every plugin file loads under.

mod common;

use std::ffi::OsString;

use common::{World, write};
use fontaine_core::config::{Origin, Slot};
use fontaine_core::extensions::{self, Scope};
use fontaine_core::permission_rules::RuleSource;
use fontaine_core::plugins::{
    self, InstalledPlugin, MarketplaceEntry, PluginContext, Registry, build_plugin, checkout_dir,
    load_installed,
};

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn context(project: &std::path::Path) -> PluginContext<'_> {
    PluginContext {
        project_root: project,
        env: &no_env,
    }
}

fn registry(marketplace: &str, plugin: &str, enabled: bool) -> Registry {
    Registry {
        marketplace: vec![MarketplaceEntry {
            name: marketplace.to_string(),
            url: format!("file://{marketplace}"),
            pin: "a1b2c3".to_string(),
        }],
        plugin: vec![InstalledPlugin {
            name: plugin.to_string(),
            marketplace: marketplace.to_string(),
            source: "./plugins/test".to_string(),
            pin: "a1b2c3".to_string(),
            enabled,
        }],
    }
}

fn checkout(w: &World, marketplace: &str, plugin: &str) -> std::path::PathBuf {
    let dir = checkout_dir(&w.config, marketplace, plugin);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest(root: &std::path::Path, name: &str, extra: &str) {
    write(
        &root.join(".claude-plugin/plugin.json"),
        &format!(r#"{{"name": "{name}"{extra}}}"#),
    );
}

fn load_extensions(w: &World, plugins: &[plugins::PluginDir]) -> extensions::Extensions {
    let mut warnings = Vec::new();
    extensions::load(&mut extensions::LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &[],
        plugins,
        warnings: &mut warnings,
    })
    .unwrap()
}

#[test]
fn build_plugin_reads_the_manifest_and_components() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(
        &root.join("skills/review/SKILL.md"),
        "---\ndescription: reviews\n---\nBody",
    );
    write(
        &root.join("commands/deploy.md"),
        "---\ndescription: deploys\n---\nRun it",
    );
    write(
        &root.join("agents/review/security.md"),
        "---\ndescription: sec\n---\nAgent body",
    );
    write(
        &root.join("output-styles/terse.md"),
        "---\ndescription: terse\n---\nStyle",
    );

    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    assert_eq!(build.summary.skills, 2); // skills/ + commands/
    assert_eq!(build.summary.agents, 1);
    assert_eq!(build.summary.styles, 1);
    assert!(build.dir.mcp_names.is_empty());

    let extensions = load_extensions(&w, std::slice::from_ref(&build.dir));
    assert!(extensions.skill("p:review").is_some());
    assert!(extensions.skill("p:deploy").is_some());
    assert!(extensions.skill("p:review:security").is_none());
    assert_eq!(extensions.agent_types[0].name, "p:review:security");
    assert_eq!(extensions.output_styles[0].name, "p:terse");
}

#[test]
fn a_plugin_skill_cannot_lend_allowed_tools() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(
        &root.join("skills/review/SKILL.md"),
        "---\nallowed-tools: shell(ls)\n---\nBody",
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let extensions = load_extensions(&w, &[build.dir]);
    let skill = extensions.skill("p:review").unwrap();
    assert_eq!(skill.scope, Scope::Plugin);
    assert!(skill.allowed_tools.is_empty());
}

#[test]
fn plugin_hooks_and_servers_carry_a_tighten_only_origin() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(
        &root.join("hooks/hooks.json"),
        r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "true", "timeout": 3}]}]}}"#,
    );
    write(
        &root.join(".mcp.json"),
        r#"{"mcpServers": {"local": {"command": "srv", "env": {"TOKEN": "${CLAUDE_PROJECT_DIR}"}}}}"#,
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    assert_eq!(build.summary.hooks, 1);
    assert_eq!(build.summary.servers, 1);
    let hook = &build.hooks[0];
    assert_eq!(hook.origin.slot, Slot::Plugin);
    assert!(!hook.origin.user_trust);
    assert_eq!(hook.matcher.as_deref(), Some("shell"));
    let server = build.servers.get("plugin_p_local").unwrap();
    assert_eq!(
        server
            .env
            .as_ref()
            .unwrap()
            .get("TOKEN")
            .map(String::as_str),
        Some(w.work.to_str().unwrap())
    );
    assert_eq!(
        build.dir.mcp_names.get("local").map(String::as_str),
        Some("plugin_p_local")
    );
}

#[test]
fn a_plugin_manifest_may_only_add_deny_side_permissions() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(
        &root,
        "p",
        r#", "permissions": {"deny": ["shell(git push *)"], "ask": ["read(.env)"]}, "guardian": {"hard_deny": ["never push on Fridays"], "soft_deny": []}"#,
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    assert_eq!(build.deny.len(), 1);
    assert!(matches!(build.deny[0].source, RuleSource::Plugin));
    assert_eq!(build.ask.len(), 1);
    assert_eq!(build.hard_deny, vec!["never push on Fridays"]);

    let mut config = w.load().unwrap();
    let contributions = plugins::PluginContributions {
        dirs: Vec::new(),
        hooks: Vec::new(),
        servers: Default::default(),
        deny: build.deny,
        ask: build.ask,
        hard_deny: build.hard_deny,
        soft_deny: Vec::new(),
        warnings: Vec::new(),
    };
    config.apply_plugins(&contributions).unwrap();
    assert!(
        config
            .permissions
            .deny
            .iter()
            .any(|r| r.rule.tool == "shell")
    );
    let (_, rules) = config.effective_auto_mode();
    assert!(rules.hard_deny.iter().any(|r| r == "never push on Fridays"));
}

#[test]
fn a_plugin_manifest_allow_or_environment_is_a_load_error() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", r#", "permissions": {"allow": ["read"]}"#);
    assert!(build_plugin(&root, "p", &context(&w.work)).is_err());
    manifest(&root, "p", r#", "guardian": {"environment": ["x"]}"#);
    assert!(build_plugin(&root, "p", &context(&w.work)).is_err());
}

#[test]
fn a_plugin_agent_file_follows_the_project_rules() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(
        &root.join("agents/with-model.md"),
        "---\nmodel: $main\n---\nBody",
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let mut warnings = Vec::new();
    let err = extensions::load(&mut extensions::LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &[],
        plugins: std::slice::from_ref(&build.dir),
        warnings: &mut warnings,
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("plugin agent file cannot choose a model")
    );

    write(
        &root.join("agents/inherit.md"),
        "---\npermissionMode: inherit\n---\nBody",
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let err = extensions::load(&mut extensions::LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &[],
        plugins: std::slice::from_ref(&build.dir),
        warnings: &mut Vec::new(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("plugin agent file cannot inherit"));
}

#[test]
fn a_plugin_agent_naming_an_external_server_is_skipped_with_a_warning() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(
        &root.join("agents/external.md"),
        "---\ntools: mcp__other__*\n---\nBody",
    );
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let mut warnings = Vec::new();
    let extensions = extensions::load(&mut extensions::LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &[],
        plugins: std::slice::from_ref(&build.dir),
        warnings: &mut warnings,
    })
    .unwrap();
    assert!(extensions.agent_types.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("mcp__other__*"));
}

#[test]
fn two_components_of_one_plugin_cannot_land_on_one_name() {
    let w = World::new();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(&root.join("skills/foo/SKILL.md"), "Skill");
    write(&root.join("commands/foo.md"), "Command");
    let build = build_plugin(&root, "p", &context(&w.work)).unwrap();
    let err = extensions::load(&mut extensions::LoadParams {
        config_dir: &w.config,
        project_root: &w.work,
        servers: &[],
        plugins: std::slice::from_ref(&build.dir),
        warnings: &mut Vec::new(),
    })
    .unwrap_err();
    assert!(err.to_string().contains("p:foo"));
}

#[test]
fn load_installed_reads_enabled_plugins_and_warns_on_corrupt_or_missing_checkouts() {
    let w = World::new();
    plugins::write_registry(&w.config, &registry("team", "p", true)).unwrap();
    let root = checkout(&w, "team", "p");
    manifest(&root, "p", "");
    write(&root.join("skills/hello/SKILL.md"), "---\n---\nBody");

    let contributions = load_installed(&w.config, &context(&w.work)).unwrap();
    assert_eq!(contributions.dirs.len(), 1);
    assert!(contributions.warnings.is_empty());

    // A disabled plugin contributes nothing.
    plugins::write_registry(&w.config, &registry("team", "p", false)).unwrap();
    let contributions = load_installed(&w.config, &context(&w.work)).unwrap();
    assert!(contributions.dirs.is_empty());

    // A vanished checkout warns and contributes nothing.
    plugins::write_registry(&w.config, &registry("team", "p", true)).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let contributions = load_installed(&w.config, &context(&w.work)).unwrap();
    assert!(contributions.dirs.is_empty());
    assert!(contributions.warnings.iter().any(|w| w.contains("missing")));

    // A corrupt registry loads nothing and warns.
    write(&w.config.join("plugins.toml"), "not [toml");
    let contributions = load_installed(&w.config, &context(&w.work)).unwrap();
    assert!(contributions.dirs.is_empty());
    assert!(
        contributions
            .warnings
            .iter()
            .any(|w| w.contains("no plugin loads"))
    );
}

#[test]
fn apply_plugins_refuses_a_composed_name_that_shadows_a_configured_server() {
    let w = World::new();
    w.user_file("[mcp.servers.s]\ncommand = \"srv\"\n");
    let mut config = w.load().unwrap();
    // Pretend a plugin ships a server that composes to `s`.
    let contributions = plugins::PluginContributions {
        dirs: Vec::new(),
        hooks: Vec::new(),
        servers: [(
            "s".to_string(),
            fontaine_core::config::McpServer {
                command: Some("plugin-srv".to_string()),
                args: None,
                env_vars: None,
                env: None,
                url: None,
            },
        )]
        .into_iter()
        .collect(),
        deny: Vec::new(),
        ask: Vec::new(),
        hard_deny: Vec::new(),
        soft_deny: Vec::new(),
        warnings: Vec::new(),
    };
    let err = config.apply_plugins(&contributions).unwrap_err();
    assert!(err.to_string().contains("collides"));
}

#[test]
fn an_identical_plugin_hook_keeps_the_plugin_origin() {
    let w = World::new();
    let mut config = w.load().unwrap();
    let project_hook = fontaine_core::config::Hook {
        matcher: None,
        command: "true".to_string(),
        timeout_secs: None,
        origin: Origin {
            slot: Slot::Project,
            user_trust: false,
        },
    };
    config.hooks.pre_tool_use.push(project_hook);
    let contributions = plugins::PluginContributions {
        dirs: Vec::new(),
        hooks: vec![fontaine_core::config::Hook {
            matcher: None,
            command: "true".to_string(),
            timeout_secs: None,
            origin: Origin {
                slot: Slot::Plugin,
                user_trust: false,
            },
        }],
        servers: Default::default(),
        deny: Vec::new(),
        ask: Vec::new(),
        hard_deny: Vec::new(),
        soft_deny: Vec::new(),
        warnings: Vec::new(),
    };
    config.apply_plugins(&contributions).unwrap();
    assert_eq!(config.hooks.pre_tool_use.len(), 1);
    assert_eq!(config.hooks.pre_tool_use[0].origin.slot, Slot::Plugin);
}
