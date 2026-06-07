//! `AdlerMcp` — the [`rmcp::ServerHandler`] that backs the MCP server.
//!
//! Built on the `rmcp` `#[tool_router]` + `#[tool_handler]` macro
//! pattern: tool methods are declared on the inherent `impl`, and the
//! macros wire `list_tools` / `call_tool` automatically. The
//! `ServerHandler` impl below adds the remaining surface — resources
//! (`list_resources` / `read_resource` /
//! `list_resource_templates`) and prompts (`list_prompts` /
//! `get_prompt`).

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
use rmcp::model::Annotated;
use rmcp::model::GetPromptRequestParams;
use rmcp::model::GetPromptResult;
use rmcp::model::Implementation;
use rmcp::model::InitializeResult;
use rmcp::model::ListPromptsResult;
use rmcp::model::ListResourceTemplatesResult;
use rmcp::model::ListResourcesResult;
use rmcp::model::Meta;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ProgressNotificationParam;
use rmcp::model::Prompt;
use rmcp::model::PromptArgument;
use rmcp::model::PromptMessage;
use rmcp::model::PromptMessageRole;
use rmcp::model::RawResource;
use rmcp::model::RawResourceTemplate;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResult;
use rmcp::model::Resource;
use rmcp::model::ResourceContents;
use rmcp::model::ServerCapabilities;
use rmcp::service::RequestContext;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// MCP server backing Adler's OSINT capabilities.
///
/// Construct via [`AdlerMcp::new`] or [`AdlerMcp::with_registry`] and
/// hand to one of the transport launchers (`adler_mcp::run_stdio` or
/// `adler_mcp::run_http`). Cloning is cheap — every internal field
/// is `Arc`-wrapped — so rmcp's transport drivers can pass the
/// server by value without measurable overhead.
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

/// Default HTTP timeout for the MCP-bundled client.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default per-scan concurrency for the `scan_username` /
/// `scan_batch` tools. Matches the executor's normal scan defaults;
/// higher values risk hammering shared throttle pools.
const DEFAULT_SCAN_CONCURRENCY: usize = 16;

/// Default `limit` for the `get_scan_history` tool when the caller
/// doesn't supply one.
const DEFAULT_HISTORY_LIMIT: usize = 20;

/// Maximum rows the `adler://scans/recent` resource returns.
const RECENT_SCANS_LIMIT: usize = 50;

/// Format an error and its full source chain into one human-readable
/// string. `thiserror`'s `Display` doesn't walk `source()` by default,
/// so wrapping a leaf error in a higher-level type loses information
/// when we just print `{e}`.
fn fmt_chain(err: &(dyn std::error::Error + 'static)) -> String {
    use std::fmt::Write;
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(e) = cur {
        let _ = write!(&mut out, "\n  caused by: {e}");
        cur = e.source();
    }
    out
}

/// Wrap a source error in an `invalid_params` `ErrorData`, prepending
/// `context` and appending the full error chain.
fn invalid_params_chain(context: &str, err: &(dyn std::error::Error + 'static)) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(format!("{context}: {}", fmt_chain(err)), None)
}

/// Wrap a source error in an `internal_error` `ErrorData`, prepending
/// `context` and appending the full error chain.
fn internal_error_chain(context: &str, err: &(dyn std::error::Error + 'static)) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(format!("{context}: {}", fmt_chain(err)), None)
}

/// Default HTTP client used when the host doesn't supply one.
fn default_client() -> crate::Result<Client> {
    Client::builder()
        .timeout(DEFAULT_TIMEOUT)
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
            .map_err(|e| invalid_params_chain("username validation", &e))?;
        let sites = self.filtered_sites(&args.filter);
        if sites.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "no sites match the supplied filter",
                None,
            ));
        }
        let total = sites.len();
        let outcomes = self
            .run_scan_with_progress(
                Arc::new(sites),
                username.clone(),
                &meta,
                &peer,
                args.concurrency,
            )
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
        // Share the filtered site list across every username in the
        // batch via Arc — cloning a Vec per iteration would be ~9k
        // unnecessary Site clones for a 1834-site registry × 5
        // usernames.
        let sites_total = sites.len();
        let sites = Arc::new(sites);
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
                    Arc::clone(&sites),
                    username.clone(),
                    &meta,
                    &peer,
                    args.concurrency,
                )
                .await?;
            let summary = ScanSummary::from_outcomes(&outcomes);
            results.push(ScanOutput {
                username: raw_username,
                total_probed: sites_total,
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
        let limit = args.limit.unwrap_or(DEFAULT_HISTORY_LIMIT).max(1);
        let filter_username = args.username;
        let entries = read_scan_history(self.scans_dir.as_ref(), limit, filter_username.as_deref())
            .map_err(|e| internal_error_chain("reading scan history", &e))?;
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
    ///
    /// `sites` is passed as `Arc<Vec<Site>>` so `scan_batch` can share
    /// the filtered site list across many usernames without cloning
    /// the Vec per call.
    async fn run_scan_with_progress(
        &self,
        sites: Arc<Vec<adler_core::Site>>,
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
        let conc = NonZeroUsize::new(concurrency.unwrap_or(DEFAULT_SCAN_CONCURRENCY).max(1))
            .expect(".max(1) guarantees non-zero");
        let opts = ExecutorOptions::default().concurrency(conc);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CheckOutcome>();
        let client = self.client.clone();
        let scan_handle = tokio::spawn(async move {
            executor::run_with_progress(
                client.as_ref(),
                sites.as_ref(),
                &username,
                opts,
                move |o| {
                    let _ = tx.send(o.clone());
                },
            )
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
            .map_err(|e| internal_error_chain("scan task panicked", &e))
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

        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        let mut result = InitializeResult::new(capabilities);
        result.server_info = server_info;
        result.instructions = Some(ADLER_MCP_INSTRUCTIONS.to_owned());
        result
    }

    async fn list_prompts(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, rmcp::ErrorData> {
        let prompts: Vec<Prompt> = PROMPT_SPECS
            .iter()
            .map(|spec| {
                let args: Vec<PromptArgument> = spec
                    .arguments
                    .iter()
                    .map(|a| {
                        PromptArgument::new(a.name)
                            .with_description(a.description.to_owned())
                            .with_required(a.required)
                    })
                    .collect();
                let arguments = if args.is_empty() { None } else { Some(args) };
                Prompt::new(spec.name, Some(spec.description), arguments)
            })
            .collect();
        Ok(ListPromptsResult {
            prompts,
            ..Default::default()
        })
    }

    async fn get_prompt(
        &self,
        req: GetPromptRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, rmcp::ErrorData> {
        let spec = PROMPT_SPECS
            .iter()
            .find(|s| s.name == req.name)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(format!("unknown prompt {:?}", req.name), None)
            })?;
        let args = req.arguments.unwrap_or_default();
        let text = render_prompt(spec, &args)?;
        let mut result =
            GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)]);
        result.description = Some(spec.description.to_owned());
        Ok(result)
    }

    async fn list_resources(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        let resources: Vec<Resource> = STATIC_RESOURCES
            .iter()
            .map(|spec| {
                Annotated::new(
                    RawResource::new(spec.uri, spec.name)
                        .with_description(spec.description.to_owned())
                        .with_mime_type(JSON_MIME.to_owned()),
                    None,
                )
            })
            .collect();
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn list_resource_templates(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, rmcp::ErrorData> {
        let template = Annotated::new(
            RawResourceTemplate::new("adler://scans/{id}", "scan_by_id")
                .with_description(
                    "Read one persisted scan by id (filename stem). Returns the full \
                     scan JSON envelope as written by `adler --web` to \
                     $XDG_CACHE_HOME/adler/scans/{id}.json."
                        .to_owned(),
                )
                .with_mime_type(JSON_MIME.to_owned()),
            None,
        );
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![template],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
        let payload = self.render_resource(&req.uri).map_err(|e| match e {
            ResourceError::Unknown => {
                rmcp::ErrorData::invalid_params(format!("unknown resource URI {:?}", req.uri), None)
            }
            ResourceError::Io(err) => {
                internal_error_chain(&format!("reading resource {:?}", req.uri), &err)
            }
            ResourceError::Json(err) => {
                internal_error_chain(&format!("serialising resource {:?}", req.uri), &err)
            }
        })?;
        let contents = json_resource_contents(payload, &req.uri);
        Ok(ReadResourceResult::new(vec![contents]))
    }
}

/// MIME type stamped onto every resource Adler exposes — list,
/// templates, and read all return `application/json`.
const JSON_MIME: &str = "application/json";

/// Build a JSON [`ResourceContents`] payload — used by `read_resource`
/// to wrap a serialised registry view or scan envelope with the right
/// URI and MIME type.
fn json_resource_contents(payload: String, uri: &str) -> ResourceContents {
    ResourceContents::text(payload, uri).with_mime_type(JSON_MIME.to_owned())
}

/// Static resource specs: `(uri, name, description)`. Resource
/// templates (parameterized URIs like `adler://scans/{id}`) live in
/// `list_resource_templates` instead.
struct StaticResourceSpec {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
}

const STATIC_RESOURCES: &[StaticResourceSpec] = &[
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
enum ResourceError {
    Unknown,
    Io(std::io::Error),
    Json(serde_json::Error),
}

/// Argument spec for one prompt template.
struct PromptArgSpec {
    name: &'static str,
    description: &'static str,
    required: bool,
}

/// Static spec for one prompt template, including the body text with
/// `{placeholder}` substitution points.
struct PromptSpec {
    name: &'static str,
    description: &'static str,
    arguments: &'static [PromptArgSpec],
    /// Body text with `{arg}` placeholders. Substitution is literal —
    /// arguments come from MCP and are quoted into the body verbatim,
    /// so a malicious-looking arg can't open new placeholders.
    body: &'static str,
}

const PROMPT_SPECS: &[PromptSpec] = &[
    PromptSpec {
        name: "investigate_username",
        description: "Walk the agent through a full OSINT investigation of a single username — pick a \
             scope from the registry, scan, and report Found accounts.",
        arguments: &[
            PromptArgSpec {
                name: "username",
                description: "The username to investigate.",
                required: true,
            },
            PromptArgSpec {
                name: "regions",
                description: "Comma-separated ISO-3166 country codes to prefer (e.g. \"ru,ua\"). \
                              Empty means all regions.",
                required: false,
            },
            PromptArgSpec {
                name: "categories",
                description: "Comma-separated registry tags to scope the scan (e.g. \"social,\
                              coding\"). Empty means every category — uses `adler://registry/\
                              tags` to pick.",
                required: false,
            },
        ],
        body: "\
Please investigate the username `{username}` across Adler's site registry.

Workflow:

1. Read `adler://registry/tags` to see what categories are available, and \
`adler://registry/sites` if you want the full enabled set.
2. Pick a scoped subset: regions = `{regions}`, categories = `{categories}`. \
If both are empty, default to the `social` + `coding` tags. If only regions are \
set, filter via the `region:<cc>` tags Adler attaches to each entry.
3. Call the `scan_username` tool with `username=\"{username}\"` and your filter. \
Subscribe to `notifications/progress` if you want to see per-site results stream.
4. Group the response by verdict (Found / NotFound / Uncertain). For each \
Found account, report the canonical URL.
5. If any sites came back Uncertain, note them but do not infer existence \
either way — that's the doctor's responsibility.

Be honest about scope: Adler is for authorised security testing and OSINT \
research. Do not generate or suggest harassment, doxxing, or unauthorised \
surveillance of individuals.\
",
    },
    PromptSpec {
        name: "audit_registry_health",
        description: "Walk the doctor + dedup + disabled-state surface and report what needs \
             maintainer attention (broken signals, stale known_present, importer \
             duplicates).",
        arguments: &[PromptArgSpec {
            name: "focus",
            description: "Optional sub-area: \"known_present\" / \"disabled\" / \"signals\". \
                          Empty means walk all three.",
            required: false,
        }],
        body: "\
Please audit the health of Adler's site registry. Focus area: `{focus}` (empty \
means walk everything).

Workflow:

1. Read `adler://registry/disabled` for the disabled-entry surface. Tally by \
`disabled_reason` prefix (`duplicate of …`, `Honest Limits: …`, \
`doctor: 3+ …`). Flag any entries whose reason looks stale (e.g. a `Honest \
Limits: …` site whose upstream restriction has plausibly lifted).
2. Read `adler://registry/tags` to spot tags with abnormally low or \
abnormally high counts — both are signs of importer-tag drift.
3. For each candidate (1-5 entries that look most-worth-investigating), \
invoke `doctor_check` to confirm the current verdict. Don't run more than \
~5 of these — the doctor takes ~1 second per site.
4. Report your findings as a short table: site name, current state, what \
maintainer action you recommend (re-enable, change reason, file an issue, no \
action). For \"file an issue\", link to \
<https://github.com/commit3296/adler/issues>.

If you find any obvious mistakes (e.g. a Honest-Limits-disabled site that \
clearly works now), state your evidence explicitly so the maintainer can \
verify before flipping the flag.\
",
    },
    PromptSpec {
        name: "correlate_accounts",
        description: "Scan multiple usernames, then look for shared profile signal (name, bio, \
             avatar) across the Found accounts to suggest whether they belong to one person.",
        arguments: &[PromptArgSpec {
            name: "usernames",
            description: "Comma-separated list of usernames to correlate (e.g. \
                          \"alice,alice_dev,a-liddell\").",
            required: true,
        }],
        body: "\
Please correlate the following usernames to see whether they likely belong to \
one person: `{usernames}`.

Workflow:

1. Call `scan_batch` with the comma-split username list. Use a small filter \
(e.g. `tag=[\"social\",\"coding\"]`) so the scan finishes quickly — broad \
sweeps add noise without helping correlation.
2. For each username's Found accounts, look at the `outcomes[].url` field to \
read each profile's name / bio / avatar where available. The agent doing \
this should call `scan_username` with `enrich=true` if it needs richer profile \
data than the bare-bones outcome.
3. Compare across usernames: shared exact name, shared bio fragments, shared \
avatar URLs are strong signals; shared sites alone are weak.
4. Report a confidence verdict (Strong / Plausible / Weak / Distinct) per \
pair, with the evidence that supports it.

Be honest about uncertainty: matching usernames across sites does NOT prove \
they're the same person. Multiple people use common handles. State your \
limits explicitly.\
",
    },
];

/// Substitute `{name}` placeholders in a prompt's body with the
/// argument values supplied by the client. Missing required args
/// produce `invalid_params`; missing optional args render as empty
/// strings so the prompt still parses cleanly.
fn render_prompt(
    spec: &PromptSpec,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, rmcp::ErrorData> {
    let mut body = spec.body.to_owned();
    for arg_spec in spec.arguments {
        let placeholder = format!("{{{}}}", arg_spec.name);
        let value = match args.get(arg_spec.name) {
            Some(v) => v.as_str().unwrap_or("").to_owned(),
            None if arg_spec.required => {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "prompt {:?} requires argument {:?}",
                        spec.name, arg_spec.name
                    ),
                    None,
                ));
            }
            None => String::new(),
        };
        body = body.replace(&placeholder, &value);
    }
    Ok(body)
}

impl AdlerMcp {
    /// Render the JSON payload for a resource URI. The MCP layer then
    /// wraps it in a `ResourceContents::TextResourceContents`.
    fn render_resource(&self, uri: &str) -> Result<String, ResourceError> {
        match uri {
            "adler://registry/sites" => self.render_registry_sites(),
            "adler://registry/tags" => self.render_registry_tags(),
            "adler://registry/disabled" => self.render_registry_disabled(),
            "adler://scans/recent" => self.render_scans_recent(),
            other => other
                .strip_prefix("adler://scans/")
                .map_or(Err(ResourceError::Unknown), |id| self.render_scan_by_id(id)),
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
}

const ADLER_MCP_INSTRUCTIONS: &str = concat!(
    "Adler is an OSINT username-search tool — given a username, it probes a curated ",
    "registry of sites for the presence of a matching account.\n\n",
    "Tools: `list_sites` (browse the registry by tag), `scan_username` (single-",
    "username scan with streaming progress notifications), `scan_batch` (sequential ",
    "multi-username scan), `doctor_check` (health probe for one named site), ",
    "`get_scan_history` (recent persisted scans from the web server's history dir).\n\n",
    "Resources: `adler://registry/sites` (full enabled registry), ",
    "`adler://registry/tags` (tags with site counts), `adler://registry/disabled` ",
    "(disabled entries + reasons — audit surface), `adler://scans/recent` (recent ",
    "history), `adler://scans/{id}` (one scan by id).\n\n",
    "Prompts: `investigate_username` (full OSINT walk for one username), ",
    "`audit_registry_health` (doctor + dedup + disabled audit), `correlate_accounts` ",
    "(scan a list and look for shared profile signal).\n\n",
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

    #[test]
    fn server_info_advertises_resources_capability() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let info = server.get_info();
        assert!(info.capabilities.resources.is_some());
    }

    #[test]
    fn registry_sites_resource_returns_enabled_entries_only() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let payload = server.render_resource("adler://registry/sites").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let total = parsed["total"].as_u64().unwrap();
        let registry_enabled = server
            .registry()
            .sites()
            .iter()
            .filter(|s| !s.disabled)
            .count() as u64;
        assert_eq!(total, registry_enabled);
        // Disabled sites must not appear by name in the payload.
        let payload_lower = payload.to_lowercase();
        assert!(
            !payload_lower.contains("\"facebook\""),
            "Facebook is disabled; must not appear in enabled-sites view",
        );
    }

    #[test]
    fn registry_tags_resource_counts_per_tag() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let payload = server.render_resource("adler://registry/tags").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let tags = parsed["tags"].as_array().unwrap();
        assert!(!tags.is_empty());
        // Pick a known-busy tag and verify the count is plausible.
        let dev_count = tags
            .iter()
            .find(|t| t["tag"] == "dev")
            .map(|t| t["site_count"].as_u64().unwrap())
            .expect("dev tag should exist");
        assert!(dev_count > 5, "dev should tag more than 5 sites");
    }

    #[test]
    fn registry_disabled_resource_includes_disabled_reason() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let payload = server.render_resource("adler://registry/disabled").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let disabled = parsed["disabled"].as_array().unwrap();
        assert!(
            !disabled.is_empty(),
            "registry has known-disabled entries from the v0.14 hygiene round",
        );
        // Every entry has a `disabled_reason` (the v0.14 work guaranteed
        // it for every disabled entry — see CONTRIBUTING.md).
        for entry in disabled {
            assert!(
                entry["disabled_reason"].is_string(),
                "expected string disabled_reason, got {entry}",
            );
        }
    }

    #[test]
    fn unknown_resource_uri_yields_unknown_error() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let err = server.render_resource("adler://nope/never").unwrap_err();
        assert!(matches!(err, ResourceError::Unknown));
    }

    #[test]
    fn scan_by_id_rejects_path_traversal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::default_embedded().unwrap());
        let client = Arc::new(default_client().unwrap());
        let server = AdlerMcp::with_components(registry, client, tmp.path().to_path_buf());
        for evil in ["../etc/passwd", "/etc/passwd", "..\\..\\foo", ""] {
            let err = server
                .render_resource(&format!("adler://scans/{evil}"))
                .unwrap_err();
            assert!(
                matches!(err, ResourceError::Unknown),
                "evil id {evil:?} should yield Unknown, not Io",
            );
        }
    }

    #[test]
    fn server_info_advertises_prompts_capability() {
        let server = AdlerMcp::new().expect("embedded registry must load");
        let info = server.get_info();
        assert!(info.capabilities.prompts.is_some());
    }

    #[test]
    fn prompt_specs_register_three_named_prompts() {
        let names: Vec<&str> = PROMPT_SPECS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "investigate_username",
                "audit_registry_health",
                "correlate_accounts"
            ],
        );
    }

    #[test]
    fn render_prompt_substitutes_placeholders() {
        let spec = PROMPT_SPECS
            .iter()
            .find(|s| s.name == "investigate_username")
            .unwrap();
        let mut args = serde_json::Map::new();
        args.insert("username".into(), serde_json::Value::String("alice".into()));
        args.insert("regions".into(), serde_json::Value::String("ru,ua".into()));
        // categories left unset — should render as empty, not panic.
        let body = render_prompt(spec, &args).unwrap();
        assert!(body.contains("`alice`"));
        assert!(body.contains("regions = `ru,ua`"));
        assert!(body.contains("categories = ``"));
        // No leftover placeholders.
        assert!(!body.contains("{username}"));
        assert!(!body.contains("{regions}"));
        assert!(!body.contains("{categories}"));
    }

    #[test]
    fn render_prompt_rejects_missing_required_arg() {
        let spec = PROMPT_SPECS
            .iter()
            .find(|s| s.name == "investigate_username")
            .unwrap();
        // Empty args — `username` is required.
        let args = serde_json::Map::new();
        let err = render_prompt(spec, &args).unwrap_err();
        assert!(err.message.contains("requires argument"));
        assert!(err.message.contains("username"));
    }

    #[test]
    fn render_prompt_allows_missing_optional_arg() {
        let spec = PROMPT_SPECS
            .iter()
            .find(|s| s.name == "audit_registry_health")
            .unwrap();
        // `focus` is optional — empty args should render fine.
        let body = render_prompt(spec, &serde_json::Map::new()).unwrap();
        assert!(body.contains("Focus area: ``"));
    }

    #[test]
    fn scan_by_id_reads_persisted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let scan_id = "smoke123";
        std::fs::write(
            tmp.path().join(format!("{scan_id}.json")),
            r#"{"id":"smoke123","username":"alice","outcomes":[]}"#,
        )
        .unwrap();
        let registry = Arc::new(Registry::default_embedded().unwrap());
        let client = Arc::new(default_client().unwrap());
        let server = AdlerMcp::with_components(registry, client, tmp.path().to_path_buf());
        let payload = server
            .render_resource(&format!("adler://scans/{scan_id}"))
            .unwrap();
        assert!(payload.contains("alice"));
        assert!(payload.contains("smoke123"));
    }
}
