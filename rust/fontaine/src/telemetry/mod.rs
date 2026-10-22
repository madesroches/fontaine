//! The tracing facade: the one place a subscriber is built. Every other crate only emits
//! `tracing` spans and events.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fontaine_core::config::Config;
use tracing::Subscriber;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt as tfmt;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

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

/// The export layer built from loaded config. No exporter exists yet, so this is always
/// `None`; the slot is in the registry so enabling export never changes how it is composed.
pub fn export_layer<S>(_config: &Config) -> Option<BoxLayer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    None
}

/// Runs `f` with a subscriber that only has the debug layer, for use before config has been
/// loaded and the global registry can be built.
pub fn with_debug_only<T>(sink: Option<&DebugSink>, f: impl FnOnce() -> T) -> T {
    let subscriber = tracing_subscriber::registry().with(sink.map(|s| s.layer()));
    tracing::subscriber::with_default(subscriber, f)
}

/// Installs the global registry: debug layer plus the optional export layer.
pub fn install(sink: Option<&DebugSink>, config: &Config) {
    let registry = tracing_subscriber::registry()
        .with(sink.map(|s| s.layer()))
        .with(export_layer(config));
    // A second install in the same process (in-process tests) keeps the first subscriber.
    let _ = registry.try_init();
}
