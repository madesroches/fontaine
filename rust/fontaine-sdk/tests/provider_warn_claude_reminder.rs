//! The claude-subscription reminder probe's failure warning reaches the session's approver. It
//! installs the process-wide warning sink, so it has a binary of its own.

#[path = "core_common/mod.rs"]
mod common;

use std::sync::Arc;

use common::claude_cli::{Claude, tagged_conversation};
use common::pipeline::Scripted;
use common::runtime::build_with_approver;
use fontaine_sdk::core::config::PermissionMode;
use serde_json::json;

#[tokio::test]
async fn a_reminder_that_cannot_be_learned_warns_once_and_a_learned_one_not_at_all() {
    for (reminder, warns) in [(json!(null), true), (json!("learned"), false)] {
        let claude = Claude::new();
        let learned = "<system-reminder>\n# userEmail\nThe user's email address is user@example.com.\n</system-reminder>";
        let reminder = if reminder == json!("learned") {
            json!(learned)
        } else {
            reminder
        };
        claude.scenario(json!({"reminder": reminder, "responses": [{"text": "ok"}]}));
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
                .call(tagged_conversation("reminder-warning", users))
                .await
                .unwrap();
        }
        let warnings = approver.warnings.lock().unwrap();
        let ours = warnings
            .iter()
            .filter(|w| w.contains("context reminder"))
            .count();
        assert_eq!(ours, usize::from(warns), "{warnings:?}");
    }
}
