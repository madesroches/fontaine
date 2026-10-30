//! One stateless, tool-less model conversation per adjudication. Every outcome that is not a
//! verdict the model returned is a denial decided here, by code.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rig_agent::completion::StructuredOutputError;
use rig_core::DynModel;
use rig_core::completion::Usage;
use rig_core::operation::Completion;
use tracing::Instrument;

use crate::cache::{CacheKey, VerdictCache, digest};
use crate::profile::Profile;
use crate::prompt_builder::{Rendered, render, with_schema_answer};
use crate::request::AdjudicationRequest;
use crate::structured::{
    self, ModeResolver, Notifier, Options, OutputMode, OutputProbe, Tally, lacks_tool_support,
};
use crate::verdict::{AuditLabel, Decision, FailClosed, Verdict};

const TARGET: &str = "fontaine::telemetry";

/// Retries after a malformed answer or a provider error, per adjudication. A malformed answer's
/// retry carries the reason it was rejected; a provider error's is plain. Other users of rig's
/// `ExtractorBuilder::retries` (`goal.rs`) get plain retries.
pub const EXTRACTOR_RETRIES: usize = 2;
/// Also the cap of the critique reply.
pub(crate) const MAX_TOKENS: u64 = 1_024;

/// The provider name and model id a model's requests carry on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireIds {
    pub provider: String,
    pub model: String,
}

/// How a guardian behaves; the model is separate because it may be missing.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Wraps the whole extraction, retries included.
    pub timeout: Duration,
    /// A destructive Allow below this confidence is turned into a Deny.
    pub confidence_threshold: f64,
    /// `[guardian] cache`.
    pub cache: bool,
    pub profile: Profile,
    /// Whether the cost a call reports is counted. An unpriced model's calls report none.
    pub priced: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            timeout: Duration::from_secs(30),
            confidence_threshold: 0.7,
            cache: true,
            profile: Profile::Full,
            priced: true,
        }
    }
}

enum Backend {
    Ready(Box<DynModel<Completion>>),
    Unavailable(String),
}

/// What one adjudication came to.
#[derive(Clone, Debug)]
pub struct Adjudication {
    pub decision: Decision,
    pub rule: AuditLabel,
    /// Shown to the user and fed back to the agent.
    pub explanation: String,
    /// The model's own confidence; absent when it gave no verdict.
    pub confidence: Option<f64>,
    pub fail_closed: Option<FailClosed>,
    pub cache_hit: bool,
    pub latency_ms: u64,
    /// `<account>/<model>`.
    pub model: String,
    pub profile: Profile,
    /// Tokens this adjudication spent; empty on a cache hit.
    pub usage: Usage,
    /// The transport the verdict was obtained on; absent when the model could not be asked.
    pub transport: Option<OutputMode>,
}

pub struct Guardian {
    backend: Backend,
    label: String,
    settings: Settings,
    cache: Mutex<VerdictCache>,
    /// Extra request parameters, e.g. the one that turns thinking off for a model that refuses a
    /// forced tool call while thinking.
    additional_params: Option<serde_json::Value>,
    mode: ModeResolver,
}

impl Guardian {
    pub fn new(model: DynModel<Completion>, label: impl Into<String>, settings: Settings) -> Self {
        Guardian {
            backend: Backend::Ready(Box::new(model)),
            label: label.into(),
            settings,
            cache: Mutex::default(),
            additional_params: None,
            mode: ModeResolver::default(),
        }
    }

    /// A guardian with no model to ask: a missing credential, or a pinned model that cannot be
    /// reached. Every adjudication is a denial.
    pub fn unavailable(
        label: impl Into<String>,
        reason: impl Into<String>,
        settings: Settings,
    ) -> Self {
        Guardian {
            backend: Backend::Unavailable(reason.into()),
            label: label.into(),
            settings,
            cache: Mutex::default(),
            additional_params: None,
            mode: ModeResolver::default(),
        }
    }

    pub fn with_additional_params(mut self, params: Option<serde_json::Value>) -> Self {
        self.additional_params = params;
        self
    }

    /// Asks `probe` which transport the model can use, once. Without one, verdicts travel as
    /// a forced tool call.
    pub fn with_output_probe(mut self, probe: Arc<dyn OutputProbe>) -> Self {
        self.mode.set_probe(probe);
        self
    }

    pub fn with_notifier(mut self, notifier: Notifier) -> Self {
        self.mode.set_notifier(notifier);
        self
    }

    /// The transport verdicts are asked for on, resolved on the first call. `Err` is why the
    /// model cannot be asked at all.
    pub async fn resolve_mode(&self) -> Result<OutputMode, String> {
        self.mode.resolve().await
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn profile(&self) -> Profile {
        self.settings.profile
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// `None` when the guardian can be asked, otherwise why not.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match &self.backend {
            Backend::Ready(_) => None,
            Backend::Unavailable(reason) => Some(reason),
        }
    }

    /// The provider name and model id the guardian's own requests carry, as the completion spans
    /// record them. `None` without a model.
    pub fn wire_ids(&self) -> Option<WireIds> {
        match &self.backend {
            Backend::Ready(model) => Some(WireIds {
                provider: model.name().to_string(),
                model: model.id().unwrap_or("unknown").to_string(),
            }),
            Backend::Unavailable(_) => None,
        }
    }

    /// A rough count of the input tokens of the request an abandoned adjudication has usually
    /// already sent: four bytes to a token over the rendered prompt.
    pub fn dropped_estimate(&self, request: &AdjudicationRequest) -> u64 {
        let (_, rendered) = self.prompt(request);
        (rendered.system.len() + rendered.user.len()).div_ceil(4) as u64
    }

    fn prompt(&self, request: &AdjudicationRequest) -> (AdjudicationRequest, Rendered) {
        let profile = self.settings.profile;
        let shaped = profile.shape(request);
        let rendered = render(profile, &shaped);
        (shaped, rendered)
    }

    pub async fn adjudicate(&self, request: &AdjudicationRequest) -> Adjudication {
        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.guardian",
            tool_name = request.tool_call.tool.as_str(),
            guardian_model = self.label.as_str(),
            guardian_profile = self.settings.profile.as_str(),
            model.priced = self.settings.priced,
            decision = tracing::field::Empty,
            fail_closed_reason = tracing::field::Empty,
            cache_hit = tracing::field::Empty,
        );
        let adjudication = self.run(request).instrument(span.clone()).await;
        span.record("decision", adjudication.decision.as_str());
        span.record("cache_hit", adjudication.cache_hit);
        if let Some(reason) = adjudication.fail_closed {
            span.record("fail_closed_reason", reason.as_str());
        }
        adjudication
    }

    async fn run(&self, request: &AdjudicationRequest) -> Adjudication {
        let started = Instant::now();
        let finish = |outcome: Outcome| self.finish(request, started, outcome, None);

        let model = match &self.backend {
            Backend::Ready(model) => model,
            Backend::Unavailable(reason) => {
                return finish(Outcome::FailClosed(
                    FailClosed::Unavailable,
                    format!("the guardian model {} is unavailable: {reason}", self.label),
                    Usage::default(),
                ));
            }
        };

        let mode = match self.mode.resolve().await {
            Ok(mode) => mode,
            Err(reason) => {
                return finish(Outcome::FailClosed(
                    FailClosed::Unavailable,
                    format!("the guardian model {} is unavailable: {reason}", self.label),
                    Usage::default(),
                ));
            }
        };

        let finish = |outcome: Outcome| self.finish(request, started, outcome, Some(mode));
        let profile = self.settings.profile;
        let (shaped, mut rendered) = self.prompt(request);
        if mode == OutputMode::Schema {
            rendered.system = with_schema_answer(&rendered.system);
        }
        let key = CacheKey {
            tool: shaped.tool_call.tool.clone(),
            args_digest: shaped.tool_call.args_digest.clone(),
            model: self.label.clone(),
            profile: profile.as_str(),
            rules_version: digest(&rendered.system),
            request: rendered.user.clone(),
        };

        if self.settings.cache
            && let Some(verdict) = self
                .cache
                .lock()
                .expect("the verdict cache lock is not poisoned")
                .get(&key, tokio::time::Instant::now())
        {
            return finish(Outcome::Verdict {
                verdict,
                cache_hit: true,
                usage: Box::default(),
            });
        }

        let spent = Tally::default();
        let options = Options {
            retries: EXTRACTOR_RETRIES,
            max_tokens: MAX_TOKENS,
            additional_params: self.additional_params.as_ref(),
            rejection: Some(crate::verdict::rejection_note),
        };
        let extraction = tokio::time::timeout(
            self.settings.timeout,
            structured::run::<Verdict>(
                model,
                mode,
                &rendered.system,
                rendered.user,
                options,
                &spent,
            ),
        )
        .await;
        match extraction {
            Err(_) => finish(Outcome::FailClosed(
                FailClosed::Timeout,
                format!(
                    "the guardian did not answer within {} seconds",
                    self.settings.timeout.as_secs()
                ),
                spent.total(),
            )),
            Ok(Err(StructuredOutputError::Prompt(error))) => {
                tracing::warn!(error = %error, "the guardian model call failed");
                let explanation = if lacks_tool_support(&error) {
                    format!(
                        "the guardian model could not be used: this model does not support tool calls, so the guardian could not use the `submit` output tool: {error}"
                    )
                } else {
                    format!("the guardian model could not be reached: {error}")
                };
                finish(Outcome::FailClosed(
                    FailClosed::Error,
                    explanation,
                    spent.total(),
                ))
            }
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "the guardian's answer could not be used");
                finish(Outcome::FailClosed(
                    FailClosed::Unparseable,
                    format!("the guardian's answer was not a valid verdict: {error}"),
                    spent.total(),
                ))
            }
            Ok(Ok(verdict)) => {
                if self.settings.cache {
                    self.cache
                        .lock()
                        .expect("the verdict cache lock is not poisoned")
                        .insert(key, verdict.clone(), tokio::time::Instant::now());
                }
                finish(Outcome::Verdict {
                    verdict,
                    cache_hit: false,
                    usage: Box::new(spent.total()),
                })
            }
        }
    }

    /// Applies the fail-closed rewrite, which runs after every cache lookup, hit or miss.
    fn finish(
        &self,
        request: &AdjudicationRequest,
        started: Instant,
        outcome: Outcome,
        transport: Option<OutputMode>,
    ) -> Adjudication {
        let latency_ms = started.elapsed().as_millis() as u64;
        let base =
            |decision, rule, explanation: String, confidence, fail_closed, cache_hit, usage| {
                let mut usage: Usage = usage;
                if !self.settings.priced {
                    usage.cost = None;
                }
                Adjudication {
                    decision,
                    rule,
                    explanation,
                    confidence,
                    fail_closed,
                    cache_hit,
                    latency_ms,
                    model: self.label.clone(),
                    profile: self.settings.profile,
                    usage,
                    transport,
                }
            };
        match outcome {
            Outcome::FailClosed(reason, explanation, usage) => base(
                Decision::Deny,
                reason.label(),
                explanation,
                None,
                Some(reason),
                false,
                usage,
            ),
            Outcome::Verdict {
                verdict,
                cache_hit,
                usage,
            } => {
                if verdict.decision == Decision::Allow
                    && request.tool_call.destructive
                    && verdict.confidence < self.settings.confidence_threshold
                {
                    let reason = FailClosed::LowConfidence;
                    return base(
                        Decision::Deny,
                        reason.label(),
                        format!(
                            "the guardian allowed this destructive call with only {:.0}% confidence, below the {:.0}% it needs: {}",
                            verdict.confidence * 100.0,
                            self.settings.confidence_threshold * 100.0,
                            verdict.explanation
                        ),
                        Some(verdict.confidence),
                        Some(reason),
                        cache_hit,
                        *usage,
                    );
                }
                base(
                    verdict.decision,
                    AuditLabel::Model(verdict.rule),
                    verdict.explanation,
                    Some(verdict.confidence),
                    None,
                    cache_hit,
                    *usage,
                )
            }
        }
    }
}

enum Outcome {
    Verdict {
        verdict: Verdict,
        cache_hit: bool,
        usage: Box<Usage>,
    },
    FailClosed(FailClosed, String, Usage),
}
