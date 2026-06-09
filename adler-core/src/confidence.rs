//! Conservative confidence scoring for scan outcomes.
//!
//! This is intentionally rule-based and explainable. The goal is not to
//! claim identity-level certainty; it is to say how trustworthy Adler's
//! per-site verdict is and why.

use serde::{Deserialize, Serialize};

use crate::check::{MatchKind, UncertainReason};

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
    /// Human-readable detection signal evidence was recorded.
    SignalEvidence {
        /// Number of signal evidence lines.
        count: usize,
    },
    /// The probe could not produce a found/not-found verdict.
    UncertainOutcome,
    /// The site needs an operator session before Adler can judge presence.
    SessionRequired,
    /// Transport/access conditions blocked a reliable probe.
    TransportBlocked,
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
        let mut score: u8 = match kind {
            MatchKind::Found => 65,
            MatchKind::NotFound => 60,
            MatchKind::Uncertain => 15,
        };
        let mut reasons = Vec::new();

        match kind {
            MatchKind::Found => reasons.push(ConfidenceReason::FoundBySignal),
            MatchKind::NotFound => reasons.push(ConfidenceReason::NotFoundBySignal),
            MatchKind::Uncertain => reasons.push(ConfidenceReason::UncertainOutcome),
        }

        if signal_evidence_count > 0 {
            score = score.saturating_add(10);
            reasons.push(ConfidenceReason::SignalEvidence {
                count: signal_evidence_count,
            });
        }

        if profile_evidence_count > 0 {
            let lift = if profile_evidence_count >= 3 { 15 } else { 10 };
            score = score.saturating_add(lift);
            reasons.push(ConfidenceReason::ProfileMetadataExtracted {
                count: profile_evidence_count,
            });
        }

        if let Some(reason) = reason {
            match reason {
                UncertainReason::SessionRequired => {
                    score = 0;
                    reasons.push(ConfidenceReason::SessionRequired);
                }
                UncertainReason::CloudflareChallenge
                | UncertainReason::Captcha
                | UncertainReason::RateLimited
                | UncertainReason::BrowserBudget
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
