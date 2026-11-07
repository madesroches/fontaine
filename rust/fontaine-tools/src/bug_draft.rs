//! The `draft_bug_report` tool: queues a bug report for the user to review with `/bug`. It does
//! no network I/O and never submits; the draft lands in the data directory behind the
//! [`DraftSink`] seam, redacted by the implementer.

use std::sync::Arc;

use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use crate::descriptor::DRAFT_BUG_REPORT;
use crate::gate::{AgentId, ROOT_AGENT};

/// Drafts one session may queue.
pub const MAX_DRAFTS: usize = 3;
const MAX_TITLE_CHARS: usize = 120;
const MAX_AREA_CHARS: usize = 60;
const MAX_DETAILS_CHARS: usize = 4_000;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftArgs {
    pub title: String,
    pub area: String,
    pub details: String,
}

/// Where a draft goes. `fontaine-core` implements it over the data directory.
pub trait DraftSink: Send + Sync {
    /// Stores the draft and returns its id, or says why it cannot.
    fn save(&self, draft: &DraftArgs) -> Result<String, String>;
}

#[derive(Clone)]
pub struct DraftBugReportTool {
    pub sink: Arc<dyn DraftSink>,
}

fn check(field: &str, value: &str, max: usize) -> Result<(), ToolExecutionError> {
    if value.trim().is_empty() {
        return Err(ToolExecutionError::invalid_args(format!(
            "`{field}` is empty"
        )));
    }
    if value.chars().count() > max {
        return Err(ToolExecutionError::invalid_args(format!(
            "`{field}` is longer than {max} characters"
        )));
    }
    Ok(())
}

impl Tool for DraftBugReportTool {
    const NAME: &'static str = DRAFT_BUG_REPORT;
    type Args = DraftArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Queue a bug report about Fontaine itself (not about the user's project) for the user \
             to review. Use it only when Fontaine misbehaves in a way the user would want \
             reported. It writes a redacted draft locally and sends nothing; the user decides \
             whether to file it with `/bug`. At most {MAX_DRAFTS} drafts per session."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "One line saying what went wrong.",
                },
                "area": {
                    "type": "string",
                    "description": "The part of Fontaine involved, such as `permissions` or `tui`.",
                },
                "details": {
                    "type": "string",
                    "description": "What happened and what was expected. Leave out secrets and file contents.",
                },
            },
            "required": ["title", "area", "details"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: DraftArgs,
    ) -> Result<String, ToolExecutionError> {
        let root = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .is_some_and(|id| id.0 == ROOT_AGENT);
        if !root {
            return Err(ToolExecutionError::other(
                "bug-report drafts are only available to the main agent",
            ));
        }
        check("title", &args.title, MAX_TITLE_CHARS)?;
        check("area", &args.area, MAX_AREA_CHARS)?;
        check("details", &args.details, MAX_DETAILS_CHARS)?;
        let id = self.sink.save(&args).map_err(ToolExecutionError::other)?;
        Ok(format!(
            "Draft {id} saved. The user can review and file it with /bug; nothing was sent."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory(Mutex<Vec<DraftArgs>>);

    impl DraftSink for Memory {
        fn save(&self, draft: &DraftArgs) -> Result<String, String> {
            let mut drafts = self.0.lock().unwrap();
            drafts.push(draft.clone());
            Ok(format!("d{}", drafts.len()))
        }
    }

    fn args(title: &str) -> DraftArgs {
        DraftArgs {
            title: title.into(),
            area: "tui".into(),
            details: "it broke".into(),
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let bad = serde_json::from_value::<DraftArgs>(
            json!({"title": "t", "area": "a", "details": "d", "extra": 1}),
        );
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn only_the_main_agent_saves_a_valid_draft() {
        let sink = Arc::new(Memory::default());
        let tool = DraftBugReportTool { sink: sink.clone() };

        let mut bare = ToolContext::new();
        assert!(tool.call(&mut bare, args("t")).await.is_err());

        let mut root = ToolContext::new();
        root.insert(AgentId(ROOT_AGENT.to_string())).unwrap();
        assert!(tool.call(&mut root, args("  ")).await.is_err());
        assert!(
            tool.call(&mut root, args(&"x".repeat(MAX_TITLE_CHARS + 1)))
                .await
                .is_err()
        );
        let said = tool.call(&mut root, args("crash")).await.unwrap();
        assert!(said.contains("d1") && said.contains("nothing was sent"));
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }
}
