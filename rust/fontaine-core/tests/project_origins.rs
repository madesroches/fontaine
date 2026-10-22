//! Where a hook or an MCP server was defined decides whether it needs workspace trust and
//! whether its `allow` counts; the trust store remembers which of a repository's servers the
//! user approved.

mod common;

use std::sync::Arc;

use common::{World, git, git_init};
use fontaine_core::config::{Config, Slot};
use fontaine_core::trust;
use fontaine_core::user_hooks::{self, HookProcess};

const HOOK: &str = "[[hooks.pre_tool_use]]\ncommand = \"check.sh\"\n";
const SERVER: &str = "[mcp.servers.srv]\ncommand = \"srv\"\n";

fn process() -> Arc<HookProcess> {
    Arc::new(HookProcess {
        scrub_env: Vec::new(),
        git_bash: None,
        project_dir: std::env::temp_dir(),
    })
}

fn counts(config: &Config, project_executables: bool) -> (usize, usize) {
    let built = user_hooks::build(config, project_executables, process());
    (built.pre.len(), built.post.len())
}

#[test]
fn a_hooks_matcher_is_optional() {
    let w = World::new();
    w.user_file(HOOK);
    let c = w.load().unwrap();
    assert_eq!(c.hooks.pre_tool_use[0].matcher, None);
}

#[test]
fn user_cli_and_project_hooks_and_servers_know_where_they_came_from() {
    let w = World::new();
    w.user_file(&format!("{HOOK}{SERVER}"));
    w.project_file(
        "[[hooks.pre_tool_use]]\ncommand = \"project.sh\"\n[mcp.servers.repo]\ncommand = \"repo\"\n",
    );
    let c = w.load().unwrap();
    let origin = |command: &str| {
        c.hooks
            .pre_tool_use
            .iter()
            .find(|h| h.command == command)
            .unwrap()
            .origin
    };
    assert_eq!(origin("check.sh").slot, Slot::User);
    assert!(origin("check.sh").user_trust && !origin("check.sh").is_project());
    assert_eq!(origin("project.sh").slot, Slot::Project);
    assert!(!origin("project.sh").user_trust && origin("project.sh").is_project());
    assert!(!c.mcp_origins["srv"].is_project());
    assert!(c.mcp_origins["repo"].is_project() && !c.mcp_origins["repo"].user_trust);
}

#[test]
fn an_untracked_local_file_in_a_trusted_root_has_user_trust_but_is_still_a_project_executable() {
    let w = World::new();
    w.trusted_repo();
    w.local_file(&format!("{HOOK}{SERVER}"));
    let c = w.load().unwrap();
    let hook = &c.hooks.pre_tool_use[0];
    assert_eq!(hook.origin.slot, Slot::ProjectLocal);
    assert!(hook.origin.user_trust, "its allow counts as a user grant");
    assert!(
        hook.origin.is_project(),
        "and it still waits for workspace trust"
    );
    assert!(c.mcp_origins["srv"].user_trust);
}

#[test]
fn a_local_file_that_is_tracked_or_in_an_untrusted_root_is_project_scope() {
    // Untrusted root.
    let w = World::new();
    git_init(&w.work);
    w.local_file(HOOK);
    assert!(!w.load().unwrap().hooks.pre_tool_use[0].origin.user_trust);
    // Tracked file.
    let w = World::new();
    w.trusted_repo();
    w.local_file(HOOK);
    git(&w.work, &["add", "-f", ".fontaine/settings.local.toml"]);
    assert!(!w.load().unwrap().hooks.pre_tool_use[0].origin.user_trust);
}

#[test]
fn without_workspace_trust_the_repositorys_hooks_are_not_built_and_the_users_are() {
    let w = World::new();
    w.user_file(HOOK);
    w.project_file(
        "[[hooks.pre_tool_use]]\ncommand = \"project.sh\"\n[[hooks.post_tool_use]]\ncommand = \"post.sh\"\n",
    );
    let c = w.load().unwrap();
    assert_eq!(counts(&c, true), (2, 1));
    assert_eq!(counts(&c, false), (1, 0));
}

#[test]
fn the_same_hook_in_two_scopes_runs_once_under_the_higher_trust() {
    let w = World::new();
    w.user_file(HOOK);
    w.project_file(HOOK);
    let c = w.load().unwrap();
    assert_eq!(c.hooks.pre_tool_use.len(), 1);
    assert!(c.hooks.pre_tool_use[0].origin.user_trust);
}

#[test]
fn a_server_approval_is_for_one_root_one_name_and_one_definition() {
    let w = World::new();
    let other = w.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    assert!(!trust::server_approved(&w.data, &w.work, "srv", "d1"));

    trust::approve_server(&w.data, &w.work, "srv", "d1").unwrap();
    assert!(trust::server_approved(&w.data, &w.work, "srv", "d1"));
    assert!(
        !trust::server_approved(&w.data, &w.work, "srv", "d2"),
        "a changed definition"
    );
    assert!(!trust::server_approved(&w.data, &w.work, "srv2", "d1"));
    assert!(
        !trust::server_approved(&w.data, &other, "srv", "d1"),
        "another root"
    );

    // Approving twice writes it once, and trusting a root keeps the approvals.
    trust::approve_server(&w.data, &w.work, "srv", "d1").unwrap();
    trust::record(&w.data, &w.work).unwrap();
    let text = std::fs::read_to_string(trust::trust_path(&w.data)).unwrap();
    assert_eq!(text.matches("[[approved_servers]]").count(), 1, "{text}");
    assert!(trust::is_trusted(&w.data, &w.work));
    trust::validate_trust_toml(&text).unwrap();
}

#[test]
fn approving_a_server_trusts_nothing() {
    let w = World::new();
    trust::approve_server(&w.data, &w.work, "srv", "d1").unwrap();
    assert!(!trust::is_trusted(&w.data, &w.work));
}

#[cfg(unix)]
#[test]
fn the_trust_store_stays_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    trust::approve_server(&w.data, &w.work, "srv", "d1").unwrap();
    let mode = std::fs::metadata(trust::trust_path(&w.data))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
}

#[test]
fn a_hook_defined_in_several_scopes_is_kept_under_the_origin_with_more_trust() {
    let w = World::new();
    w.project_file(HOOK);
    let cli = w.cli_file(HOOK);
    let c = w.load_with_settings(&cli).unwrap();
    assert_eq!(c.hooks.pre_tool_use.len(), 1);
    let origin = c.hooks.pre_tool_use[0].origin;
    assert_eq!(origin.slot, Slot::Cli);
    assert!(origin.user_trust && !origin.is_project());
}
