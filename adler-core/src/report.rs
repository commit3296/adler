//! Core investigation-report model.
//!
//! This module is intentionally presentation-agnostic. It does not render
//! Markdown/HTML and it does not depend on `adler-server` persisted-scan
//! structs. Callers adapt their scan artifacts into the stable core inputs:
//! outcomes, identity clusters, optional timeline events, and disabled-site
//! diagnostics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::check::{CheckOutcome, MatchKind, UncertainReason};
use crate::confidence::{ConfidenceLabel, ConfidenceReason, ConfidenceScore};
use crate::escalation::TransportTier;
use crate::identity::IdentityCluster;
use crate::profile::{EvidenceSource, ProfileEvidence, ProfileEvidenceKind};

/// Current schema version for investigation report JSON.
pub const INVESTIGATION_REPORT_SCHEMA_VERSION: u16 = 1;

/// Structured report over one scan or scan-derived artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestigationReport {
    /// Version of this report model.
    pub schema_version: u16,
    /// Username or handle being investigated.
    pub username: String,
    /// Unix epoch milliseconds when the report was generated, if the caller
    /// supplies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at_ms: Option<u64>,
    /// Aggregate scan/report counts.
    pub summary: ReportSummary,
    /// All found accounts, sorted by confidence descending and then site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found_accounts: Vec<ReportAccount>,
    /// High-confidence found accounts, split out as a first-class section for
    /// renderers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub high_confidence_accounts: Vec<ReportAccount>,
    /// Inconclusive site outcomes that must not be treated as present or
    /// absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain_accounts: Vec<ReportUncertainAccount>,
    /// Flat structured evidence table for found accounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_table: Vec<ReportEvidence>,
    /// Identity candidates supplied by the scan artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_clusters: Vec<IdentityCluster>,
    /// Optional timeline/change events supplied by higher layers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ReportTimelineEvent>,
    /// Disabled or parked sites that matched the scan scope but were omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_sites: Vec<ReportDisabledSite>,
    /// Explicit caveats that renderers should include in the report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<ReportLimitation>,
}

impl InvestigationReport {
    /// Build a report from one scan's outcomes and precomputed identity
    /// clusters.
    #[must_use]
    pub fn from_scan(
        username: impl Into<String>,
        outcomes: &[CheckOutcome],
        identity_clusters: Vec<IdentityCluster>,
    ) -> Self {
        Self::builder(username, outcomes)
            .identity_clusters(identity_clusters)
            .build()
    }

    /// Start a report builder.
    #[must_use]
    pub fn builder(
        username: impl Into<String>,
        outcomes: &[CheckOutcome],
    ) -> InvestigationReportBuilder {
        InvestigationReportBuilder {
            username: username.into(),
            outcomes: outcomes.to_vec(),
            identity_clusters: Vec::new(),
            timeline: Vec::new(),
            disabled_sites: Vec::new(),
            generated_at_ms: None,
        }
    }
}

/// Builder for optional report inputs that do not live in `CheckOutcome`.
#[derive(Debug, Clone)]
pub struct InvestigationReportBuilder {
    username: String,
    outcomes: Vec<CheckOutcome>,
    identity_clusters: Vec<IdentityCluster>,
    timeline: Vec<ReportTimelineEvent>,
    disabled_sites: Vec<ReportDisabledSite>,
    generated_at_ms: Option<u64>,
}

impl InvestigationReportBuilder {
    /// Attach precomputed identity clusters.
    #[must_use]
    pub fn identity_clusters(mut self, clusters: Vec<IdentityCluster>) -> Self {
        self.identity_clusters = clusters;
        self
    }

    /// Attach timeline/change events from scan history.
    #[must_use]
    pub fn timeline(mut self, timeline: Vec<ReportTimelineEvent>) -> Self {
        self.timeline = timeline;
        self
    }

    /// Attach disabled/parked sites that matched report scope.
    #[must_use]
    pub fn disabled_sites(mut self, disabled_sites: Vec<ReportDisabledSite>) -> Self {
        self.disabled_sites = disabled_sites;
        self
    }

    /// Stamp a caller-supplied generation time.
    #[must_use]
    pub fn generated_at_ms(mut self, generated_at_ms: u64) -> Self {
        self.generated_at_ms = Some(generated_at_ms);
        self
    }

    /// Finish building the report.
    #[must_use]
    pub fn build(self) -> InvestigationReport {
        let mut sections = collect_report_sections(
            &self.outcomes,
            &self.identity_clusters,
            &self.disabled_sites,
        );
        sort_report_sections(&mut sections);
        let high_confidence_accounts = high_confidence_accounts(&sections.found_accounts);
        let summary = ReportSummary::from_parts(
            &self.outcomes,
            &sections.found_accounts,
            &sections.uncertain_accounts,
            sections.evidence_table.len(),
            &self.identity_clusters,
            self.timeline.len(),
            self.disabled_sites.len(),
        );

        InvestigationReport {
            schema_version: INVESTIGATION_REPORT_SCHEMA_VERSION,
            username: self.username,
            generated_at_ms: self.generated_at_ms,
            summary,
            found_accounts: sections.found_accounts,
            high_confidence_accounts,
            uncertain_accounts: sections.uncertain_accounts,
            evidence_table: sections.evidence_table,
            identity_clusters: self.identity_clusters,
            timeline: self.timeline,
            disabled_sites: self.disabled_sites,
            limitations: sections.limitations,
        }
    }
}

#[derive(Debug, Default)]
struct ReportSections {
    found_accounts: Vec<ReportAccount>,
    uncertain_accounts: Vec<ReportUncertainAccount>,
    evidence_table: Vec<ReportEvidence>,
    limitations: Vec<ReportLimitation>,
}

fn collect_report_sections(
    outcomes: &[CheckOutcome],
    identity_clusters: &[IdentityCluster],
    disabled_sites: &[ReportDisabledSite],
) -> ReportSections {
    let cluster_ids = cluster_ids_by_member(identity_clusters);
    let mut sections = ReportSections::default();

    for outcome in outcomes {
        collect_outcome_sections(outcome, &cluster_ids, &mut sections);
    }
    for disabled in disabled_sites {
        sections.limitations.push(ReportLimitation {
            kind: ReportLimitationKind::DisabledSiteOmitted,
            site: Some(disabled.name.clone()),
            detail: Some(disabled.disabled_reason.clone()),
        });
    }

    sections
}

fn collect_outcome_sections(
    outcome: &CheckOutcome,
    cluster_ids: &BTreeMap<(String, String), Vec<String>>,
    sections: &mut ReportSections,
) {
    match outcome.kind {
        MatchKind::Found => collect_found_sections(outcome, cluster_ids, sections),
        MatchKind::Uncertain => {
            sections
                .uncertain_accounts
                .push(ReportUncertainAccount::from_outcome(outcome));
            push_uncertain_limitations(outcome, &mut sections.limitations);
        }
        MatchKind::NotFound => {}
    }

    if outcome
        .confidence
        .reasons
        .iter()
        .any(|reason| matches!(reason, ConfidenceReason::TransportBlocked))
    {
        sections.limitations.push(ReportLimitation::site(
            ReportLimitationKind::TransportBlocked,
            &outcome.site,
        ));
    }
}

fn collect_found_sections(
    outcome: &CheckOutcome,
    cluster_ids: &BTreeMap<(String, String), Vec<String>>,
    sections: &mut ReportSections,
) {
    let account = ReportAccount::from_outcome(
        outcome,
        cluster_ids
            .get(&(outcome.site.clone(), outcome.url.clone()))
            .cloned()
            .unwrap_or_default(),
    );
    if account.profile_evidence.is_empty() {
        sections.limitations.push(ReportLimitation::site(
            ReportLimitationKind::MissingProfileEvidence,
            &account.site,
        ));
    }
    if matches!(account.confidence.label, ConfidenceLabel::Low) {
        sections.limitations.push(ReportLimitation::site(
            ReportLimitationKind::LowConfidenceFound,
            &account.site,
        ));
    }
    sections.found_accounts.push(account);
    sections
        .evidence_table
        .extend(outcome.profile_evidence.iter().map(ReportEvidence::from));
}

fn sort_report_sections(sections: &mut ReportSections) {
    sections.found_accounts.sort_by(|left, right| {
        right
            .confidence
            .score
            .cmp(&left.confidence.score)
            .then_with(|| left.site.cmp(&right.site))
            .then_with(|| left.url.cmp(&right.url))
    });
    sections.uncertain_accounts.sort_by(|left, right| {
        left.site
            .cmp(&right.site)
            .then_with(|| left.url.cmp(&right.url))
    });
    sections.evidence_table.sort_by(|left, right| {
        left.site
            .cmp(&right.site)
            .then_with(|| evidence_kind_rank(left.kind).cmp(&evidence_kind_rank(right.kind)))
            .then_with(|| left.value.cmp(&right.value))
    });
    sections.limitations.sort_by(|left, right| {
        left.site
            .cmp(&right.site)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    sections.limitations.dedup();
}

fn high_confidence_accounts(found_accounts: &[ReportAccount]) -> Vec<ReportAccount> {
    found_accounts
        .iter()
        .filter(|account| {
            matches!(
                account.confidence.label,
                ConfidenceLabel::High | ConfidenceLabel::Verified
            )
        })
        .cloned()
        .collect()
}

/// Aggregate counts used by report renderers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSummary {
    /// Number of outcomes in the scan.
    pub total: usize,
    /// Found outcome count.
    pub found: usize,
    /// Not-found outcome count.
    pub not_found: usize,
    /// Uncertain outcome count.
    pub uncertain: usize,
    /// Found outcomes with `high` or `verified` confidence.
    pub high_confidence_found: usize,
    /// Found outcomes with at least one structured profile evidence item.
    pub found_with_profile_evidence: usize,
    /// Flat profile evidence row count.
    pub profile_evidence_items: usize,
    /// Identity cluster count.
    pub identity_clusters: usize,
    /// Identity clusters marked uncertain.
    pub uncertain_identity_clusters: usize,
    /// Profiles participating in at least one identity cluster.
    pub clustered_profiles: usize,
    /// Timeline event count.
    pub timeline_events: usize,
    /// Disabled/parked site count.
    pub disabled_sites: usize,
}

impl ReportSummary {
    fn from_parts(
        outcomes: &[CheckOutcome],
        found_accounts: &[ReportAccount],
        uncertain_accounts: &[ReportUncertainAccount],
        profile_evidence_items: usize,
        identity_clusters: &[IdentityCluster],
        timeline_events: usize,
        disabled_sites: usize,
    ) -> Self {
        Self {
            total: outcomes.len(),
            found: found_accounts.len(),
            not_found: outcomes
                .iter()
                .filter(|outcome| outcome.kind == MatchKind::NotFound)
                .count(),
            uncertain: uncertain_accounts.len(),
            high_confidence_found: found_accounts
                .iter()
                .filter(|account| {
                    matches!(
                        account.confidence.label,
                        ConfidenceLabel::High | ConfidenceLabel::Verified
                    )
                })
                .count(),
            found_with_profile_evidence: found_accounts
                .iter()
                .filter(|account| !account.profile_evidence.is_empty())
                .count(),
            profile_evidence_items,
            identity_clusters: identity_clusters.len(),
            uncertain_identity_clusters: identity_clusters
                .iter()
                .filter(|cluster| cluster.uncertain)
                .count(),
            clustered_profiles: identity_clusters
                .iter()
                .map(|cluster| cluster.members.len())
                .sum(),
            timeline_events,
            disabled_sites,
        }
    }
}

/// Found account row used by report sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportAccount {
    /// Site name.
    pub site: String,
    /// Concrete profile URL.
    pub url: String,
    /// Confidence in this per-site verdict.
    pub confidence: ConfidenceScore,
    /// Human-readable detection signal evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signal_evidence: Vec<String>,
    /// Structured profile evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_evidence: Vec<ProfileEvidence>,
    /// Identity cluster ids containing this account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_ids: Vec<String>,
    /// Transport that produced the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportTier>,
    /// Automatic escalations beyond the primary route.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub escalations: u8,
    /// Probe duration.
    pub elapsed_ms: u64,
}

impl ReportAccount {
    fn from_outcome(outcome: &CheckOutcome, cluster_ids: Vec<String>) -> Self {
        Self {
            site: outcome.site.clone(),
            url: outcome.url.clone(),
            confidence: outcome.confidence.clone(),
            signal_evidence: outcome.evidence.clone(),
            profile_evidence: outcome.profile_evidence.clone(),
            cluster_ids,
            transport: outcome.transport,
            escalations: outcome.escalations,
            elapsed_ms: outcome.elapsed_ms,
        }
    }
}

/// Inconclusive account row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportUncertainAccount {
    /// Site name.
    pub site: String,
    /// URL that was attempted.
    pub url: String,
    /// Typed uncertain reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UncertainReason>,
    /// Confidence in the inconclusive verdict.
    pub confidence: ConfidenceScore,
    /// Transport that produced the result, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportTier>,
    /// Automatic escalations beyond the primary route.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub escalations: u8,
    /// Probe duration.
    pub elapsed_ms: u64,
}

impl ReportUncertainAccount {
    fn from_outcome(outcome: &CheckOutcome) -> Self {
        Self {
            site: outcome.site.clone(),
            url: outcome.url.clone(),
            reason: outcome.reason.clone(),
            confidence: outcome.confidence.clone(),
            transport: outcome.transport,
            escalations: outcome.escalations,
            elapsed_ms: outcome.elapsed_ms,
        }
    }
}

/// One structured evidence table row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEvidence {
    /// Site name.
    pub site: String,
    /// Profile URL.
    pub url: String,
    /// Evidence kind.
    pub kind: ProfileEvidenceKind,
    /// Original extractor field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Observed value.
    pub value: String,
    /// Full source metadata.
    pub source: EvidenceSource,
}

impl From<&ProfileEvidence> for ReportEvidence {
    fn from(evidence: &ProfileEvidence) -> Self {
        Self {
            site: evidence.source.site.clone(),
            url: evidence.source.url.clone(),
            kind: evidence.kind,
            field: evidence.field.clone(),
            value: evidence.value.clone(),
            source: evidence.source.clone(),
        }
    }
}

/// Optional history event supplied by timeline/watchlist layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTimelineEvent {
    /// Event kind.
    pub kind: ReportTimelineEventKind,
    /// Site name, when event is site-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Scan id where this event was observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_id: Option<String>,
    /// Unix epoch milliseconds for the event, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
    /// Short machine/human detail from the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Timeline event categories available to report renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportTimelineEventKind {
    /// A found profile appeared.
    AddedFound,
    /// A previously found profile disappeared or stopped resolving as found.
    RemovedFound,
    /// A site's verdict changed.
    VerdictChanged,
    /// Profile evidence changed while the site stayed found.
    EvidenceChanged,
    /// A profile disappeared and later appeared again.
    Reappeared,
}

/// Disabled or parked site omitted from the scan scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportDisabledSite {
    /// Site name.
    pub name: String,
    /// URL template or representative profile URL.
    pub url: String,
    /// Registry tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Why this site is disabled/parked.
    pub disabled_reason: String,
}

/// Explicit caveat for the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportLimitation {
    /// Limitation category.
    pub kind: ReportLimitationKind,
    /// Site name, when site-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Extra detail suitable for renderer text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ReportLimitation {
    fn site(kind: ReportLimitationKind, site: &str) -> Self {
        Self {
            kind,
            site: Some(site.to_owned()),
            detail: None,
        }
    }
}

/// Limitation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportLimitationKind {
    /// Found account has low per-site confidence.
    LowConfidenceFound,
    /// Found account has no structured profile evidence.
    MissingProfileEvidence,
    /// A site outcome was inconclusive.
    UncertainOutcome,
    /// A required operator session was not available.
    SessionRequired,
    /// Required geo/egress was unavailable.
    GeoUnavailable,
    /// CAPTCHA blocked reliable probing.
    Captcha,
    /// Rate limiting blocked reliable probing.
    RateLimited,
    /// Browser budget prevented browser probing.
    BrowserBudget,
    /// Transport/access conditions blocked reliable probing.
    TransportBlocked,
    /// A disabled/parked matching site was omitted.
    DisabledSiteOmitted,
}

fn cluster_ids_by_member(clusters: &[IdentityCluster]) -> BTreeMap<(String, String), Vec<String>> {
    let mut by_member: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for cluster in clusters {
        for member in &cluster.members {
            by_member
                .entry((member.site.clone(), member.url.clone()))
                .or_default()
                .push(cluster.id.clone());
        }
    }
    by_member
}

fn push_uncertain_limitations(outcome: &CheckOutcome, limitations: &mut Vec<ReportLimitation>) {
    limitations.push(ReportLimitation::site(
        ReportLimitationKind::UncertainOutcome,
        &outcome.site,
    ));

    let Some(reason) = &outcome.reason else {
        return;
    };
    let kind = match reason {
        UncertainReason::SessionRequired => Some(ReportLimitationKind::SessionRequired),
        UncertainReason::GeoUnavailable => Some(ReportLimitationKind::GeoUnavailable),
        UncertainReason::Captcha => Some(ReportLimitationKind::Captcha),
        UncertainReason::RateLimited => Some(ReportLimitationKind::RateLimited),
        UncertainReason::BrowserBudget => Some(ReportLimitationKind::BrowserBudget),
        UncertainReason::CloudflareChallenge
        | UncertainReason::BrowserFailed(_)
        | UncertainReason::Network(_)
        | UncertainReason::BodyRead(_) => Some(ReportLimitationKind::TransportBlocked),
        UncertainReason::RobotsDisallowed
        | UncertainReason::Deadline
        | UncertainReason::SchedulerClosed
        | UncertainReason::UsernameNotAllowed
        | UncertainReason::Other(_) => None,
    };

    if let Some(kind) = kind {
        limitations.push(ReportLimitation {
            kind,
            site: Some(outcome.site.clone()),
            detail: Some(reason.to_string()),
        });
    }
}

const fn evidence_kind_rank(kind: ProfileEvidenceKind) -> u8 {
    match kind {
        ProfileEvidenceKind::DisplayName => 0,
        ProfileEvidenceKind::Bio => 1,
        ProfileEvidenceKind::AvatarUrl => 2,
        ProfileEvidenceKind::ExternalLink => 3,
        ProfileEvidenceKind::Location => 4,
        ProfileEvidenceKind::JoinedDate => 5,
        ProfileEvidenceKind::ProfileTitle => 6,
        ProfileEvidenceKind::MetaDescription => 7,
        ProfileEvidenceKind::ExtractedField => 8,
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProfileEvidence, build_identity_clusters};

    fn outcome(site: &str, kind: MatchKind, label: ConfidenceLabel, score: u8) -> CheckOutcome {
        CheckOutcome {
            site: site.to_owned(),
            url: format!("https://{}.example/alice", site.to_lowercase()),
            kind,
            reason: None,
            elapsed_ms: 10,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: ConfidenceScore {
                score,
                label,
                reasons: Vec::new(),
            },
            transport: Some(TransportTier::Http),
            escalations: 0,
        }
    }

    fn found_with_website(site: &str, website: &str, score: u8) -> CheckOutcome {
        let mut outcome = outcome(site, MatchKind::Found, ConfidenceLabel::High, score);
        outcome.evidence = vec!["HTTP 200 (status_found)".to_owned()];
        outcome.profile_evidence = vec![ProfileEvidence::from_enrichment(
            site,
            &outcome.url,
            "website",
            website,
        )];
        outcome
    }

    #[test]
    fn builds_report_sections_from_outcomes_and_clusters() {
        let github = found_with_website("GitHub", "https://alice.dev", 90);
        let gitlab = found_with_website("GitLab", "https://alice.dev", 82);
        let mut mastodon = outcome("Mastodon", MatchKind::Uncertain, ConfidenceLabel::Low, 20);
        mastodon.reason = Some(UncertainReason::SessionRequired);
        let hn = outcome(
            "HackerNews",
            MatchKind::NotFound,
            ConfidenceLabel::Medium,
            60,
        );
        let outcomes = vec![github, gitlab, mastodon, hn];
        let clusters = build_identity_clusters("alice", &outcomes);

        let report = InvestigationReport::from_scan("alice", &outcomes, clusters);

        assert_eq!(report.schema_version, INVESTIGATION_REPORT_SCHEMA_VERSION);
        assert_eq!(report.username, "alice");
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.found, 2);
        assert_eq!(report.summary.not_found, 1);
        assert_eq!(report.summary.uncertain, 1);
        assert_eq!(report.summary.high_confidence_found, 2);
        assert_eq!(report.summary.profile_evidence_items, 2);
        assert_eq!(report.summary.identity_clusters, 1);
        assert_eq!(report.evidence_table.len(), 2);
        assert_eq!(report.high_confidence_accounts.len(), 2);
        assert_eq!(report.uncertain_accounts[0].site, "Mastodon");
        assert!(
            report
                .found_accounts
                .iter()
                .all(|account| account.cluster_ids == ["identity-0001"])
        );
    }

    #[test]
    fn records_limitations_for_weak_inputs() {
        let low_found = outcome("GitHub", MatchKind::Found, ConfidenceLabel::Low, 35);
        let mut captcha = outcome("Forum", MatchKind::Uncertain, ConfidenceLabel::Low, 10);
        captcha.reason = Some(UncertainReason::Captcha);
        captcha.confidence.reasons = vec![ConfidenceReason::TransportBlocked];
        let disabled = ReportDisabledSite {
            name: "Threads".to_owned(),
            url: "https://threads.net/@{username}".to_owned(),
            tags: vec!["social".to_owned()],
            disabled_reason: "login wall".to_owned(),
        };

        let report = InvestigationReport::builder("alice", &[low_found, captcha])
            .disabled_sites(vec![disabled])
            .build();
        let kinds: Vec<_> = report
            .limitations
            .iter()
            .map(|limitation| limitation.kind)
            .collect();

        assert!(kinds.contains(&ReportLimitationKind::LowConfidenceFound));
        assert!(kinds.contains(&ReportLimitationKind::MissingProfileEvidence));
        assert!(kinds.contains(&ReportLimitationKind::UncertainOutcome));
        assert!(kinds.contains(&ReportLimitationKind::Captcha));
        assert!(kinds.contains(&ReportLimitationKind::TransportBlocked));
        assert!(kinds.contains(&ReportLimitationKind::DisabledSiteOmitted));
        assert_eq!(report.summary.disabled_sites, 1);
    }

    #[test]
    fn carries_timeline_and_generation_metadata() {
        let timeline = vec![ReportTimelineEvent {
            kind: ReportTimelineEventKind::AddedFound,
            site: Some("GitHub".to_owned()),
            scan_id: Some("scan_1".to_owned()),
            observed_at_ms: Some(1_781_192_451_000),
            detail: Some("first seen".to_owned()),
        }];
        let report = InvestigationReport::builder("alice", &[])
            .timeline(timeline.clone())
            .generated_at_ms(1_781_192_452_000)
            .build();

        assert_eq!(report.generated_at_ms, Some(1_781_192_452_000));
        assert_eq!(report.timeline, timeline);
        assert_eq!(report.summary.timeline_events, 1);
    }

    #[test]
    fn serializes_snake_case_report_enums() {
        let low_found = outcome("GitHub", MatchKind::Found, ConfidenceLabel::Low, 35);
        let report = InvestigationReport::builder("alice", &[low_found]).build();
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["limitations"][0]["kind"], "low_confidence_found");
        assert_eq!(json["found_accounts"][0]["confidence"]["label"], "low");
    }
}
