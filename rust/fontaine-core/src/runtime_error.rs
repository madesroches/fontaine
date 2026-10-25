use std::error::Error;
use std::fmt;

/// A runtime failure that keeps its cause. Display is only the context, so rendering with
/// `fontaine_tools::error_chain::chain` shows each level once.
#[derive(Debug)]
pub struct RuntimeError {
    context: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl RuntimeError {
    pub fn new(context: impl Into<String>) -> Self {
        RuntimeError {
            context: context.into(),
            source: None,
        }
    }

    pub fn caused(context: impl Into<String>, source: impl Error + Send + Sync + 'static) -> Self {
        RuntimeError {
            context: context.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.context)
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontaine_tools::error_chain::chain;

    #[test]
    fn display_is_only_the_context() {
        let inner = std::io::Error::other("disk full");
        let e = RuntimeError::caused("could not save", inner);
        assert_eq!(e.to_string(), "could not save");
    }

    #[test]
    fn source_is_the_cause() {
        let e = RuntimeError::caused("outer", std::io::Error::other("inner"));
        assert_eq!(e.source().expect("has a cause").to_string(), "inner");
        assert!(RuntimeError::new("alone").source().is_none());
    }

    #[test]
    fn chain_shows_every_level_once() {
        let mid = RuntimeError::caused("middle", std::io::Error::other("root"));
        let e = RuntimeError::caused("top", mid);
        assert_eq!(chain(&e), "top: middle: root");
        assert_eq!(chain(&RuntimeError::new("alone")), "alone");
    }
}
