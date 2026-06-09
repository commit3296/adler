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

use std::path::{Path, PathBuf};

use adler_core::{CheckOutcome, Site};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{Error, Result};
use crate::scan::{FinishedScan, ScanId, Summary};

/// Hard cap on how many scans we keep on disk. Beyond this, oldest
/// (by `created_at_ms`) get [`prune`]d on the next save. Picked to be
/// large enough for any plausible human-driven OSINT session.
pub(crate) const MAX_PERSISTED_SCANS: usize = 200;
/// Current on-disk schema version for [`PersistedScan`].
pub(crate) const PERSISTED_SCAN_SCHEMA_VERSION: u16 = 1;

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
        Self {
            schema_version: PERSISTED_SCAN_SCHEMA_VERSION,
            scan_id,
            username,
            request_context: None,
            site_count,
            created_at_ms,
            summary: finished.summary,
            outcomes: finished.outcomes,
            elapsed_ms: finished.elapsed_ms,
        }
    }

    /// Attach request-scope metadata to this persisted scan.
    #[must_use]
    pub fn with_request_context(mut self, context: ScanRequestContext) -> Self {
        self.request_context = Some(context);
        self
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
    let body = serde_json::to_vec_pretty(scan).map_err(Error::PersistEncode)?;
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
    serde_json::from_slice(&bytes).ok().map(refresh_confidence)
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
        out.push(refresh_confidence(scan));
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.created_at_ms));
    out
}

fn refresh_confidence(mut scan: PersistedScan) -> PersistedScan {
    for outcome in &mut scan.outcomes {
        outcome.refresh_confidence();
    }
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
    use adler_core::MatchKind;
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
            elapsed_ms: 210,
        }
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
