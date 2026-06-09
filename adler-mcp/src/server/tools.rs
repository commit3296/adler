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

use adler_core::{CheckOutcome, MatchKind};
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
}

impl From<CheckOutcome> for OutcomeRow {
    fn from(o: CheckOutcome) -> Self {
        Self {
            site: o.site,
            kind: format!("{:?}", o.kind),
            url: o.url,
            elapsed_ms: o.elapsed_ms,
            reason: o.reason.map(|r| format!("{r:?}")),
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
