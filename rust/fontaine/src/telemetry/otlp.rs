//! The OTLP/HTTP backend: protobuf over HTTP to any OpenTelemetry collector. Spans go through
//! `tracing-opentelemetry`, events through the `tracing` log appender, and metrics through an
//! OTel `Meter` driven by the span bridge.

use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, MeterProvider};
use opentelemetry::trace::TracerProvider;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{
    LogExporter, MetricExporter, SpanExporter, WithExportConfig, WithHttpConfig,
};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{
    BatchSpanProcessor, SdkTracerProvider, Span, SpanData, SpanProcessor,
};
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;

use super::BoxLayer;
use super::content::ContentPolicy;
use super::span_bridge::{Export, Metric, MetricValue};

/// How long one export request may take; the process-level flush cap is shorter than the
/// retries this would otherwise allow.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Settings {
    pub endpoint: String,
    pub api_key: String,
    pub traces: bool,
    pub resource_attributes: Vec<(String, String)>,
}

/// Held for the life of the run; shut down on `finish`.
pub struct Providers {
    tracer: Option<SdkTracerProvider>,
    meter: SdkMeterProvider,
    logger: SdkLoggerProvider,
}

impl Providers {
    pub fn shutdown(self, timeout: Duration) {
        if let Some(tracer) = &self.tracer {
            let _ = tracer.shutdown_with_timeout(timeout);
        }
        let _ = self.meter.shutdown_with_timeout(timeout);
        let _ = self.logger.shutdown_with_timeout(timeout);
    }
}

pub struct Layers<S> {
    pub tracing: Vec<BoxLayer<S>>,
    pub export: OtlpExport,
    pub providers: Providers,
}

/// Replaces the content attributes of finished spans with what the content policy allows.
#[derive(Debug)]
struct ScrubSpans<P> {
    next: P,
    policy: ContentPolicy,
}

impl<P: SpanProcessor> SpanProcessor for ScrubSpans<P> {
    fn on_start(&self, span: &mut Span, cx: &opentelemetry::Context) {
        self.next.on_start(span, cx);
    }

    fn on_end(&self, mut span: SpanData) {
        let policy = &self.policy;
        span.attributes = std::mem::take(&mut span.attributes)
            .into_iter()
            .filter_map(|kv| {
                if policy.is_open(kv.key.as_str()) {
                    return Some(kv);
                }
                let value = kv.value.as_str().into_owned();
                let kept = policy.keep(kv.key.as_str(), &value)?;
                Some(KeyValue::new(kv.key, kept))
            })
            .collect();
        self.next.on_end(span);
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.next.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.next.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.next.set_resource(resource);
    }
}

pub struct OtlpExport {
    tokens: Counter<u64>,
    cost: Counter<f64>,
    sessions: Counter<u64>,
    decisions: Counter<u64>,
}

impl Export for OtlpExport {
    fn metric(&self, metric: Metric) {
        let attrs: Vec<KeyValue> = metric
            .attrs
            .into_iter()
            .map(|(name, value)| KeyValue::new(name, value))
            .collect();
        match (metric.name, metric.value) {
            ("fontaine.token.usage", MetricValue::Int(v)) => self.tokens.add(v, &attrs),
            ("fontaine.cost.usage", MetricValue::Float(v)) => self.cost.add(v, &attrs),
            ("fontaine.session.count", MetricValue::Int(v)) => self.sessions.add(v, &attrs),
            ("fontaine.tool_decision.count", MetricValue::Int(v)) => self.decisions.add(v, &attrs),
            _ => {}
        }
    }
}

fn signal_url(endpoint: &str, path: &str) -> String {
    format!("{}{path}", endpoint.trim_end_matches('/'))
}

fn headers(api_key: &str) -> HashMap<String, String> {
    HashMap::from([("Authorization".to_string(), format!("Bearer {api_key}"))])
}

/// Builds the exporters. Blocking HTTP clients are created here, so this must run outside the
/// tokio runtime.
pub fn build<S>(settings: Settings, policy: ContentPolicy) -> Result<Layers<S>, String>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let mut attributes = vec![
        KeyValue::new("service.name", "fontaine"),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ];
    attributes.extend(
        settings
            .resource_attributes
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone())),
    );
    let resource = Resource::builder().with_attributes(attributes).build();

    let mut tracing_layers: Vec<BoxLayer<S>> = Vec::new();

    let tracer = if settings.traces {
        let exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(signal_url(&settings.endpoint, "/v1/traces"))
            .with_headers(headers(&settings.api_key))
            .with_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| err("span exporter", &e))?;
        let processor = ScrubSpans {
            next: BatchSpanProcessor::builder(exporter).build(),
            policy: policy.clone(),
        };
        let provider = SdkTracerProvider::builder()
            .with_span_processor(processor)
            .with_resource(resource.clone())
            .build();
        // Source locations and thread names would put the machine's file layout in every span.
        let layer = tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("fontaine"))
            .with_location(false)
            .with_threads(false);
        tracing_layers.push(super::filtered(layer, super::span_bridge::is_exported_span));
        Some(provider)
    } else {
        None
    };

    let metric_exporter = MetricExporter::builder()
        .with_http()
        .with_endpoint(signal_url(&settings.endpoint, "/v1/metrics"))
        .with_headers(headers(&settings.api_key))
        .with_timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| err("metric exporter", &e))?;
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(metric_exporter).build())
        .with_resource(resource.clone())
        .build();
    let meter = meter_provider.meter("fontaine");
    let export = OtlpExport {
        tokens: meter
            .u64_counter("fontaine.token.usage")
            .with_unit("{token}")
            .build(),
        cost: meter
            .f64_counter("fontaine.cost.usage")
            .with_unit("USD")
            .build(),
        sessions: meter
            .u64_counter("fontaine.session.count")
            .with_unit("{session}")
            .build(),
        decisions: meter
            .u64_counter("fontaine.tool_decision.count")
            .with_unit("{decision}")
            .build(),
    };

    let log_exporter = LogExporter::builder()
        .with_http()
        .with_endpoint(signal_url(&settings.endpoint, "/v1/logs"))
        .with_headers(headers(&settings.api_key))
        .with_timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| err("log exporter", &e))?;
    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();
    tracing_layers.push(super::filtered(
        OpenTelemetryTracingBridge::new(&logger_provider),
        super::span_bridge::is_exported_event,
    ));

    Ok(Layers {
        tracing: tracing_layers,
        export,
        providers: Providers {
            tracer,
            meter: meter_provider,
            logger: logger_provider,
        },
    })
}
