//! MCP resource surface — the URIs Adler exposes for browsing.
//!
//! Each [`StaticResourceSpec`] in [`STATIC_RESOURCES`] declares one
//! addressable view (`adler://registry/*`, `adler://scans/recent`),
//! and [`AdlerMcp::render_resource`] dispatches to the per-URI
//! renderer. The templated `adler://scans/{id}` route lives in
//! `render_scan_by_id`, which rejects ids containing path separators
//! before joining them with the scans directory — defence-in-depth
//! against a malicious id that tries to traverse out of `scans_dir`.

use rmcp::model::ResourceContents;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{
    AdlerMcp, RECENT_SCANS_LIMIT, ScanReportError, SiteEntry, read_investigation_report,
    read_scan_diff, read_scan_history, read_scan_timeline,
};
use adler_core::{
    CheckOutcome, HistoricalScanRef, SiteFilter, WatchScope, WatchlistConfig, WatchlistError,
    build_identity_clusters, historical_consistency_counts,
};

/// MIME type stamped onto every resource Adler exposes — list,
/// templates, and read all return `application/json`.
pub(super) const JSON_MIME: &str = "application/json";

/// Build a JSON [`ResourceContents`] payload — used by `read_resource`
/// to wrap a serialised registry view or scan envelope with the right
/// URI and MIME type.
pub(super) fn json_resource_contents(payload: String, uri: &str) -> ResourceContents {
    ResourceContents::text(payload, uri).with_mime_type(JSON_MIME.to_owned())
}

/// Static resource specs: `(uri, name, description)`. Resource
/// templates (parameterized URIs like `adler://scans/{id}`) live in
/// `list_resource_templates` instead.
pub(super) struct StaticResourceSpec {
    pub(super) uri: &'static str,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
}

pub(super) const STATIC_RESOURCES: &[StaticResourceSpec] = &[
    StaticResourceSpec {
        uri: "adler://registry/sites",
        name: "registry_sites",
        description: "Compact view of every enabled site in the registry: name, URL template, \
                      tags, popularity. The `list_sites` tool returns the same shape with \
                      filter parameters; the resource is for one-shot browsing.",
    },
    StaticResourceSpec {
        uri: "adler://registry/tags",
        name: "registry_tags",
        description: "Available tags with per-tag site counts, so the agent can pick a useful \
                      filter for `list_sites` / `scan_username` before scanning.",
    },
    StaticResourceSpec {
        uri: "adler://registry/disabled",
        name: "registry_disabled",
        description: "Disabled entries with their `disabled_reason` annotations. Audit surface \
                      for the dedup / Honest Limits / nightly auto-disable conventions \
                      (see CONTRIBUTING.md).",
    },
    StaticResourceSpec {
        uri: "adler://scans/recent",
        name: "scans_recent",
        // If `RECENT_SCANS_LIMIT` changes, update this description.
        description: "The 50 most recent persisted scans from the web server's history dir, \
                      one summary row each.",
    },
    StaticResourceSpec {
        uri: "adler://watchlists/default",
        name: "watchlist_default",
        description: "Summary of the default local watchlist config from $ADLER_WATCHLIST or \
                      $XDG_CONFIG_HOME/adler/watchlist.{json,toml}. Returns configured=false \
                      when no file exists.",
    },
];

/// Error from resource rendering.
#[derive(Debug)]
pub(super) enum ResourceError {
    Unknown,
    Io(std::io::Error),
    Json(serde_json::Error),
    Diff(super::ScanDiffError),
    Timeline(super::ScanTimelineError),
    Report(ScanReportError),
    Watchlist(WatchlistResourceError),
}

/// Error while reading or validating the local watchlist resource.
#[derive(Debug, thiserror::Error)]
pub(super) enum WatchlistResourceError {
    #[error("read watchlist config {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse watchlist JSON {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("parse watchlist TOML {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("validate watchlist config {path}: {source}")]
    Validation {
        path: PathBuf,
        source: WatchlistError,
    },
}

impl AdlerMcp {
    /// Render the JSON payload for a resource URI. The MCP layer then
    /// wraps it in a `ResourceContents::TextResourceContents`.
    pub(super) fn render_resource(&self, uri: &str) -> Result<String, ResourceError> {
        match uri {
            "adler://registry/sites" => self.render_registry_sites(),
            "adler://registry/tags" => self.render_registry_tags(),
            "adler://registry/disabled" => self.render_registry_disabled(),
            "adler://scans/recent" => self.render_scans_recent(),
            "adler://watchlists/default" => self.render_watchlist_default(),
            other => {
                if let Some(id) = other.strip_prefix("adler://reports/") {
                    return self.render_report_by_id(id);
                }
                if let Some(username) = other.strip_prefix("adler://timelines/") {
                    return self.render_scan_timeline(username);
                }
                let Some(tail) = other.strip_prefix("adler://scans/") else {
                    return Err(ResourceError::Unknown);
                };
                if let Some((from, to)) = tail.split_once("/diff/") {
                    self.render_scan_diff(from, to)
                } else {
                    self.render_scan_by_id(tail)
                }
            }
        }
    }

    fn render_registry_sites(&self) -> Result<String, ResourceError> {
        let entries: Vec<SiteEntry> = self
            .registry
            .sites()
            .iter()
            .filter(|s| !s.disabled)
            .map(|s| SiteEntry {
                name: s.name.clone(),
                url: s.url.as_str().to_owned(),
                tags: s.tags.clone(),
                popularity: s.popularity,
            })
            .collect();
        let envelope = serde_json::json!({
            "total": entries.len(),
            "sites": entries,
        });
        serde_json::to_string_pretty(&envelope).map_err(ResourceError::Json)
    }

    fn render_registry_tags(&self) -> Result<String, ResourceError> {
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for site in self.registry.sites() {
            if site.disabled {
                continue;
            }
            for tag in &site.tags {
                *counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
        let entries: Vec<serde_json::Value> = counts
            .into_iter()
            .map(|(tag, count)| serde_json::json!({"tag": tag, "site_count": count}))
            .collect();
        let envelope = serde_json::json!({
            "total_tags": entries.len(),
            "tags": entries,
        });
        serde_json::to_string_pretty(&envelope).map_err(ResourceError::Json)
    }

    fn render_registry_disabled(&self) -> Result<String, ResourceError> {
        let entries: Vec<serde_json::Value> = self
            .registry
            .sites()
            .iter()
            .filter(|s| s.disabled)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "url": s.url.as_str(),
                    "disabled_reason": s.disabled_reason,
                })
            })
            .collect();
        let envelope = serde_json::json!({
            "total": entries.len(),
            "disabled": entries,
        });
        serde_json::to_string_pretty(&envelope).map_err(ResourceError::Json)
    }

    fn render_scans_recent(&self) -> Result<String, ResourceError> {
        let rows = read_scan_history(self.scans_dir.as_ref(), RECENT_SCANS_LIMIT, None)
            .map_err(ResourceError::Io)?;
        let envelope = serde_json::json!({
            "total": rows.len(),
            "scans": rows,
        });
        serde_json::to_string_pretty(&envelope).map_err(ResourceError::Json)
    }

    fn render_scan_by_id(&self, id: &str) -> Result<String, ResourceError> {
        // Defensive: reject any id with a path separator so we can't be
        // tricked into reading arbitrary files via `..` or absolute
        // paths. `adler-server` writes ids that are URL-safe random
        // strings, so a legitimate id never contains slashes.
        if id.is_empty() || id.contains('/') || id.contains('\\') {
            return Err(ResourceError::Unknown);
        }
        let path = self.scans_dir.join(format!("{id}.json"));
        match std::fs::read_to_string(&path) {
            Ok(raw) => self.render_scan_by_id_payload(id, &raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ResourceError::Unknown),
            Err(e) => Err(ResourceError::Io(e)),
        }
    }

    fn render_scan_by_id_payload(
        &self,
        fallback_id: &str,
        raw: &str,
    ) -> Result<String, ResourceError> {
        let mut value: serde_json::Value =
            serde_json::from_str(raw).map_err(ResourceError::Json)?;
        let Some(mut scan) = OverlayScan::from_value(fallback_id, &value) else {
            return serde_json::to_string_pretty(&value).map_err(ResourceError::Json);
        };

        let related_scans = read_overlay_scans(self.scans_dir.as_ref());
        apply_overlay_scan_confidence(&mut scan, &related_scans);

        if let Some(object) = value.as_object_mut() {
            object.insert(
                "outcomes".to_owned(),
                serde_json::to_value(&scan.outcomes).map_err(ResourceError::Json)?,
            );
            let identity_clusters = build_identity_clusters(&scan.username, &scan.outcomes);
            if identity_clusters.is_empty() {
                object.remove("identity_clusters");
            } else {
                object.insert(
                    "identity_clusters".to_owned(),
                    serde_json::to_value(identity_clusters).map_err(ResourceError::Json)?,
                );
            }
        }

        serde_json::to_string_pretty(&value).map_err(ResourceError::Json)
    }

    fn render_scan_diff(&self, from: &str, to: &str) -> Result<String, ResourceError> {
        let diff =
            read_scan_diff(self.scans_dir.as_ref(), from, to).map_err(ResourceError::Diff)?;
        serde_json::to_string_pretty(&diff).map_err(ResourceError::Json)
    }

    fn render_scan_timeline(&self, username: &str) -> Result<String, ResourceError> {
        let timeline = read_scan_timeline(self.scans_dir.as_ref(), username)
            .map_err(ResourceError::Timeline)?;
        serde_json::to_string_pretty(&timeline).map_err(ResourceError::Json)
    }

    fn render_report_by_id(&self, id: &str) -> Result<String, ResourceError> {
        let report = read_investigation_report(self.scans_dir.as_ref(), id)
            .map_err(ResourceError::Report)?;
        serde_json::to_string_pretty(&report).map_err(ResourceError::Json)
    }

    fn render_watchlist_default(&self) -> Result<String, ResourceError> {
        let explicit_path = self
            .watchlist_path
            .as_ref()
            .map(|path| path.as_ref().as_path());
        let summary =
            read_default_watchlist_summary(explicit_path).map_err(ResourceError::Watchlist)?;
        serde_json::to_string_pretty(&summary).map_err(ResourceError::Json)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OverlayScan {
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

impl OverlayScan {
    fn from_value(fallback_id: &str, value: &serde_json::Value) -> Option<Self> {
        let mut scan: Self = serde_json::from_value(value.clone()).ok()?;
        scan.ensure_id(fallback_id);
        Some(scan)
    }

    fn ensure_id(&mut self, fallback_id: &str) {
        if self.scan_id.is_none() && self.id.is_none() {
            self.scan_id = Some(fallback_id.to_owned());
        }
    }

    fn stable_id(&self) -> &str {
        self.scan_id.as_deref().or(self.id.as_deref()).unwrap_or("")
    }
}

fn read_overlay_scans(scans_dir: &Path) -> Vec<OverlayScan> {
    let Ok(entries) = std::fs::read_dir(scans_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::io::Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                return None;
            }
            let fallback_id = path.file_stem().and_then(|s| s.to_str())?;
            let raw = std::fs::read_to_string(&path).ok()?;
            let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
            OverlayScan::from_value(fallback_id, &value)
        })
        .collect()
}

fn apply_overlay_scan_confidence(current: &mut OverlayScan, related_scans: &[OverlayScan]) {
    if current.username.is_empty() {
        for outcome in &mut current.outcomes {
            outcome.refresh_confidence();
        }
        return;
    }

    let current_ref = HistoricalScanRef {
        scan_id: current.stable_id(),
        username: &current.username,
        created_at_ms: current.created_at_ms,
        outcomes: &current.outcomes,
    };
    let related_refs = related_scans.iter().map(|scan| HistoricalScanRef {
        scan_id: scan.stable_id(),
        username: &scan.username,
        created_at_ms: scan.created_at_ms,
        outcomes: &scan.outcomes,
    });
    let counts = historical_consistency_counts(current_ref, related_refs);

    for outcome in &mut current.outcomes {
        let count = counts.get(&outcome.site).copied().unwrap_or(0);
        outcome.refresh_confidence_with_history(count);
    }
}

#[derive(Debug, Serialize)]
struct WatchlistSummary {
    configured: bool,
    searched_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_version: Option<u16>,
    target_count: usize,
    alias_count: usize,
    scan_target_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule: Option<adler_core::ScanSchedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_scope: Option<WatchScope>,
    targets: Vec<WatchTargetSummary>,
    scan_targets: Vec<WatchScanTargetSummary>,
}

#[derive(Debug, Serialize)]
struct WatchTargetSummary {
    identity: String,
    aliases: Vec<String>,
    scan_usernames: Vec<String>,
    scope: WatchScope,
    effective_scope: WatchScope,
}

#[derive(Debug, Serialize)]
struct WatchScanTargetSummary {
    identity: String,
    username: String,
    scope: SiteFilterSummary,
}

#[derive(Debug, Serialize)]
struct SiteFilterSummary {
    only: Vec<String>,
    exclude: Vec<String>,
    tag: Vec<String>,
    exclude_tag: Vec<String>,
    include_nsfw: bool,
    top: Option<u32>,
}

impl From<&SiteFilter> for SiteFilterSummary {
    fn from(scope: &SiteFilter) -> Self {
        Self {
            only: scope.include.clone(),
            exclude: scope.exclude.clone(),
            tag: scope.tags.clone(),
            exclude_tag: scope.exclude_tags.clone(),
            include_nsfw: scope.include_nsfw,
            top: scope.top,
        }
    }
}

fn read_default_watchlist_summary(
    explicit_path: Option<&Path>,
) -> Result<WatchlistSummary, WatchlistResourceError> {
    let paths = watchlist_candidate_paths(explicit_path);
    let searched_paths = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    for path in &paths {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(WatchlistResourceError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let config = parse_watchlist_config(path, &raw)?;
        config
            .validate()
            .map_err(|source| WatchlistResourceError::Validation {
                path: path.clone(),
                source,
            })?;
        return configured_watchlist_summary(path, searched_paths, config);
    }

    Ok(WatchlistSummary {
        configured: false,
        searched_paths,
        path: None,
        schema_version: None,
        target_count: 0,
        alias_count: 0,
        scan_target_count: 0,
        schedule: None,
        default_scope: None,
        targets: Vec::new(),
        scan_targets: Vec::new(),
    })
}

fn watchlist_candidate_paths(explicit_path: Option<&Path>) -> Vec<PathBuf> {
    if let Some(path) = explicit_path {
        return vec![path.to_path_buf()];
    }
    if let Some(path) = std::env::var_os("ADLER_WATCHLIST") {
        return vec![PathBuf::from(path)];
    }

    let mut paths = Vec::new();
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        let dir = PathBuf::from(config_home).join("adler");
        paths.push(dir.join("watchlist.json"));
        paths.push(dir.join("watchlist.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home).join(".config").join("adler");
        paths.push(dir.join("watchlist.json"));
        paths.push(dir.join("watchlist.toml"));
    }
    paths
}

fn parse_watchlist_config(
    path: &Path,
    raw: &str,
) -> Result<WatchlistConfig, WatchlistResourceError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => toml::from_str(raw).map_err(|source| WatchlistResourceError::Toml {
            path: path.to_path_buf(),
            source,
        }),
        _ => serde_json::from_str(raw).map_err(|source| WatchlistResourceError::Json {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn configured_watchlist_summary(
    path: &Path,
    searched_paths: Vec<String>,
    config: WatchlistConfig,
) -> Result<WatchlistSummary, WatchlistResourceError> {
    let scan_targets =
        config
            .scan_targets()
            .map_err(|source| WatchlistResourceError::Validation {
                path: path.to_path_buf(),
                source,
            })?;
    let alias_count = config
        .targets
        .iter()
        .map(|target| target.aliases.len())
        .sum();
    let targets = config
        .targets
        .iter()
        .map(|target| {
            let effective_scope = config.default_scope.merged(&target.scope);
            let mut scan_usernames = Vec::with_capacity(1 + target.aliases.len());
            scan_usernames.push(target.username.clone());
            scan_usernames.extend(target.aliases.clone());
            WatchTargetSummary {
                identity: target.username.clone(),
                aliases: target.aliases.clone(),
                scan_usernames,
                scope: target.scope.clone(),
                effective_scope,
            }
        })
        .collect();
    let scan_targets = scan_targets
        .iter()
        .map(|target| WatchScanTargetSummary {
            identity: target.identity.clone(),
            username: target.username.clone(),
            scope: SiteFilterSummary::from(&target.scope),
        })
        .collect::<Vec<_>>();
    let target_count = config.targets.len();
    let scan_target_count = scan_targets.len();

    Ok(WatchlistSummary {
        configured: true,
        searched_paths,
        path: Some(path.display().to_string()),
        schema_version: Some(config.schema_version),
        target_count,
        alias_count,
        scan_target_count,
        schedule: config.schedule,
        default_scope: Some(config.default_scope),
        targets,
        scan_targets,
    })
}
