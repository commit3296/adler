//! `AdlerMcp` — the [`rmcp::ServerHandler`] that backs the MCP server.
//!
//! Built on the `rmcp` `#[tool_router]` + `#[tool_handler]` macro
//! pattern: tool methods are declared on the inherent `impl`, and the
//! macros wire `list_tools` / `call_tool` automatically. Resource and
//! prompt support land in follow-up commits.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use adler_core::doctor::{self, DoctorReport};
use adler_core::executor;
use adler_core::{CheckOutcome, Client, ExecutorOptions, MatchKind, Registry, Username};
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Json;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::Implementation;
use rmcp::model::InitializeResult;
use rmcp::model::Meta;
use rmcp::model::ProgressNotificationParam;
use rmcp::model::ServerCapabilities;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// MCP server backing Adler's OSINT capabilities.
///
/// Construct via [`AdlerMcp::new`] or [`AdlerMcp::with_registry`] and
/// hand to one of the transport launchers
/// (`adler_mcp::run_stdio`, `run_http` — the latter ships in a
/// follow-up). Cloning is cheap (the registry sits behind an
/// [`Arc`]) — the server passes itself by `Arc` to rmcp's transport
/// drivers.
#[derive(Clone)]
pub struct AdlerMcp {
    registry: Arc<Registry>,
    client: Arc<Client>,
    scans_dir: Arc<std::path::PathBuf>,
    // The `#[tool_handler]` macro reads `self.tool_router` to dispatch
    // tool calls; the field would otherwise look unused to the
    // compiler.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl AdlerMcp {
    /// Build a server backed by the default embedded registry and a
    /// client with sensible defaults (10s timeout, no retries, no
    /// proxy / browser backend).
    ///
    /// # Errors
    ///
    /// Returns an error if the embedded registry fails to load
    /// (shouldn't happen on a release build — the registry is
    /// validated at compile time via `include_str!`) or if the
    /// default HTTP client fails to initialise.
    pub fn new() -> crate::Result<Self> {
        let registry = Arc::new(Registry::default_embedded()?);
        let client = Arc::new(default_client()?);
        Ok(Self::with_components(registry, client, default_scans_dir()))
    }

    /// Build a server with explicit components. Useful for tests and
    /// for hosts that want to pre-configure the HTTP client (proxy,
    /// browser backend, custom timeout, …) before handing it to MCP.
    #[must_use]
    pub fn with_components(
        registry: Arc<Registry>,
        client: Arc<Client>,
        scans_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            registry,
            client,
            scans_dir: Arc::new(scans_dir),
            tool_router: Self::tool_router(),
        }
    }

    /// Build a server backed by an explicit registry. Useful when the
    /// caller has already loaded a custom `--sites` file or wants the
    /// WMN-merged variant. Uses the default HTTP client + scans dir.
    ///
    /// # Errors
    ///
    /// Returns an error if the default HTTP client fails to initialise.
    pub fn with_registry(registry: Arc<Registry>) -> crate::Result<Self> {
        let client = Arc::new(default_client()?);
        Ok(Self::with_components(registry, client, default_scans_dir()))
    }

    /// The shared registry — exposed so transport launchers and
    /// future tools can reach the live site list without re-loading.
    #[must_use]
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }
}

/// Default HTTP client used when the host doesn't supply one.
fn default_client() -> crate::Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .max_retries(0)
        .build()
        .map_err(crate::Error::Core)
}

/// Default persisted-scans directory: `$XDG_CACHE_HOME/adler/scans/`,
/// falling back to `$HOME/.cache/adler/scans/`. Mirrors the path
/// `adler-server` and `adler-cli` use, so an MCP host running on the
/// same machine sees the same history.
fn default_scans_dir() -> std::path::PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return std::path::PathBuf::from(xdg).join("adler").join("scans");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home)
            .join(".cache")
            .join("adler")
            .join("scans");
    }
    std::path::PathBuf::from("adler-scans")
}

#[tool_router]
impl AdlerMcp {
    /// List sites in the embedded registry, optionally filtered.
    ///
    /// Mirrors the CLI's `--list-sites` flag: respects `tag` /
    /// `exclude_tag` / `include_nsfw`. Returns one entry per matching
    /// enabled site (disabled entries are never surfaced — for a view
    /// of disabled entries with their reasons, the planned
    /// `adler://registry/disabled` resource is the right surface).
    #[tool(
        name = "list_sites",
        description = "List enabled sites in the Adler registry, optionally filtered \
                       by tag / exclude_tag / include_nsfw. Returns name, URL template, \
                       tags, and popularity rank for each match."
    )]
    pub fn list_sites(&self, Parameters(args): Parameters<ListSitesArgs>) -> Json<ListSitesOutput> {
        let sites = self.registry.filter(
            &[],
            &[],
            &args.tag.unwrap_or_default(),
            &args.exclude_tag.unwrap_or_default(),
            args.include_nsfw.unwrap_or(false),
        );
        let entries: Vec<SiteEntry> = sites
            .into_iter()
            .map(|s| SiteEntry {
                name: s.name,
                url: s.url.as_str().to_owned(),
                tags: s.tags,
                popularity: s.popularity,
            })
            .collect();
        let total = entries.len();
        Json(ListSitesOutput {
            total,
            sites: entries,
        })
    }

    /// Scan a single username across the filtered registry. Streams
    /// per-site progress as MCP `notifications/progress` messages when
    /// the client supplies a `progressToken` in `_meta`; the final
    /// return value is always the aggregated [`ScanOutput`].
    #[tool(
        name = "scan_username",
        description = "Scan a single username across Adler's site registry, optionally \
                       filtered by only / exclude / tag / exclude_tag / include_nsfw / top. \
                       Emits MCP progress notifications per site outcome; returns the \
                       aggregated verdict array plus counts (found, not_found, uncertain) \
                       once every probe completes."
    )]
    pub async fn scan_username(
        &self,
        Parameters(args): Parameters<ScanUsernameArgs>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<Json<ScanOutput>, rmcp::ErrorData> {
        let username = Username::new(args.username.clone())
            .map_err(|e| rmcp::ErrorData::invalid_params(e.to_string(), None))?;
        let sites = self.filtered_sites(&args.filter);
        if sites.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "no sites match the supplied filter",
                None,
            ));
        }
        let total = sites.len();
        let outcomes = self
            .run_scan_with_progress(sites, username.clone(), &meta, &peer, args.concurrency)
            .await?;
        let summary = ScanSummary::from_outcomes(&outcomes);
        Ok(Json(ScanOutput {
            username: args.username,
            total_probed: total,
            summary,
            outcomes: outcomes.into_iter().map(OutcomeRow::from).collect(),
        }))
    }

    /// Scan many usernames sequentially. Streaming progress carries
    /// across the whole batch (one progress token covers all outcomes
    /// from all usernames); the return value is a per-username
    /// breakdown.
    #[tool(
        name = "scan_batch",
        description = "Scan a list of usernames against the filtered Adler registry. \
                       Runs sequentially (parallel multi-username scanning would multiply \
                       per-host throttle pressure). Streams progress across the whole batch; \
                       returns one entry per username with its own summary + outcomes."
    )]
    pub async fn scan_batch(
        &self,
        Parameters(args): Parameters<ScanBatchArgs>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<Json<BatchScanOutput>, rmcp::ErrorData> {
        if args.usernames.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "usernames array must not be empty",
                None,
            ));
        }
        let sites = self.filtered_sites(&args.filter);
        if sites.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "no sites match the supplied filter",
                None,
            ));
        }
        let mut results: Vec<ScanOutput> = Vec::with_capacity(args.usernames.len());
        for raw_username in args.usernames {
            let username = match Username::new(raw_username.clone()) {
                Ok(u) => u,
                Err(e) => {
                    results.push(ScanOutput {
                        username: raw_username,
                        total_probed: 0,
                        summary: ScanSummary {
                            error: Some(e.to_string()),
                            ..Default::default()
                        },
                        outcomes: Vec::new(),
                    });
                    continue;
                }
            };
            let outcomes = self
                .run_scan_with_progress(
                    sites.clone(),
                    username.clone(),
                    &meta,
                    &peer,
                    args.concurrency,
                )
                .await?;
            let summary = ScanSummary::from_outcomes(&outcomes);
            results.push(ScanOutput {
                username: raw_username,
                total_probed: sites.len(),
                summary,
                outcomes: outcomes.into_iter().map(OutcomeRow::from).collect(),
            });
        }
        let total_usernames = results.len();
        Ok(Json(BatchScanOutput {
            total_usernames,
            per_username: results,
        }))
    }

    /// Run the doctor's health check against a single named site.
    /// Useful for the agent to diagnose "why didn't this site come back
    /// Found?" without re-running the full doctor over the whole
    /// registry.
    #[tool(
        name = "doctor_check",
        description = "Run the doctor's health probes against one named site. The doctor \
                       probes the site's known_present user (must resolve to Found) and a \
                       random nonsense user (must NOT resolve to Found). Returns the verdict \
                       plus any issue strings. Returns invalid_params if the site name isn't \
                       in the registry or is disabled."
    )]
    pub async fn doctor_check(
        &self,
        Parameters(args): Parameters<DoctorCheckArgs>,
    ) -> Result<Json<DoctorCheckOutput>, rmcp::ErrorData> {
        let site = self
            .registry
            .sites()
            .iter()
            .find(|s| !s.disabled && s.name.eq_ignore_ascii_case(&args.site))
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("site {:?} not found in registry or is disabled", args.site),
                    None,
                )
            })?
            .clone();
        let report = doctor::check_site(self.client.as_ref(), &site).await;
        let (verdict, issues) = match report {
            DoctorReport::Healthy { .. } => ("healthy", Vec::new()),
            DoctorReport::Unhealthy { issues, .. } => ("unhealthy", issues),
        };
        Ok(Json(DoctorCheckOutput {
            site: site.name,
            verdict: verdict.to_owned(),
            issues,
        }))
    }

    /// Read the persisted scan history written by `adler --web`.
    /// Returns the most-recent N scans (default 20) with their tally
    /// metadata; the agent can drill into a specific scan via the
    /// planned `adler://scans/{id}` resource.
    #[tool(
        name = "get_scan_history",
        description = "List recent persisted scans from the web server's history directory \
                       ($XDG_CACHE_HOME/adler/scans/). Returns id, username, started_at, \
                       total/found/not_found/uncertain counts. Filter by username if given. \
                       Defaults to the 20 most recent."
    )]
    pub async fn get_scan_history(
        &self,
        Parameters(args): Parameters<ScanHistoryArgs>,
    ) -> Result<Json<ScanHistoryOutput>, rmcp::ErrorData> {
        let limit = args.limit.unwrap_or(20).max(1);
        let filter_username = args.username;
        let entries = read_scan_history(self.scans_dir.as_ref(), limit, filter_username.as_deref())
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("reading scan history: {e}"), None)
            })?;
        let total = entries.len();
        Ok(Json(ScanHistoryOutput {
            total,
            scans: entries,
        }))
    }
}

impl AdlerMcp {
    /// Filter the registry by the shared `ScanFilter` parameters,
    /// then optionally truncate to the top-N most popular.
    fn filtered_sites(&self, filter: &ScanFilter) -> Vec<adler_core::Site> {
        let mut sites = self.registry.filter(
            &filter.only.clone().unwrap_or_default(),
            &filter.exclude.clone().unwrap_or_default(),
            &filter.tag.clone().unwrap_or_default(),
            &filter.exclude_tag.clone().unwrap_or_default(),
            filter.include_nsfw.unwrap_or(false),
        );
        if let Some(top) = filter.top {
            let top = top as usize;
            sites.retain(|s| s.popularity.is_some());
            sites.sort_by_key(|s| s.popularity);
            sites.truncate(top);
        }
        sites
    }

    /// Run a scan and bridge the synchronous progress callback into
    /// MCP progress notifications when the client supplied a token.
    async fn run_scan_with_progress(
        &self,
        sites: Vec<adler_core::Site>,
        username: Username,
        meta: &Meta,
        peer: &Peer<RoleServer>,
        concurrency: Option<usize>,
    ) -> Result<Vec<CheckOutcome>, rmcp::ErrorData> {
        // Progress values are f64 per the MCP spec. The registry is
        // bounded in the low thousands, so usize-to-f64 doesn't lose
        // precision in practice.
        #[allow(clippy::cast_precision_loss)]
        let total = sites.len() as f64;
        let progress_token = meta.get_progress_token();
        let conc = NonZeroUsize::new(concurrency.unwrap_or(16).max(1))
            .unwrap_or(NonZeroUsize::new(16).expect("16 is non-zero"));
        let opts = ExecutorOptions::default().concurrency(conc);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CheckOutcome>();
        let client = self.client.clone();
        let scan_handle = tokio::spawn(async move {
            executor::run_with_progress(client.as_ref(), &sites, &username, opts, move |o| {
                let _ = tx.send(o.clone());
            })
            .await
        });

        let mut count: u64 = 0;
        while let Some(outcome) = rx.recv().await {
            count += 1;
            if let Some(token) = &progress_token {
                #[allow(clippy::cast_precision_loss)]
                let progress = count as f64;
                let _ = peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress,
                        total: Some(total),
                        message: Some(format!("{}: {:?}", outcome.site, outcome.kind)),
                    })
                    .await;
            }
        }

        scan_handle
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(format!("scan task panicked: {e}"), None))
    }
}

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

/// Envelope for the `list_sites` response.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSitesOutput {
    /// Number of sites returned after filtering.
    pub total: usize,
    /// Matching site entries, in registry order.
    pub sites: Vec<SiteEntry>,
}

/// Filter parameters shared between `scan_username` and `scan_batch`.
/// Mirrors the CLI's `--only` / `--exclude` / `--tag` / `--exclude-tag`
/// / `--include-nsfw` / `--top` flags.
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
    /// Keep only the top-N most popular sites (by `popularity` rank).
    /// Sites without a rank are excluded when `top` is set.
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
    fn from_outcomes(outcomes: &[CheckOutcome]) -> Self {
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
fn read_scan_history(
    scans_dir: &std::path::Path,
    limit: usize,
    username_filter: Option<&str>,
) -> std::io::Result<Vec<ScanHistoryRow>> {
    #[derive(Deserialize)]
    struct PersistedScanLite {
        id: Option<String>,
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

#[tool_handler]
impl ServerHandler for AdlerMcp {
    fn get_info(&self) -> InitializeResult {
        let mut server_info = Implementation::new("adler-mcp", env!("CARGO_PKG_VERSION"));
        server_info.title = Some("Adler OSINT".to_owned());
        server_info.website_url = Some("https://github.com/commit3296/adler".to_owned());

        let mut result =
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build());
        result.server_info = server_info;
        result.instructions = Some(ADLER_MCP_INSTRUCTIONS.to_owned());
        result
    }
}

const ADLER_MCP_INSTRUCTIONS: &str = concat!(
    "Adler is an OSINT username-search tool — given a username, it probes a curated ",
    "registry of sites for the presence of a matching account. Available tools: ",
    "`list_sites` (browse the registry by tag), `scan_username` (single-username ",
    "scan with streaming progress notifications), `scan_batch` (sequential multi-",
    "username scan), `doctor_check` (health probe for one named site), ",
    "`get_scan_history` (recent persisted scans from the web server's history dir). ",
    "Resources and prompt templates land in follow-up versions.\n\n",
    "For ethical use: Adler is for authorised security testing, OSINT research, and ",
    "defensive security work only. The tool detects anti-bot gates but never ",
    "circumvents them.",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs_with_embedded_registry() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        assert!(
            server.registry().len() > 100,
            "registry should be populated"
        );
    }

    #[test]
    fn list_sites_tool_returns_filtered_results() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let args = ListSitesArgs {
            tag: Some(vec!["dev".to_owned()]),
            exclude_tag: None,
            include_nsfw: Some(false),
        };
        let Json(output) = server.list_sites(Parameters(args));
        assert!(output.total > 0, "expected some dev-tagged sites");
        assert!(
            output.sites.iter().any(|s| s.name == "GitHub"),
            "GitHub should be in the dev-tagged set",
        );
        assert_eq!(output.sites.len(), output.total);
    }

    #[test]
    fn server_info_advertises_tools_capability() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, "adler-mcp");
    }

    #[test]
    fn filtered_sites_respects_top_n_popularity() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let filter = ScanFilter {
            top: Some(5),
            ..Default::default()
        };
        let sites = server.filtered_sites(&filter);
        assert_eq!(sites.len(), 5);
        // top is sorted ascending (lower rank = more popular), so all
        // entries must have a popularity field and the first must rank
        // at-most-equal to the last.
        for s in &sites {
            assert!(s.popularity.is_some(), "top-N drops unranked sites");
        }
        assert!(sites[0].popularity <= sites[sites.len() - 1].popularity);
    }

    #[tokio::test]
    async fn doctor_check_rejects_unknown_site_with_invalid_params() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let result = server
            .doctor_check(Parameters(DoctorCheckArgs {
                site: "ThisSiteIsNotInTheRegistry".to_owned(),
            }))
            .await;
        let Err(err) = result else {
            panic!("expected invalid_params for unknown site");
        };
        assert!(
            err.message.contains("not found"),
            "expected not-found message, got {err:?}",
        );
    }

    #[test]
    fn get_scan_history_returns_empty_for_nonexistent_dir() {
        let dir = std::path::PathBuf::from("/tmp/adler-mcp-nonexistent-history-dir-xyz");
        let rows = read_scan_history(&dir, 20, None).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn get_scan_history_reads_persisted_scans() {
        let tmp = tempfile::tempdir().unwrap();
        // Synthesise two persisted scans in the directory.
        for (id, name, started, found_count) in [
            ("a1", "alice", "2026-06-01T12:00:00Z", 3usize),
            ("b2", "bob", "2026-06-02T09:30:00Z", 1usize),
        ] {
            let outcomes_json: Vec<serde_json::Value> = (0..5)
                .map(|i| {
                    // MatchKind uses snake_case in JSON (see check.rs).
                    let kind = if i < found_count {
                        "found"
                    } else {
                        "not_found"
                    };
                    serde_json::json!({
                        "site": format!("Mock{i}"),
                        "url": "https://mock.example/x",
                        "kind": kind,
                        "elapsed_ms": 100u64,
                    })
                })
                .collect();
            let scan = serde_json::json!({
                "id": id,
                "username": name,
                "started_at": started,
                "outcomes": outcomes_json,
            });
            std::fs::write(
                tmp.path().join(format!("{id}.json")),
                serde_json::to_string(&scan).unwrap(),
            )
            .unwrap();
        }
        let rows = read_scan_history(tmp.path(), 10, None).unwrap();
        assert_eq!(rows.len(), 2);
        let alice = rows.iter().find(|r| r.username == "alice").unwrap();
        assert_eq!(alice.id, "a1");
        assert_eq!(alice.total, 5);
        assert_eq!(alice.found, 3);
        assert_eq!(alice.not_found, 2);

        // Username filter narrows to one row.
        let bob_only = read_scan_history(tmp.path(), 10, Some("bob")).unwrap();
        assert_eq!(bob_only.len(), 1);
        assert_eq!(bob_only[0].username, "bob");
    }

    #[test]
    fn scan_summary_counts_each_kind() {
        let outcomes = vec![
            mock_outcome("a", MatchKind::Found),
            mock_outcome("b", MatchKind::Found),
            mock_outcome("c", MatchKind::NotFound),
            mock_outcome("d", MatchKind::Uncertain),
            mock_outcome("e", MatchKind::Uncertain),
            mock_outcome("f", MatchKind::Uncertain),
        ];
        let s = ScanSummary::from_outcomes(&outcomes);
        assert_eq!(s.found, 2);
        assert_eq!(s.not_found, 1);
        assert_eq!(s.uncertain, 3);
    }

    fn mock_outcome(site: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/u"),
            kind,
            reason: None,
            elapsed_ms: 0,
            enrichment: std::collections::BTreeMap::new(),
            evidence: Vec::new(),
            transport: None,
            escalations: 0,
        }
    }
}
