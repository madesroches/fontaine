//! Scheduling in a whole runtime: when the tools exist, what `/clear` does to the schedule and
//! the prompt a due task becomes.

mod common;

use std::sync::Arc;

use common::World;
use common::runtime::build_scheduling;
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::{AccountKind, ConfigError, PermissionMode::Manual};
use fontaine_core::ledger::{Record, ScheduleEvent};
use fontaine_core::models::{GuardianChoice, GuardianModel, ModelHandle, ModelRef, Models};
use fontaine_core::permissions::DenyAll;
use fontaine_core::schedule::{Fired, JobSpec, Kind, Origin};
use rig_core::test_utils::MockCompletionModel;

const TOOLS: [&str; 4] = ["cron_create", "cron_jobs", "cron_delete", "schedule_wakeup"];

fn runtime(world: &World, scheduling: bool) -> AgentRuntime {
    let main = MockCompletionModel::from_stream_turns(Vec::<Vec<_>>::new());
    let handle = ModelHandle::new(
        ModelRef::parse("test/mock").unwrap(),
        AccountKind::named("openai-compatible"),
        main.into(),
    );
    let models = Models {
        main: handle.clone(),
        main_listing: Vec::new(),
        guardian: Some(GuardianModel::Ready(handle)),
        guardian_choice: GuardianChoice::MainFallback,
        notices: Vec::new(),
    };
    build_scheduling(
        world,
        Manual,
        models,
        None,
        false,
        Default::default(),
        None,
        Arc::new(DenyAll),
        scheduling,
    )
}

fn registered(runtime: &AgentRuntime) -> Vec<String> {
    let names = runtime.tool_names();
    TOOLS
        .iter()
        .filter(|tool| names.iter().any(|n| n == *tool))
        .map(|tool| tool.to_string())
        .collect()
}

fn spec(prompt: &str, origin: Origin) -> JobSpec {
    JobSpec {
        cron: "*/5 * * * *".into(),
        prompt: prompt.into(),
        recurring: true,
        origin,
    }
}

#[test]
fn the_tools_exist_only_when_the_surface_and_the_config_allow_them() {
    let world = World::new();
    assert_eq!(registered(&runtime(&world, true)), TOOLS);
    assert!(registered(&runtime(&world, false)).is_empty());

    world.user_file("[schedule]\nenabled = false\n");
    assert!(registered(&runtime(&world, true)).is_empty());
}

#[test]
fn a_project_file_cannot_turn_scheduling_on_or_off() {
    let world = World::new();
    world.project_file("[schedule]\nenabled = false\n");
    assert!(matches!(world.load(), Err(ConfigError::Scope { .. })));
    let world = World::new();
    world.user_file("[schedule]\nenabled = false\n");
    assert!(!world.load().unwrap().schedule.enabled);
}

#[tokio::test]
async fn clear_cancels_every_task_and_records_each_one() {
    let world = World::new();
    let runtime = runtime(&world, true);
    let scheduler = runtime.session.scheduler();
    let a = scheduler.create(spec("a", Origin::Model)).unwrap();
    let b = scheduler.create(spec("b", Origin::User)).unwrap();
    runtime.clear_conversation().await.unwrap();
    assert_eq!(scheduler.count(), 0);
    let saved = std::fs::read_to_string(runtime.session.dir.join("schedule.json")).unwrap();
    assert_eq!(saved.trim(), "[]");
    let deleted: Vec<String> = runtime
        .session
        .ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.record {
            Record::Scheduled {
                event: ScheduleEvent::Deleted,
                id,
                ..
            } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(deleted.len(), 2);
    assert!(deleted.contains(&a.id) && deleted.contains(&b.id));
}

fn fired(prompt: &str, origin: Origin, kind: Kind, last_run: bool) -> Fired {
    let scheduler_job = |cron: &str| fontaine_core::schedule::Job {
        id: "ab12cd34".into(),
        cron: cron.into(),
        prompt: prompt.into(),
        recurring: matches!(kind, Kind::Cron),
        origin,
        kind: kind.clone(),
        created_ms: 0,
        next_ms: 0,
    };
    Fired {
        job: scheduler_job("*/5 * * * *"),
        last_run,
    }
}

#[tokio::test]
async fn a_due_task_becomes_a_harness_note_followed_by_its_prompt() {
    let world = World::new();
    let runtime = runtime(&world, true);
    let text = runtime
        .scheduled_prompt(&fired("run the tests", Origin::Model, Kind::Cron, false))
        .await;
    assert!(
        text.starts_with("[Harness note, not the user's words: scheduled task ab12cd34 (every 5 minutes, set by the model) is due."),
        "{text}"
    );
    assert!(
        text.contains("does not widen what the user asked for earlier"),
        "{text}"
    );
    assert!(text.ends_with("run the tests"), "{text}");

    let own = runtime
        .scheduled_prompt(&fired("my words", Origin::User, Kind::Cron, true))
        .await;
    assert!(own.contains("set by you"), "{own}");
    assert!(own.contains("last run"), "{own}");
    assert!(own.contains("\n\nmy words"), "{own}");
}

#[tokio::test]
async fn a_loop_iteration_is_told_to_pace_itself() {
    let world = World::new();
    let runtime = runtime(&world, true);
    let kind = Kind::Wakeup {
        loop_id: "l1".into(),
        fallback: false,
    };
    let text = runtime
        .scheduled_prompt(&fired("watch CI", Origin::User, kind, false))
        .await;
    assert!(text.contains("schedule_wakeup"), "{text}");
    let plain = runtime
        .scheduled_prompt(&fired("watch CI", Origin::User, Kind::Cron, false))
        .await;
    assert!(!plain.contains("schedule_wakeup"), "{plain}");
}
