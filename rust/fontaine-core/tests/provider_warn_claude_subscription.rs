//! The claude-subscription cache self-check warning reaches the session's approver. It installs
//! the process-wide warning sink, so it has a binary of its own.

mod common;

use std::sync::Arc;

use common::claude_cli::{Claude, tagged_conversation};
use common::pipeline::Scripted;
use common::runtime::build_with_approver;
use fontaine_core::config::PermissionMode;
use serde_json::json;

fn cached(read: u64, write_1h: u64) -> serde_json::Value {
    json!({"input_tokens": 1, "output_tokens": 7, "cache_read_input_tokens": read,
           "cache_creation": {"ephemeral_1h_input_tokens": write_1h}})
}

#[tokio::test]
async fn the_cache_self_check_warning_reaches_the_approver_with_the_cli_version() {
    let claude = Claude::new();
    claude.scenario(json!({
        "claude_code_version": "2.1.402",
        "responses": [
            {"text": "a", "usage": cached(0, 500)},
            {"text": "b", "usage": cached(100, 100)},
            {"text": "c", "usage": cached(1000, 0)},
        ],
    }));
    let models = claude.resolve(None).await.unwrap();
    let handle = models.main.clone();
    let approver = Arc::new(Scripted::default());
    let _runtime = build_with_approver(
        &claude.w,
        PermissionMode::Manual,
        models,
        None,
        false,
        Default::default(),
        None,
        approver.clone(),
    );
    for users in [2, 3, 4] {
        handle
            .completion
            .call(tagged_conversation("2.1.402", users))
            .await
            .unwrap();
    }
    let warnings = approver.warnings.lock().unwrap();
    let ours: Vec<_> = warnings
        .iter()
        .filter(|w| w.contains("not read back"))
        .collect();
    assert_eq!(ours.len(), 1, "{warnings:?}");
    assert!(ours[0].contains("2.1.402"), "{warnings:?}");
}
