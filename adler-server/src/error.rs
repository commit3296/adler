//! Crate-level error type.

use std::io;

use thiserror::Error;

/// Errors surfaced by the public [`serve`](crate::serve) entry point.
///
/// All variants carry enough context (a path, the bind address, the
/// underlying I/O error) to be actionable on their own — no need to
/// chain back through the call site to figure out what went wrong.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Binding the TCP listener failed (port already in use, permission
    /// denied, …). The original I/O error is kept for diagnostics.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// Address we tried to bind.
        addr: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Loading the embedded site registry on startup failed. Practically
    /// unreachable since the JSON is shipped in the binary, but surfaced
    /// rather than swallowed so a corrupted custom override is visible.
    #[error("failed to load site registry: {0}")]
    Registry(#[from] adler_core::Error),

    /// The axum server task returned an error (typically because the
    /// listener was closed or accept failed).
    #[error("HTTP server error: {0}")]
    Server(#[source] io::Error),

    /// Reading or writing a persisted scan failed.
    #[error("persistence I/O error: {0}")]
    Persist(#[source] io::Error),

    /// Serialising a scan to JSON failed (should be unreachable —
    /// `CheckOutcome` is well-defined serde data).
    #[error("persistence encode error: {0}")]
    PersistEncode(#[source] serde_json::Error),
}

/// `Result<T, Error>` alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
