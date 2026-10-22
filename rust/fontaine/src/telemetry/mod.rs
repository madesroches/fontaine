//! The tracing facade: the one place a subscriber is built. Every other crate only emits
//! `tracing` spans and events.

pub mod content;
#[cfg(feature = "telemetry-micromegas")]
pub mod micromegas;
#[cfg(feature = "telemetry-otlp")]
pub mod otlp;
pub mod span_bridge;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fontaine_core::config::{Config, TelemetryBackend};
use fontaine_core::credentials::Credentials;
use tracing::{Metadata, Subscriber};
use tracing_subscriber::filter::{EnvFilter, filter_fn};
use tracing_subscriber::fmt as tfmt;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use content::ContentPolicy;

pub const LOG_VAR: &str = "FONTAINE_LOG";
const DEFAULT_FILTER: &str = "debug";

/// A bad `--debug-file` / `FONTAINE_LOG`; exit code 2.
#[derive(Debug)]
pub struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

pub type BoxLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// The local debug-file sink. The file is opened once and shared by every layer built from
/// it, so the scoped subscriber used while loading config and the global one write to the
/// same handle.
pub struct DebugSink {
    file: Arc<std::fs::File>,
    directives: String,
}

impl DebugSink {
    /// `None` unless `--debug-file` is given: `FONTAINE_LOG` alone only overrides the filter
    /// and never creates a file or writes to stderr.
    pub fn open(
        debug_file: Option<&Path>,
        log_var: Option<OsString>,
    ) -> Result<Option<DebugSink>, UsageError> {
        let directives = match log_var {
            Some(raw) => {
                let text = raw
                    .into_string()
                    .map_err(|_| UsageError(format!("{LOG_VAR} is not valid unicode")))?;
                EnvFilter::try_new(&text).map_err(|e| {
                    UsageError(format!("invalid {LOG_VAR} directive `{text}`: {e}"))
                })?;
                text
            }
            None => DEFAULT_FILTER.to_string(),
        };
        let Some(path) = debug_file else {
            return Ok(None);
        };
        let file = fontaine_core::owner_only::open(path, true).map_err(|e| {
            UsageError(format!(
                "cannot open --debug-file {}: {e}",
                PathBuf::from(path).display()
            ))
        })?;
        Ok(Some(DebugSink {
            file: Arc::new(file),
            directives,
        }))
    }

    pub fn layer<S>(&self) -> BoxLayer<S>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        // Validated when the sink was opened.
        let filter =
            EnvFilter::try_new(&self.directives).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
        tfmt::layer()
            .with_ansi(false)
            .with_writer(Arc::clone(&self.file))
            .with_filter(filter)
            .boxed()
    }
}

/// Exporting never delays the exit by more than this.
const FLUSH_CAP: Duration = Duration::from_secs(2);

/// Runs `f` with a subscriber that only has the debug layer, for use before config has been
/// loaded and the global registry can be built.
pub fn with_debug_only<T>(sink: Option<&DebugSink>, f: impl FnOnce() -> T) -> T {
    let subscriber = tracing_subscriber::registry().with(sink.map(|s| s.layer()));
    tracing::subscriber::with_default(subscriber, f)
}

/// A layer that only sees the spans or events `keep` admits.
pub fn filtered<S, L>(layer: L, keep: fn(&Metadata<'_>) -> bool) -> BoxLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    L: Layer<S> + Send + Sync + 'static,
{
    layer.with_filter(filter_fn(keep)).boxed()
}

/// What `install` leaves running: the export backend's guards.
#[derive(Default)]
pub struct Telemetry {
    #[cfg(feature = "telemetry-micromegas")]
    micromegas: Option<micromegas_telemetry_sink::TelemetryGuard>,
    #[cfg(feature = "telemetry-otlp")]
    otlp: Option<otlp::Providers>,
}

pub struct InstallParams<'a> {
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub data_dir: &'a Path,
}

/// The ingestion key: the variable `api_key_env` names, else the stored credential. Config
/// loading already guaranteed one of them yields a value.
fn ingestion_key(config: &Config, params: &InstallParams<'_>) -> Result<String, String> {
    let t = &config.telemetry;
    if let Some(var) = &t.api_key_env {
        return (params.env)(var)
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{var} is unset or empty"));
    }
    let stored = Credentials::load(params.data_dir).map_err(|e| e.to_string())?;
    stored
        .telemetry()
        .map(str::to_string)
        .ok_or_else(|| "no stored ingestion key".to_string())
}

fn resource_attributes(config: &Config) -> Vec<(String, String)> {
    config
        .telemetry
        .resource_attributes
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Builds the configured backend: its guards and the layers to compose. Any failure leaves
/// export off.
fn backend<S>(
    config: &Config,
    params: &InstallParams<'_>,
) -> Result<(Telemetry, Vec<BoxLayer<S>>), String>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let t = &config.telemetry;
    let endpoint = t
        .endpoint
        .clone()
        .ok_or_else(|| "no endpoint".to_string())?;
    let api_key = ingestion_key(config, params)?;
    let policy = ContentPolicy::from_config(t);
    let mut telemetry = Telemetry::default();
    let mut layers: Vec<BoxLayer<S>> = Vec::new();
    match t.backend {
        #[cfg(feature = "telemetry-micromegas")]
        TelemetryBackend::Micromegas => {
            micromegas::enable_cpu_tracing();
            let guard = micromegas::build_guard(micromegas::Settings {
                endpoint,
                api_key,
                resource_attributes: resource_attributes(config),
            })?;
            telemetry.micromegas = Some(guard);
            layers.push(filtered(
                span_bridge::SpanBridge::new(micromegas::MicromegasExport::new(t.traces), policy),
                span_bridge::is_exported_span,
            ));
            layers.push(filtered(
                micromegas::capture_layer(),
                span_bridge::is_exported_event,
            ));
        }
        #[cfg(feature = "telemetry-otlp")]
        TelemetryBackend::Otlp => {
            let built = otlp::build::<S>(
                otlp::Settings {
                    endpoint,
                    api_key,
                    traces: t.traces,
                    resource_attributes: resource_attributes(config),
                },
                policy.clone(),
            )?;
            layers.extend(built.tracing);
            layers.push(filtered(
                span_bridge::SpanBridge::new(built.export, policy),
                span_bridge::is_exported_span,
            ));
            telemetry.otlp = Some(built.providers);
        }
        #[allow(unreachable_patterns)]
        other => {
            let _ = (endpoint, api_key, policy, resource_attributes(config));
            return Err(format!(
                "the {other:?} backend is not compiled into this build"
            ));
        }
    }
    Ok((telemetry, layers))
}

/// Installs the global registry: the debug layer plus, when `[telemetry]` is enabled, one
/// export backend. A backend that cannot start prints one notice and the run continues with
/// export off.
pub fn install(sink: Option<&DebugSink>, config: &Config, params: &InstallParams<'_>) -> Telemetry {
    let base = tracing_subscriber::registry().with(sink.map(|s| s.layer()));
    let (telemetry, layers) = if config.telemetry.enabled {
        match backend(config, params) {
            Ok(built) => built,
            Err(why) => {
                eprintln!("fontaine: telemetry export is off: {why}");
                (Telemetry::default(), Vec::new())
            }
        }
    } else {
        (Telemetry::default(), Vec::new())
    };
    // A global subscriber may already be set (e.g. by an embedding host); keep the first subscriber.
    let _ = base.with(layers).try_init();
    telemetry
}

impl Telemetry {
    /// A multi-thread runtime. With the micromegas backend its workers get thread streams,
    /// which the span events ride, and flush them as they stop.
    pub fn runtime(&self) -> std::io::Result<tokio::runtime::Runtime> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();
        #[cfg(feature = "telemetry-micromegas")]
        if self.micromegas.is_some() {
            use micromegas_tracing::runtime::TracingRuntimeExt;
            builder.with_tracing_callbacks();
        }
        builder.build()
    }

    /// Flushes and shuts the exporters down, giving up after the flush cap. A fast flush
    /// returns normally; if it runs past `FLUSH_CAP` a watchdog force-exits with the run's code.
    pub fn finish(self, exit_code: u8) {
        if !self.is_active() {
            return;
        }
        // The exporters can wait on a collector that never answers; past the cap the run's own
        // exit code is the only thing left to deliver.
        std::thread::spawn(move || {
            std::thread::sleep(FLUSH_CAP);
            std::process::exit(i32::from(exit_code));
        });
        #[cfg(feature = "telemetry-otlp")]
        if let Some(providers) = self.otlp {
            providers.shutdown(FLUSH_CAP);
        }
        #[cfg(feature = "telemetry-micromegas")]
        drop(self.micromegas);
    }

    fn is_active(&self) -> bool {
        #[allow(unused_mut)]
        let mut active = false;
        #[cfg(feature = "telemetry-micromegas")]
        {
            active |= self.micromegas.is_some();
        }
        #[cfg(feature = "telemetry-otlp")]
        {
            active |= self.otlp.is_some();
        }
        active
    }
}
