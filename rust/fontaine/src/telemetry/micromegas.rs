//! The micromegas backend. The stock capture layer exports both `tracing` events and the
//! allowlisted spans: each span becomes async span events (span id, parent span id, depth) that
//! the analytics side reassembles into a tree, with its fields passed through the content policy
//! and its callsite file trimmed to a path relative to its crate. Async span events ride
//! per-thread streams, which the sink builder's CPU-tracing switch creates. Metrics come from the
//! span bridge.

use fontaine_tools::lock::lock_recovering;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use micromegas_telemetry_sink::api_key_decorator::ApiKeyRequestDecorator;
use micromegas_telemetry_sink::tracing_interop::TracingCaptureLayer;
use micromegas_telemetry_sink::{TelemetryGuard, TelemetryGuardBuilder};
use micromegas_tracing::dispatch;
use micromegas_tracing::levels::{LevelFilter, Verbosity};
use micromegas_tracing::metrics::StaticMetricMetadata;
use micromegas_tracing::property_set::{DynProperty, PropertySet};

use super::content::ContentPolicy;
use super::span_bridge::{Export, Metric, MetricValue};

pub struct Settings {
    pub endpoint: String,
    pub api_key: String,
    pub resource_attributes: Vec<(String, String)>,
}

/// The sink guard: no local sink, no global `tracing` capture (the capture layer is composed
/// into Fontaine's own registry), no system monitor, no host-identifying default properties,
/// an explicit endpoint and an explicit key, never the implicit environment fallback.
pub fn build_guard(settings: Settings) -> Result<TelemetryGuard, String> {
    let key = settings.api_key;
    let mut builder = TelemetryGuardBuilder::default()
        .with_local_sink_enabled(false)
        .with_install_tracing_capture(false)
        .with_system_metrics_enabled(false)
        .with_cpu_tracing_enabled(true)
        .with_default_system_properties_enabled(false)
        .with_telemetry_sink_url(settings.endpoint)
        .with_request_decorator(Box::new(move || Arc::new(ApiKeyRequestDecorator::new(key))));
    for (name, value) in settings.resource_attributes {
        builder = builder.with_process_property(name, value);
    }
    builder.build().map_err(|e| e.to_string())
}

/// Spans are captured only on threads that own a micromegas stream: the runtime's workers and
/// the main thread. The layer never creates one itself, because micromegas keeps a raw pointer to
/// every stream and a thread that exits without unregistering would leave it dangling.
pub fn capture_layer(spans: bool, policy: ContentPolicy) -> TracingCaptureLayer {
    TracingCaptureLayer {
        max_level: LevelFilter::Info,
        capture_spans: spans,
        span_field_hook: Some(Box::new(move |_, key, value| {
            policy.keep(key, value).map(Cow::Owned)
        })),
    }
}

/// Metrics derived by the span bridge; spans and events go through [`capture_layer`].
#[derive(Default)]
pub struct MicromegasExport {
    metrics: Mutex<HashMap<&'static str, &'static StaticMetricMetadata>>,
}

impl MicromegasExport {
    fn descriptor(
        &self,
        name: &'static str,
        unit: &'static str,
        target: &'static str,
    ) -> &'static StaticMetricMetadata {
        // Tallies of leaked statics: an entry is inserted whole or not at all.
        let mut metrics = lock_recovering(&self.metrics);
        metrics.entry(name).or_insert_with(|| {
            Box::leak(Box::new(StaticMetricMetadata {
                lod: Verbosity::Med,
                name,
                unit,
                target,
                file: "",
                line: 0,
            }))
        })
    }
}

impl Export for MicromegasExport {
    fn metric(&self, metric: Metric) {
        let desc = self.descriptor(metric.name, metric.unit, super::span_bridge::TARGET);
        let properties: Vec<DynProperty<'_>> = metric
            .attrs
            .iter()
            .map(|(name, value)| DynProperty::with_dyn_name(name, value))
            .collect();
        match metric.value {
            MetricValue::Int(v) => {
                dispatch::tagged_integer_metric_dyn(desc, PropertySet::empty(), &properties, v)
            }
            MetricValue::Float(v) => {
                dispatch::tagged_float_metric_dyn(desc, PropertySet::empty(), &properties, v)
            }
        }
    }
}
