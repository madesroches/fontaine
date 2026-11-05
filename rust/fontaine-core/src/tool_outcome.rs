//! Records how each allowed call ended. The pipeline's action record says a call was allowed;
//! only this says it ran to the end, failed, or was cut short, which is what an AFK report needs
//! to tell "allowed" from "succeeded".

use std::sync::Arc;

use fontaine_tools::gate::CallView;
use fontaine_tools::result_pipeline::{ResultStage, ToolOutcome};
use futures::future::BoxFuture;

use crate::afk_report::OutcomeStatus;
use crate::gate::SessionState;
use crate::ledger::Record;

pub struct OutcomeStage {
    pub state: Arc<SessionState>,
}

impl OutcomeStage {
    fn record(&self, call: CallView<'_>, status: OutcomeStatus) {
        if call.synthetic {
            return;
        }
        let Some(tool_use_id) = self.state.current_call(call.agent) else {
            return;
        };
        self.state.append(Record::ToolOutcome {
            tool_use_id,
            agent: call.agent.to_string(),
            status,
        });
    }
}

impl ResultStage for OutcomeStage {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        let status = if outcome.is_ok() {
            OutcomeStatus::Ok
        } else {
            OutcomeStatus::Error
        };
        self.record(call, status);
        Box::pin(async move { outcome })
    }

    fn cancelled(&self, call: CallView<'_>) {
        self.record(call, OutcomeStatus::Cancelled);
    }
}
