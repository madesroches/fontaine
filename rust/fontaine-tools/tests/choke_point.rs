mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::sync::Arc;

use common::{AllowAll, Dirs, env_with, registry, scope};
use fontaine_tools::builtins::{self, ShellSettings};
use fontaine_tools::descriptor::{TOOLS, find};
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::names::is_builtin;
use fontaine_tools::powershell::PowerShellSettings;
use fontaine_tools::risk::RiskClass;
use rig_core::tool::{Tool, ToolContext};
use rig_tools::{OutputLimit, Seen, WriteArgs};

const EXPECTED: [(&str, RiskClass); 12] = [
    ("read", RiskClass::ReadOnly),
    ("ls", RiskClass::ReadOnly),
    ("glob", RiskClass::ReadOnly),
    ("grep", RiskClass::ReadOnly),
    ("write", RiskClass::WorkspaceWrite),
    ("edit", RiskClass::WorkspaceWrite),
    ("notebook_edit", RiskClass::WorkspaceWrite),
    ("shell", RiskClass::Arbitrary),
    ("job_output", RiskClass::ReadOnly),
    ("job_cancel", RiskClass::Arbitrary),
    ("monitor", RiskClass::Arbitrary),
    ("webfetch", RiskClass::Arbitrary),
];

fn registered() -> Vec<builtins::Registered> {
    let d = Dirs::new();
    let s = scope(&d.dir("work"), &[]);
    registry(&env_with(s, Arc::new(AllowAll::default())))
}

#[test]
fn the_registered_set_is_exactly_the_documented_one() {
    let tools = registered();
    let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate registrations: {names:?}"
    );
    let expected: BTreeSet<&str> = EXPECTED.iter().map(|(name, _)| *name).collect();
    assert_eq!(unique, expected);
    for removed in ["delete", "move", "list", "search"] {
        assert!(
            !unique.contains(removed),
            "{removed} must not be registered"
        );
    }
}

#[test]
fn powershell_exists_only_when_an_interpreter_was_found() {
    let d = Dirs::new();
    let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
    let without: BTreeSet<&str> = registry(&env).iter().map(|t| t.name).collect();
    assert_eq!(without.len(), EXPECTED.len());
    assert!(!without.contains("powershell"));

    env.shell.powershell = Some(PowerShellSettings {
        path: d.base.join("pwsh.exe"),
    });
    let tools = registry(&env);
    let with: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    assert_eq!(with.len(), EXPECTED.len() + 1);
    assert!(with.contains("powershell"));
    let tool = tools.iter().find(|t| t.name == "powershell").unwrap();
    assert_eq!(tool.risk, RiskClass::Arbitrary);
    assert_eq!(find("powershell").map(|d| d.risk), Some(tool.risk));
    let definition = tool.tool.definition();
    assert!(definition.description.contains("PowerShell"));
    assert!(definition.parameters["properties"]["run_in_background"].is_object());
}

#[test]
fn registered_names_are_known_built_in_names() {
    for tool in registered() {
        assert!(is_builtin(tool.name), "{} has no descriptor row", tool.name);
    }
}

#[test]
fn every_registered_built_in_has_its_risk_class() {
    for tool in registered() {
        let table = find(tool.name).map(|d| d.risk);
        assert!(table.is_some(), "{} has no descriptor row", tool.name);
        assert_eq!(Some(tool.risk), table, "{}", tool.name);
        let expected = EXPECTED
            .iter()
            .find(|(name, _)| *name == tool.name)
            .unwrap()
            .1;
        assert_eq!(tool.risk, expected, "{}", tool.name);
    }
    let tools = registered();
    let registered_names: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
    let table: BTreeSet<&str> = TOOLS
        .iter()
        .map(|d| d.name)
        .filter(|name| EXPECTED.iter().any(|(built_in, _)| built_in == name))
        .collect();
    assert_eq!(table, registered_names);
}

#[test]
fn registered_definitions_carry_the_model_facing_schemas() {
    for tool in registered() {
        let definition = tool.tool.definition();
        assert_eq!(definition.name.to_string(), tool.name);
        assert!(!definition.description.is_empty(), "{}", tool.name);
        assert_eq!(definition.parameters["type"], "object", "{}", tool.name);
    }
}

#[test]
fn file_tools_are_confined_to_their_root() {
    let d = Dirs::new();
    let root = d.dir("work");
    let seen = Seen::default();
    assert!(builtins::read_for(&root, &seen).confine);
    assert!(builtins::edit_for(&root, &seen).confine);
    assert!(builtins::write_for(&root, &seen).confine);
    assert!(builtins::notebook_edit_for(&root, &seen).confine);
    assert!(builtins::list_for(&root).confine);
    assert!(builtins::search_for(&root).confine);
    assert_eq!(
        builtins::read_for(&root, &seen).base.as_deref(),
        Some(root.as_path())
    );
}

#[test]
fn the_external_read_fallbacks_are_unconfined_but_based_at_the_primary_root() {
    let d = Dirs::new();
    let root = d.dir("work");
    let seen = Seen::default();
    // The fallback an approved out-of-root read executes on: unconfined, because the gate
    // already adjudicated the resolved target, and read-only like the instances it backs.
    let read = builtins::read_external(&root, &seen);
    assert!(!read.confine);
    assert_eq!(read.base.as_deref(), Some(root.as_path()));
    let list = builtins::list_external(&root);
    assert!(!list.confine);
    assert_eq!(list.base.as_deref(), Some(root.as_path()));
    let search = builtins::search_external(&root);
    assert!(!search.confine);
    assert_eq!(search.base.as_deref(), Some(root.as_path()));
    // Writes never have a fallback: every write factory stays confined.
    assert!(builtins::write_for(&root, &seen).confine);
    assert!(builtins::edit_for(&root, &seen).confine);
    assert!(builtins::notebook_edit_for(&root, &seen).confine);
}

#[tokio::test]
async fn rig_itself_refuses_a_path_outside_the_root() {
    let d = Dirs::new();
    let root = d.dir("work");
    let write = builtins::write_for(&root, &Seen::default());
    let refused = write
        .call(
            &mut ToolContext::new(),
            WriteArgs::new("../escaped.txt", "x"),
        )
        .await;
    assert!(refused.is_err());
    assert!(!d.base.join("escaped.txt").exists());
}

#[test]
fn the_shell_is_confined_clamped_and_cut() {
    let d = Dirs::new();
    let root = d.dir("work");
    let settings = ShellSettings {
        max_timeout_secs: 7,
        scrub_env: vec![OsString::from("SECRET_ONE"), OsString::from("SECRET_TWO")],
        set_env: Vec::new(),
        git_bash: None,
        powershell: None,
    };
    let tool = builtins::shell_for(&root, &settings);
    assert!(!tool.through_env);
    let shell = tool.shell;
    assert!(shell.confine);
    assert_eq!(shell.max_timeout, 7);
    assert_eq!(shell.cut, Some(OutputLimit::default()));
    assert_eq!(shell.launcher.cwd.as_deref(), Some(root.as_path()));
    assert!(
        shell
            .launcher
            .unset_env
            .contains(&OsString::from("SECRET_ONE"))
    );
    assert!(
        shell
            .launcher
            .unset_env
            .contains(&OsString::from("SECRET_TWO"))
    );

    let with_git_bash = ShellSettings {
        git_bash: Some(GitBash {
            bin: root.join("bin"),
            usr_bin: root.join("usr").join("bin"),
        }),
        ..settings
    };
    assert!(builtins::shell_for(&root, &with_git_bash).through_env);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use common::{call, text};
    use serde_json::json;

    #[tokio::test]
    async fn a_longer_timeout_than_the_maximum_is_cut_to_it() {
        use fontaine_tools::jobs::NoPromote;

        let d = Dirs::new();
        let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
        env.shell.max_timeout_secs = 1;
        let tools = registry(&env);
        let started = std::time::Instant::now();
        // A call that must not move to the background ends at the clamped timeout.
        let mut context = common::as_root();
        context.insert(NoPromote(true)).unwrap();
        let out = common::tool(&tools, "shell")
            .tool
            .run_with(
                &mut context,
                json!({"command": "sleep 30", "timeout_secs": 600}),
            )
            .await
            .unwrap();
        assert!(started.elapsed().as_secs() < 20);
        assert!(text(&out).contains("timed out after 1s"), "{}", text(&out));
        // The main agent's call moves to the background at the same clamped timeout.
        let out = call(
            &tools,
            "shell",
            json!({"command": "sleep 30", "timeout_secs": 600}),
        )
        .await
        .unwrap();
        assert!(
            text(&out).contains("still running after 1s; moved to background job"),
            "{}",
            text(&out)
        );
    }

    #[tokio::test]
    async fn long_shell_output_is_cut() {
        let d = Dirs::new();
        let tools = registry(&env_with(
            scope(&d.dir("work"), &[]),
            Arc::new(AllowAll::default()),
        ));
        let out = call(&tools, "shell", json!({"command": "seq 1 20000"}))
            .await
            .unwrap();
        let rendered = text(&out);
        assert!(
            rendered.lines().count() < 5000,
            "{}",
            rendered.lines().count()
        );
        assert!(rendered.contains("20000"), "the last lines are kept");
    }

    fn through_the_environment(d: &Dirs) -> Vec<builtins::Registered> {
        let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
        let nope = d.base.join("nope");
        env.shell.git_bash = Some(GitBash {
            bin: nope.join("bin"),
            usr_bin: nope.join("usr").join("bin"),
        });
        registry(&env)
    }

    #[tokio::test]
    async fn a_command_through_the_environment_runs_exactly_as_written() {
        let d = Dirs::new();
        let tools = through_the_environment(&d);
        let command = r"printf '%s\n' '\\a' 'tail\\' '\\?\C:\x'";
        let out = call(&tools, "shell", json!({ "command": command }))
            .await
            .unwrap();
        let shown = text(&out);
        let lines: Vec<&str> = shown.lines().collect();
        for expected in [r"\\a", r"tail\\", r"\\?\C:\x"] {
            assert!(lines.contains(&expected), "{expected} in {shown}");
        }
        let out = call(
            &tools,
            "shell",
            json!({"command": "echo \"hello $((1 + 2))\" | cat"}),
        )
        .await
        .unwrap();
        assert!(text(&out).contains("hello 3"), "{}", text(&out));
    }

    #[tokio::test]
    async fn a_command_through_the_environment_keeps_its_exit_code_and_sequencing() {
        let d = Dirs::new();
        let tools = through_the_environment(&d);
        let out = call(&tools, "shell", json!({"command": "exit 3"}))
            .await
            .unwrap();
        assert!(text(&out).contains("exit code 3"), "{}", text(&out));
        let out = call(
            &tools,
            "shell",
            json!({"command": "false && echo x; echo after"}),
        )
        .await
        .unwrap();
        let shown = text(&out);
        assert!(shown.contains("after") && !shown.contains("x\n"), "{shown}");
    }

    #[tokio::test]
    async fn the_command_variable_is_not_visible_to_the_command() {
        let d = Dirs::new();
        let tools = through_the_environment(&d);
        let out = call(
            &tools,
            "shell",
            json!({"command": "printf '%s' \"${FONTAINE_SH_COMMAND-absent}\""}),
        )
        .await
        .unwrap();
        assert!(text(&out).contains("absent"), "{}", text(&out));
    }
}
