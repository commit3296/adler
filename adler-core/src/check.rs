//! Verdict types produced when a site is probed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Outcome of a single site probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// The account exists on this site.
    Found,
    /// The account does not exist on this site.
    NotFound,
    /// The response was inconclusive (network error, unexpected status,
    /// ambiguous content). Reported separately so the user can review them
    /// rather than silently dropping signal.
    Uncertain,
}

impl MatchKind {
    /// True if the verdict represents a positive (existing) account.
    pub const fn is_found(self) -> bool {
        matches!(self, Self::Found)
    }
}

/// Why a probe was inconclusive.
///
/// `Uncertain` outcomes carry a typed reason rather than a free-form string,
/// so logic that reacts to specific cases (e.g. retry on a transient ban)
/// matches an enum variant instead of a fragile string. The [`fmt::Display`]
/// rendering is what the CLI prints; serialization is the externally-tagged
/// default (unit variants → a `snake_case` string, detail-carrying variants →
/// `{ "network": "…" }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertainReason {
    /// HTTP 429, or 503 with a `Retry-After` header.
    RateLimited,
    /// A Cloudflare interstitial / "checking your browser" page.
    CloudflareChallenge,
    /// A captcha gate.
    Captcha,
    /// The path is disallowed by the host's `robots.txt` (`--respect-robots`).
    RobotsDisallowed,
    /// The scan deadline elapsed before this site finished.
    Deadline,
    /// The executor's scheduler was closed (does not happen in practice).
    SchedulerClosed,
    /// A transport/network error while issuing the request.
    Network(String),
    /// An error reading the response body.
    BodyRead(String),
    /// A `bot-protected` site needed the browser backend but the per-scan
    /// `--browser-budget` cap was already spent on earlier sites.
    BrowserBudget,
    /// The browser backend itself failed (timeout, navigation error,
    /// session drop, …) for a `bot-protected` site.
    BrowserFailed(String),
    /// Any other reason (e.g. a `doctor` pre-flight skip).
    Other(String),
}

impl fmt::Display for UncertainReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => f.write_str("rate_limited"),
            Self::CloudflareChallenge => f.write_str("cloudflare_challenge"),
            Self::Captcha => f.write_str("captcha"),
            Self::RobotsDisallowed => f.write_str("robots_disallowed"),
            Self::Deadline => f.write_str("deadline reached"),
            Self::SchedulerClosed => f.write_str("scheduler closed"),
            Self::Network(detail) => write!(f, "request: {detail}"),
            Self::BodyRead(detail) => write!(f, "body read: {detail}"),
            Self::BrowserBudget => f.write_str("browser_budget_exceeded"),
            Self::BrowserFailed(detail) => write!(f, "browser: {detail}"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

/// Result of probing a single site for a username.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutcome {
    /// Site name (matches `Site::name`).
    pub site: String,
    /// Concrete URL that was requested.
    pub url: String,
    /// Verdict produced by the site's detection strategy.
    pub kind: MatchKind,
    /// Why the outcome is `Uncertain`, if it is. `None` for `Found` /
    /// `NotFound`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UncertainReason>,
    /// Wall-clock duration of the probe.
    pub elapsed_ms: u64,
    /// Fields extracted from a `Found` profile when `--enrich` is active
    /// (e.g. `name`, `bio`, `avatar`). Empty unless enrichment ran and the
    /// site has extractor rules. Ordered by field name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enrichment: BTreeMap<String, String>,
    /// Human-readable descriptions of the signals that produced the verdict —
    /// e.g. `"HTTP 404 (status_not_found)"`. Empty for `Uncertain` (no signal
    /// fired). Surfaced by `--explain`; always present in JSON output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_kind_serialises_snake_case() {
        assert_eq!(
            serde_json::to_string(&MatchKind::Found).unwrap(),
            "\"found\""
        );
        assert_eq!(
            serde_json::to_string(&MatchKind::NotFound).unwrap(),
            "\"not_found\""
        );
        assert_eq!(
            serde_json::to_string(&MatchKind::Uncertain).unwrap(),
            "\"uncertain\""
        );
    }

    #[test]
    fn match_kind_is_found() {
        assert!(MatchKind::Found.is_found());
        assert!(!MatchKind::NotFound.is_found());
        assert!(!MatchKind::Uncertain.is_found());
    }

    #[test]
    fn outcome_skips_absent_reason() {
        let outcome = CheckOutcome {
            site: "GitHub".into(),
            url: "https://github.com/alice".into(),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 42,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(
            !json.contains("reason"),
            "reason field must be omitted when None"
        );
        assert!(
            !json.contains("enrichment"),
            "enrichment must be omitted when empty"
        );
        assert!(json.contains("\"kind\":\"found\""));
        assert!(json.contains("\"elapsed_ms\":42"));
    }

    #[test]
    fn unit_reason_serialises_as_snake_case_string() {
        let outcome = CheckOutcome {
            site: "GitHub".into(),
            url: "https://github.com/alice".into(),
            kind: MatchKind::Uncertain,
            reason: Some(UncertainReason::RateLimited),
            elapsed_ms: 5_000,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"reason\":\"rate_limited\""), "{json}");
    }

    #[test]
    fn detail_reason_serialises_as_tagged_object() {
        let json = serde_json::to_string(&UncertainReason::Network("refused".into())).unwrap();
        assert_eq!(json, "{\"network\":\"refused\"}");
    }

    #[test]
    fn reason_display_matches_legacy_note_text() {
        assert_eq!(UncertainReason::RateLimited.to_string(), "rate_limited");
        assert_eq!(UncertainReason::Deadline.to_string(), "deadline reached");
        assert_eq!(
            UncertainReason::Network("boom".into()).to_string(),
            "request: boom"
        );
    }
}
