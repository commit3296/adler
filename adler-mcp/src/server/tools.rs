//! Tool input / output schemas and the persisted-scans reader.
//!
//! All the `#[derive(JsonSchema)]` types here describe one tool's
//! arguments or return value — agents see these as MCP tool schemas
//! via `tools/list`. The tool methods themselves live in
//! [`super`] under the `#[tool_router]` impl block.
//!
//! [`read_scan_history`] is the shared `scans_dir` → row reader used
//! by both the `get_scan_history` tool and the
//! `adler://scans/recent` resource.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use adler_core::{
    CheckOutcome, ClusterReason, ConfidenceReason, ConfidenceScore, IdentityCluster, MatchKind,
    ObservedProfile, ProfileEvidence, Username, build_identity_clusters,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for the `list_sites` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSitesArgs {
    /// Keep only sites carrying at least one of these tags
    /// (case-insensitive). Empty / unset means "no tag filter".
    #[serde(default)]
    pub tag: Option<Vec<String>>,
    /// Drop sites carrying any of these tags. Useful for fast clean
    /// runs (`--exclude-tag bot-protected`).
    #[serde(default)]
    pub exclude_tag: Option<Vec<String>>,
    /// Include `nsfw`-tagged sites in the result. Defaults to
    /// `false`, mirroring Sherlock's opt-in pattern and the CLI's
    /// `--nsfw` flag.
    #[serde(default)]
    pub include_nsfw: Option<bool>,
}

/// Per-site row in the `list_sites` response.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SiteEntry {
    /// Display name.
    pub name: String,
    /// URL template with `{username}` placeholder.
    pub url: String,
    /// Tags attached to this site.
    pub tags: Vec<String>,
    /// Popularity rank (lower = more popular), if set.
    pub popularity: Option<u32>,
}

/// Disabled/parked row returned alongside enabled `list_sites` matches.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DisabledSiteEntry {
    /// Display name.
    pub name: String,
    /// URL template with `{username}` placeholder.
    pub url: String,
    /// Tags attached to this site.
    pub tags: Vec<String>,
    /// Popularity rank (lower = more popular), if set.
    pub popularity: Option<u32>,
    /// Human-readable explanation for why this site is not scannable.
    pub disabled_reason: String,
}

/// Envelope for the `list_sites` response.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSitesOutput {
    /// Number of sites returned after filtering.
    pub total: usize,
    /// Matching site entries, in registry order.
    pub sites: Vec<SiteEntry>,
    /// Disabled/parked entries that matched the same filter. These are
    /// not scannable, but agents can use them to explain honest limits.
    pub disabled_matches: Vec<DisabledSiteEntry>,
}

/// Filter parameters shared between `scan_username` and `scan_batch`.
/// Mirrors the CLI's `--only` / `--exclude` / `--tag` / `--exclude-tag`
/// / `--include-nsfw` / `--top` flags. `top` is a popularity-rank
/// ceiling (`popularity <= top`), not a result-count limit.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScanFilter {
    /// Keep only sites whose name contains at least one of these
    /// substrings (case-insensitive).
    #[serde(default)]
    pub only: Option<Vec<String>>,
    /// Drop sites whose name contains any of these substrings.
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// Tag filter (case-insensitive). Empty means "no tag filter".
    #[serde(default)]
    pub tag: Option<Vec<String>>,
    /// Drop sites carrying any of these tags.
    #[serde(default)]
    pub exclude_tag: Option<Vec<String>>,
    /// Include `nsfw`-tagged sites. Defaults to `false`.
    #[serde(default)]
    pub include_nsfw: Option<bool>,
    /// Keep only sites whose `popularity` rank is `<= top`
    /// (lower rank = more popular). Sites without a rank are excluded
    /// when `top` is set.
    #[serde(default)]
    pub top: Option<u32>,
}

/// Parameters for the `scan_username` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScanUsernameArgs {
    /// Username to probe across the filtered registry.
    pub username: String,
    /// Filter parameters narrowing which sites get probed.
    #[serde(default, flatten)]
    pub filter: ScanFilter,
    /// Maximum concurrent probes. Defaults to 16; values above ~32
    /// risk hammering shared throttle pools.
    #[serde(default)]
    pub concurrency: Option<usize>,
}

/// Parameters for the `scan_batch` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScanBatchArgs {
    /// Usernames to probe sequentially.
    pub usernames: Vec<String>,
    /// Filter parameters applied to every username in the batch.
    #[serde(default, flatten)]
    pub filter: ScanFilter,
    /// Per-username concurrency limit. Same default as
    /// `scan_username`.
    #[serde(default)]
    pub concurrency: Option<usize>,
}

/// Parameters for the `doctor_check` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DoctorCheckArgs {
    /// Site name as it appears in the registry. Matched
    /// case-insensitively.
    pub site: String,
}

/// Parameters for the `get_scan_history` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScanHistoryArgs {
    /// Maximum number of scans to return. Defaults to 20. Capped at
    /// whatever's on disk.
    #[serde(default)]
    pub limit: Option<usize>,
    /// If set, only return scans whose username matches this string
    /// exactly.
    #[serde(default)]
    pub username: Option<String>,
}

/// Parameters for the `diff_scans` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScanDiffArgs {
    /// Previous persisted scan id (filename stem).
    pub from_scan_id: String,
    /// Current persisted scan id (filename stem).
    pub to_scan_id: String,
}

/// Per-site row inside [`ScanOutput`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct OutcomeRow {
    /// Site name.
    pub site: String,
    /// Verdict — `Found`, `NotFound`, `Uncertain`.
    pub kind: String,
    /// Probed URL (final URL after any redirects).
    pub url: String,
    /// Wall-clock elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Free-form reason string when `kind == Uncertain` (rate-limit,
    /// timeout, Cloudflare challenge, …).
    pub reason: Option<String>,
    /// Human-readable detection evidence lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Structured profile evidence extracted from the found page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_evidence: Vec<ProfileEvidenceRow>,
    /// Per-site verdict confidence, when the scan computed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<ConfidenceRow>,
    /// Transport tier that produced this outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Automatic escalations beyond the primary route.
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub escalations: u8,
}

impl From<CheckOutcome> for OutcomeRow {
    fn from(o: CheckOutcome) -> Self {
        Self::from(&o)
    }
}

impl From<&CheckOutcome> for OutcomeRow {
    fn from(o: &CheckOutcome) -> Self {
        Self {
            site: o.site.clone(),
            kind: format!("{:?}", o.kind),
            url: o.url.clone(),
            elapsed_ms: o.elapsed_ms,
            reason: o.reason.as_ref().map(|r| format!("{r:?}")),
            evidence: o.evidence.clone(),
            profile_evidence: o.profile_evidence.iter().map(Into::into).collect(),
            confidence: ConfidenceRow::from_score(&o.confidence),
            transport: o.transport.as_ref().map(serde_string),
            escalations: o.escalations,
        }
    }
}

/// Structured profile evidence row exposed through MCP.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProfileEvidenceRow {
    /// Evidence kind, serialized like Adler's JSON output.
    pub kind: String,
    /// Original extractor field name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Observed value.
    pub value: String,
    /// Non-secret source metadata.
    pub source: EvidenceSourceRow,
}

impl From<&ProfileEvidence> for ProfileEvidenceRow {
    fn from(evidence: &ProfileEvidence) -> Self {
        Self {
            kind: serde_string(&evidence.kind),
            field: evidence.field.clone(),
            value: evidence.value.clone(),
            source: EvidenceSourceRow::from(&evidence.source),
        }
    }
}

/// Non-secret source metadata for profile evidence.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EvidenceSourceRow {
    /// Site name that produced the evidence.
    pub site: String,
    /// Profile URL where the evidence was observed.
    pub url: String,
    /// Evidence origin.
    pub origin: String,
    /// Unix epoch milliseconds when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
    /// Coarse access-path metadata without secrets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_path: Option<EvidenceAccessPathRow>,
}

impl From<&adler_core::EvidenceSource> for EvidenceSourceRow {
    fn from(source: &adler_core::EvidenceSource) -> Self {
        Self {
            site: source.site.clone(),
            url: source.url.clone(),
            origin: serde_string(&source.origin),
            observed_at_ms: source.observed_at_ms,
            access_path: source.access_path.as_ref().map(Into::into),
        }
    }
}

/// Coarse access path for profile evidence.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EvidenceAccessPathRow {
    /// Transport tier that produced the response.
    pub transport: String,
    /// Whether Adler escalated from a cheaper route.
    #[serde(default, skip_serializing_if = "is_false")]
    pub escalated: bool,
    /// Whether an authenticated operator session was applied.
    #[serde(default, skip_serializing_if = "is_false")]
    pub authenticated: bool,
    /// Whether this evidence records that a session was required.
    #[serde(default, skip_serializing_if = "is_false")]
    pub session_required: bool,
}

impl From<&adler_core::EvidenceAccessPath> for EvidenceAccessPathRow {
    fn from(path: &adler_core::EvidenceAccessPath) -> Self {
        Self {
            transport: serde_string(&path.transport),
            escalated: path.escalated,
            authenticated: path.authenticated,
            session_required: path.session_required,
        }
    }
}

/// Explainable per-site confidence row.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfidenceRow {
    /// Numeric confidence score, 0-100.
    pub score: u8,
    /// Coarse confidence label.
    pub label: String,
    /// Machine-readable confidence reasons.
    pub reasons: Vec<ConfidenceReasonRow>,
}

impl ConfidenceRow {
    fn from_score(score: &ConfidenceScore) -> Option<Self> {
        let row = Self {
            score: score.score,
            label: serde_string(&score.label),
            reasons: score.reasons.iter().map(Into::into).collect(),
        };
        (!row.is_empty()).then_some(row)
    }

    fn is_empty(&self) -> bool {
        self.score == 0 && self.reasons.is_empty()
    }
}

/// One machine-readable confidence reason.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfidenceReasonRow {
    /// Reason kind in `snake_case`.
    pub kind: String,
    /// Count attached to count-bearing reasons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

impl From<&ConfidenceReason> for ConfidenceReasonRow {
    fn from(reason: &ConfidenceReason) -> Self {
        let (kind, count) = match reason {
            ConfidenceReason::FoundBySignal => ("found_by_signal", None),
            ConfidenceReason::NotFoundBySignal => ("not_found_by_signal", None),
            ConfidenceReason::ProfileMetadataExtracted { count } => {
                ("profile_metadata_extracted", Some(*count))
            }
            ConfidenceReason::ProfileMetadataRich { count } => {
                ("profile_metadata_rich", Some(*count))
            }
            ConfidenceReason::SignalEvidence { count } => ("signal_evidence", Some(*count)),
            ConfidenceReason::ExactUsernameMatch { count } => {
                ("exact_username_match", Some(*count))
            }
            ConfidenceReason::AuthenticatedAccess => ("authenticated_access", None),
            ConfidenceReason::BrowserTransport => ("browser_transport", None),
            ConfidenceReason::ImpersonateTransport => ("impersonate_transport", None),
            ConfidenceReason::EscalatedTransport => ("escalated_transport", None),
            ConfidenceReason::WeakStatusOnly => ("weak_status_only", None),
            ConfidenceReason::UncertainOutcome => ("uncertain_outcome", None),
            ConfidenceReason::SessionRequired => ("session_required", None),
            ConfidenceReason::TransportBlocked => ("transport_blocked", None),
        };
        Self {
            kind: kind.to_owned(),
            count,
        }
    }
}

/// Identity cluster row exposed through MCP.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IdentityClusterRow {
    /// Stable deterministic cluster id within this scan result.
    pub id: String,
    /// Profiles included in this identity candidate.
    pub members: Vec<ObservedProfileRow>,
    /// Cluster-level confidence, 0-100.
    pub confidence: u8,
    /// Evidence reasons that linked members.
    pub reasons: Vec<ClusterReasonRow>,
    /// Whether the cluster contains weak or ambiguous links.
    pub uncertain: bool,
}

impl From<&IdentityCluster> for IdentityClusterRow {
    fn from(cluster: &IdentityCluster) -> Self {
        Self {
            id: cluster.id.clone(),
            members: cluster.members.iter().map(Into::into).collect(),
            confidence: cluster.confidence,
            reasons: cluster.reasons.iter().map(Into::into).collect(),
            uncertain: cluster.uncertain,
        }
    }
}

/// Observed profile member inside an identity cluster.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ObservedProfileRow {
    /// Site name.
    pub site: String,
    /// Username scanned.
    pub username: String,
    /// Profile URL.
    pub url: String,
    /// Structured profile evidence.
    pub evidence: Vec<ProfileEvidenceRow>,
    /// Per-profile verdict confidence.
    pub confidence: Option<ConfidenceRow>,
    /// Earliest evidence timestamp for this profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
}

impl From<&ObservedProfile> for ObservedProfileRow {
    fn from(profile: &ObservedProfile) -> Self {
        Self {
            site: profile.site.clone(),
            username: profile.username.clone(),
            url: profile.url.clone(),
            evidence: profile.evidence.iter().map(Into::into).collect(),
            confidence: ConfidenceRow::from_score(&profile.confidence),
            observed_at_ms: profile.observed_at_ms,
        }
    }
}

/// Reason that linked profiles inside an identity cluster.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ClusterReasonRow {
    /// Reason kind in `snake_case`.
    pub kind: String,
    /// Shared value for value-bearing reasons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Shared phrase for biography phrase matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phrase: Option<String>,
}

impl From<&ClusterReason> for ClusterReasonRow {
    fn from(reason: &ClusterReason) -> Self {
        match reason {
            ClusterReason::SharedDisplayName { value } => Self {
                kind: "shared_display_name".to_owned(),
                value: Some(value.clone()),
                phrase: None,
            },
            ClusterReason::SharedBioPhrase { phrase } => Self {
                kind: "shared_bio_phrase".to_owned(),
                value: None,
                phrase: Some(phrase.clone()),
            },
            ClusterReason::SharedExternalLink { value } => Self {
                kind: "shared_external_link".to_owned(),
                value: Some(value.clone()),
                phrase: None,
            },
            ClusterReason::SharedLocation { value } => Self {
                kind: "shared_location".to_owned(),
                value: Some(value.clone()),
                phrase: None,
            },
            ClusterReason::SharedAvatarUrl { value } => Self {
                kind: "shared_avatar_url".to_owned(),
                value: Some(value.clone()),
                phrase: None,
            },
            ClusterReason::HistoricalCoOccurrence => Self {
                kind: "historical_co_occurrence".to_owned(),
                value: None,
                phrase: None,
            },
        }
    }
}

/// Aggregated counts for a single scan.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ScanSummary {
    /// Number of `Found` verdicts.
    pub found: usize,
    /// Number of `NotFound` verdicts.
    pub not_found: usize,
    /// Number of `Uncertain` verdicts.
    pub uncertain: usize,
    /// Set when the username failed validation (only ever appears in
    /// `scan_batch` per-username rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ScanSummary {
    pub(super) fn from_outcomes(outcomes: &[CheckOutcome]) -> Self {
        let mut s = Self::default();
        for o in outcomes {
            match o.kind {
                MatchKind::Found => s.found += 1,
                MatchKind::NotFound => s.not_found += 1,
                MatchKind::Uncertain => s.uncertain += 1,
            }
        }
        s
    }
}

/// Envelope for `scan_username` (also the per-username row inside
/// [`BatchScanOutput`]).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanOutput {
    /// Username scanned.
    pub username: String,
    /// Number of sites actually probed (after filtering).
    pub total_probed: usize,
    /// Aggregated counts.
    pub summary: ScanSummary,
    /// Per-site outcomes, in registry order.
    pub outcomes: Vec<OutcomeRow>,
    /// Identity candidates derived from found outcomes with structured
    /// profile evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_clusters: Vec<IdentityClusterRow>,
}

impl ScanOutput {
    pub(super) fn from_outcomes(
        username: String,
        total_probed: usize,
        outcomes: &[CheckOutcome],
    ) -> Self {
        let summary = ScanSummary::from_outcomes(outcomes);
        let identity_clusters = build_identity_clusters(&username, outcomes)
            .iter()
            .map(Into::into)
            .collect();
        let outcomes = outcomes.iter().map(OutcomeRow::from).collect();
        Self {
            username,
            total_probed,
            summary,
            outcomes,
            identity_clusters,
        }
    }
}

/// Envelope for `scan_batch`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BatchScanOutput {
    /// Number of usernames in the batch.
    pub total_usernames: usize,
    /// One [`ScanOutput`] per username, in input order.
    pub per_username: Vec<ScanOutput>,
}

/// Envelope for `doctor_check`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DoctorCheckOutput {
    /// Canonical site name as it appears in the registry.
    pub site: String,
    /// Verdict — `healthy` or `unhealthy`.
    pub verdict: String,
    /// Reason strings when unhealthy; empty when healthy.
    pub issues: Vec<String>,
}

/// One persisted-scan summary row.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanHistoryRow {
    /// Scan id (filename stem).
    pub id: String,
    /// Persisted scan artifact schema version, when present.
    pub schema_version: Option<u16>,
    /// Username scanned.
    pub username: String,
    /// ISO-8601 timestamp when the scan started.
    pub started_at: Option<String>,
    /// Total sites in this scan.
    pub total: usize,
    /// Number of `Found` verdicts.
    pub found: usize,
    /// Number of `NotFound` verdicts.
    pub not_found: usize,
    /// Number of `Uncertain` verdicts.
    pub uncertain: usize,
}

/// Envelope for `get_scan_history`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanHistoryOutput {
    /// Number of rows returned.
    pub total: usize,
    /// Recent scans, newest first.
    pub scans: Vec<ScanHistoryRow>,
}

/// A verdict transition for one site.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VerdictChangeRow {
    /// Site name.
    pub site: String,
    /// Previous verdict.
    pub before: String,
    /// Current verdict.
    pub after: String,
}

/// A profile/evidence transition for one still-found site.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EvidenceChangeRow {
    /// Site name.
    pub site: String,
    /// Enrichment/profile fields whose values changed.
    pub changed_fields: Vec<String>,
    /// Number of normalized profile evidence items in the previous scan.
    pub before_profile_evidence_count: usize,
    /// Number of normalized profile evidence items in the current scan.
    pub after_profile_evidence_count: usize,
}

/// Envelope for `diff_scans`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanDiffOutput {
    /// Previous scan id.
    pub from_scan_id: String,
    /// Current scan id.
    pub to_scan_id: String,
    /// Found accounts that were not Found in the previous scan.
    pub added_found: Vec<OutcomeRow>,
    /// Accounts that were Found previously but are no longer Found.
    pub removed_found: Vec<OutcomeRow>,
    /// Sites present in both scans whose verdict changed.
    pub verdict_changes: Vec<VerdictChangeRow>,
    /// Found sites whose profile/enrichment evidence changed.
    pub evidence_changes: Vec<EvidenceChangeRow>,
}

/// Per-site lifecycle state inside a persisted scan timeline.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TimelineProfileRow {
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
    /// Last verdict observed for this site.
    pub last_verdict: Option<String>,
}

/// Timeline event category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum TimelineEventKind {
    /// Site was Found for the first time in the supplied scan sequence.
    FirstSeen,
    /// Site was Found before, then no longer Found.
    Disappeared,
    /// Site was absent/not found after a previous hit, then Found again.
    Reappeared,
    /// Site stayed Found but profile/enrichment evidence changed.
    EvidenceChanged,
}

/// One lifecycle event for a profile across scans.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TimelineEventRow {
    /// Scan id where the event was observed.
    pub scan_id: String,
    /// Scan start timestamp.
    pub at_ms: u64,
    /// Site name.
    pub site: String,
    /// Best URL known for the site at this point in the timeline.
    pub url: String,
    /// Event category.
    pub kind: TimelineEventKind,
    /// Previous verdict, when known.
    pub before: Option<String>,
    /// Current verdict, when this scan mentioned the site.
    pub after: Option<String>,
    /// Changed enrichment/profile fields for `evidence_changed` events.
    pub changed_fields: Vec<String>,
}

/// Envelope for an MCP scan timeline resource.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ScanTimelineOutput {
    /// Username whose persisted scans were included.
    pub username: String,
    /// Number of persisted scans considered.
    pub scan_count: usize,
    /// Oldest scan timestamp.
    pub from_ms: Option<u64>,
    /// Newest scan timestamp.
    pub to_ms: Option<u64>,
    /// Per-site lifecycle summary.
    pub profiles: Vec<TimelineProfileRow>,
    /// Chronological lifecycle events.
    pub events: Vec<TimelineEventRow>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ScanDiffError {
    #[error("invalid scan id {0:?}")]
    InvalidId(String),
    #[error("scan {0:?} not found")]
    NotFound(String),
    #[error("reading scan {id:?}: {source}")]
    Io {
        id: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing scan {id:?}: {source}")]
    Json {
        id: String,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ScanTimelineError {
    #[error("invalid username {username:?}: {reason}")]
    InvalidUsername { username: String, reason: String },
    #[error("reading scan history: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing scan {id:?}: {source}")]
    Json {
        id: String,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Deserialize)]
struct PersistedScanForDiff {
    #[serde(default)]
    scan_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    outcomes: Vec<CheckOutcome>,
}

#[derive(Debug, Deserialize)]
struct PersistedScanForTimeline {
    #[serde(default)]
    scan_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    created_at_ms: u64,
    #[serde(default)]
    outcomes: Vec<CheckOutcome>,
}

/// Read the persisted-scans directory and return up to `limit` rows,
/// newest first. Filters by exact username if `username_filter` is
/// set. Each file is `<scans_dir>/<id>.json` with an `outcomes`
/// array; we deserialise only the fields we need.
///
/// Synchronous — the directory is small (per-user history bounded to
/// a few hundred entries) and each read is one `read_to_string`.
/// Wrapping in `tokio::fs` adds complexity without measurable gain.
pub(super) fn read_scan_history(
    scans_dir: &std::path::Path,
    limit: usize,
    username_filter: Option<&str>,
) -> std::io::Result<Vec<ScanHistoryRow>> {
    #[derive(Deserialize)]
    struct PersistedScanLite {
        id: Option<String>,
        schema_version: Option<u16>,
        username: Option<String>,
        started_at: Option<String>,
        #[serde(default)]
        outcomes: Vec<CheckOutcome>,
    }

    let mut files: Vec<std::fs::DirEntry> = match std::fs::read_dir(scans_dir) {
        Ok(it) => it.filter_map(std::io::Result::ok).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // Sort by mtime descending so the newest scans surface first.
    files.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(std::cmp::Reverse)
    });

    let mut rows: Vec<ScanHistoryRow> = Vec::new();
    for entry in files {
        if rows.len() >= limit {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(lite) = serde_json::from_str::<PersistedScanLite>(&raw) else {
            continue;
        };
        let username = lite.username.unwrap_or_default();
        if let Some(filter) = username_filter
            && username != filter
        {
            continue;
        }
        let id = lite.id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned()
        });
        let summary = ScanSummary::from_outcomes(&lite.outcomes);
        rows.push(ScanHistoryRow {
            id,
            schema_version: lite.schema_version,
            username,
            started_at: lite.started_at,
            total: lite.outcomes.len(),
            found: summary.found,
            not_found: summary.not_found,
            uncertain: summary.uncertain,
        });
    }
    Ok(rows)
}

/// Read two persisted scans and return a deterministic diff.
pub(super) fn read_scan_diff(
    scans_dir: &Path,
    from_scan_id: &str,
    to_scan_id: &str,
) -> Result<ScanDiffOutput, ScanDiffError> {
    let previous = read_scan_for_diff(scans_dir, from_scan_id)?;
    let current = read_scan_for_diff(scans_dir, to_scan_id)?;
    Ok(diff_persisted_scans(&previous, &current))
}

/// Read persisted scans for one username and return a timeline summary.
pub(super) fn read_scan_timeline(
    scans_dir: &Path,
    username: &str,
) -> Result<ScanTimelineOutput, ScanTimelineError> {
    let username =
        Username::new(username.to_owned()).map_err(|err| ScanTimelineError::InvalidUsername {
            username: username.to_owned(),
            reason: err.to_string(),
        })?;
    let mut scans = read_timeline_scans(scans_dir, username.as_str())?;
    scans.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| timeline_scan_id(left).cmp(&timeline_scan_id(right)))
    });
    Ok(build_timeline(username.as_str(), &scans))
}

fn read_timeline_scans(
    scans_dir: &Path,
    username: &str,
) -> Result<Vec<PersistedScanForTimeline>, ScanTimelineError> {
    let mut scans = Vec::new();
    let entries = match std::fs::read_dir(scans_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(scans),
        Err(err) => return Err(ScanTimelineError::Io(err)),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let raw = std::fs::read_to_string(&path)?;
        let mut scan: PersistedScanForTimeline =
            serde_json::from_str(&raw).map_err(|source| ScanTimelineError::Json {
                id: id.clone(),
                source,
            })?;
        if scan.username != username {
            continue;
        }
        if scan.scan_id.is_none() && scan.id.is_none() {
            scan.scan_id = Some(id);
        }
        scans.push(scan);
    }
    Ok(scans)
}

fn build_timeline(username: &str, scans: &[PersistedScanForTimeline]) -> ScanTimelineOutput {
    let mut states: BTreeMap<String, TimelineProfileState> = BTreeMap::new();
    let mut events = Vec::new();

    for scan in scans {
        let current_by_site = outcomes_by_site(&scan.outcomes);
        let mut sites: Vec<String> = states.keys().cloned().collect();
        for site in current_by_site.keys() {
            if !states.contains_key(site.as_str()) {
                sites.push((*site).clone());
            }
        }
        sites.sort();
        sites.dedup();

        for site in sites {
            let current = current_by_site.get(&site).copied();
            apply_timeline_site(scan, &site, current, &mut states, &mut events);
        }
    }

    let profiles = states
        .into_iter()
        .map(|(site, state)| TimelineProfileRow {
            site,
            url: state.url,
            first_seen_ms: state.first_seen_ms,
            last_seen_ms: state.last_seen_ms,
            present_in_latest: state.present_in_latest,
            last_verdict: state.last_verdict.map(match_kind_name),
        })
        .collect();

    ScanTimelineOutput {
        username: username.to_owned(),
        scan_count: scans.len(),
        from_ms: scans.first().map(|scan| scan.created_at_ms),
        to_ms: scans.last().map(|scan| scan.created_at_ms),
        profiles,
        events,
    }
}

fn apply_timeline_site(
    scan: &PersistedScanForTimeline,
    site: &str,
    current: Option<&CheckOutcome>,
    states: &mut BTreeMap<String, TimelineProfileState>,
    events: &mut Vec<TimelineEventRow>,
) {
    let current_kind = current.map(|outcome| outcome.kind);
    let had_state = states.contains_key(site);
    let was_present = states
        .get(site)
        .is_some_and(|state| state.present_in_latest);

    if current_kind == Some(MatchKind::Found) {
        let outcome = current.expect("found outcome exists");
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
                Vec::new(),
            ));
        } else if !was_present {
            events.push(timeline_event(
                scan,
                site,
                &outcome.url,
                TimelineEventKind::Reappeared,
                state.last_verdict,
                current_kind,
                Vec::new(),
            ));
        } else if state.profile_evidence_changed(outcome) {
            events.push(timeline_event(
                scan,
                site,
                &outcome.url,
                TimelineEventKind::EvidenceChanged,
                Some(MatchKind::Found),
                current_kind,
                changed_fields_for_state(state, outcome),
            ));
        }
        states
            .get_mut(site)
            .expect("state inserted before found update")
            .update_found(outcome, scan.created_at_ms);
    } else if was_present {
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
            Vec::new(),
        ));
        state.present_in_latest = false;
        state.last_verdict = current_kind;
        if let Some(outcome) = current {
            state.url.clone_from(&outcome.url);
        }
    } else if let (Some(state), Some(outcome)) = (states.get_mut(site), current) {
        state.last_verdict = Some(outcome.kind);
        state.url.clone_from(&outcome.url);
    }
}

fn timeline_event(
    scan: &PersistedScanForTimeline,
    site: &str,
    url: &str,
    kind: TimelineEventKind,
    before: Option<MatchKind>,
    after: Option<MatchKind>,
    changed_fields: Vec<String>,
) -> TimelineEventRow {
    TimelineEventRow {
        scan_id: timeline_scan_id(scan),
        at_ms: scan.created_at_ms,
        site: site.to_owned(),
        url: url.to_owned(),
        kind,
        before: before.map(match_kind_name),
        after: after.map(match_kind_name),
        changed_fields,
    }
}

fn timeline_scan_id(scan: &PersistedScanForTimeline) -> String {
    scan.scan_id
        .as_ref()
        .or(scan.id.as_ref())
        .cloned()
        .unwrap_or_default()
}

fn match_kind_name(kind: MatchKind) -> String {
    format!("{kind:?}")
}

#[derive(Debug, Clone)]
struct TimelineProfileState {
    url: String,
    first_seen_ms: u64,
    last_seen_ms: u64,
    present_in_latest: bool,
    last_verdict: Option<MatchKind>,
    last_found_enrichment: BTreeMap<String, String>,
    last_found_profile_evidence: Vec<adler_core::ProfileEvidence>,
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

fn changed_fields_for_state(state: &TimelineProfileState, current: &CheckOutcome) -> Vec<String> {
    let mut fields = BTreeSet::new();
    for key in state
        .last_found_enrichment
        .keys()
        .chain(current.enrichment.keys())
    {
        if state.last_found_enrichment.get(key) != current.enrichment.get(key) {
            fields.insert(key.clone());
        }
    }
    for item in state
        .last_found_profile_evidence
        .iter()
        .chain(current.profile_evidence.iter())
    {
        fields.insert(
            item.field
                .clone()
                .unwrap_or_else(|| format!("{:?}", item.kind)),
        );
    }
    fields.into_iter().collect()
}

fn read_scan_for_diff(
    scans_dir: &Path,
    scan_id: &str,
) -> Result<PersistedScanForDiff, ScanDiffError> {
    if scan_id.is_empty() || scan_id.contains('/') || scan_id.contains('\\') {
        return Err(ScanDiffError::InvalidId(scan_id.to_owned()));
    }
    let path = scans_dir.join(format!("{scan_id}.json"));
    let raw = std::fs::read_to_string(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ScanDiffError::NotFound(scan_id.to_owned())
        } else {
            ScanDiffError::Io {
                id: scan_id.to_owned(),
                source,
            }
        }
    })?;
    let mut scan: PersistedScanForDiff =
        serde_json::from_str(&raw).map_err(|source| ScanDiffError::Json {
            id: scan_id.to_owned(),
            source,
        })?;
    if scan.scan_id.is_none() && scan.id.is_none() {
        scan.scan_id = Some(scan_id.to_owned());
    }
    Ok(scan)
}

fn diff_persisted_scans(
    previous: &PersistedScanForDiff,
    current: &PersistedScanForDiff,
) -> ScanDiffOutput {
    let previous_id = persisted_scan_id(previous);
    let current_id = persisted_scan_id(current);
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
            added_found.push(OutcomeRow::from(*current_outcome));
        }
        if let Some(previous_outcome) = previous_outcome {
            if previous_outcome.kind != current_outcome.kind {
                verdict_changes.push(VerdictChangeRow {
                    site: site.clone(),
                    before: format!("{:?}", previous_outcome.kind),
                    after: format!("{:?}", current_outcome.kind),
                });
            }
            if previous_outcome.kind == MatchKind::Found
                && current_outcome.kind == MatchKind::Found
                && profile_evidence_changed(previous_outcome, current_outcome)
            {
                evidence_changes.push(EvidenceChangeRow {
                    site: site.clone(),
                    changed_fields: changed_fields(previous_outcome, current_outcome),
                    before_profile_evidence_count: previous_outcome.profile_evidence.len(),
                    after_profile_evidence_count: current_outcome.profile_evidence.len(),
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
            removed_found.push(OutcomeRow::from(*previous_outcome));
        }
    }

    ScanDiffOutput {
        from_scan_id: previous_id,
        to_scan_id: current_id,
        added_found,
        removed_found,
        verdict_changes,
        evidence_changes,
    }
}

fn persisted_scan_id(scan: &PersistedScanForDiff) -> String {
    scan.scan_id
        .as_ref()
        .or(scan.id.as_ref())
        .cloned()
        .unwrap_or_default()
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

fn changed_fields(previous: &CheckOutcome, current: &CheckOutcome) -> Vec<String> {
    let mut fields = BTreeSet::new();
    for key in previous.enrichment.keys().chain(current.enrichment.keys()) {
        if previous.enrichment.get(key) != current.enrichment.get(key) {
            fields.insert(key.clone());
        }
    }
    for item in previous
        .profile_evidence
        .iter()
        .chain(current.profile_evidence.iter())
    {
        fields.insert(
            item.field
                .clone()
                .unwrap_or_else(|| format!("{:?}", item.kind)),
        );
    }
    fields.into_iter().collect()
}

fn serde_string<T>(value: &T) -> String
where
    T: Serialize + std::fmt::Debug,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{value:?}"))
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use adler_core::{EvidenceAccessPath, MatchKind, ProfileEvidence, TransportTier};

    use super::*;

    fn found_with_website(site: &str, website: &str) -> CheckOutcome {
        let url = format!("https://{}.example/alice", site.to_lowercase());
        let mut outcome = CheckOutcome {
            site: site.to_owned(),
            url: url.clone(),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 10,
            enrichment: BTreeMap::new(),
            evidence: vec!["HTTP 200 (status_found)".to_owned()],
            profile_evidence: vec![ProfileEvidence::from_enrichment(
                site, &url, "website", website,
            )],
            confidence: adler_core::ConfidenceScore::default(),
            transport: Some(TransportTier::Http),
            escalations: 0,
        };
        outcome.refresh_confidence();
        outcome
    }

    fn username_evidence(site: &str, url: &str) -> ProfileEvidence {
        ProfileEvidence::from_signal_username(
            site,
            url,
            "alice",
            Some(1_781_192_451_000),
            Some(EvidenceAccessPath::new(TransportTier::Http, 0, false)),
        )
    }

    fn rich_found_with_profile(
        site: &str,
        fields: &[(&str, &str)],
        transport: TransportTier,
        escalations: u8,
        authenticated: bool,
        observed_at_ms: u64,
    ) -> CheckOutcome {
        let url = format!("https://{}.example/alice", site.to_lowercase());
        let profile_evidence = fields
            .iter()
            .map(|(field, value)| {
                ProfileEvidence::from_enrichment_with_source(
                    site,
                    &url,
                    field,
                    value,
                    Some(observed_at_ms),
                    Some(EvidenceAccessPath::new(
                        transport,
                        escalations,
                        authenticated,
                    )),
                )
            })
            .chain(std::iter::once(username_evidence(site, &url)))
            .collect();
        let enrichment = fields
            .iter()
            .map(|(field, value)| ((*field).to_owned(), (*value).to_owned()))
            .collect();
        let mut outcome = CheckOutcome {
            site: site.to_owned(),
            url,
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 42,
            enrichment,
            evidence: vec![
                "HTTP 200 (status_found)".to_owned(),
                "body matched profile marker".to_owned(),
            ],
            profile_evidence,
            confidence: adler_core::ConfidenceScore::default(),
            transport: Some(transport),
            escalations,
        };
        outcome.refresh_confidence();
        outcome
    }

    fn contract_outcomes() -> Vec<CheckOutcome> {
        vec![
            rich_found_with_profile(
                "GitHub",
                &[
                    ("website", "https://alice.dev"),
                    ("name", "Alice Example"),
                    ("bio", "Security researcher and maintainer"),
                ],
                TransportTier::Browser,
                1,
                true,
                1_781_192_451_000,
            ),
            rich_found_with_profile(
                "GitLab",
                &[("website", "https://alice.dev"), ("name", "Alice Example")],
                TransportTier::Impersonate,
                0,
                false,
                1_781_192_452_000,
            ),
        ]
    }

    fn pretty_json<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn scan_output_includes_identity_clusters() {
        let outcomes = vec![
            found_with_website("GitHub", "https://alice.dev"),
            found_with_website("GitLab", "https://alice.dev"),
        ];
        let output = ScanOutput::from_outcomes("alice".to_owned(), 2, &outcomes);

        assert_eq!(output.summary.found, 2);
        assert_eq!(output.identity_clusters.len(), 1);
        assert_eq!(output.identity_clusters[0].members.len(), 2);
        assert!(!output.identity_clusters[0].uncertain);
        assert!(
            output.identity_clusters[0]
                .reasons
                .iter()
                .any(|reason| reason.kind == "shared_external_link")
        );
    }

    #[test]
    fn outcome_row_preserves_evidence_confidence_and_transport() {
        let outcome = found_with_website("GitHub", "https://alice.dev");
        let row = OutcomeRow::from(&outcome);
        let json = serde_json::to_value(&row).unwrap();

        assert_eq!(json["evidence"][0], "HTTP 200 (status_found)");
        assert_eq!(json["profile_evidence"][0]["kind"], "external_link");
        assert_eq!(json["profile_evidence"][0]["value"], "https://alice.dev");
        assert_eq!(json["confidence"]["label"], "high");
        assert_eq!(json["confidence"]["reasons"][0]["kind"], "found_by_signal");
        assert_eq!(json["transport"], "http");
    }

    #[test]
    fn outcome_row_json_contract() {
        let outcomes = contract_outcomes();
        let row = OutcomeRow::from(&outcomes[0]);

        insta::assert_snapshot!(pretty_json(&row));
    }

    #[test]
    fn scan_output_json_contract() {
        let outcomes = contract_outcomes();
        let output = ScanOutput::from_outcomes("alice".to_owned(), 2, &outcomes);

        insta::assert_snapshot!(pretty_json(&output));
    }

    #[test]
    fn batch_scan_output_json_contract() {
        let outcomes = contract_outcomes();
        let batch = BatchScanOutput {
            total_usernames: 2,
            per_username: vec![
                ScanOutput::from_outcomes("alice".to_owned(), 2, &outcomes),
                ScanOutput {
                    username: "bad user".to_owned(),
                    total_probed: 0,
                    summary: ScanSummary {
                        error: Some("invalid username: spaces are not allowed".to_owned()),
                        ..Default::default()
                    },
                    outcomes: Vec::new(),
                    identity_clusters: Vec::new(),
                },
            ],
        };

        insta::assert_snapshot!(pretty_json(&batch));
    }
}
