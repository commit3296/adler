//! On-disk persistence for finished scans.
//!
//! Each scan is serialised as a single JSON file under [`default_dir`]
//! (`$XDG_CACHE_HOME/adler/scans/`, falling back to
//! `$HOME/.cache/adler/scans/`). The on-disk format is the full
//! [`PersistedScan`] — enough for the history listing AND for replaying
//! the scan into the UI without a fresh probe.
//!
//! Writes are atomic: serialise to `<id>.json.tmp`, then rename onto
//! the final path. A crashed process leaves at most one orphan `.tmp`
//! file behind, never a half-written `<id>.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use adler_core::{CheckOutcome, IdentityCluster, MatchKind, ProfileEvidence, Site};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{Error, Result};
use crate::scan::{FinishedScan, ScanId, Summary};

/// Hard cap on how many scans we keep on disk. Beyond this, oldest
/// (by `created_at_ms`) get [`prune`]d on the next save. Picked to be
/// large enough for any plausible human-driven OSINT session.
pub(crate) const MAX_PERSISTED_SCANS: usize = 200;
/// Current on-disk schema version for [`PersistedScan`].
pub(crate) const PERSISTED_SCAN_SCHEMA_VERSION: u16 = 2;

/// Self-contained snapshot of a completed scan. Round-trips losslessly
/// through JSON; tests assert that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedScan {
    /// Version of this persisted scan artifact.
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    /// Stable identifier — same value as in-memory [`ScanId`].
    pub scan_id: ScanId,
    /// Username that was scanned.
    pub username: String,
    /// Request scope and parked-site diagnostics that explain how this
    /// artifact was produced. Missing on scans saved before v1 context
    /// support landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_context: Option<ScanRequestContext>,
    /// Total number of sites probed in this scan.
    pub site_count: usize,
    /// Unix epoch milliseconds when the scan was started.
    pub created_at_ms: u64,
    /// Per-verdict tally over [`Self::outcomes`].
    pub summary: Summary,
    /// All outcomes, in completion order.
    pub outcomes: Vec<CheckOutcome>,
    /// Deterministic identity candidates derived from found outcomes
    /// with structured profile evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_clusters: Vec<IdentityCluster>,
    /// Wall-clock duration, milliseconds.
    pub elapsed_ms: u64,
}

impl PersistedScan {
    /// Build a snapshot from a freshly-completed in-memory scan.
    #[must_use]
    pub fn from_finished(
        scan_id: ScanId,
        username: String,
        site_count: usize,
        created_at_ms: u64,
        finished: FinishedScan,
    ) -> Self {
        let mut scan = Self {
            schema_version: PERSISTED_SCAN_SCHEMA_VERSION,
            scan_id,
            username,
            request_context: None,
            site_count,
            created_at_ms,
            summary: finished.summary,
            outcomes: finished.outcomes,
            identity_clusters: finished.identity_clusters,
            elapsed_ms: finished.elapsed_ms,
        };
        scan.refresh_derived_fields();
        scan
    }

    /// Attach request-scope metadata to this persisted scan.
    #[must_use]
    pub fn with_request_context(mut self, context: ScanRequestContext) -> Self {
        self.request_context = Some(context);
        self
    }

    pub(crate) fn refresh_derived_fields(&mut self) {
        for outcome in &mut self.outcomes {
            outcome.refresh_confidence();
        }
        self.summary = Summary::from_outcomes(&self.outcomes);
        self.identity_clusters =
            adler_core::build_identity_clusters(&self.username, &self.outcomes);
    }
}

const fn default_schema_version() -> u16 {
    PERSISTED_SCAN_SCHEMA_VERSION
}

/// Request scope persisted with a finished scan so future timelines and
/// reports can explain what was scanned and what was intentionally out of
/// scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequestContext {
    /// Username supplied by the operator.
    pub username: String,
    /// Previous scan id when this scan was created by refiltering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<ScanId>,
    /// Site name include filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    /// Site name exclude filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Tag include filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag: Vec<String>,
    /// Tag exclude filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_tag: Vec<String>,
    /// Popularity ceiling, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<u32>,
    /// Whether NSFW-tagged entries were included.
    pub nsfw: bool,
    /// Per-scan concurrency override, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    /// Per-scan deadline override, seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_secs: Option<u64>,
    /// Egress subset requested for this scan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_names: Vec<String>,
    /// Disabled/parked sites that matched the same filter and were not
    /// included in the enabled scan set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_matches: Vec<PersistedDisabledMatch>,
}

/// Compact disabled-site diagnostic persisted with scan context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedDisabledMatch {
    /// Site name.
    pub name: String,
    /// Profile URL template.
    pub url: String,
    /// Registry tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Human-readable reason the site is parked.
    pub disabled_reason: String,
}

impl From<&Site> for PersistedDisabledMatch {
    fn from(site: &Site) -> Self {
        Self {
            name: site.name.clone(),
            url: site.url.as_str().to_owned(),
            tags: site.tags.clone(),
            disabled_reason: site
                .disabled_reason
                .clone()
                .unwrap_or_else(|| "disabled in registry".to_owned()),
        }
    }
}

/// Deterministic scan-to-scan diff used as the basis for timelines and
/// watchlists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanDiff {
    /// Previous scan id.
    pub from_scan_id: ScanId,
    /// Current scan id.
    pub to_scan_id: ScanId,
    /// Found accounts that were not Found in the previous scan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_found: Vec<CheckOutcome>,
    /// Accounts that were Found previously but are no longer Found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_found: Vec<CheckOutcome>,
    /// Sites present in both scans whose verdict changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdict_changes: Vec<VerdictChange>,
    /// Found sites whose normalized profile evidence changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_changes: Vec<EvidenceChange>,
}

/// A verdict transition for one site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictChange {
    /// Site name.
    pub site: String,
    /// Previous verdict.
    pub before: MatchKind,
    /// Current verdict.
    pub after: MatchKind,
}

/// Profile evidence transition for one still-found site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceChange {
    /// Site name.
    pub site: String,
    /// Previous legacy enrichment fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub before_enrichment: BTreeMap<String, String>,
    /// Current legacy enrichment fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub after_enrichment: BTreeMap<String, String>,
    /// Previous normalized profile evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_profile_evidence: Vec<ProfileEvidence>,
    /// Current normalized profile evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_profile_evidence: Vec<ProfileEvidence>,
}

/// Historical view derived from a sequence of persisted scans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTimeline {
    /// Username shared by the scans used to build this timeline.
    pub username: String,
    /// Number of scans considered.
    pub scan_count: usize,
    /// Oldest scan timestamp, when at least one scan was supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ms: Option<u64>,
    /// Newest scan timestamp, when at least one scan was supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_ms: Option<u64>,
    /// Per-site lifecycle summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<TimelineProfile>,
    /// Chronological lifecycle events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<TimelineEvent>,
}

/// Per-site lifecycle state in a scan timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineProfile {
    /// Site name.
    pub site: String,
    /// Last known profile URL for the site.
    pub url: String,
    /// First scan timestamp where the profile was Found.
    pub first_seen_ms: u64,
    /// Most recent scan timestamp where the profile was Found.
    pub last_seen_ms: u64,
    /// Whether the profile is Found in the newest scan that mentioned it.
    pub present_in_latest: bool,
    /// Last verdict observed for this site, if the newest scan mentioned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verdict: Option<MatchKind>,
}

/// Timeline event category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineEventKind {
    /// Site was Found for the first time in the supplied scan sequence.
    FirstSeen,
    /// Site was Found before, then no longer Found.
    Disappeared,
    /// Site was absent/not found after a previous hit, then Found again.
    Reappeared,
    /// Site stayed Found but normalized profile evidence changed.
    EvidenceChanged,
}

/// One lifecycle event for a profile across scans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    /// Scan id where the event was observed.
    pub scan_id: ScanId,
    /// Scan start timestamp.
    pub at_ms: u64,
    /// Site name.
    pub site: String,
    /// Best URL known for the site at this point in the timeline.
    pub url: String,
    /// Event category.
    pub kind: TimelineEventKind,
    /// Previous verdict, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<MatchKind>,
    /// Current verdict, when the current scan mentioned the site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<MatchKind>,
    /// Evidence transition for [`TimelineEventKind::EvidenceChanged`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_change: Option<EvidenceChange>,
}

/// Compare two persisted scans.
///
/// The diff is intentionally conservative: `added_found` and
/// `removed_found` are based only on the `Found` verdict, while
/// `evidence_changes` are reported only for sites that are Found in both
/// scans.
#[must_use]
pub fn diff_scans(previous: &PersistedScan, current: &PersistedScan) -> ScanDiff {
    let previous_by_site = outcomes_by_site(&previous.outcomes);
    let current_by_site = outcomes_by_site(&current.outcomes);

    let mut added_found = Vec::new();
    let mut removed_found = Vec::new();
    let mut verdict_changes = Vec::new();
    let mut evidence_changes = Vec::new();

    for (site, current_outcome) in &current_by_site {
        let previous_outcome = previous_by_site.get(site);
        if current_outcome.kind == MatchKind::Found
            && previous_outcome.is_none_or(|o| o.kind != MatchKind::Found)
        {
            added_found.push((*current_outcome).clone());
        }
        if let Some(previous_outcome) = previous_outcome {
            if previous_outcome.kind != current_outcome.kind {
                verdict_changes.push(VerdictChange {
                    site: site.clone(),
                    before: previous_outcome.kind,
                    after: current_outcome.kind,
                });
            }
            if previous_outcome.kind == MatchKind::Found
                && current_outcome.kind == MatchKind::Found
                && profile_evidence_changed(previous_outcome, current_outcome)
            {
                evidence_changes.push(EvidenceChange {
                    site: site.clone(),
                    before_enrichment: previous_outcome.enrichment.clone(),
                    after_enrichment: current_outcome.enrichment.clone(),
                    before_profile_evidence: previous_outcome.profile_evidence.clone(),
                    after_profile_evidence: current_outcome.profile_evidence.clone(),
                });
            }
        }
    }

    for (site, previous_outcome) in &previous_by_site {
        if previous_outcome.kind == MatchKind::Found
            && current_by_site
                .get(site)
                .is_none_or(|o| o.kind != MatchKind::Found)
        {
            removed_found.push((*previous_outcome).clone());
        }
    }

    ScanDiff {
        from_scan_id: previous.scan_id.clone(),
        to_scan_id: current.scan_id.clone(),
        added_found,
        removed_found,
        verdict_changes,
        evidence_changes,
    }
}

/// Build a chronological timeline from persisted scans.
///
/// Scans may be supplied in any order; the builder sorts them oldest-first.
/// Only `Found` outcomes create profiles. A later non-Found or missing site
/// creates a disappearance event if the profile was previously present.
#[must_use]
pub fn build_scan_timeline(scans: &[PersistedScan]) -> ScanTimeline {
    let mut ordered: Vec<&PersistedScan> = scans.iter().collect();
    ordered.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.scan_id.as_str().cmp(right.scan_id.as_str()))
    });

    let username = ordered
        .first()
        .map(|scan| scan.username.clone())
        .unwrap_or_default();
    let from_ms = ordered.first().map(|scan| scan.created_at_ms);
    let to_ms = ordered.last().map(|scan| scan.created_at_ms);
    let mut states: BTreeMap<String, TimelineProfileState> = BTreeMap::new();
    let mut events = Vec::new();

    for scan in &ordered {
        let current_by_site = outcomes_by_site(&scan.outcomes);
        let sites = timeline_site_names(&states, &current_by_site);

        for site in sites {
            apply_timeline_site(
                scan,
                &site,
                current_by_site.get(&site).copied(),
                &mut states,
                &mut events,
            );
        }
    }

    let profiles = states
        .into_iter()
        .map(|(site, state)| TimelineProfile {
            site,
            url: state.url,
            first_seen_ms: state.first_seen_ms,
            last_seen_ms: state.last_seen_ms,
            present_in_latest: state.present_in_latest,
            last_verdict: state.last_verdict,
        })
        .collect();

    ScanTimeline {
        username,
        scan_count: ordered.len(),
        from_ms,
        to_ms,
        profiles,
        events,
    }
}

fn timeline_site_names(
    states: &BTreeMap<String, TimelineProfileState>,
    current_by_site: &BTreeMap<String, &CheckOutcome>,
) -> Vec<String> {
    let mut sites: Vec<String> = states.keys().cloned().collect();
    for site in current_by_site.keys() {
        if !states.contains_key(site.as_str()) {
            sites.push((*site).clone());
        }
    }
    sites.sort();
    sites.dedup();
    sites
}

fn apply_timeline_site(
    scan: &PersistedScan,
    site: &str,
    current: Option<&CheckOutcome>,
    states: &mut BTreeMap<String, TimelineProfileState>,
    events: &mut Vec<TimelineEvent>,
) {
    let current_kind = current.map(|outcome| outcome.kind);
    let was_present = states
        .get(site)
        .is_some_and(|state| state.present_in_latest);

    if current_kind == Some(MatchKind::Found) {
        apply_found_timeline_site(scan, site, current.expect("found outcome"), states, events);
    } else if was_present {
        apply_disappeared_timeline_site(scan, site, current, current_kind, states, events);
    } else if let (Some(state), Some(outcome)) = (states.get_mut(site), current) {
        state.last_verdict = Some(outcome.kind);
        state.url.clone_from(&outcome.url);
    }
}

fn apply_found_timeline_site(
    scan: &PersistedScan,
    site: &str,
    outcome: &CheckOutcome,
    states: &mut BTreeMap<String, TimelineProfileState>,
    events: &mut Vec<TimelineEvent>,
) {
    let current_kind = Some(outcome.kind);
    let had_state = states.contains_key(site);
    let was_present = states
        .get(site)
        .is_some_and(|state| state.present_in_latest);
    let state = states
        .entry(site.to_owned())
        .or_insert_with(|| TimelineProfileState::new(outcome, scan.created_at_ms));

    if !had_state {
        events.push(timeline_event(
            scan,
            site,
            &outcome.url,
            TimelineEventKind::FirstSeen,
            None,
            current_kind,
            None,
        ));
    } else if !was_present {
        events.push(timeline_event(
            scan,
            site,
            &outcome.url,
            TimelineEventKind::Reappeared,
            state.last_verdict,
            current_kind,
            None,
        ));
    } else if state.profile_evidence_changed(outcome) {
        events.push(timeline_event(
            scan,
            site,
            &outcome.url,
            TimelineEventKind::EvidenceChanged,
            Some(MatchKind::Found),
            current_kind,
            Some(EvidenceChange {
                site: site.to_owned(),
                before_enrichment: state.last_found_enrichment.clone(),
                after_enrichment: outcome.enrichment.clone(),
                before_profile_evidence: state.last_found_profile_evidence.clone(),
                after_profile_evidence: outcome.profile_evidence.clone(),
            }),
        ));
    }

    states
        .get_mut(site)
        .expect("state inserted before found update")
        .update_found(outcome, scan.created_at_ms);
}

fn apply_disappeared_timeline_site(
    scan: &PersistedScan,
    site: &str,
    current: Option<&CheckOutcome>,
    current_kind: Option<MatchKind>,
    states: &mut BTreeMap<String, TimelineProfileState>,
    events: &mut Vec<TimelineEvent>,
) {
    let state = states
        .get_mut(site)
        .expect("present state exists before disappearance");
    let url = current.map_or_else(|| state.url.clone(), |outcome| outcome.url.clone());
    events.push(timeline_event(
        scan,
        site,
        &url,
        TimelineEventKind::Disappeared,
        state.last_verdict,
        current_kind,
        None,
    ));
    state.present_in_latest = false;
    state.last_verdict = current_kind;
    if let Some(outcome) = current {
        state.url.clone_from(&outcome.url);
    }
}

fn timeline_event(
    scan: &PersistedScan,
    site: &str,
    url: &str,
    kind: TimelineEventKind,
    before: Option<MatchKind>,
    after: Option<MatchKind>,
    evidence_change: Option<EvidenceChange>,
) -> TimelineEvent {
    TimelineEvent {
        scan_id: scan.scan_id.clone(),
        at_ms: scan.created_at_ms,
        site: site.to_owned(),
        url: url.to_owned(),
        kind,
        before,
        after,
        evidence_change,
    }
}

#[derive(Debug, Clone)]
struct TimelineProfileState {
    url: String,
    first_seen_ms: u64,
    last_seen_ms: u64,
    present_in_latest: bool,
    last_verdict: Option<MatchKind>,
    last_found_enrichment: BTreeMap<String, String>,
    last_found_profile_evidence: Vec<ProfileEvidence>,
}

impl TimelineProfileState {
    fn new(outcome: &CheckOutcome, at_ms: u64) -> Self {
        Self {
            url: outcome.url.clone(),
            first_seen_ms: at_ms,
            last_seen_ms: at_ms,
            present_in_latest: true,
            last_verdict: Some(outcome.kind),
            last_found_enrichment: outcome.enrichment.clone(),
            last_found_profile_evidence: outcome.profile_evidence.clone(),
        }
    }

    fn update_found(&mut self, outcome: &CheckOutcome, at_ms: u64) {
        self.url.clone_from(&outcome.url);
        self.last_seen_ms = at_ms;
        self.present_in_latest = true;
        self.last_verdict = Some(outcome.kind);
        self.last_found_enrichment = outcome.enrichment.clone();
        self.last_found_profile_evidence
            .clone_from(&outcome.profile_evidence);
    }

    fn profile_evidence_changed(&self, outcome: &CheckOutcome) -> bool {
        self.last_found_enrichment != outcome.enrichment
            || self.last_found_profile_evidence != outcome.profile_evidence
    }
}

fn outcomes_by_site(outcomes: &[CheckOutcome]) -> BTreeMap<String, &CheckOutcome> {
    outcomes
        .iter()
        .map(|outcome| (outcome.site.clone(), outcome))
        .collect()
}

fn profile_evidence_changed(previous: &CheckOutcome, current: &CheckOutcome) -> bool {
    previous.enrichment != current.enrichment
        || previous.profile_evidence != current.profile_evidence
}

/// Default directory for persisted scans.
///
/// Mirrors [`adler_core::Cache::default_path`]'s discovery rules:
/// `$XDG_CACHE_HOME/adler/scans/` → `$HOME/.cache/adler/scans/` →
/// a relative fallback. The directory is created lazily on first save.
#[must_use]
pub fn default_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("adler").join("scans");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("adler")
            .join("scans");
    }
    PathBuf::from("adler-scans")
}

/// Save `scan` to `<dir>/<id>.json` atomically. Creates `dir` if missing.
pub(crate) async fn save(dir: &Path, scan: &PersistedScan) -> Result<()> {
    fs::create_dir_all(dir).await.map_err(Error::Persist)?;
    let path = dir.join(format!("{}.json", scan.scan_id));
    let tmp = dir.join(format!("{}.json.tmp", scan.scan_id));
    let mut scan = scan.clone();
    scan.refresh_derived_fields();
    let body = serde_json::to_vec_pretty(&scan).map_err(Error::PersistEncode)?;
    fs::write(&tmp, &body).await.map_err(Error::Persist)?;
    fs::rename(&tmp, &path).await.map_err(Error::Persist)?;
    Ok(())
}

/// Read one scan from disk by id. Returns `None` on any I/O or parse
/// error — callers should treat a missing scan as not-found rather
/// than propagate the underlying cause.
pub(crate) async fn load(dir: &Path, scan_id: &ScanId) -> Option<PersistedScan> {
    let path = dir.join(format!("{scan_id}.json"));
    let bytes = fs::read(&path).await.ok()?;
    serde_json::from_slice(&bytes)
        .ok()
        .map(refresh_derived_fields)
}

/// Enumerate every persisted scan, newest first. Files that fail to
/// parse are silently skipped — a corrupted file shouldn't break the
/// whole listing.
pub(crate) async fn load_all(dir: &Path) -> Vec<PersistedScan> {
    let Ok(mut entries) = fs::read_dir(dir).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path).await else {
            continue;
        };
        let Ok(scan) = serde_json::from_slice::<PersistedScan>(&bytes) else {
            continue;
        };
        out.push(refresh_derived_fields(scan));
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.created_at_ms));
    out
}

fn refresh_derived_fields(mut scan: PersistedScan) -> PersistedScan {
    scan.refresh_derived_fields();
    scan
}

/// Delete scans beyond `keep_newest`. Newest-by-`created_at_ms` wins.
/// Returns the number of files actually removed.
pub(crate) async fn prune(dir: &Path, keep_newest: usize) -> usize {
    let scans = load_all(dir).await;
    if scans.len() <= keep_newest {
        return 0;
    }
    let mut removed = 0;
    for s in &scans[keep_newest..] {
        let path = dir.join(format!("{}.json", s.scan_id));
        if fs::remove_file(&path).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::{
        EvidenceAccessPath, MatchKind, ProfileEvidence, TransportTier, UncertainReason,
    };
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn sample(scan_id: &str, ts: u64) -> PersistedScan {
        PersistedScan {
            schema_version: PERSISTED_SCAN_SCHEMA_VERSION,
            scan_id: ScanId::from(scan_id.to_owned()),
            username: "alice".into(),
            request_context: None,
            site_count: 2,
            created_at_ms: ts,
            summary: Summary {
                found: 1,
                not_found: 1,
                uncertain: 0,
            },
            outcomes: vec![
                CheckOutcome {
                    site: "GitHub".into(),
                    url: "https://github.com/alice".into(),
                    kind: MatchKind::Found,
                    reason: None,
                    elapsed_ms: 120,
                    enrichment: BTreeMap::new(),
                    evidence: vec!["HTTP 200 (status_found)".into()],
                    profile_evidence: Vec::new(),
                    confidence: adler_core::ConfidenceScore::default(),
                    transport: None,
                    escalations: 0,
                },
                CheckOutcome {
                    site: "GitLab".into(),
                    url: "https://gitlab.com/alice".into(),
                    kind: MatchKind::NotFound,
                    reason: None,
                    elapsed_ms: 90,
                    enrichment: BTreeMap::new(),
                    evidence: vec!["HTTP 404 (status_not_found)".into()],
                    profile_evidence: Vec::new(),
                    confidence: adler_core::ConfidenceScore::default(),
                    transport: None,
                    escalations: 0,
                },
            ],
            identity_clusters: Vec::new(),
            elapsed_ms: 210,
        }
    }

    fn outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/alice"),
            kind,
            reason: None,
            elapsed_ms: 10,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: adler_core::ConfidenceScore::default(),
            transport: None,
            escalations: 0,
        }
    }

    fn found_with_website(site: &str, website: &str) -> CheckOutcome {
        let mut outcome = outcome(site, MatchKind::Found);
        outcome
            .profile_evidence
            .push(ProfileEvidence::from_enrichment(
                site,
                &outcome.url,
                "website",
                website,
            ));
        outcome
    }

    fn large_outcomes(count: usize, generation: usize) -> Vec<CheckOutcome> {
        (0..count)
            .map(|idx| large_outcome(idx, generation))
            .collect()
    }

    fn large_outcome(idx: usize, generation: usize) -> CheckOutcome {
        let site = format!("LargeSite{idx:04}");
        let url = format!("https://large{idx:04}.example/alice");
        let mut kind = match idx % 20 {
            0 | 1 => MatchKind::Found,
            3 => MatchKind::Uncertain,
            _ => MatchKind::NotFound,
        };
        if generation > 0 && idx % 20 == 0 {
            kind = MatchKind::NotFound;
        } else if generation > 0 && idx % 20 == 2 {
            kind = MatchKind::Found;
        }

        let mut outcome = CheckOutcome {
            site: site.clone(),
            url: url.clone(),
            kind,
            reason: (kind == MatchKind::Uncertain).then_some(UncertainReason::RateLimited),
            elapsed_ms: 10 + (idx % 75) as u64,
            enrichment: BTreeMap::new(),
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: adler_core::ConfidenceScore::default(),
            transport: Some(if idx % 7 == 0 {
                TransportTier::Browser
            } else {
                TransportTier::Http
            }),
            escalations: u8::from(idx % 7 == 0),
        };

        match kind {
            MatchKind::Found => {
                let observed_at_ms = 1_781_192_451_000 + generation as u64 * 1_000 + idx as u64;
                let website = format!("https://identity-{:02}.example", idx % 25);
                let name = format!("Alice Group {:02}", idx % 50);
                let bio = if generation > 0 && idx % 20 == 1 {
                    format!("updated profile generation {generation} for {idx}")
                } else {
                    format!("stable profile generation 0 for {idx}")
                };
                for (field, value) in [
                    ("website", website.as_str()),
                    ("name", name.as_str()),
                    ("bio", bio.as_str()),
                ] {
                    outcome
                        .enrichment
                        .insert(field.to_owned(), value.to_owned());
                    outcome
                        .profile_evidence
                        .push(ProfileEvidence::from_enrichment_with_source(
                            &site,
                            &url,
                            field,
                            value,
                            Some(observed_at_ms),
                            Some(EvidenceAccessPath::new(
                                outcome.transport.unwrap_or(TransportTier::Http),
                                outcome.escalations,
                                idx % 11 == 0,
                            )),
                        ));
                }
                outcome.evidence = vec![
                    "HTTP 200 (status_found)".to_owned(),
                    "body matched profile marker".to_owned(),
                ];
            }
            MatchKind::NotFound => {
                outcome.evidence = vec!["HTTP 404 (status_not_found)".to_owned()];
            }
            MatchKind::Uncertain => {}
        }
        outcome.refresh_confidence();
        outcome
    }

    fn large_persisted_scan(scan_id: &str, generation: usize) -> PersistedScan {
        let outcomes = large_outcomes(2_500, generation);
        let finished = FinishedScan {
            summary: Summary::from_outcomes(&outcomes),
            identity_clusters: adler_core::build_identity_clusters("alice", &outcomes),
            elapsed_ms: 30_000 + generation as u64,
            outcomes,
        };
        PersistedScan::from_finished(
            ScanId::from(scan_id.to_owned()),
            "alice".to_owned(),
            2_500,
            1_781_192_451_000 + generation as u64 * 10_000,
            finished,
        )
    }

    #[tokio::test]
    async fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let s = sample("abc123", 1_700_000_000_000);
        save(tmp.path(), &s).await.unwrap();

        let loaded = load(tmp.path(), &s.scan_id).await.expect("loaded");
        assert_eq!(loaded.scan_id, s.scan_id);
        assert_eq!(loaded.schema_version, PERSISTED_SCAN_SCHEMA_VERSION);
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.outcomes.len(), 2);
        assert_eq!(loaded.outcomes[0].site, "GitHub");
        assert_eq!(loaded.summary.found, 1);
    }

    #[tokio::test]
    async fn save_writes_schema_version() {
        let tmp = TempDir::new().unwrap();
        let s = sample("abc123", 1_700_000_000_000);
        save(tmp.path(), &s).await.unwrap();

        let raw = fs::read_to_string(tmp.path().join("abc123.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["schema_version"],
            serde_json::json!(PERSISTED_SCAN_SCHEMA_VERSION)
        );
    }

    #[tokio::test]
    async fn save_skips_empty_identity_clusters() {
        let tmp = TempDir::new().unwrap();
        let s = sample("empty-clusters", 1_700_000_000_000);
        save(tmp.path(), &s).await.unwrap();

        let raw = fs::read_to_string(tmp.path().join("empty-clusters.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["schema_version"],
            serde_json::json!(PERSISTED_SCAN_SCHEMA_VERSION)
        );
        assert!(
            value.get("identity_clusters").is_none(),
            "empty cluster cache should stay absent from persisted JSON"
        );
    }

    #[tokio::test]
    async fn save_writes_derived_identity_clusters() {
        let tmp = TempDir::new().unwrap();
        let mut s = sample("clusters", 1_700_000_000_000);
        s.outcomes = vec![
            found_with_website("GitHub", "https://alice.dev"),
            found_with_website("GitLab", "https://alice.dev"),
        ];

        save(tmp.path(), &s).await.unwrap();

        let raw = fs::read_to_string(tmp.path().join("clusters.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["identity_clusters"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["identity_clusters"][0]["members"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn save_roundtrips_request_context() {
        let tmp = TempDir::new().unwrap();
        let context = ScanRequestContext {
            username: "alice".into(),
            derived_from: Some(ScanId::from("previous".to_owned())),
            only: vec!["Git".into()],
            exclude: Vec::new(),
            tag: vec!["coding".into()],
            exclude_tag: vec!["nsfw".into()],
            top: Some(100),
            nsfw: false,
            concurrency: Some(8),
            deadline_secs: Some(30),
            egress_names: vec!["us-resi".into()],
            disabled_matches: vec![PersistedDisabledMatch {
                name: "TikTok".into(),
                url: "https://www.tiktok.com/@{username}".into(),
                tags: vec!["social".into()],
                disabled_reason: "Honest Limits: JS hydration".into(),
            }],
        };
        let s = sample("ctx", 1_700_000_000_000).with_request_context(context.clone());
        save(tmp.path(), &s).await.unwrap();

        let loaded = load(tmp.path(), &s.scan_id).await.expect("loaded");
        assert_eq!(loaded.request_context, Some(context));
    }

    #[test]
    fn diff_scans_reports_added_removed_and_verdict_changes() {
        let mut previous = sample("old", 1_000);
        previous.outcomes = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("Reddit", MatchKind::Found),
            outcome("Mastodon", MatchKind::NotFound),
        ];
        let mut current = sample("new", 2_000);
        current.outcomes = vec![
            outcome("GitHub", MatchKind::Found),
            outcome("Reddit", MatchKind::NotFound),
            outcome("Mastodon", MatchKind::Found),
        ];

        let diff = diff_scans(&previous, &current);

        assert_eq!(diff.from_scan_id.as_str(), "old");
        assert_eq!(diff.to_scan_id.as_str(), "new");
        assert_eq!(
            diff.added_found
                .iter()
                .map(|outcome| outcome.site.as_str())
                .collect::<Vec<_>>(),
            ["Mastodon"]
        );
        assert_eq!(
            diff.removed_found
                .iter()
                .map(|outcome| outcome.site.as_str())
                .collect::<Vec<_>>(),
            ["Reddit"]
        );
        assert_eq!(diff.verdict_changes.len(), 2);
        assert_eq!(diff.verdict_changes[0].site, "Mastodon");
        assert_eq!(diff.verdict_changes[0].before, MatchKind::NotFound);
        assert_eq!(diff.verdict_changes[0].after, MatchKind::Found);
        assert_eq!(diff.verdict_changes[1].site, "Reddit");
        assert!(diff.evidence_changes.is_empty());
    }

    #[test]
    fn diff_scans_reports_profile_evidence_changes_for_still_found_sites() {
        let mut previous = sample("old", 1_000);
        let mut old_github = outcome("GitHub", MatchKind::Found);
        old_github.enrichment.insert("name".into(), "Alice".into());
        old_github
            .profile_evidence
            .push(adler_core::ProfileEvidence::from_enrichment(
                "GitHub",
                "https://github.example/alice",
                "name",
                "Alice",
            ));
        previous.outcomes = vec![old_github];

        let mut current = sample("new", 2_000);
        let mut new_github = outcome("GitHub", MatchKind::Found);
        new_github
            .enrichment
            .insert("name".into(), "Alice Liddell".into());
        new_github
            .profile_evidence
            .push(adler_core::ProfileEvidence::from_enrichment(
                "GitHub",
                "https://github.example/alice",
                "name",
                "Alice Liddell",
            ));
        current.outcomes = vec![new_github];

        let diff = diff_scans(&previous, &current);

        assert!(diff.added_found.is_empty());
        assert!(diff.removed_found.is_empty());
        assert!(diff.verdict_changes.is_empty());
        assert_eq!(diff.evidence_changes.len(), 1);
        assert_eq!(diff.evidence_changes[0].site, "GitHub");
        assert_eq!(
            diff.evidence_changes[0]
                .before_enrichment
                .get("name")
                .unwrap(),
            "Alice"
        );
        assert_eq!(
            diff.evidence_changes[0]
                .after_enrichment
                .get("name")
                .unwrap(),
            "Alice Liddell"
        );
    }

    #[test]
    fn timeline_tracks_first_seen_disappeared_and_reappeared() {
        let mut first = sample("first", 1_000);
        first.outcomes = vec![outcome("GitHub", MatchKind::Found)];
        let mut second = sample("second", 2_000);
        second.outcomes = vec![outcome("GitHub", MatchKind::NotFound)];
        let mut third = sample("third", 3_000);
        third.outcomes = vec![outcome("GitHub", MatchKind::Found)];

        let timeline = build_scan_timeline(&[third, first, second]);

        assert_eq!(timeline.username, "alice");
        assert_eq!(timeline.scan_count, 3);
        assert_eq!(timeline.from_ms, Some(1_000));
        assert_eq!(timeline.to_ms, Some(3_000));
        assert_eq!(timeline.profiles.len(), 1);
        assert_eq!(timeline.profiles[0].site, "GitHub");
        assert_eq!(timeline.profiles[0].first_seen_ms, 1_000);
        assert_eq!(timeline.profiles[0].last_seen_ms, 3_000);
        assert!(timeline.profiles[0].present_in_latest);
        assert_eq!(
            timeline
                .events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            [
                TimelineEventKind::FirstSeen,
                TimelineEventKind::Disappeared,
                TimelineEventKind::Reappeared
            ]
        );
        assert_eq!(timeline.events[1].before, Some(MatchKind::Found));
        assert_eq!(timeline.events[1].after, Some(MatchKind::NotFound));
    }

    #[test]
    fn timeline_treats_missing_site_as_disappeared() {
        let mut first = sample("first", 1_000);
        first.outcomes = vec![outcome("GitHub", MatchKind::Found)];
        let mut second = sample("second", 2_000);
        second.outcomes = vec![outcome("GitLab", MatchKind::NotFound)];

        let timeline = build_scan_timeline(&[first, second]);

        assert_eq!(timeline.profiles.len(), 1);
        assert!(!timeline.profiles[0].present_in_latest);
        assert_eq!(timeline.events.len(), 2);
        assert_eq!(timeline.events[1].kind, TimelineEventKind::Disappeared);
        assert_eq!(timeline.events[1].site, "GitHub");
        assert_eq!(timeline.events[1].after, None);
    }

    #[test]
    fn timeline_tracks_evidence_changes_for_still_found_profile() {
        let mut first = sample("first", 1_000);
        let mut old_github = outcome("GitHub", MatchKind::Found);
        old_github.enrichment.insert("name".into(), "Alice".into());
        old_github
            .profile_evidence
            .push(adler_core::ProfileEvidence::from_enrichment(
                "GitHub",
                "https://github.example/alice",
                "name",
                "Alice",
            ));
        first.outcomes = vec![old_github];

        let mut second = sample("second", 2_000);
        let mut new_github = outcome("GitHub", MatchKind::Found);
        new_github
            .enrichment
            .insert("name".into(), "Alice Liddell".into());
        new_github
            .profile_evidence
            .push(adler_core::ProfileEvidence::from_enrichment(
                "GitHub",
                "https://github.example/alice",
                "name",
                "Alice Liddell",
            ));
        second.outcomes = vec![new_github];

        let timeline = build_scan_timeline(&[first, second]);

        assert_eq!(
            timeline
                .events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            [
                TimelineEventKind::FirstSeen,
                TimelineEventKind::EvidenceChanged
            ]
        );
        let evidence_change = timeline.events[1].evidence_change.as_ref().unwrap();
        assert_eq!(
            evidence_change.before_enrichment.get("name").unwrap(),
            "Alice"
        );
        assert_eq!(
            evidence_change.after_enrichment.get("name").unwrap(),
            "Alice Liddell"
        );
    }

    #[tokio::test]
    async fn load_all_returns_newest_first() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &sample("old", 1_000)).await.unwrap();
        save(tmp.path(), &sample("mid", 2_000)).await.unwrap();
        save(tmp.path(), &sample("new", 3_000)).await.unwrap();
        let all = load_all(tmp.path()).await;
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].scan_id.as_str(), "new");
        assert_eq!(all[1].scan_id.as_str(), "mid");
        assert_eq!(all[2].scan_id.as_str(), "old");
    }

    #[tokio::test]
    async fn load_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = load(tmp.path(), &ScanId::from("nope".to_owned())).await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn load_defaults_schema_version_for_legacy_scan_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("legacy.json");
        fs::write(
            &path,
            br#"{
                "scan_id": "legacy",
                "username": "alice",
                "site_count": 0,
                "created_at_ms": 1700000000000,
                "summary": { "found": 0, "not_found": 0, "uncertain": 0 },
                "outcomes": [],
                "elapsed_ms": 0
            }"#,
        )
        .await
        .unwrap();

        let loaded = load(tmp.path(), &ScanId::from("legacy".to_owned()))
            .await
            .expect("legacy scan loads");
        assert_eq!(loaded.schema_version, PERSISTED_SCAN_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn load_derives_identity_clusters_for_legacy_scan_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("legacy-clusters.json");
        fs::write(
            &path,
            br#"{
                "schema_version": 1,
                "scan_id": "legacy-clusters",
                "username": "alice",
                "site_count": 2,
                "created_at_ms": 1700000000000,
                "summary": { "found": 2, "not_found": 0, "uncertain": 0 },
                "outcomes": [
                    {
                        "site": "GitHub",
                        "url": "https://github.example/alice",
                        "kind": "found",
                        "elapsed_ms": 10,
                        "profile_evidence": [
                            {
                                "kind": "external_link",
                                "field": "website",
                                "value": "https://alice.dev",
                                "source": {
                                    "site": "GitHub",
                                    "url": "https://github.example/alice",
                                    "origin": "extractor"
                                }
                            }
                        ]
                    },
                    {
                        "site": "GitLab",
                        "url": "https://gitlab.example/alice",
                        "kind": "found",
                        "elapsed_ms": 10,
                        "profile_evidence": [
                            {
                                "kind": "external_link",
                                "field": "website",
                                "value": "https://alice.dev/",
                                "source": {
                                    "site": "GitLab",
                                    "url": "https://gitlab.example/alice",
                                    "origin": "extractor"
                                }
                            }
                        ]
                    }
                ],
                "elapsed_ms": 20
            }"#,
        )
        .await
        .unwrap();

        let loaded = load(tmp.path(), &ScanId::from("legacy-clusters".to_owned()))
            .await
            .expect("legacy scan loads");

        assert_eq!(loaded.identity_clusters.len(), 1);
        assert_eq!(loaded.identity_clusters[0].members.len(), 2);
        assert!(!loaded.identity_clusters[0].uncertain);
    }

    #[test]
    fn large_scan_artifact_paths_handle_identity_graph_payloads() {
        let previous = large_persisted_scan("large-old", 0);
        let current = large_persisted_scan("large-new", 1);

        assert_eq!(previous.outcomes.len(), 2_500);
        assert_eq!(previous.site_count, 2_500);
        assert_eq!(
            previous.summary.found + previous.summary.not_found + previous.summary.uncertain,
            2_500
        );
        assert!(!previous.identity_clusters.is_empty());

        let raw = serde_json::to_string(&previous).unwrap();
        let decoded: PersistedScan = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.outcomes.len(), 2_500);
        assert_eq!(
            decoded.identity_clusters.len(),
            previous.identity_clusters.len()
        );

        let diff = diff_scans(&previous, &current);
        assert!(!diff.added_found.is_empty());
        assert!(!diff.removed_found.is_empty());
        assert!(!diff.verdict_changes.is_empty());
        assert!(!diff.evidence_changes.is_empty());

        let timeline = build_scan_timeline(&[previous, current]);
        assert_eq!(timeline.scan_count, 2);
        assert_eq!(timeline.profiles.len(), 375);
        assert!(timeline.events.len() > timeline.profiles.len());
    }

    #[tokio::test]
    async fn load_all_skips_unrelated_files() {
        let tmp = TempDir::new().unwrap();
        // Drop a non-JSON file and a malformed JSON file alongside.
        fs::write(tmp.path().join("README"), b"not json")
            .await
            .unwrap();
        fs::write(tmp.path().join("broken.json"), b"{ invalid")
            .await
            .unwrap();
        save(tmp.path(), &sample("good", 9_999)).await.unwrap();
        let all = load_all(tmp.path()).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].scan_id.as_str(), "good");
    }

    #[tokio::test]
    async fn prune_keeps_only_newest_n() {
        let tmp = TempDir::new().unwrap();
        for i in 0u64..5 {
            save(tmp.path(), &sample(&format!("s{i}"), i * 1_000))
                .await
                .unwrap();
        }
        let removed = prune(tmp.path(), 2).await;
        assert_eq!(removed, 3);
        let remaining = load_all(tmp.path()).await;
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].scan_id.as_str(), "s4");
        assert_eq!(remaining[1].scan_id.as_str(), "s3");
    }
}
