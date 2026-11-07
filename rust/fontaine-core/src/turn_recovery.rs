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
    /// position in `log` where the saved journal begins: the messages `log` held when the
    /// run started.
    log_len: usize,
    /// The journal's length in bytes when the mark was made.
    journal_bytes: u64,
    /// How many messages the guarded append wrote, marker included: a retry whose log holds
    /// exactly `log_len + appended` knows the append landed and only cuts.
    #[serde(default)]
    appended: usize,
}

/// What a run's start records beside the journal: the prompt it sent and how many messages
/// `log` held when the run began. A fold after a kill before the run's first model call has
/// no fallback of its own; this is the fallback then, and the run-tied evidence that
/// positions the journal inside a run rig saved before the process died. Cleared when the
/// run settles.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct RunRecord {
    pub prompt: Message,
    pub log_len_before: usize,
}

fn run_record_file(log: &str) -> String {
    format!("{}.run", journal_log(log))
}

async fn read_run_record<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<Option<RunRecord>> {
    match store.read_file_async(&run_record_file(log)).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub(crate) async fn write_run_record<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
    record: &RunRecord,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(record).expect("a run record serializes");
    store.write_file_async(&run_record_file(log), &bytes).await
}

pub(crate) async fn clear_run_record<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<()> {
    store.write_file_async(&run_record_file(log), &[]).await
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

/// A cheap content fingerprint of a loaded message. rig-memory normalizes some messages on
/// load, so loaded messages cannot be compared by equality; the Debug render is stable for
/// identical content.
fn fingerprint(message: &Message) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{message:?}").hash(&mut hasher);
    hasher.finish()
}

/// Whether the journal occupies `log` starting exactly where the run record says the run
/// began: rig saves [prefix][journal][final response] before the settle hook can record
/// the save, and the failed-run save is [prefix][journal] alone, the final response never
/// existing. A save ending where the journal ends is landed too; the run record's
/// positions plus the fingerprint authenticate it. Only this run-tied evidence proves a
/// save the marks cannot; a content-only tail match is inconclusive, because a repeated
/// prompt can replay an earlier attempt's journal and cutting there would discard the new
/// turn.
fn journal_is_landed_at(
    journaled: &[Arc<Message>],
    held: &[Arc<Message>],
    count_before: usize,
) -> bool {
    let Some(end) = count_before.checked_add(journaled.len()) else {
        return false;
    };
    held.len() >= end
        && held[count_before..end]
            .iter()
            .zip(journaled)
            .all(|(held, journaled)| fingerprint(held) == fingerprint(journaled))
}

/// Whether the journal's messages are exactly the tail of `log`'s. Used only where a settled
/// mark already ties the match to the run that wrote it (`resolve_carried`).
fn journal_is_landed(journaled: &[Arc<Message>], held: &[Arc<Message>]) -> bool {
    journaled.len() <= held.len()
        && held[held.len() - journaled.len()..]
            .iter()
            .zip(journaled)
            .all(|(held, journaled)| fingerprint(held) == fingerprint(journaled))
}

/// Forgets a log's pending journal, marks, and run record. `/clear` and `/rewind` reset the
/// conversation, and an unsettled turn must not be folded into the reset one.
pub(crate) async fn discard_unsettled<S: AsyncStore + ?Sized>(
    store: &S,
    log: &str,
) -> std::io::Result<()> {
    cut_journal(store, log, 0).await?;
    clear_fold_mark(store, log).await?;
    clear_settled_mark(store, log).await?;
    clear_run_record(store, log).await
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
    let main = load_async(store, log).await?;
    // Counting, not comparing: rig-memory changes some messages on load.
    let held = main.messages().len();
    let marker = Arc::new(Message::user(cause.marker()));
    // Whether the surface passed its own fallback: only that says the run was cut short. A
    // fallback resolved from the run record below may belong to a run that saved its whole
    // turn before the process died, which owes no marker.
    let cut_short = fallback.is_some();
    // A fold after a kill has no fallback of its own; the run record the surface wrote at
    // run start is the fallback then, and the evidence tying the journal to its positions.
    // The surface's fallback outranks the record: it is fresher, taken after compaction.
    let record = read_run_record(store, log).await?;
    let fallback = fallback.or_else(|| {
        record.map(|record| Fallback {
            prompt: record.prompt,
            log_len_before: record.log_len_before,
        })
    });
    if journaled.messages().is_empty() {
        // A fold that crashed after its append left its mark behind, guarding this empty
        // journal; the log already holds what the append wrote. The mark is only cleared
        // where it does not fit: a later same-sized journal would satisfy a stale count and
        // be cut without folding.
        if let Some(mark) = read_fold_mark(store, log).await? {
            if mark.journal_bytes == 0 && held == mark.log_len + mark.appended {
                cut_journal(store, log, 0).await?;
                clear_fold_mark(store, log).await?;
                clear_settled_mark(store, log).await?;
                clear_run_record(store, log).await?;
                return Ok(true);
            }
            // Stale: whatever it recorded, it does not guard this empty journal.
            clear_fold_mark(store, log).await?;
        }
        let Some(fallback) = fallback else {
            // The settled mark outlives its journal only the same way; it goes with it.
            clear_settled_mark(store, log).await?;
            return Ok(false);
        };
        // A log past the run's start holds a failed attempt the recovery hook saved, prompt
        // included, so only the interruption is left to mark — but only when the surface
        // passed the fallback. A fallback resolved from the run record belongs to a run
        // whose save already advanced `log` past `log_len_before`: nothing is unsettled,
        // the record is spent evidence, and no marker is owed. A clear the settle lost
        // would otherwise read as a cut-short turn and insert a false one.
        if held > fallback.log_len_before && !cut_short {
            clear_settled_mark(store, log).await?;
            clear_run_record(store, log).await?;
            return Ok(false);
        }
        let messages = if held > fallback.log_len_before {
            vec![marker]
        } else {
            vec![Arc::new(fallback.prompt), marker]
        };
        // The append lands before the cleanup, and the mark guards it: a cleanup failure
        // must not strand the only copy of the prompt, and a retry must not append twice.
        write_fold_mark(
            store,
            log,
            &FoldMark {
                log_len: held,
                journal_bytes: 0,
                appended: messages.len(),
            },
        )
        .await?;
        append_messages(store, log, &messages).await?;
        cut_journal(store, log, 0).await?;
        clear_fold_mark(store, log).await?;
        clear_settled_mark(store, log).await?;
        clear_run_record(store, log).await?;
        return Ok(true);
    }
    let journaled_len = journaled.messages().len();
    // A fold that crashed after its append left its mark behind; the log already holds what
    // the append wrote. The mark fits whether the append carried the journal's messages and
    // the marker (a normal fold) or the marker alone (a settled journal owed its marker).
    if read_fold_mark(store, log).await?.is_some_and(|mark| {
        mark.journal_bytes == journal_bytes && mark.log_len + mark.appended == held
    }) {
        cut_journal(store, log, 0).await?;
        clear_fold_mark(store, log).await?;
        clear_settled_mark(store, log).await?;
        clear_run_record(store, log).await?;
        return Ok(true);
    }
    // A settled run's messages were saved before its journal was cut; a process that died
    // between the save and the cut, or a cut that failed, leaves both full. The settled mark
    // says the journal already landed in `log`, so only the cut is owed: appending again
    // would double the turn and repeat its tool-call ids. A run the surface cut short still
    // owes its marker, though; the fold mark written first guards that append, so a crash
    // before the cut re-enters above and only cuts.
    if let Some(mark) = read_settled_mark(store, log).await? {
        // A same-sized retry journal would pass a count-only check on a stale mark;
        // only the fingerprint at the mark's positions proves the journal in `log` is
        // the one the mark guarded.
        if mark.journal_bytes == journal_bytes
            && journal_is_landed_at(journaled.messages(), main.messages(), mark.log_len)
        {
            if cut_short {
                write_fold_mark(
                    store,
                    log,
                    &FoldMark {
                        log_len: held,
                        journal_bytes,
                        appended: 1,
                    },
                )
                .await?;
                append_messages(store, log, &[marker]).await?;
            }
            cut_journal(store, log, 0).await?;
            clear_settled_mark(store, log).await?;
            clear_fold_mark(store, log).await?;
            clear_run_record(store, log).await?;
            return Ok(true);
        }
        // Stale: whatever it recorded, this journal is not the settled one it guarded.
        clear_settled_mark(store, log).await?;
    }
    // No mark proves the run settled, but the save may have landed just before the process
    // died (rig saves before the settle hook; so does the failed-run path) or every mark
    // write may have failed. Only run-tied evidence decides: the run record says the run
    // began when `log` held `count_before` messages, so a saved run holds the journal at
    // [count_before..count_before+journal_len], the final response after it or nothing —
    // the failed-run save writes no final response. Without it, a tail content match stays
    // inconclusive and the fold proceeds as usual: a doubled rare-crash turn beats a lost
    // one.
    if let Some(count_before) = fallback.as_ref().map(|fallback| fallback.log_len_before)
        && journal_is_landed_at(journaled.messages(), main.messages(), count_before)
    {
        if cut_short {
            write_fold_mark(
                store,
                log,
                &FoldMark {
                    log_len: held,
                    journal_bytes,
                    appended: 1,
                },
            )
            .await?;
            append_messages(store, log, &[marker]).await?;
        }
        cut_journal(store, log, 0).await?;
        clear_fold_mark(store, log).await?;
        clear_settled_mark(store, log).await?;
        clear_run_record(store, log).await?;
        return Ok(true);
    }
    write_fold_mark(
        store,
        log,
        &FoldMark {
            log_len: held,
            journal_bytes,
            appended: journaled_len + 1,
        },
    )
    .await?;
    let mut messages = journaled.messages().to_vec();
    messages.push(marker);
    append_messages(store, log, &messages).await?;
    cut_journal(store, log, 0).await?;
    clear_fold_mark(store, log).await?;
    clear_run_record(store, log).await?;
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
    /// The root agent's hook alone writes the run record: a child agent's runs settle into
    /// their own store and log, which no fold reads.
    run_records: bool,
}

impl<S> InflightHook<S> {
    pub fn new(store: S, id: String, approver: Arc<dyn Approver>) -> InflightHook<S> {
        InflightHook {
            store,
            id,
            approver,
            run_records: false,
        }
    }

    /// The hook of the agent whose log the run record guards: also writes a fresh record at
    /// every run start, so each retry attempt carries one after the attempt before it
    /// settled and cleared its own.
    pub fn root(store: S, id: String, approver: Arc<dyn Approver>) -> InflightHook<S> {
        InflightHook {
            store,
            id,
            approver,
            run_records: true,
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

    /// Writes the run record a root hook keeps per attempt. A failure is warned; the attempt
    /// goes unrecorded rather than unrun.
    async fn record_run(&self, prompt: &Message, log_len_before: usize) {
        let record = RunRecord {
            prompt: prompt.clone(),
            log_len_before,
        };
        if let Err(e) = write_run_record(&self.store, &self.id, &record).await {
            let message = format!(
                "the run's pending record could not be written: {}",
                chain(&e)
            );
            tracing::warn!("{message}");
            self.approver.warn(&message);
        }
    }

    /// Resolves a journal prefix an earlier failed run kept only because its cut failed: the
    /// recovery hook saved the attempt, the settled mark says so, and the log's tail confirms
    /// the prefix already landed. The journal is cut to empty and the mark cleared, so the
    /// new run journals only its own messages and the prefix is not folded again. Any doubt
    /// keeps the journal, which folds as it does today. Returns whether the journal was
    /// emptied.
    async fn resolve_carried(&self, pad: &Scratchpad) -> bool {
        let Ok(Some(mark)) = read_settled_mark(&self.store, &self.id).await else {
            return false;
        };
        let Ok(bytes) = self.store.read_log_async(&journal_log(&self.id)).await else {
            return false;
        };
        if bytes.is_empty() || mark.journal_bytes != bytes.len() as u64 {
            return false;
        }
        let (Ok(held), Ok(journaled)) = (
            load_async(&self.store, &self.id).await,
            load_async(&self.store, &journal_log(&self.id)).await,
        ) else {
            return false;
        };
        if mark.log_len + journaled.messages().len() != held.messages().len() {
            return false;
        }
        if !journal_is_landed(journaled.messages(), held.messages()) {
            return false;
        }
        if let Err(e) = cut_journal(&self.store, &self.id, 0).await {
            self.warn(pad, "the kept in-flight journal could not be cleared", &e);
            return false;
        }
        if let Err(e) = clear_settled_mark(&self.store, &self.id).await {
            self.warn(pad, "the settled mark could not be cleared", &e);
        }
        true
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
        // An empty journal spends the previous attempt's evidence: its journal was cut
        // or never written, so a settled or fold mark left behind is stale and must not
        // cut a same-sized retry journal later.
        if carried_bytes == 0 {
            if let Err(e) = clear_settled_mark(&self.store, &self.id).await {
                self.warn(pad, "the stale settled mark could not be cleared", &e);
            }
            if let Err(e) = clear_fold_mark(&self.store, &self.id).await {
                self.warn(pad, "the stale fold mark could not be cleared", &e);
            }
        }
        // A kept prefix whose failed attempt is already the log's tail (its save landed, its
        // cut failed) is resolved before this run journals, so the retry folds nothing of it.
        let carried_bytes = if self.resolve_carried(pad).await {
            0
        } else {
            carried_bytes
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
    /// [`InflightHook::settle`] confirmed the recovery append landed. The mark records
    /// where the run's journal begins in a saved log, so a later fold can fingerprint the
    /// journal at those positions instead of trusting a count a same-sized retry journal
    /// would also satisfy. A fold that later finds the mark and the match cuts the journal
    /// without appending, so a settled turn is never folded twice. Nothing is recorded
    /// while the journal carries an earlier run's kept prefix: those messages are not in
    /// the log, so the mark cannot claim the whole journal landed.
    async fn mark_settled(&self, run: &JournalRun) -> std::io::Result<()> {
        if run.carried_bytes != 0 {
            return Ok(());
        }
        let bytes = self.store.read_log_async(&journal_log(&self.id)).await?;
        if bytes.is_empty() {
            return Ok(());
        }
        write_settled_mark(
            &self.store,
            &self.id,
            &FoldMark {
                log_len: run.input_len,
                journal_bytes: bytes.len() as u64,
                appended: 0,
            },
        )
        .await
    }

    async fn clear_record(&self, pad: &Scratchpad) {
        if let Err(e) = clear_run_record(&self.store, &self.id).await {
            self.warn(pad, "the run record could not be cleared", &e);
        }
    }

    /// `failed_with` is the number of messages the failed run committed, `None` on success.
    async fn settle(&self, pad: &Scratchpad, failed_with: Option<usize>) {
        let Some(run) = pad.get::<JournalRun>() else {
            // Nothing was journaled, so no settled mark will replace the run record; the
            // saved conversation (or the empty log) is the durable evidence now.
            self.clear_record(pad).await;
            return;
        };
        if run.journaled == usize::MAX {
            self.clear_record(pad).await;
            return;
        }
        if let Some(committed) = failed_with {
            // The engine did not save, so the messages are in the agent's log only if the
            // recovery hook's append landed. The settled mark is only valid once that is
            // confirmed; until then the run record stays, the run-tied evidence a later
            // fold needs. Counting, because rig-memory changes some messages on load and
            // equality is unreliable.
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
        if let Err(e) = self.mark_settled(&run).await {
            // The mark matters only when the cut below fails too, and that failure is warned.
            tracing::warn!("the settled mark could not be written: {}", chain(&e));
        }
        // The settled mark on disk now ties the journal to the saved log, so the run
        // record is done: kept, a later kill would fold the prompt again. The clear comes
        // only after the mark: a kill between rig's save and the mark must find the record
        // still there, and a kill after the mark finds the mark.
        self.clear_record(pad).await;
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
        if self.run_records {
            // Every attempt gets its own record, the retry's prompt included: the attempt
            // before it settled and cleared its record, and a kill before this attempt's
            // settled mark must still find run-tied evidence on disk.
            self.record_run(event.prompt, event.history.len()).await;
        }
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

    /// A store whose empty file writes fail: sidecar clears die, content writes land.
    struct ClearFailing {
        inner: MemoryStore,
    }

    impl AsyncStore for ClearFailing {
        async fn read_log_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_log_async(name).await
        }
        async fn append_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            self.inner.append_log_async(name, len, bytes).await
        }
        async fn replace_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            self.inner.replace_log_async(name, len, bytes).await
        }
        async fn read_file_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_file_async(name).await
        }
        async fn write_file_async(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            if bytes.is_empty() {
                return Err(std::io::Error::other("disk full"));
            }
            self.inner.write_file_async(name, bytes).await
        }
    }

    #[tokio::test]
    async fn a_cleanup_failure_does_not_strand_the_fallback_prompt() {
        let inner = MemoryStore::default();
        let store = ClearFailing {
            inner: inner.clone(),
        };
        // The fold's append lands; its sidecar cleanup fails. The error is reported, but
        // the only copy of the prompt is not stranded.
        let result =
            fold_unsettled(&store, "main", Unsettled::SessionEnded, fallback("go", 0)).await;
        assert!(result.is_err());
        let main = texts(&inner, "main").await;
        assert_eq!(
            main.len(),
            2,
            "the prompt survives the failed cleanup: {main:?}"
        );
        assert!(
            main[0].contains("go") && main[1].contains("session ended"),
            "{main:?}"
        );
        // The fold mark the fold wrote ahead of its append proves it landed: a retry on a
        // healthy store cuts and clears without a second append.
        let folded = fold_unsettled(&inner, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&inner, "main").await;
        assert_eq!(main.len(), 2, "no second append: {main:?}");
        assert!(read_run_record(&inner, "main").await.unwrap().is_none());
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
                appended: 4,
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
                appended: 4,
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
        save_main(
            &inner,
            &[
                user("go"),
                Arc::new(Message::assistant("calling")),
                user("tool result"),
                user("done"),
            ],
        )
        .await;
        hook.settle(&pad, None).await;
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
        // same-sized later journal: log_len 2 + 2 appended (the turn and its marker) == 4
        // held.
        cut_journal(&store, "main", 0).await.unwrap();
        write_fold_mark(
            &store,
            "main",
            &FoldMark {
                log_len: 2,
                journal_bytes,
                appended: 2,
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

    /// The state a failed attempt's recovery save and a lost settle leaves: the log holds
    /// the attempt's messages — exactly the journal's — and no mark was written.
    async fn saved_but_unmarked(store: &MemoryStore, log: &str) {
        let messages = [user("go"), user("a"), user("b")];
        append_messages(store, log, &messages).await.unwrap();
        append_messages(store, &journal_log(log), &messages)
            .await
            .unwrap();
    }

    /// The state a kill between a successful run's save and its settled mark leaves: the
    /// log holds the run's messages — the journal's, plus the final response a completion
    /// call never journals — the journal still holds the journal's messages, and the run
    /// record ties them to their positions.
    async fn saved_run_unmarked(store: &MemoryStore, log: &str) {
        let messages = [user("go"), user("a"), user("b")];
        append_messages(
            store,
            log,
            &[
                messages[0].clone(),
                messages[1].clone(),
                messages[2].clone(),
                user("done"),
            ],
        )
        .await
        .unwrap();
        append_messages(store, &journal_log(log), &messages)
            .await
            .unwrap();
        write_run_record(
            store,
            log,
            &RunRecord {
                prompt: Message::user("go"),
                log_len_before: 0,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_saved_run_is_cut_without_a_second_append() {
        let store = MemoryStore::default();
        // rig saved the run — the journal's messages plus the final response — and the
        // process died before the settle hook could record it.
        saved_run_unmarked(&store, "agent-1").await;
        let folded = fold_unsettled(&store, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "agent-1").await;
        assert_eq!(main.len(), 4, "no second append: {main:?}");
        assert!(
            main[3].contains("done") && main.iter().all(|m| !m.contains("Harness note")),
            "a run that saved its whole turn owes no marker: {main:?}"
        );
        assert!(journal(&store).await.is_empty());
        assert!(read_run_record(&store, "agent-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_landed_journal_still_owes_an_interrupted_run_its_marker() {
        let store = MemoryStore::default();
        saved_run_unmarked(&store, "agent-1").await;
        fold_unsettled(&store, "agent-1", Unsettled::User, fallback("go", 0))
            .await
            .unwrap();
        let main = texts(&store, "agent-1").await;
        assert_eq!(main.len(), 5, "the marker follows the saved turn: {main:?}");
        assert!(
            main[3].contains("done") && main[4].contains("interrupted by user"),
            "{main:?}"
        );
        assert_eq!(
            main.iter().filter(|m| m.contains("\"go\"")).count(),
            1,
            "{main:?}"
        );
        assert!(journal(&store).await.is_empty());
    }

    /// A repeated prompt can produce a journal identical to messages already ending `log`,
    /// from an earlier attempt of the same task. Without run-tied evidence — a run record
    /// or a valid mark — that content match proves nothing: the fold must keep the new
    /// turn, never cut it as already landed.
    #[tokio::test]
    async fn a_content_only_tail_match_without_run_evidence_folds_normally() {
        let store = MemoryStore::default();
        let identical = [
            user("go"),
            Arc::new(Message::assistant("calling")),
            user("tool result"),
        ];
        append_messages(&store, "main", &identical).await.unwrap();
        append_messages(&store, "main.inflight", &identical)
            .await
            .unwrap();
        let folded = fold_unsettled(&store, "main", Unsettled::User, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "main").await;
        assert_eq!(
            main.len(),
            7,
            "the new turn is kept, not discarded: {main:?}"
        );
        assert!(main[6].contains("interrupted by user"), "{main:?}");
        assert!(texts(&store, "main.inflight").await.is_empty());
    }

    #[tokio::test]
    async fn a_run_record_positions_the_journal_inside_the_saved_run() {
        let store = MemoryStore::default();
        // An earlier turn, then a run that saved its journal plus the final response. The
        // run record says the run began after the earlier turn.
        append_messages(&store, "main", &[user("earlier")])
            .await
            .unwrap();
        let messages = [user("go"), user("a"), user("b")];
        append_messages(
            &store,
            "main",
            &[
                messages[0].clone(),
                messages[1].clone(),
                messages[2].clone(),
                user("done"),
            ],
        )
        .await
        .unwrap();
        append_messages(&store, "main.inflight", &messages)
            .await
            .unwrap();
        write_run_record(
            &store,
            "main",
            &RunRecord {
                prompt: Message::user("go"),
                log_len_before: 1,
            },
        )
        .await
        .unwrap();
        fold_unsettled(&store, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 5, "no second append: {main:?}");
        assert!(main[0].contains("earlier"), "{main:?}");
        assert!(journal(&store).await.is_empty());
    }

    #[tokio::test]
    async fn a_run_record_with_a_mismatched_count_folds_as_usual() {
        let store = MemoryStore::default();
        saved_run_unmarked(&store, "agent-1").await;
        // The journal moved: what the record places at its positions is not the journal.
        write_run_record(
            &store,
            "agent-1",
            &RunRecord {
                prompt: Message::user("go"),
                log_len_before: 1,
            },
        )
        .await
        .unwrap();
        fold_unsettled(&store, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        let main = texts(&store, "agent-1").await;
        assert_eq!(
            main.len(),
            8,
            "a doubled rare-crash turn beats a lost one: {main:?}"
        );
        assert!(main[7].contains("session ended"), "{main:?}");
        assert!(journal(&store).await.is_empty());
    }

    /// What a kill before the run's first model call leaves: an empty journal, and the run
    /// record the surface wrote at run start. The fold reads the record as its fallback.
    #[tokio::test]
    async fn a_run_record_is_the_fallback_of_a_fold_after_a_kill() {
        let store = MemoryStore::default();
        append_messages(&store, "main", &[user("earlier")])
            .await
            .unwrap();
        write_run_record(
            &store,
            "main",
            &RunRecord {
                prompt: Message::user("go"),
                // The count predates a compaction that shrank the log: it is evidence of
                // the run, not of the log's length now.
                log_len_before: 2,
            },
        )
        .await
        .unwrap();
        let folded = fold_unsettled(&store, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "main").await;
        assert_eq!(main.len(), 3, "{main:?}");
        assert!(
            main[1].contains("go") && main[2].contains("session ended"),
            "{main:?}"
        );
        assert!(read_run_record(&store, "main").await.unwrap().is_none());
        // The record went with its fold: a later fold has nothing to repeat.
        let folded = fold_unsettled(&store, "main", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(!folded);
        assert_eq!(texts(&store, "main").await.len(), 3);
    }

    #[tokio::test]
    async fn a_settled_failed_attempt_still_gets_its_marker_when_the_cut_failed() {
        let store = MemoryStore::default();
        saved_but_unmarked(&store, "agent-1").await;
        // The settle wrote its mark, then the cut failed: journal and settled mark both stay.
        let journal_bytes = store
            .read_log_async("agent-1.inflight")
            .await
            .unwrap()
            .len() as u64;
        write_settled_mark(
            &store,
            "agent-1",
            &FoldMark {
                log_len: 0,
                journal_bytes,
                appended: 0,
            },
        )
        .await
        .unwrap();
        fold_unsettled(&store, "agent-1", Unsettled::User, fallback("go", 0))
            .await
            .unwrap();
        let main = texts(&store, "agent-1").await;
        assert_eq!(main.len(), 4, "{main:?}");
        assert_eq!(
            main.iter()
                .filter(|m| m.contains("interrupted by user"))
                .count(),
            1,
            "{main:?}"
        );
        assert_eq!(
            main.iter().filter(|m| m.contains("\"go\"")).count(),
            1,
            "{main:?}"
        );
        assert!(journal(&store).await.is_empty());
    }

    #[tokio::test]
    async fn a_marker_that_lost_its_cut_is_not_appended_again() {
        let inner = MemoryStore::default();
        saved_but_unmarked(&inner, "agent-1").await;
        let journal_bytes = inner
            .read_log_async("agent-1.inflight")
            .await
            .unwrap()
            .len() as u64;
        write_settled_mark(
            &inner,
            "agent-1",
            &FoldMark {
                log_len: 0,
                journal_bytes,
                appended: 0,
            },
        )
        .await
        .unwrap();
        // The fold appends the marker, then the cut fails.
        let store = Failing {
            inner: inner.clone(),
            appends: Vec::new(),
            replaces: vec!["agent-1.inflight"],
        };
        assert!(
            fold_unsettled(&store, "agent-1", Unsettled::User, fallback("go", 0))
                .await
                .is_err()
        );
        // The fold's retry finds the fold mark and only cuts: one marker, no second append.
        fold_unsettled(&inner, "agent-1", Unsettled::User, fallback("go", 0))
            .await
            .unwrap();
        let main = texts(&inner, "agent-1").await;
        assert_eq!(main.len(), 4, "{main:?}");
        assert_eq!(
            main.iter()
                .filter(|m| m.contains("interrupted by user"))
                .count(),
            1,
            "{main:?}"
        );
        assert!(journal(&inner).await.is_empty());
    }

    #[tokio::test]
    async fn a_root_hook_writes_a_run_record_and_its_settle_clears_it() {
        let store = Arc::new(MemoryStore::default());
        let warnings = Arc::new(Warnings::default());
        let hook = InflightHook::root(store.clone(), "main".into(), warnings);
        let pad = Scratchpad::default();
        hook.start(&pad, 1).await;
        // What on_run_start does for every attempt, the retry's prompt included.
        hook.record_run(&Message::user("go on"), 1).await;
        let record = read_run_record(&store, "main")
            .await
            .unwrap()
            .expect("the attempt's record");
        assert_eq!(record.log_len_before, 1);
        assert!(format!("{:?}", record.prompt).contains("go on"));
        two_calls(&hook, &pad, "go").await;
        hook.settle(&pad, None).await;
        assert!(journal(&store).await.is_empty());
        assert!(
            read_run_record(&store, "main").await.unwrap().is_none(),
            "the settled mark replaced the record, so it is cleared"
        );
    }

    #[tokio::test]
    async fn an_error_settle_whose_recovery_append_is_unconfirmed_keeps_the_run_record() {
        let store = Arc::new(MemoryStore::default());
        let (hook, warnings) = hook(store.clone());
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        // The record a root hook wrote at the attempt's start.
        hook.record_run(&Message::user("go"), 0).await;
        two_calls(&hook, &pad, "go").await;
        // The log never gained the failed attempt's messages: no settled mark may claim
        // the journal landed, and the record stays the run-tied evidence a later fold needs.
        hook.settle(&pad, Some(3)).await;
        assert!(
            read_run_record(&store, "agent-1").await.unwrap().is_some(),
            "without a settled mark the record must survive the settle"
        );
        assert_eq!(journal(&store).await.len(), 3);
        assert_eq!(warnings.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_retry_empties_a_kept_failed_attempt_already_in_the_log() {
        let inner = MemoryStore::default();
        let warnings = Arc::new(Warnings::default());
        let store = Failing {
            inner: inner.clone(),
            appends: Vec::new(),
            replaces: vec!["agent-1.inflight"],
        };
        let hook = InflightHook::new(store, "agent-1".into(), warnings.clone());
        let first = Scratchpad::default();
        hook.start(&first, 0).await;
        two_calls(&hook, &first, "go").await;
        // The recovery hook saved the failed attempt; the settle's cut failed, so the
        // journal and the settled mark both survive.
        save_main(
            &inner,
            &[
                user("go"),
                Arc::new(Message::assistant("calling")),
                user("tool result"),
            ],
        )
        .await;
        hook.settle(&first, Some(3)).await;
        assert_eq!(journal(&inner).await.len(), 3);

        // The retry starts against the same journal on disk and resolves the kept prefix.
        let retry = InflightHook::new(inner.clone(), "agent-1".into(), warnings);
        let second = Scratchpad::default();
        retry.start(&second, 0).await;
        assert!(
            journal(&inner).await.is_empty(),
            "the kept prefix is cut at run start"
        );
        match inner.read_file_async("agent-1.inflight.settled").await {
            Ok(bytes) => assert!(bytes.is_empty(), "{bytes:?}"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}"),
        }
        // The fold before the next turn appends nothing: the failed attempt is already in
        // the log, once.
        let folded = fold_unsettled(&inner, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(!folded);
        let main = texts(&inner, "agent-1").await;
        assert_eq!(main.len(), 3, "{main:?}");
        assert_eq!(
            main.iter().filter(|m| m.contains("\"go\"")).count(),
            1,
            "{main:?}"
        );
    }

    /// The state a failed run's save leaves when the process dies before the settled
    /// mark: `log` holds exactly the journal — the failed-run save writes no final
    /// response — and the run record ties the journal to its positions.
    #[tokio::test]
    async fn a_failed_run_saved_whole_is_cut_without_a_second_append() {
        let store = MemoryStore::default();
        let messages = [user("go"), user("a"), user("b")];
        append_messages(&store, "agent-1", &messages).await.unwrap();
        append_messages(&store, "agent-1.inflight", &messages)
            .await
            .unwrap();
        write_run_record(
            &store,
            "agent-1",
            &RunRecord {
                prompt: Message::user("go"),
                log_len_before: 0,
            },
        )
        .await
        .unwrap();
        let folded = fold_unsettled(&store, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "agent-1").await;
        assert_eq!(main.len(), 3, "no second append: {main:?}");
        assert!(
            main.iter().all(|m| !m.contains("Harness note")),
            "a failed run that saved its whole turn owes no marker: {main:?}"
        );
        assert!(journal(&store).await.is_empty());
        assert!(read_run_record(&store, "agent-1").await.unwrap().is_none());
    }

    /// A stale settled mark must not cut a retry's journal merely because its message
    /// count and byte length match: only the fingerprint at the mark's positions proves
    /// the landed journal is the one the mark guarded.
    #[tokio::test]
    async fn a_stale_settled_mark_does_not_cut_a_same_sized_retry_journal() {
        let store = MemoryStore::default();
        // The first attempt failed; its save landed and its journal was cut, but the
        // settled mark's clear was lost.
        let first = [user("go"), user("a"), user("b")];
        append_messages(&store, "agent-1", &first).await.unwrap();
        append_messages(&store, "agent-1.inflight", &first)
            .await
            .unwrap();
        cut_journal(&store, "agent-1", 0).await.unwrap();
        // The retry is interrupted with a same-sized journal; its message count fits the
        // stale mark's count check, so the mark is written with the retry journal's own
        // byte length, as a same-sized attempt's would be.
        let retry = [user("xy"), user("c"), user("d")];
        append_messages(&store, "agent-1.inflight", &retry)
            .await
            .unwrap();
        let journal_bytes = store
            .read_log_async("agent-1.inflight")
            .await
            .unwrap()
            .len() as u64;
        write_settled_mark(
            &store,
            "agent-1",
            &FoldMark {
                log_len: 0,
                journal_bytes,
                appended: 0,
            },
        )
        .await
        .unwrap();
        let folded = fold_unsettled(&store, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(folded);
        let main = texts(&store, "agent-1").await;
        assert_eq!(
            main.len(),
            7,
            "the retry's turn is folded, not cut: {main:?}"
        );
        assert!(
            main[3].contains("xy") && main[6].contains("session ended"),
            "{main:?}"
        );
        assert!(journal(&store).await.is_empty());
    }

    #[tokio::test]
    async fn run_start_clears_a_stale_settled_mark_on_an_empty_journal() {
        let store = Arc::new(MemoryStore::default());
        let (hook, warnings) = hook(store.clone());
        let pad = Scratchpad::default();
        write_settled_mark(
            &store,
            "agent-1",
            &FoldMark {
                log_len: 0,
                journal_bytes: 10,
                appended: 0,
            },
        )
        .await
        .unwrap();
        // The previous attempt's journal was cut; only its settled mark is left.
        hook.start(&pad, 0).await;
        match store.read_file_async("agent-1.inflight.settled").await {
            Ok(bytes) => assert!(bytes.is_empty(), "{bytes:?}"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}"),
        }
        assert!(warnings.0.lock().unwrap().is_empty());
    }

    /// A store whose empty writes to the run record alone fail: a settle's record clear
    /// dies while its mark write, cut, and mark clear land.
    struct RecordClearFailing {
        inner: MemoryStore,
    }

    impl AsyncStore for RecordClearFailing {
        async fn read_log_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_log_async(name).await
        }
        async fn append_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            self.inner.append_log_async(name, len, bytes).await
        }
        async fn replace_log_async(
            &self,
            name: &str,
            len: u64,
            bytes: &[u8],
        ) -> Result<(), rig_memory::LogWriteError> {
            self.inner.replace_log_async(name, len, bytes).await
        }
        async fn read_file_async(&self, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_file_async(name).await
        }
        async fn write_file_async(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            if bytes.is_empty() && name.ends_with(".run") {
                return Err(std::io::Error::other("disk full"));
            }
            self.inner.write_file_async(name, bytes).await
        }
    }

    /// A settle whose run-record clear fails leaves a stale record behind. The next fold
    /// reads it with an empty journal; the saved turn it records already landed, so no
    /// marker is owed and the record is cleared, not folded.
    #[tokio::test]
    async fn a_stale_run_record_after_a_settled_turn_owes_no_marker() {
        let inner = MemoryStore::default();
        let warnings = Arc::new(Warnings::default());
        let store = RecordClearFailing {
            inner: inner.clone(),
        };
        let hook = InflightHook::new(store, "agent-1".into(), warnings);
        let pad = Scratchpad::default();
        hook.start(&pad, 0).await;
        hook.record_run(&Message::user("go"), 0).await;
        two_calls(&hook, &pad, "go").await;
        // rig saved the run before the settle hook ran.
        save_main(
            &inner,
            &[
                user("go"),
                Arc::new(Message::assistant("calling")),
                user("tool result"),
                user("done"),
            ],
        )
        .await;
        hook.settle(&pad, None).await;
        assert!(journal(&inner).await.is_empty());
        assert!(
            read_run_record(&inner, "agent-1").await.unwrap().is_some(),
            "the lost clear keeps the record"
        );
        // The fold before the next run appends nothing and clears the record.
        let folded = fold_unsettled(&inner, "agent-1", Unsettled::SessionEnded, None)
            .await
            .unwrap();
        assert!(!folded);
        let main = texts(&inner, "agent-1").await;
        assert_eq!(main.len(), 4, "no marker after a settled turn: {main:?}");
        assert!(
            main.iter().all(|m| !m.contains("session ended")),
            "{main:?}"
        );
        assert!(read_run_record(&inner, "agent-1").await.unwrap().is_none());
    }
}
