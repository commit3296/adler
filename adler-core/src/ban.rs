//! Heuristics for spotting "you've been rate-limited / challenged" responses.
//!
//! A scan that hits 429 or a Cloudflare interstitial should not silently
//! return `NotFound` — that would let bans masquerade as "user does not
//! exist" and skew downstream counts. Each detected ban becomes a
//! [`MatchKind::Uncertain`](crate::MatchKind::Uncertain) outcome with a
//! short, machine-readable `note` so the user can spot the pattern and
//! either back off or rotate proxies.
//!
//! Detection happens in two stages:
//! - **Pre-body**: status code + select headers. Always runs.
//! - **In-body**: a few well-known interstitial substrings. Only consulted
//!   when the body was already going to be read for an existing signal.
//!   This keeps the no-body fast path free of extra reads.

use reqwest::header::HeaderMap;

use crate::check::UncertainReason;

/// Pre-body checks: 429, 503 + `Retry-After`, Cloudflare server header.
///
/// `&HeaderMap` is borrowed because we'd otherwise have to clone it before
/// consuming the response — cheap enough to read on the spot.
pub(crate) fn detect_pre_body(status: u16, headers: &HeaderMap) -> Option<UncertainReason> {
    if status == 429 {
        return Some(UncertainReason::RateLimited);
    }
    if status == 503 && headers.contains_key("retry-after") {
        return Some(UncertainReason::RateLimited);
    }
    if (status == 502 || status == 503 || status == 520) && server_is_cloudflare(headers) {
        return Some(UncertainReason::CloudflareChallenge);
    }
    None
}

/// Body-level checks: well-known interstitial markers. Only invoked when the
/// body has already been read for a signal — never trigger an extra HTTP
/// body read on this path.
pub(crate) fn detect_in_body(body: &str) -> Option<UncertainReason> {
    // Markers below are matched as substrings. The first match wins; order
    // is by specificity, not popularity.
    const MARKERS: &[(&str, UncertainReason)] = &[
        ("Just a moment...", UncertainReason::CloudflareChallenge),
        (
            "Checking your browser before accessing",
            UncertainReason::CloudflareChallenge,
        ),
        (
            "cf-browser-verification",
            UncertainReason::CloudflareChallenge,
        ),
        ("captcha-bypass", UncertainReason::Captcha),
        (
            "Please enable cookies",
            UncertainReason::CloudflareChallenge,
        ),
    ];
    MARKERS
        .iter()
        .find(|(needle, _)| body.contains(*needle))
        .map(|(_, reason)| reason.clone())
}

fn server_is_cloudflare(headers: &HeaderMap) -> bool {
    headers
        .get("server")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("cloudflare"))
        || headers.contains_key("cf-ray")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn pre_body_flags_429() {
        assert_eq!(
            detect_pre_body(429, &HeaderMap::new()),
            Some(UncertainReason::RateLimited)
        );
    }

    #[test]
    fn pre_body_flags_503_with_retry_after() {
        assert_eq!(
            detect_pre_body(503, &headers(&[("retry-after", "120")])),
            Some(UncertainReason::RateLimited)
        );
    }

    #[test]
    fn pre_body_does_not_flag_503_without_retry_after() {
        assert!(detect_pre_body(503, &HeaderMap::new()).is_none());
    }

    #[test]
    fn pre_body_flags_cloudflare_server_header() {
        assert_eq!(
            detect_pre_body(502, &headers(&[("server", "cloudflare")])),
            Some(UncertainReason::CloudflareChallenge)
        );
    }

    #[test]
    fn pre_body_flags_cf_ray_header() {
        assert_eq!(
            detect_pre_body(520, &headers(&[("cf-ray", "abc123-AMS")])),
            Some(UncertainReason::CloudflareChallenge)
        );
    }

    #[test]
    fn pre_body_ignores_normal_responses() {
        assert!(detect_pre_body(200, &HeaderMap::new()).is_none());
        assert!(detect_pre_body(404, &HeaderMap::new()).is_none());
        assert!(detect_pre_body(403, &HeaderMap::new()).is_none());
    }

    #[test]
    fn in_body_flags_cloudflare_interstitial() {
        assert_eq!(
            detect_in_body("<html>Just a moment...</html>"),
            Some(UncertainReason::CloudflareChallenge)
        );
    }

    #[test]
    fn in_body_flags_browser_check() {
        assert_eq!(
            detect_in_body("Please wait, Checking your browser before accessing reddit.com"),
            Some(UncertainReason::CloudflareChallenge)
        );
    }

    #[test]
    fn in_body_ignores_normal_html() {
        assert!(detect_in_body("<html><body><h1>Welcome</h1></body></html>").is_none());
    }
}
