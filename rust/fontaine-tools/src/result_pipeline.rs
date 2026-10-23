//! The post-call step every tool result passes through before the agent sees it. Stages run in
//! a fixed order whatever order they were attached in: the notifications queued for the agent,
//! the tool-result scanner, then the `post_tool_use` hooks. Notifications come first so that
//! what another agent said is scanned like any other result. With no stage attached it changes
//! nothing.

use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::message::ToolResultContent;
use rig_core::tool::{ToolExecutionError, ToolOutput};

use crate::gate::CallView;

pub type ToolOutcome = Result<ToolOutput, ToolExecutionError>;

/// Where a stage sits in the fixed order; the derived order is the run order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StageKind {
    Notifications,
    Scanner,
    PostToolUse,
}

pub trait ResultStage: Send + Sync {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome>;
}

#[derive(Clone, Default)]
pub struct ResultPipeline {
    stages: Vec<(StageKind, Arc<dyn ResultStage>)>,
}

impl ResultPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_stage(mut self, kind: StageKind, stage: Arc<dyn ResultStage>) -> Self {
        // Stable, so two stages of one kind keep their attachment order.
        let at = self
            .stages
            .partition_point(|(existing, _)| *existing <= kind);
        self.stages.insert(at, (kind, stage));
        self
    }

    pub async fn run(&self, call: CallView<'_>, mut outcome: ToolOutcome) -> ToolOutcome {
        for (_, stage) in &self.stages {
            outcome = stage.process(call, outcome).await;
        }
        outcome
    }
}

/// `outcome` with `note` added after what the tool returned. A failure stays a failure with the
/// note in what the model reads.
pub fn append_note(outcome: ToolOutcome, note: &str) -> ToolOutcome {
    if note.trim().is_empty() {
        return outcome;
    }
    match outcome {
        Ok(output) => {
            if let Some(text) = output.as_text() {
                return Ok(ToolOutput::text(format!("{text}\n\n{note}")));
            }
            let mut content = output.into_content();
            content.push(ToolResultContent::text(note));
            ToolOutput::content(content)
        }
        Err(error) => {
            let shown = error.model_output().render();
            let combined = ToolOutput::text(format!("{shown}\n\n{note}"));
            Err(error.with_model_output(combined))
        }
    }
}
