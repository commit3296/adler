use adler_core::{CheckOutcome, Site};
use serde::{Deserialize, Serialize};

use crate::scan::{FinishedScan, ScanId};

#[derive(Serialize)]
pub(super) struct Health {
    pub(super) ok: bool,
    pub(super) version: &'static str,
}

/// Site summary returned by `GET /api/sites`. Strictly smaller than the
/// internal [`Site`] — we don't leak detection signals, just what a UI
/// needs to render a filter list.
#[derive(Serialize)]
pub(super) struct SiteSummary {
    name: String,
    url: String,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    popularity: Option<u32>,
}

impl From<&Site> for SiteSummary {
    fn from(s: &Site) -> Self {
        Self {
            name: s.name.clone(),
            url: s.url.as_str().to_owned(),
            tags: s.tags.clone(),
            popularity: s.popularity,
        }
    }
}

/// Disabled/parked site row surfaced for diagnostics.
#[derive(Clone, Debug, Serialize)]
pub(super) struct DisabledSiteSummary {
    pub(super) name: String,
    pub(super) url: String,
    pub(super) tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) popularity: Option<u32>,
    pub(super) disabled_reason: String,
}

impl From<&Site> for DisabledSiteSummary {
    fn from(s: &Site) -> Self {
        Self {
            name: s.name.clone(),
            url: s.url.as_str().to_owned(),
            tags: s.tags.clone(),
            popularity: s.popularity,
            disabled_reason: s
                .disabled_reason
                .clone()
                .unwrap_or_else(|| "disabled in registry".to_owned()),
        }
    }
}

/// Site catalogue returned by `GET /api/sites`.
#[derive(Serialize)]
pub(super) struct SitesResponse {
    /// Enabled entries available to scans.
    pub(super) sites: Vec<SiteSummary>,
    /// Parked entries that match the server startup filter but are not
    /// scannable. The UI uses these for honest-limit hints.
    pub(super) disabled: Vec<DisabledSiteSummary>,
}

/// Read-only view of the access engine's runtime config.
#[derive(Serialize)]
pub(super) struct AccessSummary {
    pub(super) egress: Vec<adler_core::EgressSummary>,
    pub(super) sessions: Vec<SessionName>,
}

#[derive(Serialize)]
pub(super) struct SessionName {
    pub(super) name: String,
}

/// One row in `GET /api/scans`.
#[derive(Serialize)]
pub(super) struct ScanListEntry {
    pub(super) scan_id: ScanId,
    pub(super) username: String,
    pub(super) site_count: usize,
    /// Unix epoch milliseconds when the scan was started.
    pub(super) started_at_ms: u64,
    pub(super) elapsed_ms: u64,
    /// `"running"` or `"finished"`.
    pub(super) status: &'static str,
    /// Counts present only when `status == "finished"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<crate::scan::Summary>,
}

/// Request body for `POST /api/scan`.
///
/// Filter fields mirror the CLI flags one-for-one (`--only`,
/// `--exclude`, `--tag`, `--exclude-tag`, `--top`, `--nsfw`). All are
/// optional; omitting them runs the full catalog the server was
/// launched with.
#[derive(Debug, Deserialize, Default)]
pub(super) struct StartScanRequest {
    pub(super) username: String,
    /// Only sites whose name contains one of these substrings
    /// (case-insensitive). Empty = no name include filter.
    #[serde(default)]
    pub(super) only: Vec<String>,
    /// Skip sites whose name contains any of these substrings.
    #[serde(default)]
    pub(super) exclude: Vec<String>,
    /// Only sites carrying one of these tags. Empty = no tag filter.
    /// Sites with no tags are excluded when this is non-empty.
    #[serde(default)]
    pub(super) tag: Vec<String>,
    /// Skip sites carrying any of these tags.
    #[serde(default)]
    pub(super) exclude_tag: Vec<String>,
    /// Restrict to sites whose `popularity` rank is <= top, sorted by
    /// rank. Sites without a `popularity` rank are dropped.
    #[serde(default)]
    pub(super) top: Option<u32>,
    /// Include sites tagged `nsfw`. Default false — matches the CLI.
    #[serde(default)]
    pub(super) nsfw: bool,
    /// Optional per-scan concurrency override. Falls back to the
    /// executor's default if omitted.
    #[serde(default)]
    pub(super) concurrency: Option<std::num::NonZeroUsize>,
    /// Optional total scan deadline in seconds.
    #[serde(default)]
    pub(super) deadline_secs: Option<u64>,
    /// Subset of the configured egress pool to use for this scan.
    #[serde(default)]
    pub(super) egress_names: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct StartScanResponse {
    pub(super) scan_id: ScanId,
    pub(super) username: String,
    pub(super) site_count: usize,
}

/// Body for `POST /api/scan/:id/refilter`.
///
/// Mirrors [`StartScanRequest`] minus the `username` (carried over from
/// the existing scan).
#[derive(Debug, Deserialize, Default)]
pub(super) struct RefilterRequest {
    #[serde(default)]
    pub(super) only: Vec<String>,
    #[serde(default)]
    pub(super) exclude: Vec<String>,
    #[serde(default)]
    pub(super) tag: Vec<String>,
    #[serde(default)]
    pub(super) exclude_tag: Vec<String>,
    #[serde(default)]
    pub(super) top: Option<u32>,
    #[serde(default)]
    pub(super) nsfw: bool,
    #[serde(default)]
    pub(super) concurrency: Option<std::num::NonZeroUsize>,
    #[serde(default)]
    pub(super) deadline_secs: Option<u64>,
    #[serde(default)]
    pub(super) egress_names: Vec<String>,
}

impl From<&RefilterRequest> for StartScanRequest {
    fn from(r: &RefilterRequest) -> Self {
        Self {
            username: String::new(),
            only: r.only.clone(),
            exclude: r.exclude.clone(),
            tag: r.tag.clone(),
            exclude_tag: r.exclude_tag.clone(),
            top: r.top,
            nsfw: r.nsfw,
            concurrency: r.concurrency,
            deadline_secs: r.deadline_secs,
            egress_names: r.egress_names.clone(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct RefilterResponse {
    /// Fresh scan id. The SPA switches its SSE stream over to this id.
    pub(super) scan_id: ScanId,
    /// Predecessor whose outcomes were carried into the new scan.
    pub(super) derived_from: ScanId,
    /// Number of outcomes pre-populated from the predecessor.
    pub(super) carried_outcomes: usize,
    /// Total site count for the new scan.
    pub(super) site_count: usize,
}

/// Snapshot returned by `GET /api/scan/:id`.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum ScanSnapshot {
    /// Scan is still running.
    Running {
        username: String,
        site_count: usize,
        elapsed_ms: u64,
        partial: Vec<adler_core::CheckOutcome>,
    },
    /// Scan has completed; full aggregate.
    Finished {
        username: String,
        site_count: usize,
        #[serde(flatten)]
        finished: FinishedScan,
    },
}

/// `POST /api/scan/:id/retry` request body.
#[derive(Debug, Deserialize)]
pub(super) struct RetryRequest {
    /// Name of the site to re-probe (must match `Site::name`).
    pub(super) site: String,
}

#[derive(Serialize)]
pub(super) struct RetryResponse {
    pub(super) outcome: CheckOutcome,
}

#[derive(Serialize)]
pub(super) struct StartEvent {
    pub(super) username: String,
}
