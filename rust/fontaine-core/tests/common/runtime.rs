#![allow(dead_code)]

//! A whole `AgentRuntime` over scripted models, for tests that need the agent itself.

use std::sync::Arc;

use fontaine_core::agent::{AgentRuntime, AgentSetup, SessionOrigin};
use fontaine_core::config::{AccountKind, PermissionMode};
use fontaine_core::extensions::{self, LoadParams};
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::permissions::DenyAll;
use fontaine_core::session::Session;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

use super::World;

static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// A session id no other runtime in this process has used.
fn session_id() -> String {
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("00000000-0000-4000-8000-{n:012}")
}

pub struct Built {
    pub runtime: AgentRuntime,
    pub main: MockCompletionModel,
}

/// A runtime in `world` whose main model answers `turns` and whose guardian is the main model.
pub fn build(
    world: &World,
    mode: PermissionMode,
    turns: Vec<Vec<MockStreamEvent>>,
    project_executables: bool,
) -> Built {
    build_with_scanner(world, mode, turns, project_executables, Default::default())
}

/// `build`, with the tool-result scanner as started.
pub fn build_with_scanner(
    world: &World,
    mode: PermissionMode,
    turns: Vec<Vec<MockStreamEvent>>,
    project_executables: bool,
    scanner: fontaine_core::scanner_stage::ScannerStart,
) -> Built {
    let config = world.load().unwrap();
    let main = MockCompletionModel::from_stream_turns(turns);
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        main.clone().into(),
    );
    let servers: Vec<String> = config.mcp_servers.keys().cloned().collect();
    let loaded = extensions::load(&LoadParams {
        config_dir: &world.config,
        project_root: &world.work,
        servers: &servers,
    })
    .unwrap();
    let env: fontaine_core::agent::EnvLookup = Arc::new(|_| None);
    let setup = AgentSetup {
        config,
        config_dir: world.config.clone(),
        data_dir: world.data.clone(),
        models: Models {
            main: handle.clone(),
            main_listing: Vec::new(),
            guardian: Some(GuardianModel::Ready(handle)),
            guardian_choice: GuardianChoice::MainFallback,
            notices: Vec::new(),
        },
        session: Session::create(&world.data, &session_id()).unwrap(),
        roots: vec![world.work.clone()],
        mode,
        scrub_env: Vec::new(),
        git_bash: None,
        system_prompt: None,
        append_system_prompt: None,
        json_schema: None,
        headless: true,
        approver: Arc::new(DenyAll),
        home: None,
        user_config: None,
        guardian_profile: fontaine_guardian::Profile::Full,
        notices: Vec::new(),
        scanner,
        switcher: None,
        extensions: loaded,
        project_executables,
        confirm_project_servers: false,
        origin: SessionOrigin::Fresh,
        env,
    };
    Built {
        runtime: AgentRuntime::new(setup).unwrap(),
        main,
    }
}
