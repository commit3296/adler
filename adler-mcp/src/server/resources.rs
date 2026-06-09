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

use super::{AdlerMcp, RECENT_SCANS_LIMIT, SiteEntry, read_scan_diff, read_scan_history};

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
];

/// Error from resource rendering.
#[derive(Debug)]
pub(super) enum ResourceError {
    Unknown,
    Io(std::io::Error),
    Json(serde_json::Error),
    Diff(super::ScanDiffError),
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
            other => {
                other
                    .strip_prefix("adler://scans/")
                    .map_or(Err(ResourceError::Unknown), |tail| {
                        if let Some((from, to)) = tail.split_once("/diff/") {
                            self.render_scan_diff(from, to)
                        } else {
                            self.render_scan_by_id(tail)
                        }
                    })
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
            Ok(raw) => Ok(raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ResourceError::Unknown),
            Err(e) => Err(ResourceError::Io(e)),
        }
    }

    fn render_scan_diff(&self, from: &str, to: &str) -> Result<String, ResourceError> {
        let diff =
            read_scan_diff(self.scans_dir.as_ref(), from, to).map_err(ResourceError::Diff)?;
        serde_json::to_string_pretty(&diff).map_err(ResourceError::Json)
    }
}
