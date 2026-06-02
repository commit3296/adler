//! Cross-scan telemetry analysis — close the doctor loop on sites
//! that consistently need the browser backend.
//!
//! The router stamps every [`CheckOutcome`](crate::CheckOutcome) with the
//! transport that produced its verdict (HTTP / impersonate / browser) and
//! an escalation count. A site that *consistently* requires escalation
//! across many scans is one the registry hasn't yet tagged with the right
//! [`protection`](crate::ProtectionKind) hint — every fresh scan pays the
//! cost of a failing HTTP probe before the router gives up and re-tries
//! through the browser. Pre-tagging the site lets the router skip the
//! cheap path next time.
//!
//! This module is the pure analytics: given a stream of per-scan outcome
//! slices (read from `$XDG_CACHE_HOME/adler/scans/*.json` in
//! `adler-cli`, but the input shape is unopinionated), it groups by
//! site, decides which sites meet the "consistently escalates"
//! threshold, and emits [`EscalationFinding`]s that
//! `adler --doctor --suggest-protection` prints as paste-ready
//! suggestions.

use crate::check::{CheckOutcome, MatchKind, UncertainReason};
use crate::escalation::TransportTier;
use crate::site::ProtectionKind;
use std::collections::HashMap;

/// Default ratio at which `--suggest-protection` surfaces a site.
/// 60% of scans needing escalation is the boundary between
/// "intermittent edge case" and "load-bearing pattern".
pub const DEFAULT_THRESHOLD_RATIO: f32 = 0.6;

/// Default minimum scan count before a site is considered for a
/// suggestion. Three distinct scans is the smallest sample where a
/// pattern beats a coincidence.
pub const DEFAULT_MIN_SCANS: u32 = 3;

/// Per-site evidence drawn from a cross-scan outcome history.
///
/// Ready to be turned into a `protection: <kind>` suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationFinding {
    /// Site name as it appears in [`Site::name`](crate::Site::name).
    pub site: String,
    /// Distinct scans in which the site appeared in the input.
    pub scans_seen: u32,
    /// Of those, how many produced *evidence* of needing the
    /// browser: either a successful escalation (`transport=Browser`,
    /// `escalations>=1`) or an `Uncertain` outcome whose reason
    /// would have triggered escalation if a browser had been
    /// configured (`CloudflareChallenge`, `RateLimited`).
    pub escalation_evidence: u32,
    /// Most common evidence type observed for this site — used to
    /// pick the suggested [`ProtectionKind`].
    pub dominant_reason: EvidenceKind,
    /// Suggested addition to the site's [`protection`](crate::Site::protection)
    /// vector. Always populated when the finding is emitted.
    pub suggested_protection: ProtectionKind,
}

impl EscalationFinding {
    /// Ratio of scans where the site needed (or would have needed) escalation.
    /// Always in `[0.0, 1.0]`.
    #[must_use]
    pub fn ratio(&self) -> f32 {
        if self.scans_seen == 0 {
            0.0
        } else {
            f32::from(u16::try_from(self.escalation_evidence).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(self.scans_seen).unwrap_or(u16::MAX))
        }
    }
}

/// What kind of cross-scan evidence triggered the finding.
///
/// Maps 1:1 to the [`UncertainReason`] taxonomy that drives
/// escalation, plus a "browser succeeded" bucket for outcomes where
/// escalation already happened and produced a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EvidenceKind {
    /// HTTP-path `Uncertain(cloudflare_challenge)` or the same reason
    /// resolved after escalation through the browser.
    CloudflareChallenge,
    /// HTTP-path `Uncertain(rate_limited)` or the same reason resolved
    /// after escalation.
    RateLimited,
}

impl EvidenceKind {
    /// Map evidence to the [`ProtectionKind`] suggestion. Both
    /// existing kinds point at `Cloudflare` today — even rate-limit
    /// edges most commonly sit behind Cloudflare's WAF — but the
    /// mapping is enum-keyed so a future split (e.g. `DdosGuard` /
    /// `CfFirewall`) can be added without touching callers.
    #[must_use]
    pub const fn suggested_protection(self) -> ProtectionKind {
        match self {
            Self::CloudflareChallenge | Self::RateLimited => ProtectionKind::Cloudflare,
        }
    }
}

/// Tally evidence for one site across many scans.
#[derive(Default, Debug)]
struct SiteTally {
    scans_seen: u32,
    cloudflare_evidence: u32,
    ratelimit_evidence: u32,
}

impl SiteTally {
    fn total_evidence(&self) -> u32 {
        self.cloudflare_evidence + self.ratelimit_evidence
    }

    fn dominant(&self) -> Option<EvidenceKind> {
        if self.total_evidence() == 0 {
            return None;
        }
        if self.cloudflare_evidence >= self.ratelimit_evidence {
            Some(EvidenceKind::CloudflareChallenge)
        } else {
            Some(EvidenceKind::RateLimited)
        }
    }
}

/// Classify one outcome's evidence contribution.
///
/// Returns:
/// - `Some(EvidenceKind)` when the outcome shows the site needed (or
///   would have needed) escalation — either a successful escalation
///   to the browser (`transport=Browser && escalations>=1`) or a
///   cheap-path `Uncertain` with a should-escalate reason.
/// - `None` otherwise — the outcome doesn't argue for pre-tagging.
fn classify(outcome: &CheckOutcome) -> Option<EvidenceKind> {
    if matches!(outcome.transport, Some(TransportTier::Browser)) && outcome.escalations >= 1 {
        // Escalation already happened, browser produced the final
        // verdict. The original reason was either CloudflareChallenge
        // or RateLimited (only ones `should_escalate` accepts);
        // CloudflareChallenge is the conservative default since most
        // 429s on profile-search endpoints are CF-side.
        return Some(EvidenceKind::CloudflareChallenge);
    }
    if outcome.kind == MatchKind::Uncertain {
        match outcome.reason.as_ref()? {
            UncertainReason::CloudflareChallenge => return Some(EvidenceKind::CloudflareChallenge),
            UncertainReason::RateLimited => return Some(EvidenceKind::RateLimited),
            _ => {}
        }
    }
    None
}

/// Aggregate per-site evidence over a series of scans, returning
/// findings for every site that meets `threshold_ratio` and
/// `min_scans`. Sorted by ratio descending, then by site name.
///
/// Pass each scan's outcomes as one slice — the analyzer counts
/// per-site *scans* (not per-site outcomes), so a single
/// `&[CheckOutcome]` is one observation per site, even when the
/// caller has many slices.
pub fn analyze_escalation_history<'a>(
    scans: impl IntoIterator<Item = &'a [CheckOutcome]>,
    threshold_ratio: f32,
    min_scans: u32,
) -> Vec<EscalationFinding> {
    let mut tallies: HashMap<String, SiteTally> = HashMap::new();
    for outcomes in scans {
        for outcome in outcomes {
            let entry = tallies.entry(outcome.site.clone()).or_default();
            entry.scans_seen += 1;
            match classify(outcome) {
                Some(EvidenceKind::CloudflareChallenge) => entry.cloudflare_evidence += 1,
                Some(EvidenceKind::RateLimited) => entry.ratelimit_evidence += 1,
                None => {}
            }
        }
    }

    let mut findings: Vec<EscalationFinding> = tallies
        .into_iter()
        .filter_map(|(site, tally)| {
            if tally.scans_seen < min_scans {
                return None;
            }
            let dominant = tally.dominant()?;
            let evidence = tally.total_evidence();
            let ratio = f32::from(u16::try_from(evidence).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(tally.scans_seen).unwrap_or(u16::MAX));
            if ratio < threshold_ratio {
                return None;
            }
            Some(EscalationFinding {
                site,
                scans_seen: tally.scans_seen,
                escalation_evidence: evidence,
                dominant_reason: dominant,
                suggested_protection: dominant.suggested_protection(),
            })
        })
        .collect();
    findings.sort_by(|a, b| {
        b.ratio()
            .partial_cmp(&a.ratio())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.site.cmp(&b.site))
    });
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::CheckOutcome;

    fn outcome(site: &str, kind: MatchKind, reason: Option<UncertainReason>) -> CheckOutcome {
        CheckOutcome {
            site: site.to_owned(),
            url: format!("https://{site}.example/foo"),
            kind,
            reason,
            elapsed_ms: 100,
            evidence: Vec::new(),
            enrichment: std::collections::BTreeMap::new(),
            transport: None,
            escalations: 0,
        }
    }

    fn outcome_browser_escalated(site: &str) -> CheckOutcome {
        CheckOutcome {
            site: site.to_owned(),
            url: format!("https://{site}.example/foo"),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 200,
            evidence: Vec::new(),
            enrichment: std::collections::BTreeMap::new(),
            transport: Some(TransportTier::Browser),
            escalations: 1,
        }
    }

    fn outcome_http_uncertain_cf(site: &str) -> CheckOutcome {
        outcome(
            site,
            MatchKind::Uncertain,
            Some(UncertainReason::CloudflareChallenge),
        )
    }

    fn outcome_http_uncertain_rl(site: &str) -> CheckOutcome {
        outcome(
            site,
            MatchKind::Uncertain,
            Some(UncertainReason::RateLimited),
        )
    }

    fn outcome_http_found(site: &str) -> CheckOutcome {
        outcome(site, MatchKind::Found, None)
    }

    #[test]
    fn consistent_escalation_produces_finding() {
        let scans: Vec<Vec<CheckOutcome>> = (0..5)
            .map(|_| vec![outcome_browser_escalated("CDNed")])
            .collect();
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].site, "CDNed");
        assert_eq!(findings[0].scans_seen, 5);
        assert_eq!(findings[0].escalation_evidence, 5);
        assert!((findings[0].ratio() - 1.0).abs() < f32::EPSILON);
        assert_eq!(findings[0].suggested_protection, ProtectionKind::Cloudflare);
    }

    #[test]
    fn http_only_site_does_not_get_flagged() {
        // GitHub: every scan is a clean HTTP Found.
        let scans: Vec<Vec<CheckOutcome>> = (0..10)
            .map(|_| vec![outcome_http_found("GitHub")])
            .collect();
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn intermittent_escalation_below_threshold_skipped() {
        // 2 of 10 scans escalated → 20% < 60% threshold. Not flagged.
        let mut scans: Vec<Vec<CheckOutcome>> = Vec::new();
        for _ in 0..2 {
            scans.push(vec![outcome_browser_escalated("FlakyEdge")]);
        }
        for _ in 0..8 {
            scans.push(vec![outcome_http_found("FlakyEdge")]);
        }
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn too_few_scans_skipped_even_at_full_ratio() {
        // 2 of 2 escalated but min_scans=3 → not enough sample.
        let scans: Vec<Vec<CheckOutcome>> = (0..2)
            .map(|_| vec![outcome_browser_escalated("RareSite")])
            .collect();
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn http_uncertain_with_should_escalate_reason_counts_too() {
        // No browser configured this run, but the HTTP probe still
        // returned a should-escalate reason — that's evidence
        // pre-tagging would have helped.
        let scans: Vec<Vec<CheckOutcome>> = (0..4)
            .map(|_| vec![outcome_http_uncertain_cf("WalledOff")])
            .collect();
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].site, "WalledOff");
        assert_eq!(
            findings[0].dominant_reason,
            EvidenceKind::CloudflareChallenge
        );
    }

    #[test]
    fn dominant_reason_picks_higher_count() {
        // 4× CloudflareChallenge, 1× RateLimited → CloudflareChallenge wins.
        let mut scans: Vec<Vec<CheckOutcome>> = Vec::new();
        for _ in 0..4 {
            scans.push(vec![outcome_http_uncertain_cf("Mixed")]);
        }
        scans.push(vec![outcome_http_uncertain_rl("Mixed")]);
        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.6, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].dominant_reason,
            EvidenceKind::CloudflareChallenge
        );
    }

    #[test]
    fn findings_sorted_by_ratio_then_name() {
        // Two sites: A escalates 5/5, B escalates 3/5. A first.
        let mut scans: Vec<Vec<CheckOutcome>> = Vec::new();
        for _ in 0..5 {
            scans.push(vec![
                outcome_browser_escalated("Aardvark"),
                outcome_browser_escalated("Beaver"),
            ]);
        }
        // Drop 2 of Beaver's escalations.
        scans[3] = vec![
            outcome_browser_escalated("Aardvark"),
            outcome_http_found("Beaver"),
        ];
        scans[4] = vec![
            outcome_browser_escalated("Aardvark"),
            outcome_http_found("Beaver"),
        ];

        let scan_slices: Vec<&[CheckOutcome]> = scans.iter().map(Vec::as_slice).collect();
        let findings = analyze_escalation_history(scan_slices.iter().copied(), 0.5, 3);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].site, "Aardvark");
        assert!(findings[0].ratio() > findings[1].ratio());
    }

    #[test]
    fn empty_input_returns_empty() {
        let findings: Vec<EscalationFinding> =
            analyze_escalation_history(std::iter::empty::<&[CheckOutcome]>(), 0.5, 1);
        assert!(findings.is_empty());
    }
}
