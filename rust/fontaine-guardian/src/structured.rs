//! How a typed answer is obtained from a model: through a forced `submit` tool call, or, for a
//! model that has no tool support, through the provider's schema-constrained output. Both give
//! the same type through the same validation and the same retry loop, so a caller only differs in what it asks for.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use rig_agent::agent::{AgentBuilder, AgentHook, HookContext, OutcomeAction, OutcomeEvent};
use rig_agent::completion::{PromptError, StructuredOutputError};
use rig_agent::extractor::ExtractorBuilder;
use rig_core::DynModel;
use rig_core::completion::Usage;
use rig_core::error::ProviderError;
use rig_core::message::Message;
use rig_core::operation::Completion;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::OnceCell;

use crate::prompt_builder::CLOSING_LINE;

/// How long a probe may take before the call goes ahead on the tool path.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The transport a typed answer travels on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// A required `submit` tool call. The benchmarked path.
    #[default]
    Tool,
    /// The provider's schema-constrained output, with no tools in the request.
    Schema,
}

impl OutputMode {
    /// The `verdict_transport` name in the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            OutputMode::Tool => "tool",
            OutputMode::Schema => "schema",
        }
    }
}

/// What asking a model about its output capabilities came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    Mode(OutputMode),
    /// The model cannot generate text at all, e.g. an embedding model.
    Incapable(String),
    /// The question went unanswered. The call goes ahead on the tool path, and the next call
    /// asks again.
    Unreachable,
}

/// Where a guardian or scanner learns which transport its model can use.
pub trait OutputProbe: Send + Sync + std::fmt::Debug {
    fn probe(&self) -> BoxFuture<'_, ProbeOutcome>;
}

/// A probe with a known answer, for a model whose transport does not depend on the daemon.
#[derive(Debug)]
pub struct FixedProbe(pub OutputMode);

impl OutputProbe for FixedProbe {
    fn probe(&self) -> BoxFuture<'_, ProbeOutcome> {
        Box::pin(async move { ProbeOutcome::Mode(self.0) })
    }
}

/// What the first resolution of a model's output mode found worth telling the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeNotice {
    /// The model has no tool support, so answers travel as schema-constrained output.
    Schema,
    /// The model cannot generate text; why.
    Incapable(String),
}

/// Told about a [`ModeNotice`] once per resolver.
pub type Notifier = Arc<dyn Fn(ModeNotice) + Send + Sync>;

/// Learns once which transport a model can use and remembers it.
#[derive(Default)]
pub(crate) struct ModeResolver {
    probe: Option<Arc<dyn OutputProbe>>,
    /// Holds only an answer that will not change; an unanswered or incapable probe is asked
    /// again on the next call, so a daemon started later works without a restart.
    mode: OnceCell<OutputMode>,
    notifier: Option<Notifier>,
    incapable_told: AtomicBool,
}

impl ModeResolver {
    pub(crate) fn set_probe(&mut self, probe: Arc<dyn OutputProbe>) {
        self.probe = Some(probe);
    }

    pub(crate) fn set_notifier(&mut self, notifier: Notifier) {
        self.notifier = Some(notifier);
    }

    fn notify(&self, notice: ModeNotice) {
        if let Some(notifier) = &self.notifier {
            notifier(notice);
        }
    }

    /// `Err` is why the model cannot be asked at all. A probe that goes unanswered resolves to
    /// the tool path for this call only. Without a probe the answer is the tool path.
    pub(crate) async fn resolve(&self) -> Result<OutputMode, String> {
        let Some(probe) = &self.probe else {
            return Ok(OutputMode::Tool);
        };
        let resolved = self
            .mode
            .get_or_try_init(|| async {
                match tokio::time::timeout(PROBE_TIMEOUT, probe.probe()).await {
                    Ok(ProbeOutcome::Mode(mode)) => {
                        if mode == OutputMode::Schema {
                            self.notify(ModeNotice::Schema);
                        }
                        Ok(mode)
                    }
                    Ok(ProbeOutcome::Incapable(reason)) => Err(Some(reason)),
                    Ok(ProbeOutcome::Unreachable) | Err(_) => Err(None),
                }
            })
            .await;
        match resolved {
            Ok(mode) => Ok(*mode),
            Err(None) => Ok(OutputMode::Tool),
            Err(Some(reason)) => {
                if !self.incapable_told.swap(true, Ordering::Relaxed) {
                    self.notify(ModeNotice::Incapable(reason.clone()));
                }
                Err(reason.clone())
            }
        }
    }
}

/// rig's structured-output errors carry no usage, so every billed response is tallied as it
/// arrives; a fail-closed call, even one abandoned on a timeout, still spent those tokens.
#[derive(Clone, Default)]
pub(crate) struct Tally(Arc<Mutex<Usage>>);

impl Tally {
    pub(crate) fn total(&self) -> Usage {
        *self.0.lock().expect("the usage tally lock is not poisoned")
    }
}

impl AgentHook for Tally {
    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        if let Some(response) = event.completion() {
            *self.0.lock().expect("the usage tally lock is not poisoned") += response.usage;
        }
        OutcomeAction::Proceed
    }
}

pub(crate) struct Options<'a> {
    /// Attempts after the first, so a call is made at most `1 + retries` times.
    pub retries: usize,
    pub max_tokens: u64,
    pub additional_params: Option<&'a serde_json::Value>,
    /// Turns an answer that did not deserialize into a note for the retry saying why. It must
    /// not repeat the answer: that is model-authored text. `None` retries with the message
    /// unchanged.
    pub rejection: Option<fn(&str) -> String>,
}

fn retry_message(original: &str, note: &str) -> String {
    let section = format!(
        "## Your previous answer was rejected\n\n{note} Answer again, following the answer format.\n\n"
    );
    match original.rfind(CLOSING_LINE) {
        Some(at) => format!("{}{section}{}", &original[..at], &original[at..]),
        None => format!("{original}\n\n{section}"),
    }
}

/// Asks `model` for one `T`. In `Schema` mode `preamble` must already say what the answer looks
/// like, because no tool description does. What the call spent is added to `spent` as it goes,
/// so it is known even when the result never arrives.
///
/// The retry loop is here, not in rig, because rig resends the same prompt and drops the error:
/// the model never learns why its answer was rejected. Each retry carries only the latest
/// attempt's note, and only a rejected answer earns one; the error of the last attempt is what
/// comes back.
// rig's error type is large, and the callers match on it as it comes.
#[allow(clippy::result_large_err)]
pub(crate) async fn run<T>(
    model: &DynModel<Completion>,
    mode: OutputMode,
    preamble: &str,
    user_message: String,
    options: Options<'_>,
    spent: &Tally,
) -> Result<T, StructuredOutputError>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let mut message = user_message.clone();
    let mut attempt = 0;
    loop {
        let error = match attempt_once::<T>(model, mode, preamble, &message, &options, spent).await
        {
            Ok(output) => return Ok(output),
            Err(error) => error,
        };
        if attempt >= options.retries {
            return Err(error);
        }
        attempt += 1;
        tracing::warn!(attempt, error = %error, "the structured answer failed; retrying");
        message = match (&error, options.rejection) {
            (StructuredOutputError::Deserialization { output, .. }, Some(note)) => {
                retry_message(&user_message, &note(output))
            }
            _ => user_message.clone(),
        };
    }
}

#[allow(clippy::result_large_err)]
async fn attempt_once<T>(
    model: &DynModel<Completion>,
    mode: OutputMode,
    preamble: &str,
    user_message: &str,
    options: &Options<'_>,
    spent: &Tally,
) -> Result<T, StructuredOutputError>
where
    T: JsonSchema + DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let message = Message::user(user_message);
    match mode {
        OutputMode::Tool => {
            let mut builder = ExtractorBuilder::<T>::new(model.clone())
                .add_hook(spent.clone())
                .append_preamble(preamble)
                .max_tokens(options.max_tokens)
                .retries(0);
            if let Some(params) = options.additional_params {
                builder = builder.additional_params(params.clone());
            }
            builder
                .build()
                .extract(message)
                .await
                .map(|response| response.output)
        }
        OutputMode::Schema => {
            let mut builder = AgentBuilder::new(model.clone())
                .add_hook(spent.clone())
                .preamble(preamble)
                .max_tokens(options.max_tokens);
            if let Some(params) = options.additional_params {
                builder = builder.additional_params(params.clone());
            }
            builder
                .build()
                .prompt_typed::<T>(message)
                .max_turns(1)
                .retries(0)
                .await
                .map(|response| response.output)
        }
    }
}

/// Whether a failed run is the provider refusing a request because the model has no tool
/// support. Read from the typed response (status and body), not from rendered error text.
pub(crate) fn lacks_tool_support(error: &PromptError) -> bool {
    const REFUSAL: &str = "does not support tools";
    match error {
        PromptError::Provider(ProviderError::ProviderResponse(response)) => {
            response.status.is_some_and(|s| s.as_u16() == 400) && response.body.contains(REFUSAL)
        }
        PromptError::Report(report) => {
            report.http_status == Some(400)
                && report
                    .provider_response
                    .as_ref()
                    .is_some_and(|response| response.body.contains(REFUSAL))
        }
        _ => false,
    }
}
