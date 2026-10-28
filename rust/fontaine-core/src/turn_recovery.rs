//! Saves a failed run's committed messages into its conversation store. rig-agent writes the
//! conversation only when a run finishes, so without this the next run starts without the
//! interrupted turn: a `retry` would read as a new task and an auto-retry would start over
//! instead of continuing.

use std::sync::Arc;

use fontaine_tools::error_chain::chain;
use rig_agent::agent::{AgentHook, HookContext, RunSettled, SettledOutcome};
use rig_core::message::Message;
use rig_memory::{AsyncStore, SessionError, load_async, save_async};

use crate::permissions::Approver;

/// Appends a failed run's committed messages to `log` in `store`, the same save the engine
/// makes when a run finishes.
pub(crate) async fn append_messages<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    messages: &[Arc<Message>],
) -> Result<(), SessionError> {
    let mut conversation = load_async(store, log).await?;
    conversation.extend(messages.iter().cloned());
    save_async(store, log, &mut conversation).await
}

/// Persists what a failed run committed, so a retried or next run continues the same task.
/// Registered on the root agent and on subagents: every agent's own store holds its
/// conversation, and the retry primitive relies on the failed attempt being there.
pub struct RecoveryHook<S> {
    store: S,
    log: &'static str,
    approver: Arc<dyn Approver>,
}

impl<S> RecoveryHook<S> {
    pub fn new(store: S, log: &'static str, approver: Arc<dyn Approver>) -> RecoveryHook<S> {
        RecoveryHook {
            store,
            log,
            approver,
        }
    }
}

impl<S: AsyncStore + Send + Sync> AgentHook for RecoveryHook<S> {
    async fn on_run_settled(&self, _ctx: &HookContext, event: RunSettled<'_>) {
        let (SettledOutcome::Error(_), Some(messages)) = (event.outcome, event.messages) else {
            return;
        };
        if let Err(e) = append_messages(&self.store, self.log, messages).await {
            let message = format!(
                "the interrupted turn could not be saved to the conversation: {}",
                chain(&e)
            );
            tracing::warn!("{message}");
            self.approver.warn(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_memory::MemoryStore;

    #[tokio::test]
    async fn appends_to_an_empty_log() {
        let store = MemoryStore::default();
        append_messages(&store, "main", &[Arc::new(Message::user("one"))])
            .await
            .unwrap();
        let conversation = load_async(&store, "main").await.unwrap();
        let texts: Vec<_> = conversation
            .messages()
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("one"));
    }

    #[tokio::test]
    async fn appends_onto_an_existing_log() {
        let store = MemoryStore::default();
        append_messages(&store, "main", &[Arc::new(Message::user("one"))])
            .await
            .unwrap();
        append_messages(&store, "main", &[Arc::new(Message::assistant("two"))])
            .await
            .unwrap();
        let conversation = load_async(&store, "main").await.unwrap();
        assert_eq!(conversation.messages().len(), 2);
    }
}
