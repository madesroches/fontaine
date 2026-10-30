mod common;

use std::sync::{Arc, Mutex};

use common::World;
use common::runtime::build_with_approver;
use fontaine_core::adjudication;
use fontaine_core::config::{AccountKind, PermissionMode};
use fontaine_core::ledger::{Ledger, Record};
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::permissions::{Approver, DenyAll};
use fontaine_core::project_env::{self, ProjectEnvironment, RemoteFacts, VERSION};
use fontaine_guardian::prompt_builder::user_message;
use fontaine_guardian::{Protection, Visibility};
use fontaine_tools::gate::ROOT_AGENT;
use rig_core::test_utils::MockCompletionModel;

const URL: &str = "https://github.com/acme/proj.git";
const WARNING: &str = "auto mode doesn't know this project's environment";

#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Approver for Warnings {
    fn approve<'a>(
        &'a self,
        request: fontaine_core::permissions::ApprovalRequest,
    ) -> futures::future::BoxFuture<'a, fontaine_core::permissions::Answer> {
        DenyAll.approve(request)
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

impl Warnings {
    /// The warnings about the setup command; the runtime raises others (web search) too.
    fn about_setup(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.contains("auto-mode setup"))
            .cloned()
            .collect()
    }
}

fn models() -> Models {
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        MockCompletionModel::from_stream_turns(
            Vec::<Vec<rig_core::test_utils::MockStreamEvent>>::new(),
        )
        .into(),
    );
    Models {
        main: handle.clone(),
        main_listing: Vec::new(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    }
}

fn record(discovered_at: &str, visibility: Visibility, urls: &[&str]) -> ProjectEnvironment {
    ProjectEnvironment {
        version: VERSION,
        discovered_at: discovered_at.into(),
        remotes: urls
            .iter()
            .map(|url| RemoteFacts {
                url: (*url).into(),
                slug: None,
                visibility: visibility.clone(),
                default_branch: Some("main".into()),
                default_branch_protection: Protection::Protected,
                branch_rules: Vec::new(),
                deploy_environments: Vec::new(),
            })
            .collect(),
        tag_release_workflows: Vec::new(),
        workflow_environments: Vec::new(),
        package_registries: Vec::new(),
        env_files: Vec::new(),
        stack: Vec::new(),
        scan: None,
    }
}

fn now_minus_days(days: i64) -> String {
    let at = chrono::Utc::now() - chrono::TimeDelta::days(days);
    at.format("%Y-%m-%dT%H:%M:%S.000Z").to_string()
}

fn world_with_remote() -> World {
    let w = World::new();
    common::git_init(&w.work);
    common::git(&w.work, &["remote", "add", "origin", URL]);
    w
}

fn start(w: &World, mode: PermissionMode) -> (fontaine_core::agent::AgentRuntime, Arc<Warnings>) {
    let warnings = Arc::new(Warnings::default());
    let runtime = build_with_approver(
        w,
        mode,
        models(),
        None,
        false,
        Default::default(),
        None,
        warnings.clone(),
    );
    (runtime, warnings)
}

fn request(
    runtime: &fontaine_core::agent::AgentRuntime,
    caller: &str,
) -> fontaine_guardian::AdjudicationRequest {
    adjudication::build(
        &runtime.state,
        caller,
        "shell",
        &serde_json::json!({"command": "ls"}),
        Vec::new(),
        None,
    )
}

#[tokio::test]
async fn session_start_loads_filters_to_the_current_remotes_and_downgrades_old_public() {
    let w = world_with_remote();
    let (runtime, _) = start(&w, PermissionMode::Manual);
    let mut stored = record(&now_minus_days(40), Visibility::Public, &[URL]);
    stored.remotes.push(RemoteFacts {
        url: "https://github.com/acme/gone.git".into(),
        ..stored.remotes[0].clone()
    });
    project_env::store(&w.data, &runtime.primary_root(), &stored).unwrap();
    runtime.record_session_start().await;

    let facts = request(&runtime, ROOT_AGENT)
        .session_facts
        .project_environment
        .expect("the stored record reaches the guardian's facts");
    assert_eq!(facts.remotes.len(), 1);
    assert_eq!(facts.remotes[0].url, URL);
    assert!(matches!(
        facts.remotes[0].visibility,
        Visibility::Expired(_)
    ));
    let rendered = user_message(&request(&runtime, ROOT_AGENT));
    assert!(
        rendered.contains("visibility unknown (last seen public on "),
        "{rendered}"
    );
}

#[tokio::test]
async fn a_fresh_public_record_renders_public_and_no_record_renders_nothing() {
    let w = world_with_remote();
    let (runtime, _) = start(&w, PermissionMode::Manual);
    assert!(
        request(&runtime, ROOT_AGENT)
            .session_facts
            .project_environment
            .is_none()
    );
    project_env::store(
        &w.data,
        &runtime.primary_root(),
        &record(&now_minus_days(29), Visibility::Public, &[URL]),
    )
    .unwrap();
    runtime.record_session_start().await;
    let rendered = user_message(&request(&runtime, ROOT_AGENT));
    assert!(
        rendered.contains("visibility public (GitHub API)"),
        "{rendered}"
    );
}

#[tokio::test]
async fn an_auto_session_without_facts_warns_once_and_a_manual_one_warns_on_its_first_switch() {
    let w = world_with_remote();
    let (runtime, warnings) = start(&w, PermissionMode::Auto);
    runtime.record_session_start().await;
    {
        let told = warnings.about_setup();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains(WARNING) && told[0].contains("fontaine auto-mode setup"));
    }
    assert!(runtime.state.setup_warning_shown());
    runtime.state.set_mode(PermissionMode::Manual);
    runtime.state.set_mode(PermissionMode::Auto);
    assert!(runtime.setup_notice_on_auto().is_none(), "not twice");

    let w = world_with_remote();
    let (runtime, warnings) = start(&w, PermissionMode::Manual);
    runtime.record_session_start().await;
    assert!(warnings.about_setup().is_empty());
    assert!(!runtime.state.setup_warning_shown());
    assert!(runtime.setup_notice_on_auto().is_none(), "manual mode");
    runtime.state.set_mode(PermissionMode::Auto);
    let notice = runtime
        .setup_notice_on_auto()
        .expect("the first switch into auto warns");
    assert!(notice.headline.contains(WARNING));
    assert!(runtime.setup_notice_on_auto().is_none());
}

#[tokio::test]
async fn the_switch_path_is_silent_while_the_guardian_is_off_and_when_the_reminder_is_off() {
    let w = world_with_remote();
    let (runtime, _) = start(&w, PermissionMode::Manual);
    runtime.record_session_start().await;
    runtime.state.set_mode(PermissionMode::Auto);
    runtime.state.set_guardian_off(true);
    assert!(runtime.setup_notice_on_auto().is_none());
    runtime.state.set_guardian_off(false);
    assert!(runtime.setup_notice_on_auto().is_some());

    let w = world_with_remote();
    w.user_file("[auto_mode]\nsetup_reminder = false\n");
    let (runtime, warnings) = start(&w, PermissionMode::Auto);
    runtime.record_session_start().await;
    assert!(warnings.about_setup().is_empty());
    assert!(runtime.setup_notice_on_auto().is_none());
}

#[tokio::test]
async fn a_stale_or_unreadable_record_warns_to_rerun_or_about_the_file() {
    let w = world_with_remote();
    let (runtime, warnings) = start(&w, PermissionMode::Auto);
    project_env::store(
        &w.data,
        &runtime.primary_root(),
        &record(&now_minus_days(40), Visibility::Public, &[URL]),
    )
    .unwrap();
    runtime.record_session_start().await;
    let told = warnings.0.lock().unwrap().clone();
    assert!(
        told.iter()
            .any(|m| m.contains("rerun `fontaine auto-mode setup`")),
        "{told:?}"
    );

    let w = world_with_remote();
    let (runtime, warnings) = start(&w, PermissionMode::Manual);
    let file = project_env::path(&w.data, &runtime.primary_root());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{ broken").unwrap();
    runtime.record_session_start().await;
    let told = warnings.0.lock().unwrap().clone();
    assert!(
        told.iter()
            .any(|m| m.contains("ignoring the project environment")),
        "{told:?}"
    );
    assert!(
        request(&runtime, ROOT_AGENT)
            .session_facts
            .project_environment
            .is_none()
    );
}

#[tokio::test]
async fn applying_mid_session_reaches_the_next_request_of_any_agent_and_changes_what_is_cached() {
    let w = world_with_remote();
    let (runtime, warnings) = start(&w, PermissionMode::Manual);
    runtime.record_session_start().await;
    let before = request(&runtime, ROOT_AGENT);
    assert!(before.session_facts.project_environment.is_none());

    // One remote the session knows and one it does not: the start-time remotes filter.
    let mut applied = record(&now_minus_days(0), Visibility::Public, &[URL]);
    applied.remotes.push(RemoteFacts {
        url: "https://github.com/acme/other.git".into(),
        ..applied.remotes[0].clone()
    });
    runtime.apply_project_environment(applied).unwrap();

    for caller in [ROOT_AGENT, "child-1"] {
        let after = request(&runtime, caller);
        let facts = after.session_facts.project_environment.as_ref().unwrap();
        assert_eq!(facts.remotes.len(), 1, "filtered to the start-time remotes");
        assert_ne!(
            user_message(&before),
            user_message(&after),
            "the rendered request is the cache key's request, so the old verdict is not reused"
        );
    }
    // Written to the data dir exactly as the command line writes it.
    let loaded = project_env::load(&w.data, &runtime.primary_root());
    assert_eq!(loaded.record.unwrap().remotes.len(), 2);
    // And the warning is not raised again.
    runtime.state.set_mode(PermissionMode::Auto);
    assert!(runtime.setup_notice_on_auto().is_none());
    assert!(warnings.about_setup().is_empty());
}

#[test]
fn the_ledger_view_keeps_the_latest_value_and_a_new_start_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let start = |environment: Option<ProjectEnvironment>| Record::SessionStart {
        session_id: "s".into(),
        permission_mode: "auto".into(),
        workspace_roots: vec!["/w".into()],
        workspace_trust: "trusted".into(),
        git_remotes: vec![URL.into()],
        env_keys: Vec::new(),
        project_environment: environment,
    };
    let first = record("2026-10-01T00:00:00.000Z", Visibility::Public, &[URL]);
    let second = record("2026-10-02T00:00:00.000Z", Visibility::Private, &[URL]);
    let ledger = Ledger::create(dir.path()).unwrap();
    ledger.append(start(Some(first.clone()))).unwrap();
    assert_eq!(ledger.view().project_environment, Some(first));
    ledger
        .append(Record::ProjectEnvironment {
            project_environment: second.clone(),
        })
        .unwrap();
    assert_eq!(ledger.view().project_environment, Some(second.clone()));
    drop(ledger);

    // A resume replays the records, then records a new start from the current file.
    let reopened = Ledger::open(dir.path()).unwrap();
    assert_eq!(reopened.view().project_environment, Some(second));
    reopened.append(start(None)).unwrap();
    assert_eq!(reopened.view().project_environment, None);
}
