//! Backend-neutral span handling for the export backends. One layer watches the allowlisted
//! spans, tells the backend about their begin and end (the micromegas backend maps them to
//! async span events; OTLP exports spans through `tracing-opentelemetry` instead) and derives
//! the token and cost metrics and the `api_request` event from rig's completion spans.

use std::time::Instant;

use rig_core::catalog::Catalog;
use rig_core::completion::Usage;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use super::content::ContentPolicy;

pub const TARGET: &str = "fontaine::telemetry";

/// The targets whose spans are exported. Nothing else rig or a dependency logs is.
pub const SPAN_TARGETS: [&str; 4] = [
    TARGET,
    "rig::completions",
    "rig::agent_chat",
    "rig_agent::agent::telemetry",
];

pub fn is_exported_span(meta: &Metadata<'_>) -> bool {
    meta.is_span() && SPAN_TARGETS.contains(&meta.target())
}

/// Events are exported from Fontaine's own target only: rig's event targets carry request and
/// response bodies at trace level.
pub fn is_exported_event(meta: &Metadata<'_>) -> bool {
    meta.is_event() && meta.target() == TARGET
}

/// What the bridge itself observes: the allowlisted spans, plus Fontaine's events (the
/// decision event is counted from the event, not from a span).
pub fn is_bridged(meta: &Metadata<'_>) -> bool {
    is_exported_span(meta) || is_exported_event(meta)
}

pub enum MetricValue {
    Int(u64),
    Float(f64),
}

pub struct Metric {
    pub name: &'static str,
    pub unit: &'static str,
    pub value: MetricValue,
    pub attrs: Vec<(&'static str, String)>,
}

pub struct SpanEnd<'a> {
    pub name: &'a str,
    pub meta: &'static Metadata<'static>,
    pub id: u64,
    pub parent_id: u64,
    pub depth: u32,
    pub fields: &'a [(String, String)],
}

/// What a backend does with the bridge's observations.
pub trait Export: Send + Sync + 'static {
    /// A span opened. A backend that maps spans itself returns the id its children name as
    /// their parent; one that does not returns `None`.
    fn span_begin(
        &self,
        _name: &str,
        _meta: &'static Metadata<'static>,
        _parent_id: u64,
        _depth: u32,
    ) -> Option<u64> {
        None
    }

    fn span_end(&self, _end: SpanEnd<'_>) {}

    fn metric(&self, metric: Metric);
}

struct BridgeSpan {
    id: u64,
    parent_id: u64,
    depth: u32,
    started: Instant,
    fields: Vec<(String, String)>,
}

struct Collect<'a>(&'a mut Vec<(String, String)>);

impl Collect<'_> {
    fn set(&mut self, field: &Field, value: String) {
        match self.0.iter_mut().find(|(name, _)| name == field.name()) {
            Some(slot) => slot.1 = value,
            None => self.0.push((field.name().to_string(), value)),
        }
    }
}

impl Visit for Collect<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.set(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.set(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.set(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.set(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.set(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.set(field, format!("{value:?}"));
    }
}

fn lookup<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn count(fields: &[(String, String)], name: &str) -> Option<u64> {
    lookup(fields, name)?.parse().ok()
}

pub struct SpanBridge<E> {
    export: E,
    policy: ContentPolicy,
}

impl<E: Export> SpanBridge<E> {
    pub fn new(export: E, policy: ContentPolicy) -> Self {
        SpanBridge { export, policy }
    }

    fn derive(&self, name: &str, fields: &[(String, String)], elapsed: std::time::Duration) {
        match name {
            "chat" | "generate_content" => self.completion(fields, elapsed),
            "fontaine.session" => self.export.metric(Metric {
                name: "fontaine.session.count",
                unit: "{session}",
                value: MetricValue::Int(1),
                attrs: vec![(
                    "start_type",
                    lookup(fields, "start_type")
                        .unwrap_or("headless")
                        .to_string(),
                )],
            }),
            "fontaine.interaction" => {
                let prompt_id = lookup(fields, "prompt.id").unwrap_or_default();
                let length = count(fields, "prompt.length").unwrap_or(0);
                tracing::info!(target: TARGET, prompt_id = prompt_id, prompt_length = length, "user_prompt");
            }
            _ => {}
        }
    }

    fn completion(&self, fields: &[(String, String)], elapsed: std::time::Duration) {
        let input = count(fields, "gen_ai.usage.input_tokens");
        let output = count(fields, "gen_ai.usage.output_tokens");
        if input.is_none() && output.is_none() {
            return;
        }
        let provider = lookup(fields, "gen_ai.provider.name").unwrap_or("unknown");
        let model = lookup(fields, "gen_ai.request.model")
            .or_else(|| lookup(fields, "gen_ai.response.model"))
            .unwrap_or("unknown");
        let cache_read = count(fields, "gen_ai.usage.cache_read.input_tokens");
        let cache_write = count(fields, "gen_ai.usage.cache_creation.input_tokens");
        let reasoning = count(fields, "gen_ai.usage.reasoning_tokens");
        let query_source = "main";

        let usage = Usage::new()
            .input_tokens(input)
            .output_tokens(output)
            .cached_input_tokens(cache_read)
            .cache_creation_input_tokens(cache_write);
        let cost = cost_usd(provider, model, &usage);

        for (kind, tokens) in [
            ("input", input),
            ("output", output),
            ("cacheRead", cache_read),
            ("cacheCreation", cache_write),
            ("reasoning", reasoning),
        ] {
            if let Some(tokens) = tokens {
                self.export.metric(Metric {
                    name: "fontaine.token.usage",
                    unit: "{token}",
                    value: MetricValue::Int(tokens),
                    attrs: vec![
                        ("type", kind.to_string()),
                        ("model", model.to_string()),
                        ("query_source", query_source.to_string()),
                    ],
                });
            }
        }
        if let Some(cost) = cost {
            self.export.metric(Metric {
                name: "fontaine.cost.usage",
                unit: "USD",
                value: MetricValue::Float(cost),
                attrs: vec![
                    ("model", model.to_string()),
                    ("query_source", query_source.to_string()),
                ],
            });
        }
        tracing::info!(
            target: TARGET,
            model = model,
            provider = provider,
            input_tokens = input,
            output_tokens = output,
            cost_usd = cost,
            duration_ms = elapsed.as_millis() as u64,
            query_source = query_source,
            "api_request"
        );
    }
}

/// The catalog price of `usage` for `provider`'s `model`, or `None` when rig has no price.
pub fn cost_usd(provider: &str, model: &str, usage: &Usage) -> Option<f64> {
    let resolved = Catalog::builtin()
        .resolve(&format!("{provider}/{model}"))
        .ok()?;
    resolved.spec.pricing?.cost(usage).map(|cost| cost.total)
}

impl<S, E> Layer<S> for SpanBridge<E>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    E: Export,
{
    // Counted from the decision event, which every audit record emits, so the human's answer
    // counts like any other tier's.
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != TARGET {
            return;
        }
        let mut fields = Vec::new();
        event.record(&mut Collect(&mut fields));
        if lookup(&fields, "message") != Some("tool_decision") {
            return;
        }
        if let (Some(tier), Some(decision)) = (lookup(&fields, "tier"), lookup(&fields, "decision"))
        {
            self.export.metric(Metric {
                name: "fontaine.tool_decision.count",
                unit: "{decision}",
                value: MetricValue::Int(1),
                attrs: vec![
                    ("tier", tier.to_string()),
                    ("decision", decision.to_string()),
                ],
            });
        }
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = Vec::new();
        attrs.record(&mut Collect(&mut fields));
        let (parent_id, depth) = match span.parent() {
            Some(parent) => parent
                .extensions()
                .get::<BridgeSpan>()
                .map_or((0, 0), |p| (p.id, p.depth + 1)),
            None => (0, 0),
        };
        let exported = self
            .export
            .span_begin(span.name(), span.metadata(), parent_id, depth)
            .unwrap_or(0);
        span.extensions_mut().insert(BridgeSpan {
            id: exported,
            parent_id,
            depth,
            started: Instant::now(),
            fields,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if let Some(bridge) = span.extensions_mut().get_mut::<BridgeSpan>() {
            values.record(&mut Collect(&mut bridge.fields));
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(bridge) = span.extensions_mut().remove::<BridgeSpan>() else {
            return;
        };
        self.derive(span.name(), &bridge.fields, bridge.started.elapsed());
        let fields = self.policy.export_fields(&bridge.fields);
        self.export.span_end(SpanEnd {
            name: span.name(),
            meta: span.metadata(),
            id: bridge.id,
            parent_id: bridge.parent_id,
            depth: bridge.depth,
            fields: &fields,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fontaine_core::config::{LoadParams, load};
    use rig_core::test_utils::TraceCapture;
    use tracing::field::Empty;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    type Recorded = (String, f64, Vec<(String, String)>);

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Recorded>>>);

    impl Export for Recorder {
        fn metric(&self, metric: Metric) {
            let value = match metric.value {
                MetricValue::Int(v) => v as f64,
                MetricValue::Float(v) => v,
            };
            let attrs = metric
                .attrs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            self.0
                .lock()
                .unwrap()
                .push((metric.name.to_string(), value, attrs));
        }
    }

    fn policy() -> ContentPolicy {
        let dir = tempfile::tempdir().unwrap();
        let config = load(&LoadParams {
            launch_dir: dir.path(),
            config_dir: dir.path(),
            data_dir: dir.path(),
            settings_file: None,
            cli_rules: Default::default(),
            env: &|_| None,
        })
        .unwrap();
        ContentPolicy::from_config(&config.telemetry)
    }

    fn harness() -> (Recorder, TraceCapture, impl Subscriber + Send + Sync) {
        let recorder = Recorder::default();
        let capture = TraceCapture::default();
        let subscriber = tracing_subscriber::registry()
            .with(SpanBridge::new(recorder.clone(), policy()))
            .with(capture.clone());
        (recorder, capture, subscriber)
    }

    fn completion(provider: &str, model: &str) {
        let span = tracing::info_span!(
            target: "rig::completions",
            "chat",
            gen_ai.provider.name = provider,
            gen_ai.request.model = model,
            gen_ai.usage.input_tokens = Empty,
            gen_ai.usage.output_tokens = Empty,
        );
        span.record("gen_ai.usage.input_tokens", 1000u64);
        span.record("gen_ai.usage.output_tokens", 500u64);
    }

    fn metric<'a>(all: &'a [Recorded], name: &str, kind: Option<&str>) -> Option<&'a Recorded> {
        all.iter().find(|(n, _, attrs)| {
            n == name && kind.is_none_or(|k| attrs.iter().any(|(a, v)| a == "type" && v == k))
        })
    }

    #[test]
    fn a_priced_completion_yields_token_and_cost_metrics_and_an_api_request_event() {
        let (recorder, capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || completion("openai", "gpt-4o-mini"));
        let metrics = recorder.0.lock().unwrap();
        assert_eq!(
            metric(&metrics, "fontaine.token.usage", Some("input"))
                .unwrap()
                .1,
            1000.0
        );
        assert_eq!(
            metric(&metrics, "fontaine.token.usage", Some("output"))
                .unwrap()
                .1,
            500.0
        );
        let cost = metric(&metrics, "fontaine.cost.usage", None).unwrap();
        assert!((cost.1 - 0.00045).abs() < 1e-9, "{}", cost.1);
        assert!(
            cost.2
                .contains(&("model".to_string(), "gpt-4o-mini".to_string()))
        );

        let events = capture.events();
        let event = events
            .iter()
            .find(|e| e.message() == "api_request")
            .unwrap();
        assert_eq!(event.target, TARGET);
        assert_eq!(event.fields["model"], "gpt-4o-mini");
        assert_eq!(event.fields["input_tokens"], 1000);
        assert_eq!(event.fields["output_tokens"], 500);
        assert!(event.fields.contains_key("cost_usd"));
        assert_eq!(event.fields["query_source"], "main");
    }

    #[test]
    fn an_unpriced_completion_has_tokens_but_no_cost() {
        let (recorder, capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || {
            completion("openai-compatible", "scripted")
        });
        let metrics = recorder.0.lock().unwrap();
        assert!(metric(&metrics, "fontaine.token.usage", Some("input")).is_some());
        assert!(metric(&metrics, "fontaine.cost.usage", None).is_none());
        let events = capture.events();
        let event = events
            .iter()
            .find(|e| e.message() == "api_request")
            .unwrap();
        assert!(!event.fields.contains_key("cost_usd"));
    }

    #[test]
    fn model_and_provider_recorded_after_creation_are_read() {
        let (recorder, _capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "rig::agent_chat",
                "chat",
                gen_ai.provider.name = Empty,
                gen_ai.request.model = Empty,
                gen_ai.usage.input_tokens = Empty,
                gen_ai.usage.output_tokens = Empty,
            );
            span.record("gen_ai.provider.name", "openai");
            span.record("gen_ai.request.model", "gpt-4o-mini");
            span.record("gen_ai.usage.input_tokens", 1000u64);
            span.record("gen_ai.usage.output_tokens", 500u64);
        });
        let metrics = recorder.0.lock().unwrap();
        assert!(metric(&metrics, "fontaine.cost.usage", None).is_some());
    }

    #[test]
    fn spans_that_carry_no_usage_derive_nothing() {
        let (recorder, capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || {
            let _ = tracing::info_span!(target: "rig::completions", "chat");
            let _ = tracing::info_span!(target: TARGET, "something.else");
        });
        assert!(recorder.0.lock().unwrap().is_empty());
        assert!(capture.events().is_empty());
    }

    #[test]
    fn every_decision_event_counts_including_the_humans() {
        let (recorder, _capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || {
            for (tier, decision) in [("tier05", "allow"), ("mode", "prompt"), ("human", "allow")] {
                tracing::info!(target: TARGET, tier = tier, decision = decision, "tool_decision");
            }
            tracing::info!(target: TARGET, tier = "x", "tool_decision");
            tracing::info!(target: TARGET, tier = "x", decision = "allow", "something_else");
        });
        let metrics = recorder.0.lock().unwrap();
        let counts: Vec<_> = metrics
            .iter()
            .filter(|(name, _, _)| name == "fontaine.tool_decision.count")
            .collect();
        assert_eq!(counts.len(), 3, "an event with no decision counts nothing");
        for (tier, decision) in [("tier05", "allow"), ("mode", "prompt"), ("human", "allow")] {
            assert!(counts.iter().any(|c| {
                c.1 == 1.0
                    && c.2.contains(&("tier".to_string(), tier.to_string()))
                    && c.2
                        .contains(&("decision".to_string(), decision.to_string()))
            }));
        }
    }

    #[test]
    fn the_session_span_counts_a_session() {
        let (recorder, _capture, subscriber) = harness();
        tracing::subscriber::with_default(subscriber, || {
            let _ =
                tracing::info_span!(target: TARGET, "fontaine.session", start_type = "headless");
        });
        let metrics = recorder.0.lock().unwrap();
        let count = metric(&metrics, "fontaine.session.count", None).unwrap();
        assert!(
            count
                .2
                .contains(&("start_type".to_string(), "headless".to_string()))
        );
    }
}
