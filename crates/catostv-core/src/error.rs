//! Minimal error type for the dependency-free core crate.
//!
//! The GUI crate uses `anyhow`; the core crate must not depend on it, so it
//! carries a small string-based error that converts cleanly into `anyhow`.

use std::fmt;

/// A human-readable error. Messages are shown to TV users in toasts and on
/// the installer error page, so write them as short plain-language sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Prefix the message with context, e.g. `"partitioning: sgdisk failed"`.
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self {
            message: format!("{context}: {}", self.message),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::msg(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `bail!("text {x}")` returns early with a core [`Error`].
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::msg(format!($($arg)*)))
    };
}

/// Extension trait to attach context to any `Result` whose error is displayable.
pub trait ResultExt<T> {
    fn context(self, context: impl fmt::Display) -> Result<T>;
}

impl<T, E: fmt::Display> ResultExt<T> for std::result::Result<T, E> {
    fn context(self, context: impl fmt::Display) -> Result<T> {
        self.map_err(|e| Error::msg(format!("{context}: {e}")))
    }
}
