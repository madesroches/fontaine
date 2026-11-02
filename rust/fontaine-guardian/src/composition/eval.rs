//! The state one evaluation owns: the shared LLM request and the reads. The root owns this, not
//! the script's task, so what an aborted evaluation spent can still be read afterwards.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use rig_core::completion::Usage;
use tokio::task::AbortHandle;
use tracing::Instrument;

use super::Composition;
use crate::guardian::{Adjudication, Guardian};
use crate::request::AdjudicationRequest;
use crate::screen::{Prompt, Question, Read};

/// What one read came to.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadRecord {
    pub name: String,
    /// The model that answered: the id the server says it ran when it says, else the label.
    pub model: String,
    pub result: Result<f64, String>,
    pub latency_ms: u64,
    /// When the read finished, counted from the start of the evaluation.
    pub finished_ms: u64,
    /// What the request cost. Reads that share one request share its cost, which only the first of
    /// them carries, so the sum over a call's records counts it once.
    pub usage: Option<Usage>,
}

/// What one request to a model came to, shared by every read that asks the same thing.
struct Answered {
    result: Result<f64, String>,
    model: String,
    usage: Option<Usage>,
    latency_ms: u64,
    finished_ms: u64,
}

type RequestKey = (String, Prompt, Question);
type SharedRead = Shared<BoxFuture<'static, Arc<ReadRecord>>>;
type SharedRequest = Shared<BoxFuture<'static, Arc<Answered>>>;
type SharedLlm = Shared<BoxFuture<'static, Arc<Adjudication>>>;

struct Slot {
    name: String,
    done: SharedRead,
}

struct Request {
    done: SharedRequest,
    abort: AbortHandle,
}

pub(crate) struct EvalCtx {
    composition: Arc<Composition>,
    request: Arc<AdjudicationRequest>,
    started: Instant,
    llm: SharedLlm,
    llm_started: Arc<AtomicBool>,
    reads: Mutex<ReadTable>,
}

#[derive(Default)]
struct ReadTable {
    slots: Vec<Slot>,
    // Reads that ask the same model the same question send one request: the answer is the model's,
    // however many names the composition gives it.
    requests: HashMap<RequestKey, Request>,
    // Set under the table's lock so a read the script starts after the abort never spawns.
    aborted: bool,
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
        let mut table = self.reads.lock().expect("the read table is not poisoned");
        if let Some(slot) = table.slots.iter().find(|slot| slot.name == name) {
            return Ok(slot.done.clone());
        }
        if table.aborted {
            let record = Arc::new(ReadRecord {
                name: name.to_owned(),
                model: read.model.clone(),
                result: Err("the read was cancelled".to_owned()),
                latency_ms: 0,
                finished_ms: 0,
                usage: None,
            });
            let done = futures::future::ready(record).boxed().shared();
            table.slots.push(Slot {
                name: name.to_owned(),
                done: done.clone(),
            });
            return Ok(done);
        }
        let key = (read.model.clone(), read.prompt, read.question);
        let first = !table.requests.contains_key(&key);
        if first {
            let request = self.send(read);
            table.requests.insert(key.clone(), request);
        }
        let request = table.requests[&key].done.clone();
        let record_name = name.to_owned();
        let done = async move {
            let answered = request.await;
            Arc::new(ReadRecord {
                name: record_name,
                model: answered.model.clone(),
                result: answered.result.clone(),
                latency_ms: answered.latency_ms,
                finished_ms: answered.finished_ms,
                usage: if first { answered.usage } else { None },
            })
        }
        .boxed()
        .shared();
        table.slots.push(Slot {
            name: name.to_owned(),
            done: done.clone(),
        });
        Ok(done)
    }

    fn send(&self, read: &Read) -> Request {
        let reader = self.composition.reader(&read.model);
        let request = self.request.clone();
        let started = self.started;
        let label = read.model.clone();
        let sent = read.clone();
        let task = tokio::spawn(
            async move {
                let began = Instant::now();
                let outcome = match reader {
                    Some(reader) => reader.read_detailed(&sent, &request).await,
                    None => Err(format!("no reader is configured for {}", sent.model)),
                };
                let latency_ms = began.elapsed().as_millis() as u64;
                let finished_ms = started.elapsed().as_millis() as u64;
                match outcome {
                    Ok(outcome) => Answered {
                        result: Ok(outcome.risk),
                        model: outcome.resolved_model.unwrap_or(sent.model),
                        usage: outcome.usage,
                        latency_ms,
                        finished_ms,
                    },
                    Err(reason) => Answered {
                        result: Err(reason),
                        model: sent.model,
                        usage: None,
                        latency_ms,
                        finished_ms,
                    },
                }
            }
            .in_current_span(),
        );
        let abort = task.abort_handle();
        let done = async move {
            match task.await {
                Ok(answered) => Arc::new(answered),
                Err(error) => Arc::new(Answered {
                    result: Err(format!("the read did not finish: {error}")),
                    model: label,
                    usage: None,
                    latency_ms: 0,
                    finished_ms: 0,
                }),
            }
        }
        .boxed()
        .shared();
        Request { done, abort }
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
            .slots
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
        let mut table = self.reads.lock().expect("the read table is not poisoned");
        table.aborted = true;
        for request in table.requests.values() {
            request.abort.abort();
        }
    }
}
