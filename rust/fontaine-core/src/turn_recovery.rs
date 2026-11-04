//! Saves a failed run's committed messages into its conversation store. rig-agent writes the
//! conversation only when a run finishes, so without this the next run starts without the
//! interrupted turn: a `retry` would read as a new task and an auto-retry would start over
//! instead of continuing.

use std::sync::Arc;

use fontaine_tools::error_chain::chain;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RunSettled, RunStart,
    RunStartAction, Scratchpad, SettledOutcome,
};
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
    log: String,
    approver: Arc<dyn Approver>,
}

impl<S> RecoveryHook<S> {
    pub fn new(store: S, log: String, approver: Arc<dyn Approver>) -> RecoveryHook<S> {
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
        if let Err(e) = append_messages(&self.store, &self.log, messages).await {
            let message = format!(
                "the interrupted turn could not be saved to the conversation: {}",
                chain(&e)
            );
            tracing::warn!("{message}");
            self.approver.warn(&message);
        }
    }
}

/// The log holding the messages a subagent's run in progress has committed.
pub(crate) fn journal_log(id: &str) -> String {
    format!("{id}.inflight")
}

/// Per-run journal bookkeeping, kept in the run's scratchpad so a reused agent starts each
/// run clean.
#[derive(Clone)]
struct JournalRun {
    input_len: usize,
    journaled: usize,
    /// Bytes of whole lines the journal held at run start: a journal an earlier failed run
    /// kept, which this run appends after and cuts back to.
    carried_bytes: u64,
    warned: bool,
}

/// Journals a subagent run's committed messages as it progresses, because rig-agent saves the
/// conversation only when the run settles and a killed process never gets there. The journal
/// is a log of its own: appending to the agent's log would make the engine's final save
/// refuse a log that moved under it.
pub struct InflightHook<S> {
    store: S,
    id: String,
    approver: Arc<dyn Approver>,
}

impl<S> InflightHook<S> {
    pub fn new(store: S, id: String, approver: Arc<dyn Approver>) -> InflightHook<S> {
        InflightHook {
            store,
            id,
            approver,
        }
    }
}

impl<S: AsyncStore> InflightHook<S> {
    fn warn(&self, pad: &Scratchpad, detail: &str, e: &(dyn std::error::Error + 'static)) {
        let first = pad.get::<JournalRun>().is_none_or(|mut run| {
            let first = !run.warned;
            run.warned = true;
            pad.insert(run);
            first
        });
        if !first {
            return;
        }
        let message = format!("{detail}: {}", chain(e));
        tracing::warn!("{message}");
        self.approver.warn(&message);
    }

    /// Cuts the journal back to its first `keep` bytes, which end on a line boundary.
    async fn cut_back(&self, keep: u64) -> std::io::Result<()> {
        let log = journal_log(&self.id);
        let bytes = match self.store.read_log_async(&log).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let keep_len = usize::try_from(keep).expect("a journal prefix fits in memory");
        if bytes.len() <= keep_len {
            return Ok(());
        }
        self.store
            .replace_log_async(&log, bytes.len() as u64, &bytes[..keep_len])
            .await
            .map_err(std::io::Error::from)
    }
}

impl<S: AsyncStore + Send + Sync> InflightHook<S> {
    async fn start(&self, pad: &Scratchpad, input_len: usize) {
        let log = journal_log(&self.id);
        let carried_bytes = match self.store.read_log_async(&log).await {
            Ok(bytes) => bytes
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |at| at as u64 + 1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => {
                // Unreadable now means the run goes unjournaled rather than risk cutting
                // back to the wrong prefix at settle.
                pad.insert(JournalRun {
                    input_len,
                    journaled: usize::MAX,
                    carried_bytes: 0,
                    warned: false,
                });
                self.warn(pad, "the in-flight journal could not be read", &e);
                return;
            }
        };
        pad.insert(JournalRun {
            input_len,
            journaled: 0,
            carried_bytes,
            warned: false,
        });
    }

    async fn call(&self, pad: &Scratchpad, history: &[Arc<Message>], prompt: &Message) {
        let Some(mut run) = pad.get::<JournalRun>() else {
            return;
        };
        if run.journaled == usize::MAX {
            return;
        }
        let committed: Vec<Arc<Message>> = history
            .iter()
            .skip(run.input_len)
            .cloned()
            .chain(std::iter::once(Arc::new(prompt.clone())))
            .collect();
        let Some(new) = committed.get(run.journaled..).filter(|new| !new.is_empty()) else {
            return;
        };
        match append_messages(&self.store, &journal_log(&self.id), new).await {
            Ok(()) => {
                run.journaled = committed.len();
                pad.insert(run);
            }
            Err(e) => self.warn(pad, "the in-flight journal could not be written", &e),
        }
    }

    /// `failed_with` is the number of messages the failed run committed, `None` on success.
    async fn settle(&self, pad: &Scratchpad, failed_with: Option<usize>) {
        let Some(run) = pad.get::<JournalRun>() else {
            return;
        };
        if run.journaled == usize::MAX {
            return;
        }
        if let Some(committed) = failed_with {
            // The engine did not save, so the messages are in the agent's log only if the
            // recovery hook's append landed. Counting, because rig-memory changes some
            // messages on load and equality is unreliable.
            match load_async(&self.store, &self.id).await {
                Ok(c) if c.messages().len() == run.input_len + committed => {}
                Ok(_) => {
                    let e = std::io::Error::other("the conversation lacks the run's messages");
                    self.warn(pad, "the in-flight journal is kept", &e);
                    return;
                }
                Err(e) => {
                    self.warn(
                        pad,
                        "the conversation could not be checked, so the in-flight journal is kept",
                        &e,
                    );
                    return;
                }
            }
        }
        if let Err(e) = self.cut_back(run.carried_bytes).await {
            self.warn(pad, "the in-flight journal could not be cleared", &e);
        }
    }
}

impl<S: AsyncStore + Send + Sync> AgentHook for InflightHook<S> {
    async fn on_run_start(&self, ctx: &HookContext, event: RunStart<'_>) -> RunStartAction {
        self.start(ctx.scratchpad(), event.history.len()).await;
        RunStartAction::Continue
    }

    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        self.call(ctx.scratchpad(), event.history, event.prompt)
            .await;
        CompletionCallAction::Continue
    }

    async fn on_run_settled(&self, ctx: &HookContext, event: RunSettled<'_>) {
        let failed_with = match event.outcome {
            SettledOutcome::Error(_) => Some(event.messages.map_or(0, <[_]>::len)),
            SettledOutcome::Response(_) => None,
        };
        self.settle(ctx.scratchpad(), failed_with).await;
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

    #[tokio::test]
    async fn a_torn_log_remains_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = rig_memory::DirStore::open(dir.path()).unwrap();
        append_messages(&store, "main", &[Arc::new(Message::user("one"))])
            .await
            .unwrap();
        // A crash mid-write leaves a torn tail: bytes after the last newline.
        let log = dir.path().join("logs").join("main.jsonl");
        let mut bytes = std::fs::read(&log).unwrap();
        bytes.extend_from_slice(b"{\"torn");
        std::fs::write(&log, bytes).unwrap();
        let conversation = load_async(&store, "main").await.unwrap();
        assert_eq!(conversation.messages().len(), 1);
        append_messages(&store, "main", &[Arc::new(Message::user("two"))])
            .await
            .unwrap();
        let conversation = load_async(&store, "main").await.unwrap();
        assert_eq!(conversation.messages().len(), 2);
    }

    #[derive(Default)]
    struct Warnings(std::sync::Mutex<Vec<String>>);

    impl Approver for Warnings {
        fn approve<'a>(
            &'a self,
            _request: crate::permissions::ApprovalRequest,
        ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
            Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
        }

        fn warn(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    fn hook(store: Arc<MemoryStore>) -> (InflightHook<Arc<MemoryStore>>, Arc<Warnings>) {
        let warnings = Arc::new(Warnings::default());
        let hook = InflightHook::new(store, "agent-1".into(), warnings.clone());
        (hook, warnings)
    }

    fn user(text: &str) -> Arc<Message> {
        Arc::new(Message::user(text))
    }

    async fn journal(store: &MemoryStore) -> Vec<String> {
        let c = load_async(store, "agent-1.inflight").await.unwrap();
        c.messages().iter().map(|m| format!("{m:?}")).collect()
    }

    async fn save_main(store: &MemoryStore, messages: &[Arc<Message>]) {
        append_messages(store, "agent-1", messages).await.unwrap();
    }

    /// A run that makes two model calls: the prompt, then an assistant turn and a tool result.
    async fn two_calls(hook: &InflightHook<Arc<MemoryStore>>, pad: &Scratchpad, prompt: &str) {
        hook.call(pad, &[], &Message::user(prompt)).await;
        let history = [user(prompt), Arc::new(Message::assistant("calling"))];
        hook.call(pad, &history, &Message::user("tool result"))
            .await;
    }

    #[tokio::test]
    async fn the_journal_holds_the_committed_messages_in_order() {
        let store = Arc::new(MemoryStore::default());
        let (hook, _) = hook(store.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        hook.call(&pad, &[], &Message::user("go")).await;
        assert_eq!(journal(&store).await.len(), 1);
        let history = [user("go"), Arc::new(Message::assistant("calling"))];
        hook.call(&pad, &history, &Message::user("tool result"))
            .await;
        let lines = journal(&store).await;
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("go") && lines[1].contains("calling"));
        assert!(lines[2].contains("tool result"));
    }

    #[tokio::test]
    async fn input_history_is_not_journaled() {
        let store = Arc::new(MemoryStore::default());
        let (hook, _) = hook(store.clone());
        let pad = Scratchpad::default();
        let input = [user("old"), Arc::new(Message::assistant("older"))];
        hook.start(&pad, input.len()).await;
        let mut history = input.to_vec();
        hook.call(&pad, &history, &Message::user("new")).await;
        history.push(user("new"));
        let lines = journal(&store).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("new"));
    }

    #[tokio::test]
    async fn a_response_settle_empties_the_journal() {
        let store = Arc::new(MemoryStore::default());
        let (hook, _) = hook(store.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        two_calls(&hook, &pad, "go").await;
        hook.settle(&pad, None).await;
        assert!(journal(&store).await.is_empty());
    }

    #[tokio::test]
    async fn an_error_settle_empties_the_journal_once_the_log_holds_the_messages() {
        let store = Arc::new(MemoryStore::default());
        let (hook, warnings) = hook(store.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        two_calls(&hook, &pad, "go").await;
        save_main(&store, &[user("go"), user("a"), user("b")]).await;
        hook.settle(&pad, Some(3)).await;
        assert!(journal(&store).await.is_empty());
        assert!(warnings.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_error_settle_keeps_the_journal_when_the_log_lacks_the_messages() {
        let store = Arc::new(MemoryStore::default());
        let (hook, warnings) = hook(store.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        two_calls(&hook, &pad, "go").await;
        hook.settle(&pad, Some(3)).await;
        assert_eq!(journal(&store).await.len(), 3);
        assert_eq!(warnings.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_kept_journal_is_carried_through_the_next_run() {
        let store = Arc::new(MemoryStore::default());
        let (hook, _) = hook(store.clone());
        let first = Scratchpad::default();
        hook.start(&first, 0).await;
        two_calls(&hook, &first, "first").await;
        hook.settle(&first, Some(3)).await;
        let kept = journal(&store).await;
        assert_eq!(kept.len(), 3);

        let second = Scratchpad::default();
        hook.start(&second, 0).await;
        assert_eq!(journal(&store).await, kept, "run start must not truncate");
        two_calls(&hook, &second, "second").await;
        assert_eq!(journal(&store).await.len(), 6);
        hook.settle(&second, None).await;
        assert_eq!(journal(&store).await, kept);
    }

    #[tokio::test]
    async fn a_settle_without_a_run_start_leaves_the_journal_alone() {
        let store = Arc::new(MemoryStore::default());
        let (hook, warnings) = hook(store.clone());
        let started = Scratchpad::default();
        hook.start(&started, 0).await;
        hook.call(&started, &[], &Message::user("go")).await;
        let fresh = Scratchpad::default();
        hook.call(&fresh, &[], &Message::user("ignored")).await;
        hook.settle(&fresh, Some(0)).await;
        hook.settle(&fresh, None).await;
        assert_eq!(journal(&store).await.len(), 1);
        assert!(warnings.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_torn_journal_still_loads_and_is_carried() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(rig_memory::DirStore::open(dir.path()).unwrap());
        let warnings = Arc::new(Warnings::default());
        let hook = InflightHook::new(store.clone(), "agent-1".into(), warnings);
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        hook.call(&pad, &[], &Message::user("go")).await;
        let log = dir.path().join("logs").join("agent-1.inflight.jsonl");
        let mut bytes = std::fs::read(&log).unwrap();
        bytes.extend_from_slice(b"{\"torn");
        std::fs::write(&log, bytes).unwrap();
        let loaded = load_async(&*store, "agent-1.inflight").await.unwrap();
        assert_eq!(loaded.messages().len(), 1);

        let next = Scratchpad::default();
        hook.start(&next, 0).await;
        hook.call(&next, &[], &Message::user("again")).await;
        assert_eq!(
            load_async(&*store, "agent-1.inflight")
                .await
                .unwrap()
                .messages()
                .len(),
            2
        );
        hook.settle(&next, None).await;
        assert_eq!(
            load_async(&*store, "agent-1.inflight")
                .await
                .unwrap()
                .messages()
                .len(),
            1
        );
    }

    /// A store whose reads fail.
    struct Broken;

    impl AsyncStore for Broken {
        async fn read_log_async(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other("disk gone"))
        }
        async fn append_log_async(
            &self,
            _: &str,
            _: u64,
            _: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            Err(std::io::Error::other("disk gone").into())
        }
        async fn replace_log_async(
            &self,
            _: &str,
            _: u64,
            _: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            Err(std::io::Error::other("disk gone").into())
        }
        async fn read_file_async(&self, _: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other("disk gone"))
        }
        async fn write_file_async(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("disk gone"))
        }
    }

    #[tokio::test]
    async fn a_store_failure_warns_once_and_the_run_continues() {
        let warnings = Arc::new(Warnings::default());
        let hook = InflightHook::new(Broken, "agent-1".into(), warnings.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        hook.call(&pad, &[], &Message::user("go")).await;
        hook.call(&pad, &[user("go")], &Message::user("more")).await;
        hook.settle(&pad, None).await;
        let warned = warnings.0.lock().unwrap();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert!(warned[0].contains("disk gone"), "{warned:?}");
    }
}
