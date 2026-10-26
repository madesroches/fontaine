//! The post-call step every tool result passes through before the agent sees it. Stages run in
//! a fixed order whatever order they were attached in: the notifications queued for the agent,
//! the instruction files a read brings into scope, the tool-result scanner, then the
//! `post_tool_use` hooks. Notifications and instructions come first so that what another agent
//! said, and instruction text that arrives in a result, is scanned like any other result. With
//! no stage attached it changes nothing.

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
    Instructions,
    Scanner,
    PostToolUse,
}

/// What the scanner stage concluded about one result, for the `tool_result` event. It carries
/// no content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanRecord {
    pub verdict: ScanVerdict,
    pub latency_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Flagged,
    /// The scanner could not judge, so the result went on unscanned.
    Error,
}

impl ScanVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanVerdict::Clean => "clean",
            ScanVerdict::Flagged => "flagged",
            ScanVerdict::Error => "error",
        }
    }
}

pub trait ResultStage: Send + Sync {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome>;

    /// `process`, along with what the stage learned about the scan. Only the scanner stage
    /// reports one.
    fn process_reported<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, (ToolOutcome, Option<ScanRecord>)> {
        Box::pin(async move { (self.process(call, outcome).await, None) })
    }
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

    pub async fn run(
        &self,
        call: CallView<'_>,
        mut outcome: ToolOutcome,
    ) -> (ToolOutcome, Option<ScanRecord>) {
        let mut record = None;
        for (_, stage) in &self.stages {
            let (next, reported) = stage.process_reported(call, outcome).await;
            outcome = next;
            record = reported.or(record);
        }
        (outcome, record)
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
