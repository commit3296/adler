//! Internal helpers shared by [`Client`](super::Client)'s probing,
//! fetch, and builder paths. Not exposed beyond the `client` module.

use std::time::Instant;

use crate::check::{CheckOutcome, MatchKind, UncertainReason};

/// Default `User-Agent` used by every Adler-built `reqwest::Client`.
/// Carries the crate version so a target site sees a stable identity
/// per release. Overridable via [`ClientBuilder::user_agent`](super::ClientBuilder::user_agent).
pub(super) fn default_user_agent() -> String {
    format!("adler/{}", env!("CARGO_PKG_VERSION"))
}

/// Best-effort host extraction for throttle keying and trace fields.
/// Falls back to the literal `"unknown"` so the throttle never panics
/// on a malformed URL.
pub(super) fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// Split a URL into its origin (`scheme://host[:port]`) and path-with-query,
/// for `robots.txt` lookup. `None` if the URL won't parse or lacks a host.
pub(super) fn origin_and_path(url: &str) -> Option<(String, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port().map_or_else(String::new, |p| format!(":{p}"));
    let origin = format!("{}://{host}{port}", parsed.scheme());
    let path = parsed.query().map_or_else(
        || parsed.path().to_owned(),
        |q| format!("{}?{q}", parsed.path()),
    );
    Some((origin, path))
}

/// Build a [`CheckOutcome`] with the given verdict and elapsed time;
/// `enrichment`, `evidence`, `transport`, `escalations` start blank
/// so callers fill only what they have.
pub(super) fn outcome(site: &str, url: String, started: Instant, kind: MatchKind) -> CheckOutcome {
    CheckOutcome {
        site: site.to_owned(),
        url,
        kind,
        reason: None,
        elapsed_ms: elapsed_ms(started),
        enrichment: std::collections::BTreeMap::new(),
        evidence: Vec::new(),
        transport: None,
        escalations: 0,
    }
}

/// Convenience constructor for an `Uncertain` outcome with a reason
/// attached. Mirrors [`outcome`] otherwise.
pub(super) fn uncertain(
    site: &str,
    url: String,
    started: Instant,
    reason: UncertainReason,
) -> CheckOutcome {
    CheckOutcome {
        site: site.to_owned(),
        url,
        kind: MatchKind::Uncertain,
        reason: Some(reason),
        elapsed_ms: elapsed_ms(started),
        enrichment: std::collections::BTreeMap::new(),
        evidence: Vec::new(),
        transport: None,
        escalations: 0,
    }
}

/// `Instant`-to-millis without panicking on the (~584M-year) overflow case.
pub(super) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
