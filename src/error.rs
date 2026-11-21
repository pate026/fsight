//! Error types for the library.
//!
//! The library uses [`thiserror`] to define precise, matchable error enums.
//! The binary (`main.rs`) wraps them in [`anyhow::Error`] to add context and
//! turn them into a message and an exit code. This split is idiomatic Rust:
//! libraries expose *typed* errors that callers can inspect, applications
//! mostly just report them.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Convenient alias used throughout the library: `Result<T>` means
/// `Result<T, fsight::Error>` unless a different error type is given.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a size such as `10K` or `1.5G` could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SizeError {
    #[error("size is empty")]
    Empty,
    #[error("invalid number in '{0}' (expected e.g. 200B, 10K, 5M, 1.5G)")]
    InvalidNumber(String),
    #[error("unknown unit '{0}' (expected B, K, M, G, T, P or E)")]
    UnknownUnit(String),
}

/// Why a duration such as `2d` or `3h` could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DurationError {
    #[error("duration is empty")]
    Empty,
    #[error("invalid number in '{0}' (use whole numbers, e.g. 90m instead of 1.5h)")]
    InvalidNumber(String),
    #[error("missing unit in '{0}' (expected s, m, h, d, w or y)")]
    MissingUnit(String),
    #[error("unknown unit '{0}' (expected s, m, h, d, w or y)")]
    UnknownUnit(String),
    #[error("duration is too large")]
    Overflow,
}

/// The main error type of the library.
#[derive(Debug, Error)]
pub enum Error {
    /// The path given on the command line does not exist or cannot be read.
    #[error("cannot access '{}': {source}", path.display())]
    Path {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A glob or regular expression could not be compiled.
    #[error("invalid pattern: {0}")]
    Pattern(String),

    /// The configuration (file or environment variable) is invalid.
    #[error("{0}")]
    Config(String),

    /// Writing the results (usually to stdout) failed.
    #[error("cannot write output: {0}")]
    Output(#[source] io::Error),

    /// The terminal user interface failed.
    #[error("terminal error: {0}")]
    Terminal(#[source] io::Error),
}

impl Error {
    /// Returns `true` if this error only means "the reader of our output went
    /// away", e.g. `fsight find | head`. Such errors are not worth reporting.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Error::Output(err) if err.kind() == io::ErrorKind::BrokenPipe)
    }
}

impl From<regex::Error> for Error {
    fn from(err: regex::Error) -> Self {
        Error::Pattern(err.to_string())
    }
}

impl From<globset::Error> for Error {
    fn from(err: globset::Error) -> Self {
        Error::Pattern(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_is_recognised_only_for_output_errors() {
        let pipe = || io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed");
        assert!(Error::Output(pipe()).is_broken_pipe());
        assert!(!Error::Terminal(pipe()).is_broken_pipe());
        let other = Error::Output(io::Error::other("disk full"));
        assert!(!other.is_broken_pipe());
    }
}
