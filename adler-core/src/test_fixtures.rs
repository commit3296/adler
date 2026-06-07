//! Shared test fixtures for inner modules.
//!
//! Crate-internal — not exposed beyond `#[cfg(test)]`. The helpers
//! here capture the cross-module test scaffolding that `client.rs`,
//! `doctor.rs`, and `executor.rs` each had near-identical copies of:
//! a deterministic [`Client`] for `wiremock`-driven tests, and a
//! default [`Site`] builder that callers customise field-by-field
//! before use.

use std::time::Duration;

use crate::client::{Client, ClientBuilder};
use crate::site::{HttpMethod, Site, UrlTemplate};

/// Build a [`Client`] tuned for unit tests:
///
/// - 2-second timeout so a hung mock fails fast,
/// - no retry budget (each test owns its own retry logic),
/// - no min-request-interval throttle (tests share `127.0.0.1` so
///   the throttle would serialise calls that should run in parallel).
///
/// The caller may keep tuning via the returned builder before calling
/// `.build()`; the no-arg [`test_client`] is the common shortcut.
pub(crate) fn test_client_builder() -> ClientBuilder {
    Client::builder()
        .timeout(Duration::from_secs(2))
        .min_request_interval(Duration::ZERO)
        .max_retries(0)
}

/// Default [`Client`] for tests — `unwrap()` is fine here because the
/// failure modes (DNS, TLS init) don't apply to a no-network config.
pub(crate) fn test_client() -> Client {
    test_client_builder().build().expect("test client builds")
}

/// A [`Site`] with all the non-signal fields populated to harmless
/// defaults. Callers fill `signals` (and anything else they need to
/// vary) inline.
///
/// `url_template` is the full `{username}`-substitutable URL — the
/// caller's mock server URI plus whatever path it serves at, e.g.
/// `format!("{}/{{username}}", server.uri())`.
pub(crate) fn default_site(name: &str, url_template: &str) -> Site {
    Site {
        name: name.into(),
        url: UrlTemplate::new(url_template)
            .expect("test_fixtures::default_site: caller passed a malformed URL template"),
        signals: Vec::new(),
        known_present: None,
        known_absent: None,
        extract: Vec::new(),
        tags: Vec::new(),
        request_headers: std::collections::BTreeMap::new(),
        regex_check: None,
        engine: None,
        strip_bad_char: None,
        request_method: HttpMethod::Get,
        request_body: None,
        protection: Vec::new(),
        disabled: false,
        disabled_reason: None,
        source: None,
        popularity: None,
        access: crate::AccessPolicy::default(),
    }
}
