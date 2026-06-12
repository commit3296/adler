//! History-derived confidence signals shared by persisted read surfaces.

use std::collections::BTreeMap;

use crate::{CheckOutcome, MatchKind, ProfileEvidenceKind};

/// Borrowed scan metadata needed to compare one scan against prior scans.
#[derive(Debug, Clone, Copy)]
pub struct HistoricalScanRef<'a> {
    /// Stable scan identifier.
    pub scan_id: &'a str,
    /// Username this scan observed.
    pub username: &'a str,
    /// Scan start timestamp in Unix epoch milliseconds.
    pub created_at_ms: u64,
    /// Per-site outcomes in this scan.
    pub outcomes: &'a [CheckOutcome],
}

/// Count previous stable Found observations per current Found site.
///
/// A site is stable while previous scans for the same username keep returning
/// Found with the same normalized profile evidence signature. Scans that did
/// not mention the site are ignored; explicit non-Found outcomes and evidence
/// changes break the consecutive window.
#[must_use]
pub fn historical_consistency_counts<'a>(
    current: HistoricalScanRef<'a>,
    related_scans: impl IntoIterator<Item = HistoricalScanRef<'a>>,
) -> BTreeMap<String, usize> {
    let mut prior_scans: Vec<_> = related_scans
        .into_iter()
        .filter(|scan| scan.username == current.username)
        .filter(|scan| scan.scan_id != current.scan_id)
        .filter(|scan| scan_is_before(*scan, current))
        .collect();
    prior_scans.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| right.scan_id.cmp(left.scan_id))
    });

    current
        .outcomes
        .iter()
        .filter(|outcome| outcome.kind == MatchKind::Found)
        .filter_map(|outcome| {
            let count = stable_found_history_count(outcome, &prior_scans);
            (count >= 2).then(|| (outcome.site.clone(), count))
        })
        .collect()
}

fn scan_is_before(left: HistoricalScanRef<'_>, right: HistoricalScanRef<'_>) -> bool {
    (left.created_at_ms, left.scan_id) < (right.created_at_ms, right.scan_id)
}

fn stable_found_history_count(
    current: &CheckOutcome,
    prior_scans: &[HistoricalScanRef<'_>],
) -> usize {
    let current_signature = profile_evidence_signature(current);
    let mut count = 0;

    for scan in prior_scans {
        let Some(previous) = scan
            .outcomes
            .iter()
            .find(|outcome| outcome.site == current.site)
        else {
            continue;
        };

        if previous.kind != MatchKind::Found {
            break;
        }

        if profile_evidence_signature(previous) != current_signature {
            break;
        }

        count += 1;
    }

    count
}

fn profile_evidence_signature(outcome: &CheckOutcome) -> Vec<(u8, Option<String>, String)> {
    let mut signature: Vec<_> = outcome
        .profile_evidence
        .iter()
        .map(|evidence| {
            (
                profile_evidence_kind_rank(evidence.kind),
                evidence.field.clone(),
                evidence.value.clone(),
            )
        })
        .collect();
    signature.sort();
    signature
}

const fn profile_evidence_kind_rank(kind: ProfileEvidenceKind) -> u8 {
    match kind {
        ProfileEvidenceKind::Username => 0,
        ProfileEvidenceKind::DisplayName => 1,
        ProfileEvidenceKind::Bio => 2,
        ProfileEvidenceKind::AvatarUrl => 3,
        ProfileEvidenceKind::ExternalLink => 4,
        ProfileEvidenceKind::Location => 5,
        ProfileEvidenceKind::JoinedDate => 6,
        ProfileEvidenceKind::ProfileTitle => 7,
        ProfileEvidenceKind::MetaDescription => 8,
        ProfileEvidenceKind::ExtractedField => 9,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{CheckOutcome, ConfidenceScore, ProfileEvidence};

    fn scan<'a>(
        scan_id: &'a str,
        created_at_ms: u64,
        outcomes: &'a [CheckOutcome],
    ) -> HistoricalScanRef<'a> {
        HistoricalScanRef {
            scan_id,
            username: "alice",
            created_at_ms,
            outcomes,
        }
    }

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.to_owned(),
            url: format!("https://{site}.example/alice"),
            kind,
            reason: None,
            elapsed_ms: 10,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: ConfidenceScore::default(),
            transport: None,
            escalations: 0,
        }
    }

    fn found_with_website(site: &str, value: &str, observed_at_ms: Option<u64>) -> CheckOutcome {
        let mut outcome = outcome(site, MatchKind::Found);
        outcome
            .profile_evidence
            .push(ProfileEvidence::from_enrichment_with_source(
                site,
                &outcome.url,
                "website",
                value,
                observed_at_ms,
                None,
            ));
        outcome
    }

    #[test]
    fn two_prior_stable_found_observations_count() {
        let current = [found_with_website("GitHub", "https://alice.dev", Some(3))];
        let previous = [found_with_website("GitHub", "https://alice.dev", Some(2))];
        let older = [found_with_website("GitHub", "https://alice.dev", Some(1))];

        let counts = historical_consistency_counts(
            scan("current", 30, &current),
            [scan("previous", 20, &previous), scan("older", 10, &older)],
        );

        assert_eq!(counts.get("GitHub"), Some(&2));
    }

    #[test]
    fn one_prior_found_is_below_threshold() {
        let current = [found_with_website("GitHub", "https://alice.dev", None)];
        let previous = [found_with_website("GitHub", "https://alice.dev", None)];

        let counts = historical_consistency_counts(
            scan("current", 20, &current),
            [scan("previous", 10, &previous)],
        );

        assert!(counts.is_empty());
    }

    #[test]
    fn non_found_interrupts_history_window() {
        let current = [found_with_website("GitHub", "https://alice.dev", None)];
        let previous = [outcome("GitHub", MatchKind::NotFound)];
        let older = [found_with_website("GitHub", "https://alice.dev", None)];
        let oldest = [found_with_website("GitHub", "https://alice.dev", None)];

        let counts = historical_consistency_counts(
            scan("current", 40, &current),
            [
                scan("previous", 30, &previous),
                scan("older", 20, &older),
                scan("oldest", 10, &oldest),
            ],
        );

        assert!(counts.is_empty());
    }

    #[test]
    fn missing_filtered_scan_is_ignored() {
        let current = [found_with_website("GitHub", "https://alice.dev", None)];
        let missing = [found_with_website("GitLab", "https://alice.dev", None)];
        let older = [found_with_website("GitHub", "https://alice.dev", None)];
        let oldest = [found_with_website("GitHub", "https://alice.dev", None)];

        let counts = historical_consistency_counts(
            scan("current", 40, &current),
            [
                scan("missing", 30, &missing),
                scan("older", 20, &older),
                scan("oldest", 10, &oldest),
            ],
        );

        assert_eq!(counts.get("GitHub"), Some(&2));
    }

    #[test]
    fn profile_evidence_change_breaks_history_window() {
        let current = [found_with_website("GitHub", "https://alice.dev", None)];
        let previous = [found_with_website("GitHub", "https://other.example", None)];
        let older = [found_with_website("GitHub", "https://alice.dev", None)];
        let oldest = [found_with_website("GitHub", "https://alice.dev", None)];

        let counts = historical_consistency_counts(
            scan("current", 40, &current),
            [
                scan("previous", 30, &previous),
                scan("older", 20, &older),
                scan("oldest", 10, &oldest),
            ],
        );

        assert!(counts.is_empty());
    }
}
