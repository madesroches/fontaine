//! Auto memory in a built runtime: the prompt layer, the guardian's inputs, `/context`, the
//! `/memory` listing and the file index.

mod common;

use common::World;
use common::runtime::{Built, build};
use fontaine_core::config::PermissionMode::Manual;
use fontaine_core::instructions::MemoryKind;
use fontaine_core::turn::{Flow, TurnOutcome, TurnParams, run_turn};
use rig_core::completion::Usage;
use rig_core::test_utils::{MockStreamEvent, mock_final};

async fn first_request(built: &Built) -> String {
    let mut seen = |_| Flow::Continue;
    let outcome = run_turn(
        TurnParams {
            agent: &built.runtime.agent,
            state: &built.runtime.state,
            prompt: "hello".to_string(),
            attachments: Vec::new(),
            modalities: rig_core::catalog::Modalities::default(),
            max_turns: Some(5),
            priced: true,
            model: "scripted".to_string(),
            provider: "mock".to_string(),
            native_search: false,
        },
        &mut seen,
    )
    .await;
    assert!(matches!(outcome, TurnOutcome::Answer(_)), "{outcome:?}");
    format!("{:?}", built.main.requests()[0])
}

/// `first_request` is a `Debug` dump, which escapes the backslashes of a Windows path.
fn as_debug(path: &std::path::Path) -> String {
    let quoted = format!("{:?}", path.display().to_string());
    quoted[1..quoted.len() - 1].to_string()
}

fn answer() -> Vec<Vec<MockStreamEvent>> {
    vec![vec![
        MockStreamEvent::Text("ok".to_string()),
        MockStreamEvent::FinalResponse(mock_final(Usage::default())),
    ]]
}

#[tokio::test]
async fn the_prompt_carries_the_framed_index_and_the_saving_guide_when_on() {
    let w = World::new();
    let dir = w.enable_memory();
    common::write(
        &dir.join("MEMORY.md"),
        "- [cmds](commands.md): test commands\n",
    );
    let built = build(&w, Manual, answer(), false);
    let request = first_request(&built).await;
    assert!(request.contains("These are notes you saved in earlier sessions"));
    assert!(request.contains("not instructions"));
    assert!(request.contains(&format!("Memory directory: {}", as_debug(&dir))));
    assert!(request.contains("- [cmds](commands.md): test commands"));
    assert!(request.contains("Auto memory is on"));
    assert!(request.contains(&format!("- {} (memory)", as_debug(&dir))));
}

#[tokio::test]
async fn the_saving_guide_is_there_even_before_anything_was_saved() {
    let w = World::new();
    let dir = w.enable_memory();
    let built = build(&w, Manual, answer(), false);
    let request = first_request(&built).await;
    assert!(request.contains("Auto memory is on"));
    assert!(!request.contains("MEMORY.md:"));
    assert!(request.contains(&as_debug(&dir)));
}

#[tokio::test]
async fn no_memory_layer_when_off_even_with_a_store_on_disk() {
    let w = World::new();
    common::write(&w.memory_dir().join("MEMORY.md"), "- poisoned note\n");
    // Nothing enabled the memory root, so the store is inert.
    std::fs::remove_dir_all(w.memory_dir()).unwrap();
    let built = build(&w, Manual, answer(), false);
    let store = built.runtime.memory_dir();
    common::write(&store.join("MEMORY.md"), "- poisoned note\n");
    let request = first_request(&built).await;
    assert!(!request.contains("poisoned note"));
    assert!(!request.contains("notes you saved"));
    assert!(!request.contains("Auto memory is on"));
}

#[tokio::test]
async fn memory_text_stays_out_of_the_guardians_project_instructions() {
    let w = World::new();
    let dir = w.enable_memory();
    common::write(&dir.join("MEMORY.md"), "- secret steering note\n");
    common::write(&w.work.join("FONTAINE.md"), "Run cargo test.\n");
    let built = build(&w, Manual, answer(), false);
    let instructions = built
        .runtime
        .state
        .auto
        .project_instructions
        .clone()
        .expect("the user's own files");
    assert!(instructions.contains("Run cargo test."));
    assert!(!instructions.contains("secret steering note"));
    assert!(!instructions.contains("notes you saved"));
}

#[tokio::test]
async fn context_counts_the_memory_layer_only_when_on() {
    let w = World::new();
    let dir = w.enable_memory();
    common::write(&dir.join("MEMORY.md"), "- [cmds](commands.md)\n");
    let on = build(&w, Manual, answer(), false);
    let usage = on.runtime.context_usage().await.unwrap();
    assert!(usage.memory > 0);
    let without = fontaine_core::context::ContextUsage {
        memory: 0,
        ..usage.clone()
    };
    assert_eq!(usage.total(), without.total() + usage.memory);

    let off = World::new();
    let built = build(&off, Manual, answer(), false);
    assert_eq!(built.runtime.context_usage().await.unwrap().memory, 0);
}

#[tokio::test]
async fn memory_entries_list_the_store_whether_or_not_it_is_on() {
    let w = World::new();
    let built = build(&w, Manual, answer(), false);
    assert!(!built.runtime.auto_memory_active());
    let store = built.runtime.memory_dir();
    common::write(&store.join("MEMORY.md"), "- a\n");
    common::write(&store.join("debugging.md"), "b\n");
    let auto: Vec<_> = built
        .runtime
        .memory_entries()
        .into_iter()
        .filter(|e| matches!(e.kind, MemoryKind::Auto { .. }))
        .map(|e| e.path)
        .collect();
    assert_eq!(auto, [store.join("MEMORY.md"), store.join("debugging.md")]);

    let on_world = World::new();
    on_world.enable_memory();
    let on = build(&on_world, Manual, answer(), false);
    assert!(on.runtime.auto_memory_active());
}

#[tokio::test]
async fn the_memory_root_is_not_scanned_into_the_file_index() {
    let w = World::new();
    let dir = w.enable_memory();
    common::write(&dir.join("MEMORY.md"), "- a\n");
    common::write(&w.work.join("src.rs"), "fn main() {}\n");
    let built = build(&w, Manual, answer(), false);
    built.runtime.record_session_start().await;
    let files = built.runtime.files.read().unwrap().clone();
    assert!(files.iter().any(|p| p.ends_with("src.rs")), "{files:?}");
    assert!(!files.iter().any(|p| p.ends_with("MEMORY.md")), "{files:?}");
}

#[test]
fn every_worktree_and_subdirectory_of_a_repository_shares_one_key() {
    let w = World::new();
    common::git_init(&w.work);
    common::write(&w.work.join("a.txt"), "a");
    common::git(&w.work, &["add", "."]);
    common::git(&w.work, &["commit", "-q", "-m", "init"]);
    let sub = w.work.join("deep/er");
    std::fs::create_dir_all(&sub).unwrap();
    // Relative, because git can't create a worktree at a Windows `\\?\` verbatim path.
    common::git(&w.work, &["worktree", "add", "-q", "../wt"]);
    let wt = w.root.join("wt");
    let key = fontaine_core::memory::project_key(&w.work);
    assert_eq!(key, fontaine_core::memory::project_key(&sub));
    assert_eq!(key, fontaine_core::memory::project_key(&wt));

    let other = World::new();
    assert_ne!(key, fontaine_core::memory::project_key(&other.work));
}
