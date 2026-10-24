//! The micromegas backend. The stock capture layer's span capture is off: it exports every field
//! verbatim and the absolute source path of each callsite, so the span bridge turns each
//! allowlisted span into async span events (span id, parent span id, depth) that the analytics
//! side reassembles into a tree. `tracing` events reach it through the stock capture layer. Async
//! span events ride per-thread streams, which the sink builder's CPU-tracing switch creates.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use micromegas_telemetry_sink::api_key_decorator::ApiKeyRequestDecorator;
use micromegas_telemetry_sink::{TelemetryGuard, TelemetryGuardBuilder};
use micromegas_tracing::dispatch;
use micromegas_tracing::levels::{LevelFilter, Verbosity};
use micromegas_tracing::metrics::StaticMetricMetadata;
use micromegas_tracing::property_set::{DynProperty, PropertySet};
use micromegas_tracing::spans::SpanLocation;
use tracing::Metadata;
use tracing::callsite::Identifier;

use super::span_bridge::{Export, Metric, MetricValue, SpanEnd};

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

pub fn capture_layer() -> micromegas_telemetry_sink::tracing_interop::TracingCaptureLayer {
    micromegas_telemetry_sink::tracing_interop::TracingCaptureLayer {
        max_level: LevelFilter::Info,
        capture_spans: false,
    }
}

pub struct MicromegasExport {
    /// Whether spans are exported (`[telemetry] traces`); metrics and events always are.
    spans: bool,
    locations: Mutex<HashMap<Identifier, &'static SpanLocation>>,
    metrics: Mutex<HashMap<&'static str, &'static StaticMetricMetadata>>,
}

impl MicromegasExport {
    pub fn new(spans: bool) -> Self {
        MicromegasExport {
            spans,
            locations: Mutex::default(),
            metrics: Mutex::default(),
        }
    }

    /// One location per `tracing` callsite, leaked once: bounded by the number of callsites.
    fn location(&self, meta: &'static Metadata<'static>) -> &'static SpanLocation {
        let mut locations = self.locations.lock().unwrap_or_else(|e| e.into_inner());
        locations.entry(meta.callsite()).or_insert_with(|| {
            Box::leak(Box::new(SpanLocation {
                lod: Verbosity::Med,
                target: meta.target(),
                module_path: meta.module_path().unwrap_or_default(),
                // Callsite files are absolute paths inside the user's home for dependencies.
                file: "",
                line: 0,
            }))
        })
    }

    fn descriptor(
        &self,
        name: &'static str,
        unit: &'static str,
        target: &'static str,
    ) -> &'static StaticMetricMetadata {
        let mut metrics = self.metrics.lock().unwrap_or_else(|e| e.into_inner());
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

/// Spans ride the emitting thread's stream. The runtime's workers and the main thread have
/// one; the bridge never creates one itself, because micromegas keeps a raw pointer to every
/// stream and a thread that exits without unregistering would leave it dangling.
impl Export for MicromegasExport {
    fn span_begin(
        &self,
        name: &str,
        meta: &'static Metadata<'static>,
        parent_id: u64,
        depth: u32,
    ) -> Option<u64> {
        if !self.spans {
            return None;
        }
        Some(dispatch::on_begin_async_named_scope_dyn(
            self.location(meta),
            name,
            parent_id,
            depth,
        ))
    }

    fn span_end(&self, end: SpanEnd<'_>) {
        if !self.spans {
            return;
        }
        let properties: Vec<DynProperty<'_>> = end
            .fields
            .iter()
            .map(|(name, value)| DynProperty::with_dyn_name(name, value))
            .collect();
        dispatch::on_end_async_named_scope_tagged(
            end.id,
            end.parent_id,
            self.location(end.meta),
            end.name,
            end.depth,
            PropertySet::empty(),
            &properties,
        );
    }

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
