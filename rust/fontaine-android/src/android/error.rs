//! The activity's error type: a message that keeps the EGL or camera status it came from as its
//! `source()`, rendered once with `chain` where it is logged.

use std::error::Error as StdError;
use std::fmt;

#[derive(Debug)]
pub struct Error {
    message: String,
    source: Option<Box<dyn StdError>>,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(message: impl Into<String>, source: impl StdError + 'static) -> Error {
        Error {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref()
    }
}

pub trait Context<T> {
    fn context(self, message: impl Into<String>) -> Result<T, Error>;
}

impl<T, E: StdError + 'static> Context<T> for Result<T, E> {
    fn context(self, message: impl Into<String>) -> Result<T, Error> {
        self.map_err(|e| Error {
            message: message.into(),
            source: Some(Box::new(e)),
        })
    }
}

/// A non-OK `camera_status_t`, which carries no message of its own.
#[derive(Debug)]
pub struct CameraStatus(pub i32);

impl fmt::Display for CameraStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "camera status {}", self.0)
    }
}

impl StdError for CameraStatus {}

/// The message followed by each cause, joined with `: `.
pub fn chain(error: &(dyn StdError + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
