//! Loading, reloading and unloading plugins and skills in a built runtime without a restart.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::World;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::models::{GuardianChoice, Models};
use fontaine_core::permissions::{Answer, ApprovalRequest, Approver};
use fontaine_core::plugin_ops::{
    Fetch, GitMode, Workdir, commit_install, prepare_install, set_enabled, uninstall,
};
use fontaine_core::plugins::{MarketplaceEntry, Registry, marketplace_dir, write_registry};
use futures::future::BoxFuture;
use rig_core::test_utils::MockCompletionModel;

#[derive(Default)]
struct Capture {
    notices: Mutex<Vec<String>>,
    warnings: Mutex<Vec<String>>,
}

impl Approver for Capture {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, Answer> {
        Box::pin(async { Answer::Deny { feedback: None } })
    }

    fn notice(&self, message: &str) {
        self.notices.lock().unwrap().push(message.to_string());
    }

    fn warn(&self, message: &str) {
        self.warnings.lock().unwrap().push(message.to_string());
    }
}

impl Capture {
    fn notices(&self) -> Vec<String> {
        self.notices.lock().unwrap().clone()
    }

    fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock().unwrap())
    }
}

fn runtime(w: &World, approver: Arc<dyn Approver>) -> AgentRuntime {
    let main = MockCompletionModel::from_stream_turns(Vec::<
        Vec<rig_core::test_utils::MockStreamEvent>,
    >::new());
    let handle = fontaine_core::models::ModelHandle::new(
        fontaine_core::models::ModelRef::parse("test/mock").unwrap(),
        fontaine_core::config::AccountKind::named("openai-compatible"),
        main.into(),
    );
    let models = Models {
        main_chain: fontaine_core::model_roles::ModelChain::single(handle.reference.clone()),
        main: handle,
        main_listing: fontaine_core::models::Listing::default(),
        guardian: None,
        guardian_choice: GuardianChoice::Pinned,
        notices: Vec::new(),
    };
    common::runtime::build_with_approver(
        w,
        Manual,
        models,
        None,
        false,
        Default::default(),
        None,
        approver,
    )
}

const SKILL: &str = "---\ndescription: reviews the diff\n---\nLook at the diff.\n";

/// A local marketplace `team` offering the path-source plugin `review`.
fn marketplace(config: &Path) {
    let market = marketplace_dir(config, "team");
    common::write(
        &market.join(".claude-plugin/marketplace.json"),
        r#"{"name": "team", "plugins": [{"name": "review", "source": "./plugins/review"}]}"#,
    );
    let plugin = market.join("plugins/review");
    common::write(
        &plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "review"}"#,
    );
    common::write(&plugin.join("skills/review/SKILL.md"), SKILL);
    write_registry(
        config,
        &Registry {
            marketplace: vec![MarketplaceEntry {
                name: "team".into(),
                url: "unused".into(),
                pin: "pin-one".into(),
                ..Default::default()
            }],
            plugin: Vec::new(),
        },
    )
    .unwrap();
}

fn install(config: &Path) {
    let fx = Fetch::new(config, &|_| None, GitMode::Interactive);
    let prepared = prepare_install(&fx, "review@team", Workdir::Cli).unwrap();
    commit_install(&fx, prepared).unwrap();
}

fn skill_names(runtime: &AgentRuntime) -> Vec<String> {
    runtime
        .extensions()
        .skills
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

#[tokio::test]
async fn a_plugin_loads_reloads_and_unloads_without_a_restart() {
    let w = World::new();
    marketplace(&w.config);
    let approver = Arc::new(Capture::default());
    let mut runtime = runtime(&w, approver.clone());
    assert!(!skill_names(&runtime).contains(&"review:review".to_string()));

    install(&w.config);
    let report = runtime.reload_extensions(&[]).await;
    assert!(!report.failed, "{report:?}");
    assert!(
        report.lines.contains(&"loaded plugin `review`".to_string()),
        "{report:?}"
    );
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));
    assert!(
        runtime
            .skills()
            .extensions()
            .skill("review:review")
            .is_some(),
        "the skill tool reads the same cell"
    );
    assert!(report.rebuilt, "the skill index is part of the prompt");
    assert!(
        runtime.state.take_hook_context().is_empty(),
        "nothing went away"
    );

    // Nothing changed: no rebuild, one line saying so.
    let again = runtime.reload_extensions(&[]).await;
    assert!(!again.rebuilt);
    assert_eq!(again.lines, ["plugins and skills are already up to date"]);

    // An edit to the plugin's skill is a reload of that plugin only.
    let skill = fontaine_core::plugins::checkout_dir(&w.config, "team", "review")
        .join("skills/review/SKILL.md");
    std::fs::write(
        &skill,
        "---\ndescription: reviews the diff, carefully\n---\nBody.\n",
    )
    .unwrap();
    let edited = runtime.reload_extensions(&[]).await;
    assert!(
        edited.lines.iter().any(|l| l == "reloaded plugin `review`"),
        "{edited:?}"
    );
    assert!(edited.rebuilt);
    let note = runtime.state.take_hook_context().join("\n");
    assert!(
        note.contains("plugin `review`") && note.contains("may be stale"),
        "{note}"
    );

    // Disabling unloads it; enabling loads it again.
    set_enabled(&w.config, "review", false).unwrap();
    let off = runtime.reload_extensions(&[]).await;
    assert!(
        off.lines.contains(&"unloaded plugin `review`".to_string()),
        "{off:?}"
    );
    assert!(!skill_names(&runtime).contains(&"review:review".to_string()));
    assert!(
        runtime
            .state
            .take_hook_context()
            .join("\n")
            .contains("may be stale")
    );
    set_enabled(&w.config, "review", true).unwrap();
    runtime.reload_extensions(&[]).await;
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));

    uninstall(&w.config, "review").unwrap();
    runtime.reload_extensions(&[]).await;
    assert!(!skill_names(&runtime).contains(&"review:review".to_string()));
    assert!(
        approver
            .notices()
            .iter()
            .any(|n| n.contains("unloaded plugin `review`"))
    );
}

#[tokio::test]
async fn a_replaced_checkout_reloads_the_plugin_whatever_its_definitions_say() {
    let w = World::new();
    marketplace(&w.config);
    install(&w.config);
    let mut runtime = runtime(&w, Arc::new(Capture::default()));
    // Loaded at startup by the same registry; the runtime above started with none, so the first
    // reload loads it, and the forced one after reloads it though nothing differs.
    runtime.reload_extensions(&[]).await;
    let quiet = runtime.reload_extensions(&[]).await;
    assert_eq!(quiet.lines, ["plugins and skills are already up to date"]);
    let forced = runtime.reload_extensions(&["review".to_string()]).await;
    assert_eq!(forced.lines, ["reloaded plugin `review`"], "{forced:?}");
}

#[tokio::test]
async fn a_load_that_fails_keeps_the_current_set_running() {
    let w = World::new();
    marketplace(&w.config);
    install(&w.config);
    let approver = Arc::new(Capture::default());
    let mut runtime = runtime(&w, approver.clone());
    runtime.reload_extensions(&[]).await;
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));

    // A registry that cannot be read must not unload every plugin.
    std::fs::write(w.config.join("plugins.toml"), "this is = not [valid").unwrap();
    let report = runtime.reload_extensions(&[]).await;
    assert!(report.failed, "{report:?}");
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));
    assert!(
        approver
            .warnings
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("the current set stays active"))
    );
}

#[tokio::test]
async fn an_mcp_name_collision_on_reload_keeps_the_current_set_running() {
    let w = World::new();
    marketplace(&w.config);
    install(&w.config);
    let approver = Arc::new(Capture::default());
    let mut runtime = runtime(&w, approver.clone());
    runtime.reload_extensions(&[]).await;
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));

    // The plugin gains a server whose composed name a configured server already has.
    common::write(
        &fontaine_core::plugins::checkout_dir(&w.config, "team", "review").join(".mcp.json"),
        r#"{"mcpServers": {"srv": {"command": "fontaine-no-such-binary"}}}"#,
    );
    let mut config = runtime.config.clone();
    config.mcp_servers.insert(
        "plugin_review_srv".to_string(),
        fontaine_core::mcp_config::stdio("true", &[], Default::default(), Vec::new()),
    );
    runtime.config = config;
    let report = runtime.reload_extensions(&[]).await;
    assert!(report.failed, "{report:?}");
    assert!(
        approver
            .warnings
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("collides") && w.contains("the current set stays active")),
        "{:?}",
        approver.warnings.lock().unwrap()
    );
    assert!(skill_names(&runtime).contains(&"review:review".to_string()));
    assert!(runtime.config.mcp_servers.contains_key("plugin_review_srv"));
}

#[tokio::test]
async fn a_plugins_mcp_server_starts_and_stops_with_the_plugin() {
    let w = World::new();
    marketplace(&w.config);
    common::write(
        &marketplace_dir(&w.config, "team").join("plugins/review/.mcp.json"),
        r#"{"mcpServers": {"srv": {"command": "fontaine-no-such-binary"}}}"#,
    );
    install(&w.config);
    let approver = Arc::new(Capture::default());
    let mut runtime = runtime(&w, approver.clone());
    assert!(runtime.mcp_states().is_empty());

    // The binary does not exist, so the server fails to start: it keeps a failed slot, and says so.
    runtime.reload_extensions(&[]).await;
    let states = runtime.mcp_states();
    assert_eq!(states.len(), 1, "{states:?}");
    assert_eq!(states[0].0, "plugin_review_srv");
    assert!(
        matches!(states[0].1, fontaine_core::agent::McpServerState::Failed(_)),
        "{states:?}"
    );
    assert!(!approver.warnings.lock().unwrap().is_empty());

    uninstall(&w.config, "review").unwrap();
    runtime.reload_extensions(&[]).await;
    assert!(
        runtime.mcp_states().is_empty(),
        "{:?}",
        runtime.mcp_states()
    );
}

#[tokio::test]
async fn an_edited_user_skill_is_noticed_once_and_applies_on_reload() {
    let w = World::new();
    common::write(&w.config.join("skills/mine/SKILL.md"), SKILL);
    let approver = Arc::new(Capture::default());
    let mut runtime = runtime(&w, approver.clone());
    assert_eq!(
        runtime.extensions().skill("mine").unwrap().description,
        "reviews the diff"
    );

    runtime.refresh_before_turn().unwrap();
    assert!(approver.take_notices().is_empty(), "nothing changed yet");

    // A new mtime and length, whatever the filesystem's timestamp resolution.
    std::fs::write(
        w.config.join("skills/mine/SKILL.md"),
        "---\ndescription: reviews it all\n---\nA longer body than before.\n",
    )
    .unwrap();
    runtime.refresh_before_turn().unwrap();
    let notices = approver.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("run /reload-plugins"), "{notices:?}");
    runtime.refresh_before_turn().unwrap();
    assert!(approver.take_notices().is_empty(), "told once per change");

    // The session still has the old skill until the reload.
    assert_eq!(
        runtime.extensions().skill("mine").unwrap().description,
        "reviews the diff"
    );
    let report = runtime.reload_extensions(&[]).await;
    assert!(
        report.lines.iter().any(|l| l == "skills changed: mine"),
        "{report:?}"
    );
    assert_eq!(
        runtime.extensions().skill("mine").unwrap().description,
        "reviews it all"
    );
    assert!(
        runtime
            .state
            .take_hook_context()
            .join("\n")
            .contains("skill `mine`")
    );

    // A removed skill goes, and the reload that applied it left a fresh baseline.
    std::fs::remove_dir_all(w.config.join("skills/mine")).unwrap();
    runtime.reload_extensions(&[]).await;
    assert!(runtime.extensions().skill("mine").is_none());
    approver.take_notices();
    runtime.refresh_before_turn().unwrap();
    assert!(approver.take_notices().is_empty());
}

#[tokio::test]
async fn a_changed_agent_type_alone_rebuilds_the_agent() {
    let w = World::new();
    let mut runtime = runtime(&w, Arc::new(Capture::default()));
    let first = runtime.reload_extensions(&[]).await;
    assert!(!first.rebuilt);
    common::write(
        &w.config.join("agents/helper.md"),
        "---\nname: helper\ndescription: helps out\n---\nYou help.\n",
    );
    let report = runtime.reload_extensions(&[]).await;
    assert!(
        report.rebuilt,
        "the agent tool's description lists the type"
    );
    assert!(runtime.agent_types().get("helper").is_some());
}
