//! The guardian as a Rune script. The script is the composition; the host supplies the leaves (the
//! LLM guardian, logprob reads) and mints every verdict, and the root here wraps the script so that
//! whatever it does, an unreadable outcome is a denial.

mod eval;
mod host;
mod preset;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use rig_core::completion::Usage;
use rune::runtime::{RuntimeContext, ToValue, Value, budget};
use rune::termcolor::NoColor;
use rune::{Context, Diagnostics, Source, Sources, Unit, Vm};
use tracing::Instrument;

pub use eval::ReadRecord;
pub use preset::{NAMES as PRESETS, SCREEN};

use eval::EvalCtx;
use host::{Call, VerdictKind, verdict_of};

use crate::guardian::{Adjudication, Guardian};
use crate::request::AdjudicationRequest;
use crate::screen::{Read, Reader, check_pairing};
use crate::verdict::{Decision, FailClosed};

const TARGET: &str = "fontaine::telemetry";

/// The most instructions one evaluation may run.
pub const INSTRUCTION_BUDGET: usize = 1_000_000;
/// Added to the guardian and read timeouts to bound a whole evaluation.
pub const COMPOSITION_SLACK: Duration = Duration::from_secs(2);
/// The bound of a read when no reader says otherwise.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// A composition that cannot be built, or a parameter that is not valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionError {
    /// The parameter or read at fault, when one is.
    pub key: Option<String>,
    pub message: String,
}

impl CompositionError {
    fn new(message: impl Into<String>) -> Self {
        CompositionError {
            key: None,
            message: message.into(),
        }
    }

    fn at(key: &str, message: impl Into<String>) -> Self {
        CompositionError {
            key: Some(key.to_owned()),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.key {
            Some(key) => write!(f, "{key}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for CompositionError {}

/// A value of the `params` object a script receives.
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Number(f64),
    Str(String),
    Bool(bool),
    /// An optional number a script matches on: `Some(n)` or `None`.
    Optional(Option<f64>),
    /// A parameter that is not set; a script sees `None`.
    Unset,
}

impl ToValue for Param {
    fn to_value(self) -> Result<Value, rune::runtime::RuntimeError> {
        match self {
            Param::Number(n) => n.to_value(),
            Param::Str(s) => s.to_value(),
            Param::Bool(b) => b.to_value(),
            Param::Optional(n) => n.to_value(),
            Param::Unset => Option::<bool>::None.to_value(),
        }
    }
}

/// Checks a preset's parameters alone, whatever model it reads on and whether or not it is in
/// shadow mode.
pub fn check_preset_params(
    name: &str,
    params: &BTreeMap<String, Param>,
) -> Result<(), CompositionError> {
    preset::resolve(name, "", params, true).map(|_| ())
}

/// Where a composition's script comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Script {
    /// A built-in preset by name.
    Preset(String),
    /// Rune source.
    Source(String),
}

#[derive(Clone, Debug)]
pub struct CompositionSpec {
    pub script: Script,
    /// The label of the default read model: a preset reads on it.
    pub model: String,
    /// The reads a script declares; a preset declares its own.
    pub reads: BTreeMap<String, Read>,
    /// Preset parameters, or the values passed to a script.
    pub params: BTreeMap<String, Param>,
    pub shadow: bool,
}

impl CompositionSpec {
    /// The reads this composition declares.
    pub fn declared_reads(&self) -> Result<BTreeMap<String, Read>, CompositionError> {
        match &self.script {
            Script::Preset(name) => {
                if !self.reads.is_empty() {
                    return Err(CompositionError::new(
                        "a preset declares its own reads; `reads` is for scripts",
                    ));
                }
                preset::declared_reads(name, &self.model, &self.params)
            }
            Script::Source(_) => Ok(self.reads.clone()),
        }
    }

    /// The labels of the models the reads use.
    pub fn read_models(&self) -> Result<BTreeSet<String>, CompositionError> {
        Ok(self
            .declared_reads()?
            .into_values()
            .map(|read| read.model)
            .collect())
    }
}

pub struct Composition {
    unit: Arc<Unit>,
    runtime: Arc<RuntimeContext>,
    reads: BTreeMap<String, Read>,
    readers: HashMap<String, Arc<Reader>>,
    params: HashMap<String, Param>,
    shadow: bool,
}

impl Composition {
    /// Builds the composition: resolves the preset, compiles the script against the host module
    /// with its link checks on, and requires a `guardian` entry point of two parameters.
    /// `readers` are keyed by the model label a read names.
    pub fn compile(
        spec: CompositionSpec,
        readers: Vec<(String, Reader)>,
    ) -> Result<Composition, CompositionError> {
        let (source, reads, params) = match &spec.script {
            Script::Preset(name) => {
                if !spec.reads.is_empty() {
                    return Err(CompositionError::new(
                        "a preset declares its own reads; `reads` is for scripts",
                    ));
                }
                let resolved = preset::resolve(name, &spec.model, &spec.params, spec.shadow)?;
                (resolved.source.to_owned(), resolved.reads, resolved.params)
            }
            Script::Source(source) => (
                source.clone(),
                spec.reads.clone(),
                spec.params.clone().into_iter().collect(),
            ),
        };
        for (name, read) in &reads {
            check_pairing(read.prompt, read.question)
                .map_err(|message| CompositionError::at(name, message))?;
        }
        let (unit, runtime) = build(&source)?;
        Ok(Composition {
            unit,
            runtime,
            reads,
            readers: readers
                .into_iter()
                .map(|(label, reader)| (label, Arc::new(reader)))
                .collect(),
            params,
            shadow: spec.shadow,
        })
    }

    pub fn shadow(&self) -> bool {
        self.shadow
    }

    /// The readers, for warming up.
    pub fn readers(&self) -> impl Iterator<Item = (&str, &Reader)> {
        self.readers
            .iter()
            .map(|(label, reader)| (label.as_str(), reader.as_ref()))
    }

    /// Loads every reader's model into its daemon, together; each result is keyed by the label.
    pub async fn warm_up(&self) -> Vec<(String, Result<(), String>)> {
        futures::future::join_all(
            self.readers
                .iter()
                .map(|(label, reader)| async move { (label.clone(), reader.warm_up().await) }),
        )
        .await
    }

    /// The distinct model labels of the declared reads, sorted.
    pub fn models(&self) -> Vec<String> {
        self.reads
            .values()
            .map(|read| read.model.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn read(&self, name: &str) -> Option<&Read> {
        self.reads.get(name)
    }

    pub(crate) fn read_names(&self) -> impl Iterator<Item = &str> {
        self.reads.keys().map(String::as_str)
    }

    pub(crate) fn reader(&self, label: &str) -> Option<Arc<Reader>> {
        self.readers.get(label).cloned()
    }

    /// Each read's bound is its reader's; the longest bounds a stage of reads.
    fn read_timeout(&self) -> Duration {
        self.readers
            .values()
            .map(|reader| reader.settings().timeout)
            .max()
            .unwrap_or(DEFAULT_READ_TIMEOUT)
    }
}

fn build(source: &str) -> Result<(Arc<Unit>, Arc<RuntimeContext>), CompositionError> {
    let fail = |what: &str, error: &dyn std::fmt::Display| {
        CompositionError::new(format!("{what}: {error}"))
    };
    // Without stdio, a script cannot write to the terminal the harness owns.
    let mut context =
        Context::with_config(false).map_err(|e| fail("building the script context", &e))?;
    context
        .install(host::module().map_err(|e| fail("building the host module", &e))?)
        .map_err(|e| fail("installing the host module", &e))?;
    let mut sources = Sources::new();
    sources
        .insert(Source::memory(source).map_err(|e| fail("reading the script", &e))?)
        .map_err(|e| fail("reading the script", &e))?;
    let mut diagnostics = Diagnostics::new();
    let built = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build();
    let unit = match built {
        Ok(unit) if !diagnostics.has_error() => unit,
        _ => {
            let mut text = NoColor::new(Vec::new());
            let rendered = diagnostics.emit(&mut text, &sources).map(|()| {
                String::from_utf8_lossy(&text.into_inner())
                    .trim()
                    .to_owned()
            });
            return Err(CompositionError::new(format!(
                "the script does not compile: {}",
                rendered.unwrap_or_else(|e| e.to_string())
            )));
        }
    };
    let takes_two = unit.debug_info().is_some_and(|debug| {
        debug.functions.values().any(|signature| {
            signature.path.to_string() == "guardian"
                && match &signature.args {
                    rune::runtime::debug::DebugArgs::TupleArgs(n) => *n == 2,
                    rune::runtime::debug::DebugArgs::Named(names) => names.len() == 2,
                    rune::runtime::debug::DebugArgs::EmptyArgs => false,
                }
        })
    });
    if !takes_two {
        return Err(CompositionError::new(
            "the script must define `pub async fn guardian(call, params)`",
        ));
    }
    let runtime = context
        .runtime()
        .map_err(|e| fail("building the script runtime", &e))?;
    Ok((Arc::new(unit), Arc::new(runtime)))
}

/// What the script came to, as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stop,
    Deny,
    Allow,
    /// The LLM guardian decided, or the screen left the call to it.
    Pass,
    /// The script failed or reached no verdict.
    Failed(String),
}

#[derive(Debug)]
pub enum Decided {
    /// The LLM decided (also every shadow-mode call).
    Guardian(Box<Adjudication>),
    Escalate {
        node: String,
        reason: String,
    },
    Deny {
        node: String,
        reason: String,
        /// Set when the composition failed rather than chose to deny.
        fail_closed: Option<FailClosed>,
    },
    Allow {
        node: String,
        score: f64,
        threshold: f64,
    },
}

#[derive(Debug)]
pub struct Composed {
    pub decided: Decided,
    pub reads: Vec<ReadRecord>,
    pub outcome: Outcome,
    /// An estimate of the input tokens of the guardian request that was dropped unanswered, whose
    /// request has usually been sent and billed by then.
    pub dropped_input_tokens: Option<u64>,
    /// A completed LLM request the final verdict did not use.
    pub llm_usage: Option<Usage>,
}

impl Composed {
    /// The score of the read called `name`, when it succeeded.
    pub fn score(&self, name: &str) -> Option<f64> {
        self.reads
            .iter()
            .find(|read| read.name == name)
            .and_then(|read| read.result.as_ref().ok().copied())
    }

    /// Whether a read or the script failed.
    pub fn unavailable(&self) -> bool {
        matches!(self.outcome, Outcome::Failed(_)) || self.reads.iter().any(|r| r.result.is_err())
    }

    /// The `screen_outcome` of the audit record.
    pub fn outcome_name(&self) -> &'static str {
        if self.unavailable() {
            return "unavailable";
        }
        match self.outcome {
            Outcome::Stop => "stop",
            Outcome::Deny => "deny",
            Outcome::Allow => "allow",
            Outcome::Pass => "pass",
            Outcome::Failed(_) => "unavailable",
        }
    }

    /// From the start of the evaluation to the last read finishing.
    pub fn reads_latency_ms(&self) -> u64 {
        self.reads.iter().map(|r| r.finished_ms).max().unwrap_or(0)
    }

    /// Why a read or the script failed, for the one warning a session shows.
    pub fn unavailable_reason(&self) -> Option<String> {
        if let Outcome::Failed(reason) = &self.outcome {
            return Some(reason.clone());
        }
        self.reads.iter().find_map(|read| {
            read.result
                .as_ref()
                .err()
                .map(|reason| format!("the {} read failed: {reason}", read.name))
        })
    }

    /// The distinct models the reads used, comma-joined.
    pub fn models(&self) -> String {
        let mut models: Vec<&str> = Vec::new();
        for read in &self.reads {
            if !models.contains(&read.model.as_str()) {
                models.push(&read.model);
            }
        }
        models.join(",")
    }
}

enum Ran {
    Verdict(VerdictKind),
    Failed(FailClosed, String),
}

/// Evaluates the composition for one call. Every outcome that is not a verdict the host minted is a
/// denial decided here.
pub async fn adjudicate(
    composition: Arc<Composition>,
    guardian: Arc<Guardian>,
    request: Arc<AdjudicationRequest>,
) -> Composed {
    let span = tracing::info_span!(
        target: TARGET,
        "fontaine.guardian.screen",
        screen_model = composition.models().join(",").as_str(),
        risk = tracing::field::Empty,
        allow_risk = tracing::field::Empty,
        steering = tracing::field::Empty,
        outcome = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
    );
    let composed = run(composition, guardian, request)
        .instrument(span.clone())
        .await;
    span.record("outcome", composed.outcome_name());
    span.record("latency_ms", composed.reads_latency_ms());
    for (field, name) in [
        ("risk", "stop"),
        ("allow_risk", "allow"),
        ("steering", "steering"),
    ] {
        if let Some(score) = composed.score(name) {
            span.record(field, score);
        }
    }
    composed
}

async fn run_script(
    composition: Arc<Composition>,
    ctx: Arc<EvalCtx>,
) -> Result<VerdictKind, String> {
    let vm = Vm::new(composition.runtime.clone(), composition.unit.clone());
    let args = (Call { ctx }, composition.params.clone());
    let execution = vm
        .send_execute(["guardian"], args)
        .map_err(|e| format!("starting the composition failed: {e}"))?;
    let value = budget::with(INSTRUCTION_BUDGET, execution.async_complete())
        .await
        .into_result()
        .map_err(|e| format!("the composition failed: {e}"))?;
    verdict_of(&value)
}

async fn run(
    composition: Arc<Composition>,
    guardian: Arc<Guardian>,
    request: Arc<AdjudicationRequest>,
) -> Composed {
    // The probe runs outside the guardian's timeout, and concurrent callers wait for it serially,
    // so the bound below starts only once it has answered. An error here is the LLM's own
    // fail-closed denial to report.
    let _ = guardian.resolve_mode().await;
    let shadow = composition.shadow;
    let bound = guardian.settings().timeout + composition.read_timeout() + COMPOSITION_SLACK;
    let ctx = EvalCtx::new(composition.clone(), guardian.clone(), request.clone());
    if shadow {
        ctx.start_all_reads();
    }
    let task = tokio::spawn(run_script(composition, ctx.clone()).in_current_span());
    let abort = task.abort_handle();
    let evaluation = async {
        match tokio::time::timeout(bound, task).await {
            Ok(Ok(Ok(kind))) => Ran::Verdict(kind),
            Ok(Ok(Err(message))) => Ran::Failed(FailClosed::Error, message),
            Ok(Err(error)) => Ran::Failed(
                FailClosed::Error,
                if error.is_panic() {
                    "the composition panicked".to_owned()
                } else {
                    format!("the composition did not finish: {error}")
                },
            ),
            Err(_) => {
                abort.abort();
                ctx.abort_reads();
                Ran::Failed(
                    FailClosed::Timeout,
                    format!(
                        "the composition did not finish within {} seconds",
                        bound.as_secs()
                    ),
                )
            }
        }
    };
    let ran = if shadow {
        // The LLM request is the guardian's own in shadow mode: it runs whatever the script does.
        futures::future::join(evaluation, ctx.llm()).await.0
    } else {
        evaluation.await
    };
    let reads = ctx.finish_reads().await;

    let held = ctx.llm_result();
    let held_refusal = held
        .as_ref()
        .filter(|adjudication| adjudication.decision != Decision::Allow);
    let outcome = match &ran {
        Ran::Verdict(VerdictKind::Llm(_)) => Outcome::Pass,
        Ran::Verdict(VerdictKind::Escalate { .. }) => Outcome::Stop,
        Ran::Verdict(VerdictKind::Deny { .. }) => Outcome::Deny,
        Ran::Verdict(VerdictKind::Allow { .. }) => Outcome::Allow,
        Ran::Verdict(VerdictKind::Abstain) => {
            Outcome::Failed("the composition reached no verdict".to_owned())
        }
        Ran::Failed(_, reason) => Outcome::Failed(reason.clone()),
    };
    let guardian_decided = |adjudication: &Arc<Adjudication>| {
        Decided::Guardian(Box::new(Adjudication::clone(adjudication)))
    };
    let decided = if shadow {
        let adjudication = held
            .as_ref()
            .expect("shadow mode joins the LLM request, which has answered");
        guardian_decided(adjudication)
    } else if let Some(adjudication) = held_refusal {
        // A refusal in hand is never overridden, so a computed denial stays non-approvable and
        // still reaches the breaker.
        guardian_decided(adjudication)
    } else {
        match ran {
            Ran::Verdict(VerdictKind::Llm(adjudication)) => guardian_decided(&adjudication),
            Ran::Verdict(VerdictKind::Escalate { node, reason }) => {
                Decided::Escalate { node, reason }
            }
            Ran::Verdict(VerdictKind::Deny { node, reason }) => Decided::Deny {
                node,
                reason,
                fail_closed: None,
            },
            Ran::Verdict(VerdictKind::Allow {
                node,
                score,
                threshold,
            }) => match &held {
                Some(adjudication) => guardian_decided(adjudication),
                None => Decided::Allow {
                    node,
                    score,
                    threshold,
                },
            },
            Ran::Verdict(VerdictKind::Abstain) => Decided::Deny {
                node: "composition".to_owned(),
                reason: "the composition reached no verdict".to_owned(),
                fail_closed: Some(FailClosed::Unparseable),
            },
            Ran::Failed(kind, reason) => Decided::Deny {
                node: "composition".to_owned(),
                reason,
                fail_closed: Some(kind),
            },
        }
    };
    let dropped_input_tokens = (ctx.llm_in_flight() && guardian.unavailable_reason().is_none())
        .then(|| guardian.dropped_estimate(&request));
    let llm_usage = match (&decided, &held) {
        (Decided::Guardian(_), _) | (_, None) => None,
        (_, Some(adjudication)) => Some(adjudication.usage),
    };
    Composed {
        decided,
        reads,
        outcome,
        dropped_input_tokens,
        llm_usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardian::Settings;
    use crate::request::SessionFacts;
    use crate::rules::{EnvironmentConfig, RuleConfig};

    #[tokio::test]
    async fn a_panic_in_a_host_function_is_a_deny() {
        let composition = Composition::compile(
            CompositionSpec {
                script: Script::Source(
                    "pub async fn guardian(call, params) { host_panic(); abstain() }".into(),
                ),
                model: "m".into(),
                reads: BTreeMap::new(),
                params: BTreeMap::new(),
                shadow: false,
            },
            vec![],
        )
        .unwrap();
        let guardian = Guardian::unavailable("mock/none", "no model", Settings::default());
        let request = AdjudicationRequest {
            task_intent: "fix the build".into(),
            recent_user_messages: vec![],
            project_instructions: None,
            environment: EnvironmentConfig::default(),
            rules: RuleConfig::default(),
            tool_call: crate::request::ToolCallSummary {
                tool: "shell".into(),
                args: serde_json::json!({}),
                args_digest: "d".into(),
                destructive: false,
                annotations: None,
            },
            session_facts: SessionFacts::default(),
            hook_context: Vec::new(),
            prior_denials: Vec::new(),
        };
        let composed =
            adjudicate(Arc::new(composition), Arc::new(guardian), Arc::new(request)).await;
        assert!(matches!(
            composed.decided,
            Decided::Deny { fail_closed: Some(FailClosed::Error), ref reason, .. } if reason.contains("panicked")
        ));
        assert!(matches!(composed.outcome, Outcome::Failed(_)));
    }
}
