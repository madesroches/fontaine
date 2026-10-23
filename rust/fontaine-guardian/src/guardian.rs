//! One stateless, tool-less model conversation per adjudication. Every outcome that is not a
//! verdict the model returned is a denial decided here, by code.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rig_agent::agent::{AgentHook, HookContext, OutcomeAction, OutcomeEvent};
use rig_agent::completion::StructuredOutputError;
use rig_agent::extractor::ExtractorBuilder;
use rig_core::DynModel;
use rig_core::completion::Usage;
use rig_core::message::Message;
use rig_core::operation::Completion;
use tracing::Instrument;

use crate::cache::{CacheKey, VerdictCache, digest};
use crate::profile::Profile;
use crate::prompt_builder::render;
use crate::request::AdjudicationRequest;
use crate::verdict::{AuditLabel, Decision, FailClosed, Verdict};

const TARGET: &str = "fontaine::telemetry";

/// Retries after a malformed answer or a provider error, per adjudication.
pub const EXTRACTOR_RETRIES: usize = 2;
const MAX_TOKENS: u64 = 1_024;

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

/// rig's structured-output errors carry no usage, so every billed response is tallied as it
/// arrives; a fail-closed adjudication still spent those tokens.
struct UsageTally(Arc<Mutex<Usage>>);

impl AgentHook for UsageTally {
    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        if let Some(response) = event.completion() {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) += response.usage;
        }
        OutcomeAction::Proceed
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
}

pub struct Guardian {
    backend: Backend,
    label: String,
    settings: Settings,
    cache: Mutex<VerdictCache>,
}

impl Guardian {
    pub fn new(model: DynModel<Completion>, label: impl Into<String>, settings: Settings) -> Self {
        Guardian {
            backend: Backend::Ready(Box::new(model)),
            label: label.into(),
            settings,
            cache: Mutex::default(),
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
        }
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
        let finish = |outcome: Outcome| self.finish(request, started, outcome);

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

        let profile = self.settings.profile;
        let shaped = profile.shape(request);
        let rendered = render(profile, &shaped);
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
                .unwrap_or_else(|e| e.into_inner())
                .get(&key, tokio::time::Instant::now())
        {
            return finish(Outcome::Verdict {
                verdict,
                cache_hit: true,
                usage: Box::default(),
            });
        }

        let spent = Arc::new(Mutex::new(Usage::default()));
        let tally = || *spent.lock().unwrap_or_else(|e| e.into_inner());
        let extractor = ExtractorBuilder::<Verdict>::new(model.as_ref().clone())
            .add_hook(UsageTally(spent.clone()))
            .append_preamble(&rendered.system)
            .max_tokens(MAX_TOKENS)
            .retries(EXTRACTOR_RETRIES)
            .build();
        let extraction = tokio::time::timeout(
            self.settings.timeout,
            extractor.extract(Message::user(rendered.user)),
        )
        .await;
        match extraction {
            Err(_) => finish(Outcome::FailClosed(
                FailClosed::Timeout,
                format!(
                    "the guardian did not answer within {} seconds",
                    self.settings.timeout.as_secs()
                ),
                tally(),
            )),
            Ok(Err(StructuredOutputError::Prompt(error))) => {
                tracing::warn!(error = %error, "the guardian model call failed");
                finish(Outcome::FailClosed(
                    FailClosed::Error,
                    format!("the guardian model could not be reached: {error}"),
                    tally(),
                ))
            }
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "the guardian's answer could not be used");
                finish(Outcome::FailClosed(
                    FailClosed::Unparseable,
                    format!("the guardian's answer was not a valid verdict: {error}"),
                    tally(),
                ))
            }
            Ok(Ok(response)) => {
                if self.settings.cache {
                    self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
                        key,
                        response.output.clone(),
                        tokio::time::Instant::now(),
                    );
                }
                finish(Outcome::Verdict {
                    verdict: response.output,
                    cache_hit: false,
                    usage: Box::new(tally()),
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
