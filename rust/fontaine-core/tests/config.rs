mod common;

use std::ffi::OsString;
use std::path::Path;

use common::{World, write};
use fontaine_core::config::{
    AccountKind, Column, ConfigError, GuardianProfile, PermissionMode, ThemeName, parse_settings,
};
use fontaine_core::permission_rules::SourcedRule;

fn user_ok(text: &str) {
    parse_settings(text, Path::new("user.toml"), Column::User)
        .unwrap_or_else(|e| panic!("user scope should accept:\n{text}\n{e}"));
}

#[test]
fn defaults_with_no_files() {
    let w = World::new();
    let c = w.load().unwrap();
    assert_eq!(c.permissions.default_mode, None);
    assert_eq!(c.guardian.profile, GuardianProfile::Full);
    assert_eq!(c.guardian.timeout_secs, 30);
    assert_eq!(c.agents.max_parallel, 4);
    assert!(!c.telemetry.enabled);
    assert!(c.accounts.is_empty());
}

#[test]
fn workspace_is_the_canonicalized_launch_dir() {
    let w = World::new();
    let c = w
        .load_from(&w.work.join("."), None, &common::no_env)
        .unwrap();
    assert_eq!(c.workspace, w.work);
    assert!(c.workspace.is_absolute());
}

#[cfg(unix)]
#[test]
fn workspace_resolves_symlinks() {
    let w = World::new();
    let link = w.root.join("link");
    std::os::unix::fs::symlink(&w.work, &link).unwrap();
    let c = w.load_from(&link, None, &common::no_env).unwrap();
    assert_eq!(c.workspace, w.work);
}

#[test]
fn project_config_is_read_only_from_the_primary_root() {
    let w = World::new();
    // Would be rejected from project scope if it were read.
    write(
        &w.root.join(".fontaine/settings.toml"),
        "[permissions]\nallow = [\"shell\"]\n",
    );
    write(
        &w.work.join(".fontaine/settings.toml"),
        "[permissions]\ndeny = [\"shell(rm)\"]\n",
    );
    let sub = w.work.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let c = w.load_from(&sub, None, &common::no_env).unwrap();
    assert!(c.permissions.deny.is_empty(), "no parent-directory walk");

    let c = w.load().unwrap();
    assert_eq!(c.permissions.deny.len(), 1);
    assert!(c.permissions.allow.is_empty());
}

#[test]
fn scalar_precedence_is_cli_then_project_local_then_project_then_user() {
    let w = World::new();
    w.trusted_repo();
    w.user_file("[permissions]\ndefault_mode = \"acceptEdits\"\n");
    w.project_file("[permissions]\ndefault_mode = \"dontAsk\"\n");
    w.local_file("[permissions]\ndefault_mode = \"manual\"\n");
    let cli = w.cli_file("[permissions]\ndefault_mode = \"plan\"\n");

    assert_eq!(
        w.load_with_settings(&cli).unwrap().permissions.default_mode,
        Some(PermissionMode::Plan)
    );
    assert_eq!(
        w.load().unwrap().permissions.default_mode,
        Some(PermissionMode::Manual)
    );
    std::fs::remove_file(w.work.join(".fontaine/settings.local.toml")).unwrap();
    assert_eq!(
        w.load().unwrap().permissions.default_mode,
        Some(PermissionMode::DontAsk)
    );
    std::fs::remove_file(w.work.join(".fontaine/settings.toml")).unwrap();
    assert_eq!(
        w.load().unwrap().permissions.default_mode,
        Some(PermissionMode::AcceptEdits)
    );
}

#[test]
fn tui_defaults_parse_and_merge_cli_over_user() {
    let w = World::new();
    let c = w.load().unwrap();
    assert_eq!(c.tui.theme, ThemeName::DarkAnsi);
    assert_eq!(c.tui.screen_reader, None);

    w.user_file("[tui]\ntheme = \"light-daltonized\"\nscreen_reader = false\n");
    let c = w.load().unwrap();
    assert_eq!(c.tui.theme, ThemeName::LightDaltonized);
    assert_eq!(c.tui.screen_reader, Some(false));

    let cli = w.cli_file("[tui]\ntheme = \"dark\"\n");
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.tui.theme, ThemeName::Dark);
    assert_eq!(c.tui.screen_reader, Some(false));
}

#[test]
fn an_unknown_theme_name_is_rejected() {
    assert!(
        parse_settings(
            "[tui]\ntheme = \"solarized\"\n",
            Path::new("u.toml"),
            Column::User
        )
        .is_err()
    );
    assert_eq!(ThemeName::parse("dark-ansi"), Some(ThemeName::DarkAnsi));
    assert_eq!(ThemeName::parse("nope"), None);
    for name in ThemeName::ALL {
        assert_eq!(ThemeName::parse(name.as_str()), Some(name));
    }
}

#[test]
fn model_scalar_comes_from_the_highest_scope_that_may_set_it() {
    let w = World::new();
    w.trusted_repo();
    w.user_file("[model]\nmodel = \"a/user\"\n");
    assert_eq!(w.load().unwrap().model.as_deref(), Some("a/user"));
    w.local_file("[model]\nmodel = \"a/local\"\n");
    assert_eq!(w.load().unwrap().model.as_deref(), Some("a/local"));
    let cli = w.cli_file("[model]\nmodel = \"a/cli\"\n");
    assert_eq!(
        w.load_with_settings(&cli).unwrap().model.as_deref(),
        Some("a/cli")
    );
}

#[test]
fn lists_merge_as_a_union_across_scopes() {
    let w = World::new();
    w.trusted_repo();
    w.user_file("[permissions]\ndeny = [\"read(.env)\"]\nask = [\"agent\"]\n");
    w.project_file("[permissions]\ndeny = [\"shell(rm)\", \"read(.env)\"]\n");
    w.local_file("[permissions]\nallow = [\"shell(ls)\"]\n[auto_mode]\nhard_deny = [\"a\"]\n");
    let cli = w.cli_file(
        "[permissions]\ndeny = [\"webfetch\"]\nallow = [\"shell(ls)\"]\n[auto_mode]\nhard_deny = [\"b\", \"a\"]\n",
    );
    let c = w.load_with_settings(&cli).unwrap();
    // A rule listed in two scopes is kept once per scope, so each source stays attributable.
    let deny: Vec<_> = c.permissions.deny.iter().map(SourcedRule::label).collect();
    assert_eq!(deny.len(), 4, "{deny:?}");
    for rule in [
        "read(.env) [user]",
        "read(.env) [project]",
        "shell(rm) [project]",
        "webfetch [cli]",
    ] {
        assert!(deny.contains(&rule.to_string()), "{rule} in {deny:?}");
    }
    assert_eq!(c.permissions.ask.len(), 1);
    let allow: Vec<_> = c.permissions.allow.iter().map(SourcedRule::label).collect();
    assert_eq!(allow, ["shell(ls) [project-local]", "shell(ls) [cli]"]);
    assert_eq!(c.auto_mode.hard_deny.len(), 2);
}

#[test]
fn a_named_table_entry_is_replaced_whole_by_the_higher_scope() {
    let w = World::new();
    w.user_file(
        r#"
[accounts.work]
kind = "anthropic"
api_key_env = "KEY"
models = ["m1"]

[mcp.servers.gh]
command = "gh-mcp"
args = ["stdio"]

[agents.types.explore]
model = "$main"
tools = ["read"]
mode = "manual"
"#,
    );
    let cli = w.cli_file(
        r#"
[accounts.work]
kind = "ollama"

[mcp.servers.gh]
url = "https://mcp.example.com"

[agents.types.explore]
tools = ["grep"]
"#,
    );
    let c = w.load_with_settings(&cli).unwrap();
    let account = &c.accounts["work"];
    assert_eq!(account.kind, AccountKind::named("ollama"));
    assert_eq!(account.api_key_env, None);
    assert_eq!(account.models, None);
    let server = &c.mcp_servers["gh"];
    assert_eq!(server.command, None);
    assert_eq!(server.args, None);
    assert_eq!(server.url.as_deref(), Some("https://mcp.example.com"));
    let ty = &c.agents.types["explore"];
    assert_eq!(ty.model, None);
    assert_eq!(ty.mode, None);
    assert_eq!(ty.tools.as_deref(), Some(&["grep".to_string()][..]));
}

#[test]
fn hooks_arrays_union() {
    let w = World::new();
    w.user_file("[[hooks.pre_tool_use]]\nmatcher = \"shell(git *)\"\ncommand = \"a.sh\"\n");
    let cli = w.cli_file(
        "[[hooks.pre_tool_use]]\nmatcher = \"shell\"\ncommand = \"b.sh\"\n[[hooks.post_tool_use]]\nmatcher = \"read\"\ncommand = \"c.sh\"\n",
    );
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.hooks.pre_tool_use.len(), 2);
    assert_eq!(c.hooks.post_tool_use.len(), 1);
}

#[test]
fn resource_attributes_merge_per_key_higher_scope_winning() {
    let w = World::new();
    w.user_file("[telemetry]\nresource_attributes = { a = \"1\", b = \"2\" }\n");
    let cli = w.cli_file("[telemetry]\nresource_attributes = { b = \"9\", c = \"3\" }\n");
    let c = w.load_with_settings(&cli).unwrap();
    let attrs = &c.telemetry.resource_attributes;
    assert_eq!(attrs.len(), 3);
    assert_eq!(attrs["a"], "1");
    assert_eq!(attrs["b"], "9");
    assert_eq!(attrs["c"], "3");
}

#[test]
fn project_telemetry_enabled_false_overrides_a_user_enable() {
    let w = World::new();
    w.user_file(
        "[telemetry]\nenabled = true\nendpoint = \"https://t.example.com\"\napi_key_env = \"K\"\n",
    );
    w.project_file("[telemetry]\nenabled = false\n");
    assert!(!w.load().unwrap().telemetry.enabled);
}

#[test]
fn scope_stack_lists_every_slot_highest_precedence_first() {
    let w = World::new();
    let cli = w.cli_file("");
    let c = w.load_with_settings(&cli).unwrap();
    let slots: Vec<_> = c.scopes.iter().map(|s| s.slot.label()).collect();
    assert_eq!(
        slots,
        ["managed", "cli", "project-local", "project", "user"]
    );
    assert!(c.scopes[0].path.is_none() && !c.scopes[0].loaded);
    assert!(c.scopes[1].loaded);
}

#[test]
fn config_errors_map_to_exit_code_2() {
    let w = World::new();
    w.user_file("bogus = 1\n");
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
    assert_eq!(err.exit_code(), 2);
}

// --- rejection ---

#[test]
fn unknown_keys_and_sections_are_rejected_in_every_scope() {
    for bad in [
        "[permisions]\ndeny = [\"shell\"]\n",
        "[permissions]\ndney = []\n",
        "[guardian]\nprofil = \"full\"\n",
        "[telemetry]\nenable = true\n",
        "[accounts.x]\nkind = \"ollama\"\nbase_ur = \"x\"\n",
        "[accounts.x]\nkind = \"ollama\"\napi_key = \"sk-raw\"\n",
    ] {
        for column in [
            Column::User,
            Column::Cli,
            Column::ProjectLocal,
            Column::Project,
        ] {
            let err = parse_settings(bad, Path::new("s.toml"), column).unwrap_err();
            assert!(
                matches!(err, ConfigError::Parse { .. }),
                "{column:?} {bad}: {err:?}"
            );
        }
    }

    let w = World::new();
    w.trusted_repo();
    w.user_file("[permisions]\n");
    assert!(matches!(w.load().unwrap_err(), ConfigError::Parse { .. }));
    std::fs::remove_file(w.config.join("config.toml")).unwrap();
    w.project_file("[permisions]\n");
    assert!(matches!(w.load().unwrap_err(), ConfigError::Parse { .. }));
    std::fs::remove_file(w.work.join(".fontaine/settings.toml")).unwrap();
    w.local_file("[permisions]\n");
    assert!(matches!(w.load().unwrap_err(), ConfigError::Parse { .. }));
    std::fs::remove_file(w.work.join(".fontaine/settings.local.toml")).unwrap();
    let cli = w.cli_file("[permisions]\n");
    assert!(matches!(
        w.load_with_settings(&cli).unwrap_err(),
        ConfigError::Parse { .. }
    ));
}

#[test]
fn rule_strings_must_parse_and_name_a_known_tool() {
    for bad in [
        "shell(",
        "shell()",
        "Shell",
        "bash",
        "nope(x)",
        "webfetch(example.com)",
        "mcp__",
    ] {
        for key in ["deny", "ask", "allow"] {
            let text = format!("[permissions]\n{key} = [\"{bad}\"]\n");
            let err = parse_settings(&text, Path::new("u.toml"), Column::User).unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { .. }),
                "{key} {bad}: {err:?}"
            );
        }
    }
    user_ok(
        "[permissions]\ndeny = [\"shell(git push *)\", \"mcp__gh__*\", \"webfetch(domain:*.evil.com)\", \"read(//etc/hosts)\"]\n",
    );
}

#[test]
fn guardian_profile_only_validates_as_full_or_minimal() {
    user_ok("[guardian]\nprofile = \"full\"\n");
    user_ok("[guardian]\nprofile = \"minimal\"\n");
    for bad in ["off", "none", "disabled", "Full", ""] {
        let text = format!("[guardian]\nprofile = \"{bad}\"\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad}"
        );
    }
    for bad in ["false", "0"] {
        let text = format!("[guardian]\nprofile = {bad}\n");
        assert!(parse_settings(&text, Path::new("u.toml"), Column::User).is_err());
    }
}

#[test]
fn agent_cost_budgets_must_be_finite_and_non_negative() {
    for bad in [
        "subagent_cost_budget_usd = -1.0",
        "subagent_cost_budget_usd = nan",
        "session_cost_budget_usd = inf",
        "session_cost_budget_usd = -0.5",
    ] {
        let text = format!("[agents]\n{bad}\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad}"
        );
    }
    let ok = "[agents]\nsubagent_cost_budget_usd = 0.0\nsession_cost_budget_usd = 5.0\n";
    assert!(parse_settings(ok, Path::new("u.toml"), Column::User).is_ok());
}

#[test]
fn guardian_values_are_range_checked() {
    for bad in [
        "timeout_secs = 0",
        "confidence_threshold = 1.5",
        "confidence_threshold = -0.1",
        "model = \"no-slash\"",
        "model = \"/m\"",
    ] {
        let text = format!("[guardian]\n{bad}\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn default_mode_auto_loads_from_user_and_cli_scope_and_is_unset_by_default() {
    let w = World::new();
    assert_eq!(w.load().unwrap().permissions.default_mode, None);

    w.user_file("[permissions]\ndefault_mode = \"auto\"\n");
    assert_eq!(
        w.load().unwrap().permissions.default_mode,
        Some(PermissionMode::Auto)
    );
    std::fs::remove_file(w.config.join("config.toml")).unwrap();

    let cli = w.cli_file("[permissions]\ndefault_mode = \"auto\"\n");
    assert_eq!(
        w.load_with_settings(&cli).unwrap().permissions.default_mode,
        Some(PermissionMode::Auto)
    );
}

#[test]
fn default_mode_auto_is_refused_from_every_project_scope() {
    let w = World::new();
    w.project_file("[permissions]\ndefault_mode = \"auto\"\n");
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Scope { .. }), "{err:?}");
    assert_eq!(err.exit_code(), 2);
    std::fs::remove_file(w.work.join(".fontaine/settings.toml")).unwrap();

    w.local_file("[permissions]\ndefault_mode = \"auto\"\n");
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Scope { .. }), "{err:?}");
}

#[test]
fn agent_type_mode_accepts_only_inherit_manual_dont_ask() {
    for ok in ["inherit", "manual", "dontAsk"] {
        user_ok(&format!("[agents.types.t]\nmode = \"{ok}\"\n"));
    }
    for bad in [
        "plan",
        "acceptEdits",
        "auto",
        "bypassPermissions",
        "default",
    ] {
        let text = format!("[agents.types.t]\nmode = \"{bad}\"\n");
        let err = parse_settings(&text, Path::new("u.toml"), Column::User).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{bad}: {err:?}");
    }
}

#[test]
fn agent_type_tools_must_be_known() {
    user_ok("[agents.types.t]\ntools = [\"read\", \"mcp__gh__issue\"]\n");
    assert!(
        parse_settings(
            "[agents.types.t]\ntools = [\"bash\"]\n",
            Path::new("u.toml"),
            Column::User
        )
        .is_err()
    );
    for bad in ["mcp__", "mcp__x__", "mcp__git hub", "mcp__a.b"] {
        let text = format!("[agents.types.t]\ntools = [\"{bad}\"]\n");
        assert!(parse_settings(&text, Path::new("u.toml"), Column::User).is_err());
    }
}

#[test]
fn server_names_use_the_rule_grammar_character_set() {
    for bad in ["my.server", "a__b", "a b", "a_", "_a"] {
        let text = format!("[mcp.servers.\"{bad}\"]\ncommand = \"a\"\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad}"
        );
    }
    user_ok("[mcp.servers.my-server_1]\ncommand = \"a\"\n");
}

#[test]
fn account_and_server_shapes() {
    assert!(
        parse_settings(
            "[accounts.x]\nkind = \"openai-compatible\"\n",
            Path::new("u.toml"),
            Column::User
        )
        .is_err(),
        "openai-compatible needs a base_url"
    );
    user_ok(
        "[accounts.x]\nkind = \"openai-compatible\"\nbase_url = \"http://localhost:1234/v1\"\n",
    );
    assert!(
        parse_settings(
            "[accounts.x]\nkind = \"mystery\"\n",
            Path::new("u.toml"),
            Column::User
        )
        .is_err()
    );
    assert!(
        parse_settings(
            "[mcp.servers.s]\ncommand = \"a\"\nurl = \"https://b\"\n",
            Path::new("u.toml"),
            Column::User
        )
        .is_err()
    );
    assert!(parse_settings("[mcp.servers.s]\n", Path::new("u.toml"), Column::User).is_err());
    for empty in ["command", "url"] {
        let text = format!("[mcp.servers.s]\n{empty} = \"\"\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{empty}"
        );
    }
}

#[test]
fn allow_extra_entries_are_flag_free_command_pairs() {
    user_ok("[shell_preclassifier]\nallow_extra = [\"tree\", \"cargo metadata\"]\n");
    for bad in ["ls -la", "a b c", "-x", ""] {
        let text = format!("[shell_preclassifier]\nallow_extra = [\"{bad}\"]\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn a_missing_settings_path_is_a_config_error_naming_the_path() {
    let w = World::new();
    let missing = w.root.join("nope.toml");
    let err = w.load_with_settings(&missing).unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    assert!(err.to_string().contains("nope.toml"));
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn a_directory_in_place_of_a_settings_file_is_a_config_error_naming_the_path() {
    let w = World::new();
    std::fs::create_dir_all(w.work.join(".fontaine/settings.toml")).unwrap();
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    assert!(err.to_string().contains("settings.toml"));
    std::fs::remove_dir(w.work.join(".fontaine/settings.toml")).unwrap();

    std::fs::create_dir_all(w.work.join(".fontaine/settings.local.toml")).unwrap();
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    assert!(err.to_string().contains("settings.local.toml"));
    std::fs::remove_dir(w.work.join(".fontaine/settings.local.toml")).unwrap();

    std::fs::create_dir_all(w.config.join("config.toml")).unwrap();
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    assert!(err.to_string().contains("config.toml"));
}

#[test]
fn absent_files_load_as_empty() {
    let w = World::new();
    let c = w.load().unwrap();
    assert!(c.scopes.iter().skip(1).all(|s| !s.loaded));
}

// --- the per-key scope table ---

fn assert_scope_error(r: Result<impl std::fmt::Debug, ConfigError>, what: &str) {
    match r {
        Err(ConfigError::Scope { .. }) => {}
        other => panic!("{what}: expected a scope error, got {other:?}"),
    }
}

#[test]
fn cli_scope_rejects_user_file_only_keys() {
    let w = World::new();
    let cli = w.cli_file("[guardian]\nscanner = \"llm\"\n");
    assert_scope_error(w.load_with_settings(&cli), "cli: guardian.scanner");
}

#[test]
fn project_local_scope_rejects_user_file_or_cli_keys() {
    let w = World::new();
    w.trusted_repo();
    w.local_file("[tui]\ntheme = \"dark\"\n");
    assert_scope_error(w.load(), "project-local: tui.theme");
}

#[test]
fn project_scope_accepts_the_tightening_cells() {
    let w = World::new();
    w.project_file(
        r#"
[permissions]
default_mode = "dontAsk"
deny = ["shell(rm)"]
ask = ["webfetch"]

[auto_mode]
soft_deny = ["x"]
hard_deny = ["y"]

[telemetry]
enabled = false

[[hooks.pre_tool_use]]
matcher = "shell"
command = "guard.sh"

[mcp.servers.s]
command = "srv"
"#,
    );
    let c = w.load().unwrap();
    assert_eq!(c.permissions.default_mode, Some(PermissionMode::DontAsk));
    assert_eq!(c.permissions.deny.len(), 1);
    assert_eq!(c.hooks.pre_tool_use.len(), 1);
    assert!(c.mcp_servers.contains_key("s"));

    w.project_file("[permissions]\ndefault_mode = \"manual\"\n");
    assert_eq!(
        w.load().unwrap().permissions.default_mode,
        Some(PermissionMode::Manual)
    );
}

#[test]
fn project_local_scope_accepts_the_cells_project_scope_rejects() {
    let w = World::new();
    w.trusted_repo();
    w.local_file(
        r#"
[model]
model = "a/m"

[guardian]
model = "a/g"
profile = "minimal"
timeout_secs = 12
confidence_threshold = 0.8
cache = false

[permissions]
default_mode = "dontAsk"
allow = ["shell(ls)"]
additional_directories = ["/tmp/extra"]

[auto_mode]
environment = ["e"]
allow = ["a"]

[agents]
max_parallel = 2

[agents.types.t]
tools = ["read"]

[shell_preclassifier]
allow_extra = ["tree"]

[telemetry]
enabled = false

[[hooks.pre_tool_use]]
matcher = "shell"
command = "guard.sh"

[mcp.servers.s]
command = "srv"
"#,
    );
    let c = w.load().unwrap();
    assert_eq!(c.model.as_deref(), Some("a/m"));
    assert_eq!(c.guardian.profile, GuardianProfile::Minimal);
    assert_eq!(c.permissions.allow.len(), 1);
    assert_eq!(c.permissions.additional_directories, ["/tmp/extra"]);
    assert_eq!(c.agents.max_parallel, 2);
    assert_eq!(c.shell_preclassifier_allow_extra, ["tree"]);
}

#[test]
fn cli_scope_accepts_everything_but_the_user_file_only_keys() {
    let w = World::new();
    let cli = w.cli_file(
        r#"
[accounts.a]
kind = "ollama"

[model]
model = "a/m"

[guardian]
profile = "minimal"
model = "a/g"

[permissions]
default_mode = "plan"
allow = ["shell(ls)"]
additional_directories = ["/x"]

[auto_mode]
environment = ["e"]

[agents]
max_parallel = 3

[shell_preclassifier]
allow_extra = ["tree"]

[tui]
theme = "light"
screen_reader = true
"#,
    );
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.tui.theme, ThemeName::Light);
    assert_eq!(c.permissions.default_mode, Some(PermissionMode::Plan));
    assert_eq!(c.guardian.model.as_deref(), Some("a/g"));
}

#[test]
fn no_value_disables_the_guardian() {
    // There is no key that turns it off: every spelling of "off" is a parse error.
    for text in [
        "[guardian]\nenabled = false\n",
        "[guardian]\nprofile = \"off\"\n",
        "[guardian]\nprofile = \"none\"\n",
        "[guardian]\ndisabled = true\n",
        "[guardian]\ntimeout_secs = 0\n",
    ] {
        assert!(
            parse_settings(text, Path::new("u.toml"), Column::User).is_err(),
            "{text}"
        );
    }
}

// --- telemetry required inputs ---

fn env_with(var: &'static str, value: &'static str) -> impl Fn(&str) -> Option<OsString> {
    move |k| (k == var).then(|| OsString::from(value))
}

#[test]
fn telemetry_enabled_requires_an_endpoint() {
    let w = World::new();
    w.user_file("[telemetry]\nenabled = true\napi_key_env = \"K\"\n");
    let env = env_with("K", "secret");
    let err = w.load_from(&w.work, None, &env).unwrap_err();
    assert!(matches!(err, ConfigError::Telemetry { .. }), "{err:?}");
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("endpoint"));
}

#[test]
fn telemetry_enabled_requires_a_resolvable_ingestion_key() {
    let w = World::new();
    let base = "[telemetry]\nenabled = true\nendpoint = \"https://t.example.com\"\n";

    // No api_key_env and no stored credential.
    w.user_file(base);
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Telemetry { .. }), "{err:?}");

    // The named env var is unset.
    w.user_file(&format!("{base}api_key_env = \"INGEST\"\n"));
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Telemetry { .. }), "{err:?}");
    assert!(err.to_string().contains("INGEST"));

    // The named env var is empty.
    let empty = env_with("INGEST", "");
    assert!(matches!(
        w.load_from(&w.work, None, &empty).unwrap_err(),
        ConfigError::Telemetry { .. }
    ));

    // A stored credential does not stand in for a named-but-missing env var.
    write(
        &w.data.join("credentials.toml"),
        "[telemetry]\napi_key = \"k\"\n",
    );
    assert!(matches!(
        w.load().unwrap_err(),
        ConfigError::Telemetry { .. }
    ));
}

#[test]
fn telemetry_enabled_with_endpoint_and_a_key_loads() {
    let w = World::new();
    w.user_file(
        "[telemetry]\nenabled = true\nendpoint = \"https://t.example.com\"\napi_key_env = \"INGEST\"\n",
    );
    let env = env_with("INGEST", "secret");
    let c = w.load_from(&w.work, None, &env).unwrap();
    assert!(c.telemetry.enabled);

    w.user_file("[telemetry]\nenabled = true\nendpoint = \"https://t.example.com\"\n");
    write(
        &w.data.join("credentials.toml"),
        "[telemetry]\napi_key = \"stored\"\n",
    );
    assert!(w.load().unwrap().telemetry.enabled);
}

#[test]
fn telemetry_disabled_needs_nothing() {
    let w = World::new();
    w.user_file("[telemetry]\nenabled = false\n");
    assert!(!w.load().unwrap().telemetry.enabled);
}

#[test]
fn an_unparseable_credentials_file_fails_telemetry_resolution() {
    let w = World::new();
    w.user_file("[telemetry]\nenabled = true\nendpoint = \"https://t.example.com\"\n");
    write(&w.data.join("credentials.toml"), "not toml [");
    let err = w.load().unwrap_err();
    assert!(matches!(err, ConfigError::Credentials { .. }), "{err:?}");
}

mod scanner_selection {
    use fontaine_core::config::{ConfigError, Scanner};

    use super::World;

    fn load(text: &str) -> Result<fontaine_core::config::Config, ConfigError> {
        let w = World::new();
        w.user_file(text);
        w.load()
    }

    #[test]
    fn the_scanner_is_off_by_default_and_the_token_variable_is_always_resolved() {
        let config = World::new().load().unwrap();
        assert_eq!(config.guardian.scanner, Scanner::Off);
        assert_eq!(config.guardian.scanner_hf_token_env, "HF_TOKEN");
        let named = load("[guardian]\nscanner_hf_token_env = \"MY_HF\"\n").unwrap();
        assert_eq!(named.guardian.scanner, Scanner::Off);
        assert_eq!(named.guardian.scanner_hf_token_env, "MY_HF");
    }

    #[test]
    fn exactly_off_llm_command_and_pg2_are_accepted_values() {
        assert_eq!(
            load("[guardian]\nscanner = \"off\"\n")
                .unwrap()
                .guardian
                .scanner,
            Scanner::Off
        );
        assert_eq!(
            load("[guardian]\nscanner = \"llm\"\n")
                .unwrap()
                .guardian
                .scanner,
            Scanner::Llm
        );
        let command =
            load("[guardian]\nscanner = \"command\"\nscanner_command = \"/bin/scan\"\n").unwrap();
        assert_eq!(command.guardian.scanner, Scanner::Command);
        for bad in ["on", "true", "LLM", "regex", ""] {
            let err = load(&format!("[guardian]\nscanner = \"{bad}\"\n")).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad}");
        }
        let err = load("[guardian]\nscanner = true\n").unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn the_command_scanner_needs_its_program() {
        for text in [
            "[guardian]\nscanner = \"command\"\n",
            "[guardian]\nscanner = \"command\"\nscanner_command = \"\"\n",
        ] {
            let err = load(text).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
            assert_eq!(err.exit_code(), 2);
            assert!(err.to_string().contains("scanner_command"), "{err}");
        }
    }

    #[cfg(not(feature = "pg2"))]
    #[test]
    fn pg2_in_a_build_without_the_feature_is_a_config_error() {
        let err = load("[guardian]\nscanner = \"pg2\"\n").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("pg2"), "{err}");
    }

    #[cfg(feature = "pg2")]
    #[test]
    fn pg2_in_a_build_with_the_feature_is_accepted() {
        let config = load("[guardian]\nscanner = \"pg2\"\n").unwrap();
        assert_eq!(config.guardian.scanner, Scanner::Pg2);
    }
}

// --- [guardian.screen] ---

const SCREEN_ACCOUNT: &str = "[accounts.local]\nkind = \"ollama\"\n";

fn screen_world(screen: &str) -> World {
    let w = World::new();
    w.user_file(&format!("{SCREEN_ACCOUNT}\n[guardian.screen]\n{screen}"));
    w
}

#[test]
fn the_screen_is_off_unless_configured() {
    let w = World::new();
    assert!(w.load().unwrap().guardian.screen.is_none());
}

#[test]
fn a_screen_defaults_to_shadow_with_no_threshold() {
    let w = screen_world("model = \"local/qwen3:4b\"\n");
    let screen = w.load().unwrap().guardian.screen.unwrap();
    assert!(screen.shadow);
    assert_eq!(screen.stop_threshold, None);
    assert_eq!(screen.timeout_ms, 2_000);
    assert_eq!(
        screen.question,
        fontaine_guardian::Question::EmployeeAllowed
    );
}

#[test]
fn stopping_calls_requires_a_threshold() {
    let w = screen_world("model = \"local/qwen3:4b\"\nshadow = false\n");
    let err = w.load().unwrap_err();
    assert!(err.to_string().contains("stop_threshold"), "{err}");
    assert_eq!(err.exit_code(), 2);

    let w = screen_world("model = \"local/qwen3:4b\"\nshadow = false\nstop_threshold = 0.95\n");
    let screen = w.load().unwrap().guardian.screen.unwrap();
    assert_eq!((screen.shadow, screen.stop_threshold), (false, Some(0.95)));
}

#[test]
fn a_screen_needs_a_model_on_an_ollama_account() {
    for text in [
        "[guardian.screen]\nstop_threshold = 0.9\n".to_string(),
        format!("{SCREEN_ACCOUNT}\n[guardian.screen]\nmodel = \"other/m\"\n"),
        "[accounts.o]\nkind = \"openai\"\n\n[guardian.screen]\nmodel = \"o/m\"\n".to_string(),
    ] {
        let w = World::new();
        w.user_file(&text);
        let err = w.load().unwrap_err();
        assert_eq!(err.exit_code(), 2, "{text}");
        assert!(err.to_string().contains("guardian.screen.model"), "{err}");
    }
}

#[test]
fn a_screen_account_needs_a_usable_base_url() {
    for url in ["not a url", "ftp://host/", "http://"] {
        let text = format!(
            "[accounts.local]\nkind = \"ollama\"\nbase_url = \"{url}\"\n\n[guardian.screen]\nmodel = \"local/m\"\n"
        );
        let w = World::new();
        w.user_file(&text);
        let err = w.load().unwrap_err();
        assert_eq!(err.exit_code(), 2, "{url}");
        assert!(err.to_string().contains("accounts.local.base_url"), "{err}");
    }
    let w = World::new();
    w.user_file(
        "[accounts.local]\nkind = \"ollama\"\nbase_url = \"http://box:11434\"\n\n[guardian.screen]\nmodel = \"local/m\"\n",
    );
    w.load().unwrap();
}

#[test]
fn screen_values_are_validated() {
    for bad in [
        "stop_threshold = 0.0",
        "stop_threshold = 1.5",
        "allow_threshold = 0.0",
        "allow_threshold = 1.0",
        "steering_veto = 0.0",
        "steering_veto = 1.5",
        "timeout_ms = 0",
        "question = \"anything_goes\"",
        "model = \"no-slash\"",
    ] {
        let text = format!("[guardian.screen]\nmodel = \"local/m\"\n{bad}\n");
        assert!(
            parse_settings(&text, Path::new("u.toml"), Column::User).is_err(),
            "{bad}"
        );
    }
    user_ok("[guardian.screen]\nmodel = \"local/m\"\nstop_threshold = 1.0\n");
    user_ok(
        "[guardian.screen]\nmodel = \"local/m\"\nallow_threshold = 0.00000156\nsteering_veto = 1.0\n",
    );
}

#[test]
fn an_allow_only_stage_loads_with_the_default_veto() {
    let w =
        screen_world("model = \"local/qwen3:4b\"\nshadow = false\nallow_threshold = 0.00000156\n");
    let screen = w.load().unwrap().guardian.screen.unwrap();
    assert_eq!(screen.stop_threshold, None);
    assert_eq!(screen.allow_threshold, Some(0.00000156));
    assert_eq!(screen.steering_veto, 0.001);
    assert!(!screen.shadow);
}

#[test]
fn a_shadow_stage_has_no_allow_threshold_until_one_is_set() {
    let w = screen_world("model = \"local/qwen3:4b\"\n");
    let screen = w.load().unwrap().guardian.screen.unwrap();
    assert_eq!(screen.allow_threshold, None);
    assert_eq!(screen.steering_veto, 0.001);
}

#[test]
fn a_stage_that_decides_needs_a_stop_or_an_allow_threshold() {
    let w = screen_world("model = \"local/qwen3:4b\"\nshadow = false\nsteering_veto = 0.01\n");
    let err = w.load().unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.to_string().contains("stop_threshold") && err.to_string().contains("allow_threshold"),
        "{err}"
    );
}

#[test]
fn only_user_scope_may_set_the_allow_keys() {
    for text in [
        "[guardian.screen]\nallow_threshold = 0.1\n",
        "[guardian.screen]\nsteering_veto = 0.5\n",
    ] {
        for column in [Column::Project, Column::ProjectLocal, Column::Cli] {
            let err = parse_settings(text, Path::new("p.toml"), column).unwrap_err();
            assert!(matches!(err, ConfigError::Scope { .. }), "{text}: {err:?}");
        }
    }
}

#[test]
fn shadow_stages_build_allow_settings() {
    let w = screen_world("model = \"local/qwen3:4b\"\n");
    let config = w.load().unwrap();
    let stage = fontaine_core::agent::build_screen(&config, fontaine_guardian::Profile::Full)
        .unwrap()
        .unwrap();
    assert_eq!(
        stage.screen.settings().allow,
        Some(fontaine_guardian::AllowSettings {
            t_allow: None,
            steering_veto: 0.001
        })
    );
    let w = screen_world("model = \"local/qwen3:4b\"\nshadow = false\nstop_threshold = 0.9\n");
    let config = w.load().unwrap();
    let stage = fontaine_core::agent::build_screen(&config, fontaine_guardian::Profile::Full)
        .unwrap()
        .unwrap();
    assert_eq!(stage.screen.settings().allow, None);
}

#[test]
fn project_scope_cannot_enable_the_screen_but_may_ask_for_shadow() {
    let w = World::new();
    w.user_file(&format!(
        "{SCREEN_ACCOUNT}\n[guardian.screen]\nmodel = \"local/m\"\nshadow = false\nstop_threshold = 0.9\n"
    ));
    for text in [
        "[guardian.screen]\nmodel = \"local/m\"\n",
        "[guardian.screen]\nstop_threshold = 0.5\n",
        "[guardian.screen]\nshadow = false\n",
    ] {
        for column in [Column::Project, Column::ProjectLocal, Column::Cli] {
            let err = parse_settings(text, Path::new("p.toml"), column).unwrap_err();
            assert!(matches!(err, ConfigError::Scope { .. }), "{text}: {err:?}");
        }
    }
    parse_settings(
        "[guardian.screen]\nshadow = true\n",
        Path::new("p.toml"),
        Column::Project,
    )
    .unwrap();

    // Shadow asked for by the repository holds over the user's `shadow = false`.
    w.project_file("[guardian.screen]\nshadow = true\n");
    let screen = w.load().unwrap().guardian.screen.unwrap();
    assert!(screen.shadow);
    assert_eq!(screen.stop_threshold, Some(0.9));
}

#[test]
fn a_project_file_alone_cannot_create_a_screen() {
    let w = World::new();
    w.project_file("[guardian.screen]\nshadow = true\n");
    assert!(w.load().unwrap().guardian.screen.is_none());
}

#[test]
fn the_autocompact_window_is_read_and_a_higher_scope_wins() {
    let w = World::new();
    assert_eq!(w.load().unwrap().context.autocompact_window, None);
    w.user_file("[context]\nautocompact_window = 150000\n");
    assert_eq!(w.load().unwrap().context.autocompact_window, Some(150_000));
    w.trusted_repo();
    w.local_file("[context]\nautocompact_window = 90000\n");
    assert_eq!(w.load().unwrap().context.autocompact_window, Some(90_000));
    let cli = w.cli_file("[context]\nautocompact_window = 8000\n");
    assert_eq!(
        w.load_with_settings(&cli)
            .unwrap()
            .context
            .autocompact_window,
        Some(8_000)
    );
}

#[test]
fn the_autocompact_window_must_be_positive_and_the_section_is_closed() {
    let err = parse_settings(
        "[context]\nautocompact_window = 0\n",
        Path::new("u.toml"),
        Column::User,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("context.autocompact_window"),
        "{err}"
    );
    let err = parse_settings(
        "[context]\nautocompact_pct = 80\n",
        Path::new("u.toml"),
        Column::User,
    );
    assert!(err.is_err());
}

#[test]
fn the_autocompact_window_is_refused_from_shared_project_scope_only() {
    let w = World::new();
    w.project_file("[context]\nautocompact_window = 150000\n");
    let err = w.load().unwrap_err();
    assert!(err.to_string().contains("context"), "{err}");
    let ok = "[context]\nautocompact_window = 150000\n";
    parse_settings(ok, Path::new("p.toml"), Column::ProjectLocal).unwrap();
    user_ok(ok);
}

// --- [env] ---

const SECRET: &str = "s3cr3t-pa55w0rd";

#[test]
fn env_merges_per_key_with_the_higher_scope_winning() {
    let w = World::new();
    w.trusted_repo();
    w.user_file("[env]\nA = \"user\"\nB = \"user\"\n");
    w.local_file("[env]\nB = \"local\"\nC = \"local\"\n");
    let cli = w.cli_file("[env]\nC = \"cli\"\nD = \"cli\"\n");
    let c = w.load_with_settings(&cli).unwrap();
    let got: Vec<(&str, &str)> = c
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        got,
        [("A", "user"), ("B", "local"), ("C", "cli"), ("D", "cli")]
    );
}

#[cfg(windows)]
#[test]
fn env_names_merge_case_insensitively_on_windows() {
    let w = World::new();
    w.user_file("[env]\nPATH = \"user\"\n");
    let cli = w.cli_file("[env]\nPath = \"cli\"\n");
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.env.len(), 1);
    assert_eq!(c.env["Path"].as_str(), "cli");
}

#[cfg(windows)]
#[test]
fn env_names_differing_only_by_case_in_one_file_are_rejected_on_windows() {
    let text = format!("[env]\nPATH = \"{SECRET}\"\nPath = \"{SECRET}\"\n");
    let err = parse_settings(&text, Path::new("u.toml"), Column::User).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    let shown = err.to_string();
    assert!(shown.contains("env.Path"), "{shown}");
    assert!(!shown.contains(SECRET), "{shown}");
}

#[test]
fn env_is_accepted_from_user_cli_and_trusted_local_scopes() {
    let text = "[env]\nX = \"1\"\n";
    for column in [Column::User, Column::Cli, Column::ProjectLocal] {
        parse_settings(text, Path::new("s.toml"), column)
            .unwrap_or_else(|e| panic!("{column:?}: {e}"));
    }
    let w = World::new();
    w.trusted_repo();
    w.local_file(text);
    assert_eq!(w.load().unwrap().env["X"].as_str(), "1");
}

#[test]
fn env_from_the_project_column_is_a_scope_error_naming_the_key() {
    let w = World::new();
    w.project_file(&format!("[env]\nTOKEN = \"{SECRET}\"\n"));
    let err = w.load().unwrap_err();
    assert!(
        matches!(&err, ConfigError::Scope { key, .. } if key == "env"),
        "{err:?}"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(!err.to_string().contains(SECRET));
}

#[test]
fn env_in_an_untrusted_local_file_is_rejected_because_it_loads_as_project() {
    let w = World::new();
    common::git_init(&w.work);
    w.local_file("[env]\nX = \"1\"\n");
    assert_scope_error(w.load(), "untrusted local");
}

#[test]
fn env_rejects_bad_names_and_values_without_echoing_the_value() {
    for (text, key) in [
        (format!("[env]\n\"\" = \"{SECRET}\"\n"), "env."),
        (format!("[env]\n\"A=B\" = \"{SECRET}\"\n"), "env.A=B"),
        (format!("[env]\n\"A\\u0000B\" = \"{SECRET}\"\n"), "env.A"),
        (format!("[env]\nA = \"{SECRET}\\u0000\"\n"), "env.A"),
        (
            format!("[env]\nFONTAINE_SH_COMMAND = \"{SECRET}\"\n"),
            "env.FONTAINE_SH_COMMAND",
        ),
        (
            format!("[env]\nfontaine_x = \"{SECRET}\"\n"),
            "env.fontaine_x",
        ),
    ] {
        let err = parse_settings(&text, Path::new("u.toml"), Column::User).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { .. }),
            "{text}: {err:?}"
        );
        assert_eq!(err.exit_code(), 2);
        let shown = err.to_string();
        assert!(shown.contains(key), "{key} in {shown}");
        assert!(!shown.contains(SECRET), "{shown}");
    }
}

#[test]
fn env_debug_output_redacts_values() {
    let w = World::new();
    w.user_file(&format!("[env]\nTOKEN = \"{SECRET}\"\n"));
    let c = w.load().unwrap();
    let shown = format!("{c:?}");
    assert!(shown.contains("TOKEN"));
    assert!(!shown.contains(SECRET), "{shown}");
    let settings = parse_settings(
        &format!("[env]\nTOKEN = \"{SECRET}\"\n"),
        Path::new("u.toml"),
        Column::User,
    )
    .unwrap();
    assert!(!format!("{settings:?}").contains(SECRET));
}

#[test]
fn a_malformed_env_entry_is_a_parse_error_that_does_not_quote_the_value() {
    for text in [
        format!("[env]\nTOKEN = \"{SECRET}\n"),
        "[env]\nPIN = 12345\n".to_string(),
        "[env]\nFLAG = true\n".to_string(),
        "[env]\nPIN = 1.5\n".to_string(),
        "[env]\nLIST = [\"a\"]\n".to_string(),
        "[env]\nSUB = { a = \"b\" }\n".to_string(),
    ] {
        let err = parse_settings(&text, Path::new("u.toml"), Column::User).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{text}: {err:?}");
        let shown = err.to_string();
        for leaked in [SECRET, "12345", "1.5", "true"] {
            assert!(!shown.contains(leaked), "{shown}");
        }
        assert!(shown.contains("u.toml"), "{shown}");
        if text.contains("12345") {
            assert!(
                shown.contains("environment values must be strings"),
                "{shown}"
            );
            assert!(shown.contains("2:"), "{shown}");
        }
    }
}

#[test]
fn network_is_accepted_in_user_and_settings_scope_and_validates_without_reading_files() {
    let text = "[network]\nhttps_proxy = \"http://proxy.corp:8080\"\nno_proxy = \"localhost\"\nca_bundle = \"/nonexistent/ca.pem\"\nclient_cert = \"/nonexistent/c.pem\"\nclient_key = \"/nonexistent/k.pem\"\n";
    user_ok(text);
    parse_settings(text, Path::new("cli.toml"), Column::Cli).unwrap();
    user_ok(
        "[accounts.w]\nkind = \"ollama\"\nhttps_proxy = \"http://p:1\"\nca_bundle = \"/nonexistent/ca.pem\"\n",
    );
}

#[test]
fn network_keys_merge_per_key_with_settings_over_user_and_accounts_over_both() {
    let w = World::new();
    w.user_file(
        "[network]\nhttps_proxy = \"http://user:1\"\nno_proxy = \"a\"\n\n[accounts.w]\nkind = \"ollama\"\nhttps_proxy = \"http://acct:2\"\n",
    );
    let cli = w.cli_file("[network]\nhttps_proxy = \"http://cli:3\"\n");
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.network.https_proxy.as_deref(), Some("http://cli:3"));
    assert_eq!(c.network.no_proxy.as_deref(), Some("a"));
    let acct = c.network_for(Some("w"));
    assert_eq!(acct.https_proxy.as_deref(), Some("http://acct:2"));
    assert_eq!(acct.no_proxy.as_deref(), Some("a"));
    assert_eq!(c.network_for(Some("absent")), c.network);
}

#[test]
fn empty_network_values_are_rejected() {
    for text in [
        "[network]\nhttps_proxy = \"\"\n",
        "[network]\nca_bundle = \"\"\n",
        "[accounts.x]\nkind = \"ollama\"\nno_proxy = \" \"\n",
    ] {
        assert!(
            parse_settings(text, Path::new("u.toml"), Column::User).is_err(),
            "{text}"
        );
    }
}

fn network_error(w: &World) -> String {
    match w.load() {
        Err(e @ ConfigError::Network { .. }) => {
            assert_eq!(e.exit_code(), 2);
            e.to_string()
        }
        other => panic!("expected a network error, got {other:?}"),
    }
}

#[test]
fn a_network_setting_that_cannot_load_stops_the_config_load() {
    let w = World::new();
    w.user_file("[network]\nca_bundle = \"/nonexistent/ca.pem\"\n");
    let text = network_error(&w);
    assert!(
        text.contains("ca_bundle") && text.contains("/nonexistent/ca.pem"),
        "{text}"
    );

    let w = World::new();
    w.user_file("[network]\nno_proxy = \"x\"\n");
    assert!(network_error(&w).contains("no_proxy"));

    let w = World::new();
    w.user_file("[network]\nclient_cert = \"/c.pem\"\n");
    assert!(network_error(&w).contains("client_cert and client_key"));
}

#[test]
fn an_account_that_breaks_the_network_rules_names_itself() {
    let w = World::new();
    w.user_file("[accounts.w]\nkind = \"ollama\"\nno_proxy = \"x\"\n");
    assert!(network_error(&w).contains("accounts.w"));

    // A proxy on the account does not rescue a `no_proxy` in `[network]`, which stays unmatched
    // for every other account.
    let w = World::new();
    w.user_file(
        "[network]\nno_proxy = \"x\"\n\n[accounts.w]\nkind = \"ollama\"\nhttps_proxy = \"http://p:1\"\n",
    );
    assert!(network_error(&w).contains("`network`"));
}

mod provenance {
    use super::common::World;
    use fontaine_core::config::{CliRules, LoadParams, Slot, load};

    fn load_with_rules(
        w: &World,
        settings: Option<&std::path::Path>,
        deny: &[String],
    ) -> Vec<Slot> {
        let config = load(&LoadParams {
            launch_dir: &w.work,
            config_dir: &w.config,
            data_dir: &w.data,
            settings_file: settings,
            cli_rules: CliRules { allow: &[], deny },
            env: &super::common::no_env,
        })
        .unwrap();
        config.sources["permissions.deny"].clone()
    }

    #[test]
    fn a_key_set_in_one_layer_names_that_layer() {
        let w = World::new();
        w.user_file("[tui]\ntheme = \"dark\"\n");
        let c = w.load().unwrap();
        assert_eq!(c.sources["tui.theme"], [Slot::User]);
    }

    #[test]
    fn layers_are_listed_highest_precedence_first() {
        let w = World::new();
        w.trusted_repo();
        w.user_file("[context]\nautocompact_window = 100\n");
        w.local_file("[context]\nautocompact_window = 200\n");
        let c = w.load().unwrap();
        assert_eq!(
            c.sources["context.autocompact_window"],
            [Slot::ProjectLocal, Slot::User]
        );
    }

    #[test]
    fn a_list_names_every_contributing_layer() {
        let w = World::new();
        w.user_file("[permissions]\ndeny = [\"shell(rm)\"]\n");
        w.project_file("[permissions]\ndeny = [\"shell(dd)\"]\n");
        let c = w.load().unwrap();
        assert_eq!(c.sources["permissions.deny"], [Slot::Project, Slot::User]);
    }

    #[test]
    fn an_unset_key_has_no_entry() {
        let w = World::new();
        w.user_file("[tui]\ntheme = \"dark\"\n");
        let c = w.load().unwrap();
        assert!(!c.sources.contains_key("tui.screen_reader"));
        assert!(!c.sources.contains_key("context.autocompact_window"));
    }

    #[test]
    fn env_and_account_entries_are_leaves_by_name() {
        let w = World::new();
        w.user_file("[env]\nHTTPS_PROXY = \"http://p\"\n\n[accounts.work]\nkind = \"anthropic\"\n");
        let c = w.load().unwrap();
        assert_eq!(c.sources["env.HTTPS_PROXY"], [Slot::User]);
        assert_eq!(c.sources["accounts.work.kind"], [Slot::User]);
    }

    #[test]
    fn flag_rules_are_cli_provenance() {
        let w = World::new();
        assert_eq!(
            load_with_rules(&w, None, &["shell(rm)".to_string()]),
            [Slot::Cli]
        );
    }

    #[test]
    fn flag_rules_join_a_settings_file_under_cli() {
        let w = World::new();
        w.user_file("[permissions]\ndeny = [\"shell(dd)\"]\n");
        let cli = w.cli_file("[tui]\ntheme = \"dark\"\n");
        let slots = load_with_rules(&w, Some(&cli), &["shell(rm)".to_string()]);
        assert_eq!(slots, [Slot::Cli, Slot::User]);
    }
}
