//! What a forked subagent starts from: the messages the main agent's model read on its latest
//! call. The stored conversation cannot serve, because rig-agent writes it only when a run is
//! done, so mid-turn it lacks the current prompt and everything the turn has done so far.

use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use fontaine_tools::lock::lock;
use rig_agent::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext};
use rig_core::conversation::Conversation;
use rig_core::conversation::pairing::PairingError;
use rig_core::message::Message;
use rig_memory::{DirStore, SessionError, save_async};

/// The snapshot ends in a user-role message, so the fork's history needs an assistant turn
/// before its task arrives as the next user turn.
const BRIDGE: &str = "Understood. I will take this on as a forked subagent.";

/// Goes in front of a fork's task so the model reads the task as the new turn, not as a
/// continuation of the conversation above.
pub const FORK_NOTE: &str =
    "You are a forked subagent of the conversation above; do only the task below and report.";

/// The latest model call of the main agent, shared between the hook that records it and the
/// runner that forks from it.
#[derive(Clone, Default)]
pub struct ParentContext {
    messages: Arc<Mutex<Option<Vec<Arc<Message>>>>>,
}

impl ParentContext {
    /// A context fixed to `messages`, for a runner under test.
    pub fn fixed(messages: Vec<Arc<Message>>) -> ParentContext {
        ParentContext {
            messages: Arc::new(Mutex::new(Some(messages))),
        }
    }

    fn record(&self, history: &[Arc<Message>], prompt: &Message) {
        let mut messages = history.to_vec();
        messages.push(Arc::new(prompt.clone()));
        *lock(&self.messages, "parent context") = Some(messages);
    }

    /// Saves the parent's conversation into `log` of the session's store, ready for the
    /// fork's task as its next user turn. The log is the child's own, so its runs persist
    /// beside the root transcript.
    pub async fn seed(&self, store: &DirStore, log: &str) -> Result<(), ForkError> {
        let messages = lock(&self.messages, "parent context")
            .clone()
            .ok_or(ForkError::NoContext)?;
        let mut conversation = Conversation::from_messages(messages);
        conversation.validate().map_err(ForkError::Unpaired)?;
        conversation.push(Message::assistant(BRIDGE));
        save_async(store, log, &mut conversation)
            .await
            .map_err(ForkError::Store)?;
        Ok(())
    }
}

/// Why a fork cannot start. Never answered with an empty-context fork: its prompt assumes the
/// conversation.
#[derive(Debug)]
pub enum ForkError {
    NoContext,
    Unpaired(PairingError),
    Store(SessionError),
}

impl fmt::Display for ForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ForkError::NoContext => {
                f.write_str("the main conversation has no context to fork from yet")
            }
            ForkError::Unpaired(_) => {
                f.write_str("the main conversation is not a consistent history to fork from")
            }
            ForkError::Store(_) => f.write_str("the forked conversation could not be stored"),
        }
    }
}

impl Error for ForkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ForkError::NoContext => None,
            ForkError::Unpaired(e) => Some(e),
            ForkError::Store(e) => Some(e),
        }
    }
}

/// Records what the root agent's model reads on every call. Registered on the root agent only,
/// so a child never feeds it.
pub struct ParentContextHook {
    context: ParentContext,
}

impl ParentContextHook {
    pub fn new(context: ParentContext) -> ParentContextHook {
        ParentContextHook { context }
    }
}

impl AgentHook for ParentContextHook {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        self.context.record(event.history, event.prompt);
        CompletionCallAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{AssistantContent, AssistantMessage, ToolName};
    use rig_memory::load_async;

    fn user(text: &str) -> Arc<Message> {
        Arc::new(Message::user(text))
    }

    #[tokio::test]
    async fn the_hook_keeps_the_latest_call_history_and_prompt() {
        let context = ParentContext::default();
        let history = vec![user("one"), Arc::new(Message::assistant("two"))];
        context.record(&history, &Message::user("three"));
        context.record(&history[..1], &Message::user("four"));
        let dir = tempfile::tempdir().unwrap();
        let store = rig_memory::DirStore::open(dir.path()).unwrap();
        context.seed(&store, "agent-1").await.ok().unwrap();
        let seeded = load_async(&store, "agent-1").await.unwrap();
        let texts: Vec<_> = seeded.messages().iter().map(|m| format!("{m:?}")).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(texts[0].contains("one") && texts[1].contains("four"));
        assert!(texts[2].contains("forked subagent"));
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("two") || t.contains("three"))
        );
        // The seed is the child's own log, beside the root's in the session store.
        assert!(dir.path().join("logs").join("agent-1.jsonl").is_file());
    }

    #[tokio::test]
    async fn an_empty_slot_fails_the_seed() {
        let dir = tempfile::tempdir().unwrap();
        let store = rig_memory::DirStore::open(dir.path()).unwrap();
        let error = ParentContext::default()
            .seed(&store, "agent-1")
            .await
            .err()
            .unwrap();
        assert!(matches!(error, ForkError::NoContext), "{error}");
    }

    #[tokio::test]
    async fn an_unpaired_snapshot_fails_the_seed() {
        let call = Message::Assistant(AssistantMessage::new(vec![AssistantContent::tool_call(
            "c1",
            ToolName::new("read").unwrap(),
            serde_json::json!({}),
        )]));
        let context = ParentContext::fixed(vec![user("go"), Arc::new(call)]);
        let dir = tempfile::tempdir().unwrap();
        let store = rig_memory::DirStore::open(dir.path()).unwrap();
        let error = context.seed(&store, "agent-1").await.err().unwrap();
        assert!(matches!(error, ForkError::Unpaired(_)), "{error}");
        assert!(error.source().is_some());
    }
}
