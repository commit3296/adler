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
            authenticated_access: false,
            transport: None,
            escalations: 0,
        })
    }

    /// Score an outcome from normalized confidence signals.
    #[must_use]
    pub(crate) fn from_signals(signals: &ConfidenceSignals) -> Self {
        let mut score: u8 = match signals.kind {
            MatchKind::Found => 65,
            MatchKind::NotFound => 60,
            MatchKind::Uncertain => 15,
        };
        let mut reasons = Vec::new();

        match signals.kind {
            MatchKind::Found => reasons.push(ConfidenceReason::FoundBySignal),
            MatchKind::NotFound => reasons.push(ConfidenceReason::NotFoundBySignal),
            MatchKind::Uncertain => reasons.push(ConfidenceReason::UncertainOutcome),
        }

        if signals.signal_evidence_count > 0 {
            score = score.saturating_add(10);
            reasons.push(ConfidenceReason::SignalEvidence {
                count: signals.signal_evidence_count,
            });
        }

        if signals.profile_evidence_count > 0 {
            score = score.saturating_add(10);
            reasons.push(ConfidenceReason::ProfileMetadataExtracted {
                count: signals.profile_evidence_count,
            });
        }

        if signals.profile_evidence_count >= 3 {
            score = score.saturating_add(5);
            reasons.push(ConfidenceReason::ProfileMetadataRich {
                count: signals.profile_evidence_count,
            });
        }

        if signals.authenticated_access && signals.kind != MatchKind::Uncertain {
            score = score.saturating_add(10);
            reasons.push(ConfidenceReason::AuthenticatedAccess);
        }

        if signals.kind != MatchKind::Uncertain {
            match signals.transport {
                Some(TransportTier::Browser) => {
                    score = score.saturating_add(5);
                    reasons.push(ConfidenceReason::BrowserTransport);
                }
                Some(TransportTier::Impersonate) => {
                    score = score.saturating_add(5);
                    reasons.push(ConfidenceReason::ImpersonateTransport);
                }
                Some(TransportTier::Http) | None => {}
            }
            if signals.escalations > 0 {
                score = score.saturating_add(10);
                reasons.push(ConfidenceReason::EscalatedTransport);
            }
        }

        if is_weak_status_only(signals) {
            score = score.min(70);
            reasons.push(ConfidenceReason::WeakStatusOnly);
        }

        if let Some(reason) = &signals.reason {
            match reason {
                UncertainReason::SessionRequired => {
                    score = 0;
                    reasons.push(ConfidenceReason::SessionRequired);
                }
                UncertainReason::CloudflareChallenge
                | UncertainReason::Captcha
                | UncertainReason::RateLimited
                | UncertainReason::BrowserBudget
                | UncertainReason::BrowserFailed(_)
                | UncertainReason::GeoUnavailable => {
                    score = score.min(20);
                    reasons.push(ConfidenceReason::TransportBlocked);
                }
                _ => {}
            }
        }

        score = score.min(100);
        Self {
            score,
            label: ConfidenceLabel::from_score(score),
            reasons,
        }
    }
}

fn is_weak_status_only(signals: &ConfidenceSignals) -> bool {
    matches!(signals.kind, MatchKind::Found | MatchKind::NotFound)
        && signals.signal_evidence_count == 1
        && signals.profile_evidence_count == 0
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
            reason: Some(UncertainReason::GeoUnavailable),
            signal_evidence_count: 0,
            profile_evidence_count: 0,
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
}
