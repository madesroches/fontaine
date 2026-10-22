//! The window a subagent tree has onto the root session's ledger. In a tree there is one
//! intent source: the human's messages in the root's ledger. What a spawn prompt or a message
//! says reaches the guardian only as untrusted context, never through here.

/// What the tree records about its own life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    Spawned {
        agent: String,
        parent: String,
        agent_type: String,
    },
    Finished {
        agent: String,
        status: String,
    },
    Message {
        from: String,
        to: String,
    },
}

pub trait RootLedger: Send + Sync {
    /// The root's human messages, verbatim, oldest first.
    fn human_messages(&self) -> Vec<String>;

    fn append_event(&self, event: AgentEvent);
}
