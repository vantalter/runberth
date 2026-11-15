//! A single error type: every failure ends as one message on stderr.

use std::fmt;

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error(format!("invalid JSON: {error}"))
    }
}

/// Attach what was being done to a lower-level error.
pub trait Context<T> {
    fn context(self, what: impl fmt::Display) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|error| Error(format!("{what}: {error}")))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, what: impl fmt::Display) -> Result<T> {
        self.ok_or_else(|| Error(what.to_string()))
    }
}

#[macro_export]
macro_rules! bail {
    ($($message:tt)*) => {
        return Err($crate::error::Error(format!($($message)*)))
    };
}
