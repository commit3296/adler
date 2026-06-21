//! Verdict types produced when a site is probed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::confidence::{ConfidenceScore, ConfidenceSignals};
use crate::profile::{ProfileEvidence, ProfileEvidenceKind};

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
    /// An AWS `CloudFront` edge block / challenge response.
    CloudfrontChallenge,
    /// A generic JavaScript/browser client challenge that is not tied to a
    /// known WAF vendor.
    ClientChallenge,
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
    /// The username doesn't satisfy the site's `regex_check`
    /// (e.g. too short, contains forbidden characters). Reported
    /// without issuing any HTTP request — saves both network and the
    /// false-positive class where the site 404s on illegal usernames
    /// in ways our signal can't tell apart from a missing account.
    UsernameNotAllowed,
    /// The browser backend itself failed (timeout, navigation error,
    /// session drop, …) for a `bot-protected` site.
    BrowserFailed(String),
    /// The site's [`AccessPolicy`](crate::AccessPolicy) requires an
    /// egress (country / IP type) that no configured proxy in the pool
    /// satisfies, so the probe was skipped rather than fetched from the
    /// wrong location. "Couldn't reach from the required geo" is not
    /// "account absent" — hence `Uncertain`, never `NotFound`.
    GeoUnavailable,
    /// The site's [`AccessPolicy`](crate::AccessPolicy) names a session
    /// (`access.session`) that wasn't supplied, so the probe was skipped
    /// rather than sent unauthenticated into a login wall — which reads
    /// the same for an existing and a missing account.
    SessionRequired,
    /// Any other reason (e.g. a `doctor` pre-flight skip).
    Other(String),
}

impl fmt::Display for UncertainReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => f.write_str("rate_limited"),
            Self::CloudflareChallenge => f.write_str("cloudflare_challenge"),
            Self::CloudfrontChallenge => f.write_str("cloudfront_challenge"),
            Self::ClientChallenge => f.write_str("client_challenge"),
            Self::Captcha => f.write_str("captcha"),
            Self::RobotsDisallowed => f.write_str("robots_disallowed"),
            Self::Deadline => f.write_str("deadline reached"),
            Self::SchedulerClosed => f.write_str("scheduler closed"),
            Self::Network(detail) => write!(f, "request: {detail}"),
            Self::BodyRead(detail) => write!(f, "body read: {detail}"),
            Self::BrowserBudget => f.write_str("browser_budget_exceeded"),
            Self::UsernameNotAllowed => f.write_str("username_not_allowed"),
            Self::BrowserFailed(detail) => write!(f, "browser: {detail}"),
            Self::GeoUnavailable => f.write_str("geo_unavailable"),
            Self::SessionRequired => f.write_str("session_required"),
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
    /// Normalized profile facts collected from extraction/enrichment. This is
    /// distinct from the legacy `evidence` field above: `evidence` explains
    /// the detection signal, while `profile_evidence` is structured product
    /// data for confidence scoring, identity clustering, timelines, and
    /// reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_evidence: Vec<ProfileEvidence>,
    /// Explainable confidence in this per-site verdict.
    #[serde(default)]
    pub confidence: ConfidenceScore,
    /// Which transport produced this outcome (HTTP / impersonate / browser).
    /// `None` only on outcomes from older persisted scans saved before this
    /// field existed; live scans always populate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<crate::escalation::TransportTier>,
    /// Number of *automatic* escalations to a heavier transport beyond the
    /// site's primary route — usually 0, at most 1 today (HTTP / impersonate
    /// → browser on `Uncertain(CloudflareChallenge | RateLimited)`).
    /// Stamped so the doctor can spot sites where the primary route
    /// systematically fails and the registry should pre-tag them.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub escalations: u8,
}

impl CheckOutcome {
    /// Recompute confidence after callers attach signal or profile evidence.
    pub fn refresh_confidence(&mut self) {
        self.refresh_confidence_with_history(0);
    }

    /// Recompute confidence with a derived historical-consistency overlay.
    ///
    /// Live scan paths should call [`Self::refresh_confidence`] so outcomes
    /// remain stateless. Persisted/history views can call this method after
    /// computing a non-persisted history count.
    pub fn refresh_confidence_with_history(&mut self, historical_consistency_count: usize) {
        let access_paths = self
            .profile_evidence
            .iter()
            .filter_map(|evidence| evidence.source.access_path.as_ref());
        let authenticated_access = access_paths.clone().any(|path| path.authenticated);
        let metadata_transport = access_paths.clone().map(|path| path.transport).next();
        let metadata_escalated = access_paths.clone().any(|path| path.escalated);
        let username_evidence_count = self
            .profile_evidence
            .iter()
            .filter(|evidence| evidence.kind == ProfileEvidenceKind::Username)
            .count();
        let non_metadata_evidence_count = self
            .profile_evidence
            .iter()
            .filter(|evidence| {
                matches!(
                    evidence.kind,
                    ProfileEvidenceKind::Username | ProfileEvidenceKind::AvatarHash
                )
            })
            .count();
        let profile_evidence_count = self
            .profile_evidence
            .len()
            .saturating_sub(non_metadata_evidence_count);
        self.confidence = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: self.kind,
            reason: self.reason.clone(),
            signal_evidence_count: self.evidence.len(),
            profile_evidence_count,
            username_evidence_count,
            historical_consistency_count,
            authenticated_access,
            transport: metadata_transport.or(self.transport),
            escalations: if metadata_escalated && self.escalations == 0 {
                1
            } else {
                self.escalations
            },
        });
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u8(n: &u8) -> bool {
    *n == 0
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
            profile_evidence: Vec::new(),
            confidence: ConfidenceScore::default(),
            transport: None,
            escalations: 0,
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
        assert!(
            !json.contains("transport"),
            "transport must be omitted when None"
        );
        assert!(
            !json.contains("escalations"),
            "escalations must be omitted when zero"
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
            profile_evidence: Vec::new(),
            confidence: ConfidenceScore::default(),
            transport: None,
            escalations: 0,
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
        assert_eq!(
            UncertainReason::CloudfrontChallenge.to_string(),
            "cloudfront_challenge"
        );
        assert_eq!(
            UncertainReason::ClientChallenge.to_string(),
            "client_challenge"
        );
        assert_eq!(UncertainReason::Deadline.to_string(), "deadline reached");
        assert_eq!(
            UncertainReason::Network("boom".into()).to_string(),
            "request: boom"
        );
    }

    #[test]
    fn old_outcome_json_defaults_confidence_and_profile_evidence() {
        let json = r#"{
            "site": "GitHub",
            "url": "https://github.com/alice",
            "kind": "found",
            "elapsed_ms": 42
        }"#;
        let mut outcome: CheckOutcome = serde_json::from_str(json).unwrap();
        assert!(outcome.profile_evidence.is_empty());
        assert_eq!(outcome.confidence, ConfidenceScore::default());
        outcome.refresh_confidence();
        assert_eq!(outcome.confidence.score, 65);
    }

    #[test]
    fn avatar_hash_evidence_does_not_count_as_profile_metadata() {
        let mut outcome = CheckOutcome {
            site: "Example".to_owned(),
            url: "https://example.com/alice".to_owned(),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 10,
            enrichment: std::collections::BTreeMap::new(),
            evidence: vec!["HTTP 200 (status_found)".to_owned()],
            profile_evidence: vec![ProfileEvidence::from_avatar_hash(
                "Example",
                "https://example.com/alice",
                "dhash64_v1:0123456789abcdef",
                Some(100),
                None,
            )],
            confidence: ConfidenceScore::default(),
            transport: Some(crate::TransportTier::Http),
            escalations: 0,
        };

        outcome.refresh_confidence();

        assert_eq!(outcome.confidence.score, 70);
        assert!(
            outcome
                .confidence
                .reasons
                .contains(&crate::ConfidenceReason::WeakStatusOnly)
        );
        assert!(!outcome.confidence.reasons.iter().any(|reason| {
            matches!(
                reason,
                crate::ConfidenceReason::ProfileMetadataExtracted { .. }
            )
        }));
    }
}
