//! Conservative confidence scoring for scan outcomes.
//!
//! This is intentionally rule-based and explainable. The goal is not to
//! claim identity-level certainty; it is to say how trustworthy Adler's
//! per-site verdict is and why.

use serde::{Deserialize, Serialize};

use crate::check::{MatchKind, UncertainReason};
use crate::escalation::TransportTier;

/// Confidence attached to a single scan outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfidenceScore {
    /// 0-100 confidence in the reported verdict.
    pub score: u8,
    /// Coarse label for humans and UI badges.
    pub label: ConfidenceLabel,
    /// Explainable reasons that contributed to the score.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<ConfidenceReason>,
}

/// Human-facing confidence bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLabel {
    /// Weak or inconclusive evidence.
    Low,
    /// Usable evidence, but not enough to treat the verdict as strong.
    Medium,
    /// Strong per-site evidence for the reported verdict.
    High,
    /// Reserved for future cases backed by explicit verification.
    Verified,
}

/// Machine-readable confidence rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfidenceReason {
    /// The site detection rules produced a positive verdict.
    FoundBySignal,
    /// The site detection rules produced a negative verdict.
    NotFoundBySignal,
    /// Profile metadata was extracted from the found page.
    ProfileMetadataExtracted {
        /// Number of normalized profile evidence items.
        count: usize,
    },
    /// Several normalized profile metadata fields were extracted.
    ProfileMetadataRich {
        /// Number of normalized profile evidence items.
        count: usize,
    },
    /// Human-readable detection signal evidence was recorded.
    SignalEvidence {
        /// Number of signal evidence lines.
        count: usize,
    },
    /// A detection signal confirmed the concrete probed username.
    ExactUsernameMatch {
        /// Number of exact username evidence items.
        count: usize,
    },
    /// The same site has been observed as a stable Found result in
    /// previous scans for this username.
    HistoricalConsistency {
        /// Number of previous stable Found observations.
        count: usize,
    },
    /// The result was produced with an operator-supplied authenticated
    /// access path.
    AuthenticatedAccess,
    /// A browser transport produced a clear verdict.
    BrowserTransport,
    /// An impersonating HTTP transport produced a clear verdict.
    ImpersonateTransport,
    /// A cheap transport was automatically escalated to a heavier transport
    /// that produced a clear verdict.
    EscalatedTransport,
    /// The verdict is based on one weak status-only signal and no supporting
    /// profile/access evidence.
    WeakStatusOnly,
    /// The probe could not produce a found/not-found verdict.
    UncertainOutcome,
    /// The site needs an operator session before Adler can judge presence.
    SessionRequired,
    /// Transport/access conditions blocked a reliable probe.
    TransportBlocked,
}

/// Normalized scoring input built from a [`crate::CheckOutcome`].
#[derive(Debug, Clone)]
pub(crate) struct ConfidenceSignals {
    pub(crate) kind: MatchKind,
    pub(crate) reason: Option<UncertainReason>,
    pub(crate) signal_evidence_count: usize,
    pub(crate) profile_evidence_count: usize,
    pub(crate) username_evidence_count: usize,
    pub(crate) historical_consistency_count: usize,
    pub(crate) authenticated_access: bool,
    pub(crate) transport: Option<TransportTier>,
    pub(crate) escalations: u8,
}

impl Default for ConfidenceScore {
    fn default() -> Self {
        Self {
            score: 0,
            label: ConfidenceLabel::Low,
            reasons: Vec::new(),
        }
    }
}

impl ConfidenceScore {
    /// Score an outcome from the pieces available on `CheckOutcome`.
    #[must_use]
    pub fn from_parts(
        kind: MatchKind,
        reason: Option<&UncertainReason>,
        signal_evidence_count: usize,
        profile_evidence_count: usize,
    ) -> Self {
        Self::from_signals(&ConfidenceSignals {
            kind,
            reason: reason.cloned(),
            signal_evidence_count,
            profile_evidence_count,
            username_evidence_count: 0,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: None,
            escalations: 0,
        })
    }

    /// Score an outcome from normalized confidence signals.
    #[must_use]
    pub(crate) fn from_signals(signals: &ConfidenceSignals) -> Self {
        let mut score = ConfidenceAccumulator::new(base_score(signals.kind));
        score.add_reason(base_reason(signals.kind));

        apply_evidence_rules(signals, &mut score);
        apply_profile_rules(signals, &mut score);
        apply_verification_rules(signals, &mut score);
        apply_access_rules(signals, &mut score);
        apply_cap_rules(signals, &mut score);
        apply_uncertain_reason_rules(signals, &mut score);

        score.finish()
    }
}

struct ConfidenceAccumulator {
    score: u8,
    reasons: Vec<ConfidenceReason>,
}

impl ConfidenceAccumulator {
    const fn new(score: u8) -> Self {
        Self {
            score,
            reasons: Vec::new(),
        }
    }

    fn add_score(&mut self, value: u8) {
        self.score = self.score.saturating_add(value);
    }

    fn add_reason(&mut self, reason: ConfidenceReason) {
        self.reasons.push(reason);
    }

    fn add_score_and_reason(&mut self, value: u8, reason: ConfidenceReason) {
        self.add_score(value);
        self.add_reason(reason);
    }

    fn cap_score(&mut self, cap: u8) {
        self.score = self.score.min(cap);
    }

    fn force_score(&mut self, value: u8) {
        self.score = value;
    }

    fn finish(mut self) -> ConfidenceScore {
        self.cap_score(100);
        ConfidenceScore {
            score: self.score,
            label: ConfidenceLabel::from_score(self.score),
            reasons: self.reasons,
        }
    }
}

const fn base_score(kind: MatchKind) -> u8 {
    match kind {
        MatchKind::Found => 65,
        MatchKind::NotFound => 60,
        MatchKind::Uncertain => 15,
    }
}

const fn base_reason(kind: MatchKind) -> ConfidenceReason {
    match kind {
        MatchKind::Found => ConfidenceReason::FoundBySignal,
        MatchKind::NotFound => ConfidenceReason::NotFoundBySignal,
        MatchKind::Uncertain => ConfidenceReason::UncertainOutcome,
    }
}

fn apply_evidence_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    if signals.signal_evidence_count > 0 {
        score.add_score_and_reason(
            10,
            ConfidenceReason::SignalEvidence {
                count: signals.signal_evidence_count,
            },
        );
    }
}

fn apply_profile_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    if signals.profile_evidence_count > 0 {
        score.add_score_and_reason(
            10,
            ConfidenceReason::ProfileMetadataExtracted {
                count: signals.profile_evidence_count,
            },
        );
    }

    if signals.profile_evidence_count >= 3 {
        score.add_score_and_reason(
            5,
            ConfidenceReason::ProfileMetadataRich {
                count: signals.profile_evidence_count,
            },
        );
    }
}

fn apply_verification_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    if signals.kind == MatchKind::Found && signals.username_evidence_count > 0 {
        score.add_score_and_reason(
            10,
            ConfidenceReason::ExactUsernameMatch {
                count: signals.username_evidence_count,
            },
        );
    }

    if signals.kind == MatchKind::Found && signals.historical_consistency_count >= 2 {
        score.add_score_and_reason(
            4,
            ConfidenceReason::HistoricalConsistency {
                count: signals.historical_consistency_count,
            },
        );
    }
}

fn apply_access_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    if signals.authenticated_access && signals.kind != MatchKind::Uncertain {
        score.add_score_and_reason(10, ConfidenceReason::AuthenticatedAccess);
    }

    if signals.kind == MatchKind::Uncertain {
        return;
    }

    match signals.transport {
        Some(TransportTier::Browser) => {
            score.add_score_and_reason(5, ConfidenceReason::BrowserTransport);
        }
        Some(TransportTier::Impersonate) => {
            score.add_score_and_reason(5, ConfidenceReason::ImpersonateTransport);
        }
        Some(TransportTier::Http) | None => {}
    }

    if signals.escalations > 0 {
        score.add_score_and_reason(10, ConfidenceReason::EscalatedTransport);
    }
}

fn apply_cap_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    if is_weak_status_only(signals) {
        score.cap_score(70);
        score.add_reason(ConfidenceReason::WeakStatusOnly);
    }
}

fn apply_uncertain_reason_rules(signals: &ConfidenceSignals, score: &mut ConfidenceAccumulator) {
    let Some(reason) = &signals.reason else {
        return;
    };

    match reason {
        UncertainReason::SessionRequired => {
            score.force_score(0);
            score.add_reason(ConfidenceReason::SessionRequired);
        }
        UncertainReason::CloudflareChallenge
        | UncertainReason::ClientChallenge
        | UncertainReason::Captcha
        | UncertainReason::RateLimited
        | UncertainReason::BrowserBudget
        | UncertainReason::BrowserFailed(_)
        | UncertainReason::GeoUnavailable => {
            score.cap_score(20);
            score.add_reason(ConfidenceReason::TransportBlocked);
        }
        _ => {}
    }
}

fn is_weak_status_only(signals: &ConfidenceSignals) -> bool {
    matches!(signals.kind, MatchKind::Found | MatchKind::NotFound)
        && signals.signal_evidence_count == 1
        && signals.profile_evidence_count == 0
        && signals.username_evidence_count == 0
        && !signals.authenticated_access
        && signals.escalations == 0
        && matches!(signals.transport, Some(TransportTier::Http) | None)
}

impl ConfidenceLabel {
    /// Convert a numeric 0-100 score into a coarse confidence label.
    #[must_use]
    pub const fn from_score(score: u8) -> Self {
        match score {
            90..=100 => Self::Verified,
            75..=89 => Self::High,
            40..=74 => Self::Medium,
            _ => Self::Low,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_with_signal_and_profile_metadata_scores_high() {
        let score = ConfidenceScore::from_parts(MatchKind::Found, None, 1, 2);
        assert_eq!(score.score, 85);
        assert_eq!(score.label, ConfidenceLabel::High);
        assert!(matches!(
            score.reasons.as_slice(),
            [
                ConfidenceReason::FoundBySignal,
                ConfidenceReason::SignalEvidence { count: 1 },
                ConfidenceReason::ProfileMetadataExtracted { count: 2 },
            ]
        ));
    }

    #[test]
    fn status_only_found_is_capped_as_medium_confidence() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 0,
            username_evidence_count: 0,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });

        assert_eq!(score.score, 70);
        assert_eq!(score.label, ConfidenceLabel::Medium);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::WeakStatusOnly))
        );
    }

    #[test]
    fn authenticated_found_scores_higher_than_unauthenticated() {
        let base = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 1,
            username_evidence_count: 0,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });
        let authed = ConfidenceScore::from_signals(&ConfidenceSignals {
            authenticated_access: true,
            ..ConfidenceSignals {
                kind: MatchKind::Found,
                reason: None,
                signal_evidence_count: 1,
                profile_evidence_count: 1,
                username_evidence_count: 0,
                historical_consistency_count: 0,
                authenticated_access: false,
                transport: Some(TransportTier::Http),
                escalations: 0,
            }
        });

        assert!(authed.score > base.score);
        assert!(
            authed
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::AuthenticatedAccess))
        );
    }

    #[test]
    fn escalated_browser_success_records_transport_reasons() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 0,
            username_evidence_count: 0,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: Some(TransportTier::Browser),
            escalations: 1,
        });

        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::BrowserTransport))
        );
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::EscalatedTransport))
        );
    }

    #[test]
    fn transport_blocked_outcome_remains_low_confidence() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Uncertain,
            reason: Some(UncertainReason::ClientChallenge),
            signal_evidence_count: 0,
            profile_evidence_count: 0,
            username_evidence_count: 0,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });

        assert_eq!(score.label, ConfidenceLabel::Low);
        assert!(score.score <= 20);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::TransportBlocked))
        );
    }

    #[test]
    fn session_required_is_low_confidence_about_presence() {
        let score = ConfidenceScore::from_parts(
            MatchKind::Uncertain,
            Some(&UncertainReason::SessionRequired),
            0,
            0,
        );
        assert_eq!(score.score, 0);
        assert_eq!(score.label, ConfidenceLabel::Low);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::SessionRequired))
        );
    }

    #[test]
    fn exact_username_match_boosts_found_without_profile_metadata_reason() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 0,
            username_evidence_count: 1,
            historical_consistency_count: 0,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });

        assert_eq!(score.score, 85);
        assert_eq!(score.label, ConfidenceLabel::High);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::ExactUsernameMatch { count: 1 }))
        );
        assert!(!score.reasons.iter().any(|r| matches!(
            r,
            ConfidenceReason::ProfileMetadataExtracted { .. }
                | ConfidenceReason::ProfileMetadataRich { .. }
        )));
    }

    #[test]
    fn historical_consistency_boosts_found_after_two_prior_observations() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 1,
            username_evidence_count: 0,
            historical_consistency_count: 2,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });

        assert_eq!(score.score, 89);
        assert_eq!(score.label, ConfidenceLabel::High);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::HistoricalConsistency { count: 2 }))
        );
    }

    #[test]
    fn weak_status_only_remains_medium_with_history() {
        let score = ConfidenceScore::from_signals(&ConfidenceSignals {
            kind: MatchKind::Found,
            reason: None,
            signal_evidence_count: 1,
            profile_evidence_count: 0,
            username_evidence_count: 0,
            historical_consistency_count: 3,
            authenticated_access: false,
            transport: Some(TransportTier::Http),
            escalations: 0,
        });

        assert_eq!(score.score, 70);
        assert_eq!(score.label, ConfidenceLabel::Medium);
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::HistoricalConsistency { count: 3 }))
        );
        assert!(
            score
                .reasons
                .iter()
                .any(|r| matches!(r, ConfidenceReason::WeakStatusOnly))
        );
    }
}
