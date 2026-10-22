mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::sync::Arc;

use common::{AllowAll, Dirs, env_with, registry, scope};
use fontaine_tools::builtins::{self, ShellSettings};
use fontaine_tools::names::{BUILTIN_TOOL_NAMES, is_builtin};
use fontaine_tools::risk::{BUILTIN_RISK, RiskClass, builtin_risk};
use rig_core::tool::{Tool, ToolContext};
use rig_tools::{OutputLimit, Seen, WriteArgs};

const EXPECTED: [(&str, RiskClass); 8] = [
    ("read", RiskClass::ReadOnly),
    ("ls", RiskClass::ReadOnly),
    ("glob", RiskClass::ReadOnly),
    ("grep", RiskClass::ReadOnly),
    ("write", RiskClass::WorkspaceWrite),
    ("edit", RiskClass::WorkspaceWrite),
    ("shell", RiskClass::Arbitrary),
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
fn registered_names_are_known_built_in_names() {
    for tool in registered() {
        assert!(
            is_builtin(tool.name),
            "{} is not in BUILTIN_TOOL_NAMES",
            tool.name
        );
        assert!(BUILTIN_TOOL_NAMES.contains(&tool.name));
    }
}

#[test]
fn every_registered_built_in_has_its_risk_class() {
    for tool in registered() {
        let table = builtin_risk(tool.name);
        assert!(
            table.is_some(),
            "{} has no entry in the risk table",
            tool.name
        );
        assert_eq!(Some(tool.risk), table, "{}", tool.name);
        let expected = EXPECTED
            .iter()
            .find(|(name, _)| *name == tool.name)
            .unwrap()
            .1;
        assert_eq!(tool.risk, expected, "{}", tool.name);
    }
    let table: BTreeSet<&str> = BUILTIN_RISK.iter().map(|(name, _)| *name).collect();
    let tools = registered();
    let registered_names: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
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
    assert!(builtins::list_for(&root).confine);
    assert!(builtins::search_for(&root).confine);
    assert_eq!(
        builtins::read_for(&root, &seen).base.as_deref(),
        Some(root.as_path())
    );
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
        git_bash: None,
    };
    let shell = builtins::shell_for(&root, &settings);
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
}

#[cfg(unix)]
mod unix {
    use super::*;
    use common::{call, text};
    use serde_json::json;

    #[tokio::test]
    async fn a_longer_timeout_than_the_maximum_is_cut_to_it() {
        let d = Dirs::new();
        let mut env = env_with(scope(&d.dir("work"), &[]), Arc::new(AllowAll::default()));
        env.shell.max_timeout_secs = 1;
        let tools = registry(&env);
        let started = std::time::Instant::now();
        let out = call(
            &tools,
            "shell",
            json!({"command": "sleep 30", "timeout_secs": 600}),
        )
        .await
        .unwrap();
        assert!(started.elapsed().as_secs() < 20);
        assert!(text(&out).contains("timed out after 1s"), "{}", text(&out));
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
}
