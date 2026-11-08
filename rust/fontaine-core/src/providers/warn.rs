//! Where a provider's advisory warnings go. Clients are built before a session's `Approver`
//! exists, so the handle looks the sink up when a warning is emitted, not when it is created.

use fontaine_tools::lock::lock;
use std::sync::{Arc, Mutex, OnceLock};

type Sink = Arc<dyn Fn(&str) + Send + Sync>;

fn process_sink() -> &'static Mutex<Option<Sink>> {
    static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    SINK.get_or_init(Default::default)
}

/// Where warnings are reported. Token sources and clients outlive any one session, so the sink is
/// process-wide and the latest session wins.
pub fn set_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    *lock(process_sink(), "warning sink") = Some(Arc::new(sink));
}

#[derive(Clone)]
pub struct Warn(Option<Sink>);

impl Warn {
    /// Late-bound: reads the process-wide sink on every `emit`.
    pub fn process() -> Warn {
        Warn(None)
    }

    /// Pinned to one sink, for tests and callers that own their destination.
    pub fn to(sink: impl Fn(&str) + Send + Sync + 'static) -> Warn {
        Warn(Some(Arc::new(sink)))
    }

    /// The sink if any, else `tracing::warn!`.
    pub fn emit(&self, message: &str) {
        let sink = match &self.0 {
            Some(sink) => Some(sink.clone()),
            None => lock(process_sink(), "warning sink").clone(),
        };
        match sink {
            Some(sink) => sink(message),
            None => tracing::warn!("{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_calls_the_pinned_sink() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let warn = Warn::to(move |m| sink.lock().unwrap().push(m.to_owned()));
        warn.emit("one");
        assert_eq!(*seen.lock().unwrap(), ["one"]);
    }
}
