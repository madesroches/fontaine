//! The state one evaluation owns: the shared LLM request and the reads. The root owns this, not
//! the script's task, so what an aborted evaluation spent can still be read afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use tokio::task::AbortHandle;
use tracing::Instrument;

use super::Composition;
use crate::guardian::{Adjudication, Guardian};
use crate::request::AdjudicationRequest;

/// What one read came to.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadRecord {
    pub name: String,
    /// The label of the model that answered.
    pub model: String,
    pub result: Result<f64, String>,
    pub latency_ms: u64,
    /// When the read finished, counted from the start of the evaluation.
    pub finished_ms: u64,
}

type SharedRead = Shared<BoxFuture<'static, Arc<ReadRecord>>>;
type SharedLlm = Shared<BoxFuture<'static, Arc<Adjudication>>>;

struct Slot {
    name: String,
    done: SharedRead,
    abort: AbortHandle,
}

pub(crate) struct EvalCtx {
    composition: Arc<Composition>,
    request: Arc<AdjudicationRequest>,
    started: Instant,
    llm: SharedLlm,
    llm_started: Arc<AtomicBool>,
    reads: Mutex<Vec<Slot>>,
}

impl EvalCtx {
    pub(crate) fn new(
        composition: Arc<Composition>,
        guardian: Arc<Guardian>,
        request: Arc<AdjudicationRequest>,
    ) -> Arc<EvalCtx> {
        let llm_started = Arc::new(AtomicBool::new(false));
        let llm = {
            let started = llm_started.clone();
            let request = request.clone();
            async move {
                started.store(true, Ordering::SeqCst);
                Arc::new(guardian.adjudicate(&request).await)
            }
            .boxed()
            .shared()
        };
        Arc::new(EvalCtx {
            composition,
            request,
            started: Instant::now(),
            llm,
            llm_started,
            reads: Mutex::default(),
        })
    }

    pub(crate) fn request(&self) -> &AdjudicationRequest {
        &self.request
    }

    /// The one LLM request of this evaluation; every caller shares it.
    pub(crate) fn llm(&self) -> SharedLlm {
        self.llm.clone()
    }

    /// The LLM verdict, once it is in hand.
    pub(crate) fn llm_result(&self) -> Option<Arc<Adjudication>> {
        self.llm.peek().cloned()
    }

    /// Whether the LLM request was sent and has not answered.
    pub(crate) fn llm_in_flight(&self) -> bool {
        self.llm_started.load(Ordering::SeqCst) && self.llm.peek().is_none()
    }

    /// Starts the read `name` unless it already runs, and returns the shared handle to its record.
    /// An undeclared name is an error.
    pub(crate) fn start_read(&self, name: &str) -> Result<SharedRead, String> {
        let Some(read) = self.composition.read(name) else {
            return Err(format!("`{name}` is not a declared read"));
        };
        let mut slots = self.reads.lock().expect("the read table is not poisoned");
        if let Some(slot) = slots.iter().find(|slot| slot.name == name) {
            return Ok(slot.done.clone());
        }
        let reader = self.composition.reader(&read.model);
        let request = self.request.clone();
        let started = self.started;
        let record_name = name.to_owned();
        let read = read.clone();
        let task = tokio::spawn(
            async move {
                let began = Instant::now();
                let result = match reader {
                    Some(reader) => reader.read(&read, &request).await,
                    None => Err(format!("no reader is configured for {}", read.model)),
                };
                ReadRecord {
                    name: record_name,
                    model: read.model,
                    result,
                    latency_ms: began.elapsed().as_millis() as u64,
                    finished_ms: started.elapsed().as_millis() as u64,
                }
            }
            .in_current_span(),
        );
        let abort = task.abort_handle();
        let failed = ReadRecord {
            name: name.to_owned(),
            model: self
                .composition
                .read(name)
                .map(|read| read.model.clone())
                .unwrap_or_default(),
            result: Err("the read was cancelled".to_owned()),
            latency_ms: 0,
            finished_ms: 0,
        };
        let done = async move {
            match task.await {
                Ok(record) => Arc::new(record),
                Err(error) => Arc::new(ReadRecord {
                    result: Err(format!("the read did not finish: {error}")),
                    ..failed
                }),
            }
        }
        .boxed()
        .shared();
        slots.push(Slot {
            name: name.to_owned(),
            done: done.clone(),
            abort,
        });
        Ok(done)
    }

    /// Starts every declared read.
    pub(crate) fn start_all_reads(&self) {
        for name in self.composition.read_names() {
            // Declared names always start.
            let _ = self.start_read(name);
        }
    }

    /// Waits for every started read, in the order they started.
    pub(crate) async fn finish_reads(&self) -> Vec<ReadRecord> {
        let pending: Vec<SharedRead> = self
            .reads
            .lock()
            .expect("the read table is not poisoned")
            .iter()
            .map(|slot| slot.done.clone())
            .collect();
        let mut records = Vec::with_capacity(pending.len());
        for done in pending {
            records.push((*done.await).clone());
        }
        records
    }

    /// Cancels every read still running.
    pub(crate) fn abort_reads(&self) {
        for slot in self
            .reads
            .lock()
            .expect("the read table is not poisoned")
            .iter()
        {
            slot.abort.abort();
        }
    }
}
