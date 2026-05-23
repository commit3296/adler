use std::io;

/// Errors produced by the Adler engine.
///
/// New variants are added as subsystems land. The enum is
/// `#[non_exhaustive]` so growth is not a breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// I/O error while reading or writing local data (sites file, cache, etc.).
    // The message omits the source detail on purpose: the `#[from]` source is
    // surfaced by the error chain (anyhow's `{:#}`), so embedding `{0}` here
    // would print it twice.
    #[error("i/o error")]
    Io(#[from] io::Error),

    /// Failed to (de)serialize JSON — typically a site-definitions file.
    #[error("json error")]
    Json(#[from] serde_json::Error),

    /// The supplied string is not a valid username.
    ///
    /// The original input is preserved so callers can echo it back to the
    /// user without re-deriving it from the error message.
    #[error("invalid username {input:?}: {reason}")]
    InvalidUsername {
        /// The string the caller attempted to use as a username.
        input: String,
        /// Human-readable failure reason.
        reason: String,
    },

    /// A loaded site definition failed validation (bad template, empty marker, …).
    #[error("invalid site definition: {reason}")]
    InvalidSite {
        /// Human-readable failure reason, including the site name when available.
        reason: String,
    },

    /// Building the HTTP client failed (TLS init, bad config, …).
    ///
    /// Per-request errors do **not** surface here — they become
    /// [`MatchKind::Uncertain`](crate::MatchKind::Uncertain) so a single
    /// flaky site can't abort a full run.
    #[error("http client setup failed: {message}")]
    HttpSetup {
        /// Underlying error description (reqwest's error chain rendered).
        message: String,
    },
}

/// Result alias used throughout the engine.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts_via_from_and_keeps_source() {
        use std::error::Error as _;
        let io = io::Error::new(io::ErrorKind::NotFound, "missing sites file");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
        // Detail lives in the source (shown by the error chain), not the
        // top-level message — which avoids double-printing under `{:#}`.
        assert_eq!(err.to_string(), "i/o error");
        let source = err.source().expect("io error has a source");
        assert!(source.to_string().contains("missing sites file"));
    }

    #[test]
    fn json_error_converts_via_from() {
        let parse: serde_json::Error = serde_json::from_str::<i32>("not json").unwrap_err();
        let err: Error = parse.into();
        assert!(matches!(err, Error::Json(_)));
    }
}
