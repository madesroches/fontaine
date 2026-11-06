//! Saves a failed or abandoned run's committed messages into its conversation store. rig-agent
//! writes the conversation only when a run finishes, so without this the next run starts
//! without the interrupted turn: a `retry` would read as a new task, an auto-retry would start
//! over instead of continuing, and a prompt after Esc would find the task gone.

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

/// The log holding the messages an agent's run in progress has committed, beside the agent's
/// own log `id`.
pub(crate) fn journal_log(id: &str) -> String {
    format!("{id}.inflight")
}

/// Cuts the journal of the log `log` back to its first `keep` bytes, which end on a line
/// boundary. A missing journal is already cut.
async fn cut_journal<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    keep: u64,
) -> std::io::Result<()> {
    let journal = journal_log(log);
    let bytes = match store.read_log_async(&journal).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let keep_len = usize::try_from(keep).expect("a journal prefix fits in memory");
    if bytes.len() <= keep_len {
        return Ok(());
    }
    store
        .replace_log_async(&journal, bytes.len() as u64, &bytes[..keep_len])
        .await
        .map_err(std::io::Error::from)
}

/// Why a run ended without settling; picks the marker the model reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsettled {
    /// Esc or Ctrl-C.
    User,
    /// The AFK temperature ceiling stopped the session.
    AfkStop,
    /// The process ended, or an earlier fold failed, before the turn finished.
    SessionEnded,
}

impl Unsettled {
    /// What follows the kept turn in the conversation. The user's interrupt reads as Claude
    /// Code's; the other causes are not the user's doing, so they are harness notes.
    pub fn marker(self) -> &'static str {
        match self {
            Unsettled::User => "[Request interrupted by user]",
            Unsettled::AfkStop => {
                "[Harness note, not the user's words: the turn was stopped because the AFK \
                 temperature ceiling was reached.]"
            }
            Unsettled::SessionEnded => {
                "[Harness note, not the user's words: the session ended before this turn \
                 finished.]"
            }
        }
    }
}

/// The prompt an unsettled run sent, and how many messages its log held when the run started
/// (after any compaction).
pub struct Fallback {
    pub prompt: Message,
    pub log_len_before: usize,
}

/// What a fold or a settle records beside the journal: with it, a journal whose messages
/// already landed in `log` but whose cut failed is recognised and not appended twice.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
struct FoldMark {
    /// For a fold mark, the messages `log` held before the append. For a settled mark, the
    /// messages `log` held after the run's save, minus the journal's messages.
    log_len: usize,
    /// The journal's length in bytes when the mark was made.
    journal_bytes: u64,
}

fn fold_mark_file(log: &str) -> String {
    format!("{}.fold", journal_log(log))
}

fn settled_mark_file(log: &str) -> String {
    format!("{}.settled", journal_log(log))
}

/// The recorded mark; an absent, cleared or unreadable one reads as none, which folds as usual.
async fn read_mark<S: AsyncStore + ?Sized>(
    store: &S,
    file: &str,
) -> std::io::Result<Option<FoldMark>> {
    match store.read_file_async(file).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

async fn write_mark<S: AsyncStore + ?Sized>(
    store: &S,
    file: &str,
    mark: &FoldMark,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(mark).expect("a fold mark serializes");
    store.write_file_async(file, &bytes).await
}

/// The store has no removal, so a cleared mark is an empty file.
async fn clear_mark<S: AsyncStore + ?Sized>(store: &S, file: &str) -> std::io::Result<()> {
    store.write_file_async(file, &[]).await
}

async fn read_fold_mark<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<Option<FoldMark>> {
    read_mark(store, &fold_mark_file(log)).await
}

async fn write_fold_mark<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    mark: &FoldMark,
) -> std::io::Result<()> {
    write_mark(store, &fold_mark_file(log), mark).await
}

async fn clear_fold_mark<S: AsyncStore + ?Sized>(store: &S, log: &str) -> std::io::Result<()> {
    clear_mark(store, &fold_mark_file(log)).await
}

async fn read_settled_mark<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<Option<FoldMark>> {
    read_mark(store, &settled_mark_file(log)).await
}

async fn write_settled_mark<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    mark: &FoldMark,
) -> std::io::Result<()> {
    write_mark(store, &settled_mark_file(log), mark).await
}

async fn clear_settled_mark<S: AsyncStore + ?Sized>(store: &S, log: &str) -> std::io::Result<()> {
    clear_mark(store, &settled_mark_file(log)).await
}

/// Forgets a log's pending journal and marks. `/clear` and `/rewind` reset the conversation,
/// and an unsettled turn must not be folded into the reset one.
pub(crate) async fn discard_unsettled<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<()> {
    cut_journal(store, log, 0).await?;
    clear_fold_mark(store, log).await?;
    clear_settled_mark(store, log).await
}

/// Appends what an unsettled run committed to `log`, then the marker for `cause`, then empties
/// the journal. `fallback` is used when the journal holds nothing: the run was dropped before
/// its first model call, or every journal write failed. Returns whether anything was folded.
pub(crate) async fn fold_unsettled<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    cause: Unsettled,
    fallback: Option<Fallback>,
) -> Result<bool, SessionError> {
    let journal = journal_log(log);
    let journal_bytes = match store.read_log_async(&journal).await {
        Ok(bytes) => bytes.len() as u64,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    let journaled = load_async(store, &journal).await?;
    // Counting, not comparing: rig-memory changes some messages on load.
    let held = load_async(store, log).await?.messages().len();
    let marker = Arc::new(Message::user(cause.marker()));
    if journaled.messages().is_empty() {
        // A mark outlives its journal only when the cut that should have removed the pair
        // failed or never ran; a later same-sized journal would satisfy the stale count and
        // be cut without folding, so the mark goes with the empty journal.
        clear_fold_mark(store, log).await?;
        clear_settled_mark(store, log).await?;
        let Some(fallback) = fallback else {
            return Ok(false);
        };
        // A log past the run's start holds a failed attempt the recovery hook saved, prompt
        // included, so only the interruption is left to mark.
        let messages = if held > fallback.log_len_before {
            vec![marker]
        } else {
            vec![Arc::new(fallback.prompt), marker]
        };
        append_messages(store, log, &messages).await?;
        return Ok(true);
    }
    // A settled run's messages were saved before its journal was cut; a process that died
    // between the save and the cut, or a cut that failed, leaves both full. The settled mark
    // says the journal already landed in `log`, so only the cut is owed: appending again
    // would double the turn and repeat its tool-call ids.
    if let Some(mark) = read_settled_mark(store, log).await? {
        if mark.journal_bytes == journal_bytes && mark.log_len + journaled.messages().len() == held
        {
            cut_journal(store, log, 0).await?;
            clear_settled_mark(store, log).await?;
            return Ok(true);
        }
        // Stale: whatever it recorded, this journal is not the settled one it guarded.
        clear_settled_mark(store, log).await?;
    }
    let landed = read_fold_mark(store, log).await?.is_some_and(|mark| {
        mark.journal_bytes == journal_bytes && mark.log_len + journaled.messages().len() + 1 == held
    });
    if !landed {
        write_fold_mark(
            store,
            log,
            &FoldMark {
                log_len: held,
                journal_bytes,
            },
        )
        .await?;
        let mut messages = journaled.messages().to_vec();
        messages.push(marker);
        append_messages(store, log, &messages).await?;
    }
    cut_journal(store, log, 0).await?;
    clear_fold_mark(store, log).await?;
    Ok(true)
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

/// Journals an agent run's committed messages as it progresses, because rig-agent saves the
/// conversation only when the run settles, and neither a dropped run nor a killed process gets
/// there. The journal is a log of its own: appending to the agent's log would make the engine's
/// final save refuse a log that moved under it.
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
        cut_journal(&self.store, &self.id, keep).await
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

    /// Records, ahead of the cut, that the journal's messages are in the agent's log: the
    /// engine saved before `on_run_settled` ran, or the failed-run check in
    /// [`InflightHook::settle`] confirmed the recovery append landed. A fold that later finds
    /// the mark cuts the journal without appending, so a settled turn is never folded twice.
    /// Nothing is recorded while the journal carries an earlier run's kept prefix: those
    /// messages are not in the log, so the mark cannot claim the whole journal landed.
    async fn mark_settled(&self, run: &JournalRun, held: usize) -> std::io::Result<()> {
        if run.carried_bytes != 0 {
            return Ok(());
        }
        let bytes = self.store.read_log_async(&journal_log(&self.id)).await?;
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(log_len) = held.checked_sub(run.journaled) else {
            return Ok(());
        };
        write_settled_mark(
            &self.store,
            &self.id,
            &FoldMark {
                log_len,
                journal_bytes: bytes.len() as u64,
            },
        )
        .await
    }

    /// `failed_with` is the number of messages the failed run committed, `None` on success.
    /// `total` is the run's committed message count either way.
    async fn settle(&self, pad: &Scratchpad, failed_with: Option<usize>, total: usize) {
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
        if let Err(e) = self.mark_settled(&run, run.input_len + total).await {
            // The mark matters only when the cut below fails too, and that failure is warned.
            tracing::warn!("the settled mark could not be written: {}", chain(&e));
        }
        if let Err(e) = self.cut_back(run.carried_bytes).await {
            self.warn(pad, "the in-flight journal could not be cleared", &e);
            return;
        }
        if let Err(e) = clear_settled_mark(&self.store, &self.id).await {
            self.warn(pad, "the settled mark could not be cleared", &e);
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
        let total = event.messages.map_or(0, <[_]>::len);
        let failed_with = match event.outcome {
            SettledOutcome::Error(_) => Some(total),
            SettledOutcome::Response(_) => None,
        };
        self.settle(ctx.scratchpad(), failed_with, total).await;
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
    async fn two_calls<S: AsyncStore + Send + Sync>(
        hook: &InflightHook<S>,
        pad: &Scratchpad,
        prompt: &str,
    ) {
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
        let history = input.to_vec();
        hook.call(&pad, &history, &Message::user("new")).await;
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
        hook.settle(&pad, None, 4).await;
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
        hook.settle(&pad, Some(3), 3).await;
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
        hook.settle(&pad, Some(3), 3).await;
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
        hook.settle(&first, Some(3), 3).await;
        let kept = journal(&store).await;
        assert_eq!(kept.len(), 3);

        let second = Scratchpad::default();
        hook.start(&second, 0).await;
        assert_eq!(journal(&store).await, kept, "run start must not truncate");
        two_calls(&hook, &second, "second").await;
        assert_eq!(journal(&store).await.len(), 6);
        hook.settle(&second, None, 4).await;
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
        hook.settle(&fresh, Some(0), 0).await;
        hook.settle(&fresh, None, 0).await;
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
        hook.settle(&next, None, 3).await;
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
        hook.settle(&pad, None, 3).await;
        let warned = warnings.0.lock().unwrap();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert!(warned[0].contains("disk gone"), "{warned:?}");
    }

    async fn texts(store: &(impl AsyncStore + ?Sized), log: &str) -> Vec<String> {
        let c = load_async(store, log).await.unwrap();
        c.messages().iter().map(|m| format!("{m:?}")).collect()
    }

    /// A main log holding one earlier exchange, and a journal holding a run's prompt, its
    /// tool-call turn and the tool result.
    async fn interrupted(store: &MemoryStore) {
        append_messages(
            store,
            "main",
            &[user("earlier"), Arc::new(Message::assistant("answered"))],
        )
        .await
        .unwrap();
        append_messages(
            store,
            "main.inflight",
            &[
                user("go"),
                Arc::new(Message::assistant("calling")),
                user("tool result"),
            ],
        )
        .await
        .unwrap();
    }

    fn fallback(prompt: &str, log_len_before: usize) -> Option<Fallback> {
        Some(Fallback {
            prompt: Message::user(prompt),
            log_len_before,
        })
    }

    #[tokio::test]
    async fn a_fold_appends_the_journal_then_the_marker_and_empties_the_journal() {
        let store = MemoryStore::default();
        interrupted(&store).await;
        let folded = fold_unsettled(&store, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 6, "{main:?}");
        assert!(
            main[0].contains("earlier") && main[2].contains("go"),
            "{main:?}"
        );
        assert!(main[3].contains("calling") && main[4].contains("tool result"));
        assert!(
            main[5].contains("[Request interrupted by user]"),
            "{main:?}"
        );
        assert!(texts(&store, "main.inflight").await.is_empty());
    }

    #[tokio::test]
    async fn an_empty_journal_folds_the_fallback_prompt_and_the_marker() {
        let store = MemoryStore::default();
        let folded = fold_unsettled(&store, "main", Unsettled::User, fallback("go", 0))
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 2, "{main:?}");
        assert!(main[0].contains("go") && main[1].contains("interrupted"));
    }

    #[tokio::test]
    async fn an_empty_journal_without_a_fallback_folds_nothing() {
        let store = MemoryStore::default();
        let folded = fold_unsettled(&store, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(!folded);
        assert!(texts(&store, "main").await.is_empty());
    }

    #[tokio::test]
    async fn a_journal_wins_over_the_fallback_so_the_prompt_is_not_doubled() {
        let store = MemoryStore::default();
        interrupted(&store).await;
        fold_unsettled(&store, "main", Unsettled::User, fallback("go", 2))
            .await
            .unwrap();
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 6, "{main:?}");
        assert_eq!(main.iter().filter(|m| m.contains("\"go\"")).count(), 1);
    }

    #[tokio::test]
    async fn after_a_saved_failed_attempt_only_the_marker_is_appended() {
        let store = MemoryStore::default();
        // The recovery hook saved the failed attempt's prompt and cut the journal back.
        append_messages(&store, "main", &[user("go")])
            .await
            .unwrap();
        fold_unsettled(&store, "main", Unsettled::User, fallback("go", 0))
            .await
            .unwrap();
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 2, "{main:?}");
        assert!(main[0].contains("go") && main[1].contains("interrupted"));
    }

    #[tokio::test]
    async fn each_cause_has_its_marker() {
        for (cause, words) in [
            (Unsettled::User, "[Request interrupted by user]"),
            (Unsettled::AfkStop, "AFK temperature ceiling was reached"),
            (
                Unsettled::SessionEnded,
                "the session ended before this turn",
            ),
        ] {
            let store = MemoryStore::default();
            fold_unsettled(&store, "main", cause, fallback("go", 0))
                .await
                .unwrap();
            let main = texts(&store, "main").await;
            assert!(main[1].contains(words), "{cause:?}: {main:?}");
        }
        assert!(Unsettled::AfkStop.marker().starts_with("[Harness note"));
        assert!(
            Unsettled::SessionEnded
                .marker()
                .starts_with("[Harness note")
        );
    }

    /// A memory store whose log writes to the logs named in `appends` and `replaces` fail.
    struct Failing {
        inner: MemoryStore,
        appends: Vec<&'static str>,
        replaces: Vec<&'static str>,
    }

    impl AsyncStore for Failing {
        async fn read_log_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_log_async(name).await
        }
        async fn append_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            if self.appends.contains(&name) {
                return Err(std::io::Error::other("disk full").into());
            }
            self.inner.append_log_async(name, len, bytes).await
        }
        async fn replace_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            if self.replaces.contains(&name) {
                return Err(std::io::Error::other("disk full").into());
            }
            self.inner.replace_log_async(name, len, bytes).await
        }
        async fn read_file_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_file_async(name).await
        }
        async fn write_file_async(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.inner.write_file_async(name, bytes).await
        }
    }

    #[tokio::test]
    async fn a_failed_append_leaves_the_journal_for_the_next_fold() {
        let inner = MemoryStore::default();
        interrupted(&inner).await;
        let store = Failing {
            inner: inner.clone(),
            appends: vec!["main"],
            replaces: vec!["main"],
        };
        let result = fold_unsettled(&store, "main", Unsettled::User, None).await;
        assert!(result.is_err());
        assert_eq!(texts(&inner, "main").await.len(), 2);
        assert_eq!(texts(&inner, "main.inflight").await.len(), 3);
        // The next fold, on a healthy store, keeps the turn once.
        fold_unsettled(&inner, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert_eq!(texts(&inner, "main").await.len(), 6);
        assert!(texts(&inner, "main.inflight").await.is_empty());
    }

    #[tokio::test]
    async fn a_fold_whose_cut_failed_is_only_cut_the_next_time() {
        let inner = MemoryStore::default();
        interrupted(&inner).await;
        let store = Failing {
            inner: inner.clone(),
            appends: Vec::new(),
            replaces: vec!["main.inflight"],
        };
        assert!(
            fold_unsettled(&store, "main", Unsettled::User, None)
                .await
                .is_err()
        );
        assert_eq!(texts(&inner, "main").await.len(), 6);
        assert_eq!(texts(&inner, "main.inflight").await.len(), 3);
        fold_unsettled(&inner, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        let main = texts(&inner, "main").await;
        assert_eq!(main.len(), 6, "no second append: {main:?}");
        assert!(main[5].contains("interrupted by user"), "{main:?}");
        assert!(texts(&inner, "main.inflight").await.is_empty());
        // The mark is cleared, so a later journal folds as usual.
        append_messages(&inner, "main.inflight", &[user("next")])
            .await
            .unwrap();
        fold_unsettled(&inner, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert_eq!(texts(&inner, "main").await.len(), 8);
    }

    #[tokio::test]
    async fn a_mark_whose_count_does_not_fit_folds_as_usual() {
        let store = MemoryStore::default();
        interrupted(&store).await;
        let journal_bytes = store.read_log_async("main.inflight").await.unwrap().len() as u64;
        // The mark says the log held what it holds now: the append did not land.
        write_fold_mark(
            &store,
            "main",
            &FoldMark {
                log_len: 2,
                journal_bytes,
            },
        )
        .await
        .unwrap();
        fold_unsettled(&store, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert_eq!(texts(&store, "main").await.len(), 6);
    }

    #[tokio::test]
    async fn a_mark_for_another_journal_folds_as_usual() {
        let store = MemoryStore::default();
        interrupted(&store).await;
        // A count that would fit, but the journal is not the one the mark was made for.
        append_messages(
            &store,
            "main",
            &[user("a"), user("b"), user("c"), user("d")],
        )
        .await
        .unwrap();
        write_fold_mark(
            &store,
            "main",
            &FoldMark {
                log_len: 2,
                journal_bytes: 1,
            },
        )
        .await
        .unwrap();
        fold_unsettled(&store, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert_eq!(texts(&store, "main").await.len(), 10);
        assert!(texts(&store, "main.inflight").await.is_empty());
    }

    #[tokio::test]
    async fn a_settled_turn_is_not_folded_again_after_a_lost_cut() {
        let inner = MemoryStore::default();
        let warnings = Arc::new(Warnings::default());
        let store = Failing {
            inner: inner.clone(),
            appends: Vec::new(),
            replaces: vec!["agent-1.inflight"],
        };
        let hook = InflightHook::new(store, "agent-1".into(), warnings);
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        two_calls(&hook, &pad, "go").await;
        // rig saved the run's messages before the settle hook ran: the journal, plus the
        // final answer a completion call never journals.
        save_main(&inner, &[user("go"), user("a"), user("b"), user("c")]).await;
        hook.settle(&pad, None, 4).await;
        assert_eq!(
            journal(&inner).await.len(),
            3,
            "the lost cut keeps the journal"
        );
        // The fold finds the settled mark, so the journal is only cut: no second append of
        // messages the log already holds.
        fold_unsettled(&inner, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        let main = texts(&inner, "agent-1").await;
        assert_eq!(main.len(), 4, "no second append: {main:?}");
        assert!(journal(&inner).await.is_empty());
    }

    #[tokio::test]
    async fn a_stale_mark_does_not_discard_a_same_sized_later_journal() {
        let store = MemoryStore::default();
        append_messages(
            &store,
            "main",
            &[user("one"), user("two"), user("three"), user("four")],
        )
        .await
        .unwrap();
        append_messages(&store, "main.inflight", &[user("later turn")])
            .await
            .unwrap();
        let journal_bytes = store.read_log_async("main.inflight").await.unwrap().len() as u64;
        // The journal is cut but its mark's clear is lost. The stale count would fit a
        // same-sized later journal: log_len 2 + 1 journaled + 1 marker == 4 held.
        cut_journal(&store, "main", 0).await.unwrap();
        write_fold_mark(
            &store,
            "main",
            &FoldMark {
                log_len: 2,
                journal_bytes,
            },
        )
        .await
        .unwrap();
        // The empty journal's fold clears the stale mark.
        let folded = fold_unsettled(&store, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(!folded);
        // The later interrupted turn folds; the stale mark does not cut it as landed.
        append_messages(&store, "main.inflight", &[user("later turn")])
            .await
            .unwrap();
        let folded = fold_unsettled(&store, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 6, "{main:?}");
        assert!(
            main[4].contains("later turn") && main[5].contains("interrupted"),
            "{main:?}"
        );
        assert!(texts(&store, "main.inflight").await.is_empty());
    }
}
