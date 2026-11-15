//! The failure of building or running an embedded session.

use std::error::Error;
use std::fmt;

type Source = Box<dyn Error + Send + Sync>;

/// What failed, and the cause it wraps. `Display` is the message alone; render the whole chain
/// with [`crate::tools::error_chain::chain`].
#[derive(Debug)]
pub struct Detail {
    message: String,
    source: Option<Source>,
    exit_code: u8,
}

impl Detail {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Why a session could not be built or stopped running. Each variant keeps its cause as
/// `source()`.
#[derive(Debug)]
#[non_exhaustive]
pub enum EmbedError {
    /// A builder argument or a settings file that is not acceptable.
    Config(Detail),
    /// The session's files could not be created, opened or resumed.
    Session(Detail),
    /// An account the session needs has nobody signed in.
    Credentials(Detail),
    /// The main model could not be resolved.
    Model(Detail),
    /// The runtime could not be built, or the run ended with a failure.
    Runtime(Detail),
}

impl EmbedError {
    fn detail(&self) -> &Detail {
        match self {
            EmbedError::Config(d)
            | EmbedError::Session(d)
            | EmbedError::Credentials(d)
            | EmbedError::Model(d)
            | EmbedError::Runtime(d) => d,
        }
    }

    /// The process exit code `fontaine` ends with for this failure: 2 for a usage or settings
    /// error, 1 for a failed run.
    pub fn exit_code(&self) -> u8 {
        self.detail().exit_code
    }

    pub(crate) fn config(message: impl Into<String>) -> EmbedError {
        EmbedError::Config(Detail {
            message: message.into(),
            source: None,
            exit_code: crate::core::config::CONFIG_EXIT_CODE,
        })
    }

    /// A settings or argument failure that wraps its cause. The message is the cause's own, so
    /// the chain shows it once.
    pub(crate) fn config_from(cause: impl Error + Send + Sync + 'static) -> EmbedError {
        EmbedError::Config(Detail {
            message: cause.to_string(),
            source: Some(Box::new(cause)),
            exit_code: crate::core::config::CONFIG_EXIT_CODE,
        })
    }

    /// Like [`EmbedError::config_from`], with a message of its own ahead of the cause.
    pub(crate) fn config_caused(
        message: impl Into<String>,
        cause: impl Error + Send + Sync + 'static,
    ) -> EmbedError {
        EmbedError::Config(Detail {
            message: message.into(),
            source: Some(Box::new(cause)),
            exit_code: crate::core::config::CONFIG_EXIT_CODE,
        })
    }

    pub(crate) fn session_from(
        cause: impl Error + Send + Sync + 'static,
        exit_code: u8,
    ) -> EmbedError {
        EmbedError::Session(Detail {
            message: cause.to_string(),
            source: Some(Box::new(cause)),
            exit_code,
        })
    }

    pub(crate) fn credentials(message: impl Into<String>) -> EmbedError {
        EmbedError::Credentials(Detail {
            message: message.into(),
            source: None,
            exit_code: 1,
        })
    }

    pub(crate) fn credentials_from(
        message: impl Into<String>,
        cause: impl Error + Send + Sync + 'static,
        exit_code: u8,
    ) -> EmbedError {
        EmbedError::Credentials(Detail {
            message: message.into(),
            source: Some(Box::new(cause)),
            exit_code,
        })
    }

    pub(crate) fn model_from(
        cause: impl Error + Send + Sync + 'static,
        exit_code: u8,
    ) -> EmbedError {
        EmbedError::Model(Detail {
            message: cause.to_string(),
            source: Some(Box::new(cause)),
            exit_code,
        })
    }

    pub(crate) fn runtime(message: impl Into<String>) -> EmbedError {
        EmbedError::Runtime(Detail {
            message: message.into(),
            source: None,
            exit_code: 1,
        })
    }

    pub(crate) fn runtime_from(cause: impl Error + Send + Sync + 'static) -> EmbedError {
        EmbedError::Runtime(Detail {
            message: cause.to_string(),
            source: Some(Box::new(cause)),
            exit_code: 1,
        })
    }
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail().message)
    }
}

impl Error for EmbedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.detail()
            .source
            .as_deref()
            .map(|e| e as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::error_chain::chain;

    #[test]
    fn a_wrapped_cause_is_shown_once_and_kept_as_the_source() {
        let e = EmbedError::config_from(std::io::Error::other("disk full"));
        assert_eq!(chain(&e), "disk full");
        assert!(e.source().is_some());
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn a_failed_run_exits_one() {
        assert_eq!(EmbedError::runtime("the run ended").exit_code(), 1);
        assert_eq!(EmbedError::credentials("not signed in").exit_code(), 1);
    }
}
