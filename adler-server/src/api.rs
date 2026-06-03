//! Axum router and request handlers.
//!
//! Routes:
//!
//! - `GET  /api/health`           — liveness probe (returns `{ "ok": true }`).
//! - `GET  /api/sites`            — site catalogue available to scans.
//! - `POST /api/scan`             — start a scan; returns a [`ScanId`].
//! - `GET  /api/scan/:id`         — final aggregate (or 404 / 202 in-progress).
//! - `GET  /api/scan/:id/stream`  — Server-Sent Events stream of outcomes.
//!
//! All endpoints emit JSON. Errors carry a stable `{ "error": "<code>",
//! "message": "<human>" }` shape so the `SolidJS` frontend can branch on
//! `error` without parsing free-text.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use adler_core::{CheckOutcome, ExecutorOptions, Site, Username};
use async_stream::stream;
use axum::Json;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::scan::{FinishedScan, ScanHandle, ScanId};
use crate::state::AppState;

/// Build the axum router. Public so test harnesses can drive it
/// directly without going through [`crate::serve`].
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/sites", get(list_sites))
        .route("/api/access", get(list_access))
        .route("/api/scans", get(list_scans))
        .route("/api/scan", post(start_scan))
        .route("/api/scan/{id}", get(get_scan))
        .route("/api/scan/{id}/stream", get(stream_scan))
        .route("/api/scan/{id}/retry", post(retry_site))
        .route("/api/scan/{id}/refilter", post(refilter_scan))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Site summary returned by `GET /api/sites`. Strictly smaller than the
/// internal [`Site`] — we don't leak detection signals, just what a UI
/// needs to render a filter list.
#[derive(Serialize)]
struct SiteSummary {
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

async fn list_sites(State(state): State<AppState>) -> Json<Vec<SiteSummary>> {
    Json(state.sites.iter().map(SiteSummary::from).collect())
}

/// Read-only view of the access engine's runtime config — what's
/// configured via `--proxy-pool` and `--sessions`, *without* leaking
/// any secrets the operator supplied:
///   - egress entries surface only `(country, kind)` — proxy URLs
///     typically embed credentials (`socks5://user:pass@host:1080`),
///     so we never put them in the response;
///   - sessions surface only their *names* — session header values
///     are cookies / auth tokens that have no business reaching a
///     browser over this HTTP API.
///
/// Editing happens out-of-band: the operator updates the pool / session
/// TOML files and restarts the server. The SPA exposes this view as a
/// read-only panel so an operator can confirm what's loaded without
/// shell access to the server.
#[derive(Serialize)]
struct AccessSummary {
    egress: Vec<adler_core::EgressSummary>,
    sessions: Vec<SessionName>,
}

#[derive(Serialize)]
struct SessionName {
    name: String,
}

async fn list_access(State(state): State<AppState>) -> Json<AccessSummary> {
    let egress = state.client.egress_summary();
    let sessions = state
        .client
        .session_names()
        .into_iter()
        .map(|name| SessionName { name })
        .collect();
    Json(AccessSummary { egress, sessions })
}

/// One row in `GET /api/scans`.
#[derive(Serialize)]
struct ScanListEntry {
    scan_id: ScanId,
    username: String,
    site_count: usize,
    /// Unix epoch milliseconds when the scan was started.
    started_at_ms: u64,
    elapsed_ms: u64,
    /// `"running"` or `"finished"`.
    status: &'static str,
    /// Counts present only when `status == "finished"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<crate::scan::Summary>,
}

async fn list_scans(State(state): State<AppState>) -> Json<Vec<ScanListEntry>> {
    // Snapshot the in-memory handles out from under the lock so the
    // per-handle `.finished()` awaits don't serialise on the outer
    // map's read guard.
    let handles: Vec<(ScanId, ScanHandle)> = {
        let scans = state.scans.read().await;
        scans
            .iter()
            .map(|(id, h)| (id.clone(), h.clone()))
            .collect()
    };
    let mut by_id: HashMap<ScanId, ScanListEntry> = HashMap::with_capacity(handles.len());
    for (id, handle) in handles {
        let finished = handle.finished().await;
        by_id.insert(
            id.clone(),
            ScanListEntry {
                scan_id: id,
                username: handle.username().to_owned(),
                site_count: handle.site_count(),
                started_at_ms: handle.created_at_ms(),
                elapsed_ms: u64::try_from(handle.elapsed().as_millis()).unwrap_or(u64::MAX),
                status: if finished.is_some() {
                    "finished"
                } else {
                    "running"
                },
                summary: finished.map(|f| f.summary),
            },
        );
    }
    // Layer in on-disk archive (older scans evicted from memory).
    // In-memory entries always win — they may still be running.
    if let Some(dir) = &state.scans_dir {
        for ps in crate::persist::load_all(dir).await {
            by_id.entry(ps.scan_id.clone()).or_insert(ScanListEntry {
                scan_id: ps.scan_id,
                username: ps.username,
                site_count: ps.site_count,
                started_at_ms: ps.created_at_ms,
                elapsed_ms: ps.elapsed_ms,
                status: "finished",
                summary: Some(ps.summary),
            });
        }
    }
    let mut entries: Vec<ScanListEntry> = by_id.into_values().collect();
    // Newest first — convenient for a history sidebar.
    entries.sort_by_key(|e| std::cmp::Reverse(e.started_at_ms));
    Json(entries)
}

/// Request body for `POST /api/scan`.
///
/// Filter fields mirror the CLI flags one-for-one (`--only`,
/// `--exclude`, `--tag`, `--exclude-tag`, `--top`, `--nsfw`). All are
/// optional; omitting them runs the full catalog the server was
/// launched with.
#[derive(Debug, Deserialize, Default)]
struct StartScanRequest {
    username: String,
    /// Only sites whose name contains one of these substrings
    /// (case-insensitive). Empty = no name include filter.
    #[serde(default)]
    only: Vec<String>,
    /// Skip sites whose name contains any of these substrings.
    #[serde(default)]
    exclude: Vec<String>,
    /// Only sites carrying one of these tags. Empty = no tag filter.
    /// Sites with no tags are excluded when this is non-empty.
    #[serde(default)]
    tag: Vec<String>,
    /// Skip sites carrying any of these tags.
    #[serde(default)]
    exclude_tag: Vec<String>,
    /// Restrict to ranked sites within the top N most-popular, sorted
    /// by rank. Sites without a `popularity` rank are dropped.
    #[serde(default)]
    top: Option<u32>,
    /// Include sites tagged `nsfw`. Default false — matches the CLI.
    #[serde(default)]
    nsfw: bool,
    /// Optional per-scan concurrency override. Falls back to the
    /// executor's default if omitted.
    #[serde(default)]
    concurrency: Option<std::num::NonZeroUsize>,
    /// Optional total scan deadline in seconds.
    #[serde(default)]
    deadline_secs: Option<u64>,
    /// Subset of the configured egress pool to use for this scan,
    /// selected by `name`. Empty (or omitted) uses the full pool.
    /// Unknown names → 400 `unknown_egress`. Sites whose access policy
    /// can't be satisfied by the chosen subset land in
    /// `Uncertain(geo_unavailable)` — the same honest verdict the engine
    /// returns when a constrained policy can't be matched at all.
    #[serde(default)]
    egress_names: Vec<String>,
}

#[derive(Serialize)]
struct StartScanResponse {
    scan_id: ScanId,
    username: String,
    site_count: usize,
}

/// Apply per-scan name/tag/popularity filters to a catalog slice.
///
/// Mirrors [`adler_core::Registry::filter`] semantics but works on a
/// `&[Site]` so it can compose with the catalog already filtered at
/// server startup.
fn filter_catalog(catalog: &[Site], req: &StartScanRequest) -> Vec<Site> {
    let only_lc: Vec<String> = req.only.iter().map(|s| s.to_lowercase()).collect();
    let exclude_lc: Vec<String> = req.exclude.iter().map(|s| s.to_lowercase()).collect();
    let tag_set: std::collections::HashSet<&str> = req.tag.iter().map(String::as_str).collect();
    let exclude_tag_set: std::collections::HashSet<&str> =
        req.exclude_tag.iter().map(String::as_str).collect();

    let mut filtered: Vec<Site> = catalog
        .iter()
        .filter(|s| {
            let name_lc = s.name.to_lowercase();
            if !only_lc.is_empty() && !only_lc.iter().any(|n| name_lc.contains(n)) {
                return false;
            }
            if exclude_lc.iter().any(|n| name_lc.contains(n)) {
                return false;
            }
            if !tag_set.is_empty() {
                if s.tags.is_empty() {
                    return false;
                }
                if !s.tags.iter().any(|t| tag_set.contains(t.as_str())) {
                    return false;
                }
            }
            if s.tags.iter().any(|t| exclude_tag_set.contains(t.as_str())) {
                return false;
            }
            if !req.nsfw && s.tags.iter().any(|t| t == "nsfw") {
                return false;
            }
            true
        })
        .cloned()
        .collect();

    if let Some(n) = req.top {
        filtered.retain(|s| s.popularity.is_some_and(|p| p <= n));
        filtered.sort_by_key(|s| s.popularity.unwrap_or(u32::MAX));
    }
    filtered
}

async fn start_scan(
    State(state): State<AppState>,
    Json(req): Json<StartScanRequest>,
) -> Result<Json<StartScanResponse>, ApiError> {
    let username = Username::new(req.username.clone())
        .map_err(|e| ApiError::bad_request("invalid_username", e.to_string()))?;

    let sites = filter_catalog(&state.sites, &req);
    if sites.is_empty() {
        return Err(ApiError::bad_request(
            "empty_site_filter",
            "no sites match the requested filter",
        ));
    }

    // Validate per-scan egress subset (if any) against the configured
    // pool. Unknown names are rejected at the boundary rather than
    // silently dropping to "no egress matched" — that would make a
    // user-facing typo look like a deeper config problem.
    if !req.egress_names.is_empty() {
        let known: std::collections::HashSet<String> =
            state.client.egress_names().into_iter().collect();
        let bad: Vec<&String> = req
            .egress_names
            .iter()
            .filter(|n| !known.contains(n.as_str()))
            .collect();
        if !bad.is_empty() {
            let names: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
            return Err(ApiError::bad_request(
                "unknown_egress",
                format!("egress not in pool: {}", names.join(", ")),
            ));
        }
    }

    let mut options = ExecutorOptions::default();
    if let Some(c) = req.concurrency {
        options = options.concurrency(c);
    }
    if let Some(d) = req.deadline_secs {
        options = options.deadline(Duration::from_secs(d));
    }

    let id = ScanId::new();
    let site_count = sites.len();
    let handle = ScanHandle::new(req.username.clone(), site_count, site_count.max(64));
    state.insert_scan(id.clone(), handle.clone()).await;

    let persist_ctx = state
        .scans_dir
        .as_ref()
        .map(|dir| crate::scan::PersistContext {
            scan_id: id.clone(),
            dir: dir.clone(),
        });

    // Per-scan client: when egress_names is non-empty, swap the pool
    // for a subset. The new Client shares all other state (throttle,
    // sessions, budgets) with the parent. When egress_names is empty,
    // skip the wrap entirely so the shared default client is re-used.
    let scan_client: Arc<adler_core::Client> = if req.egress_names.is_empty() {
        state.client.clone()
    } else {
        Arc::new(state.client.with_egress_subset(&req.egress_names))
    };

    let task = crate::scan::spawn(
        handle,
        scan_client,
        Arc::from(sites.into_boxed_slice()),
        username,
        options,
        persist_ctx,
    );
    state.register_scan_task(id.clone(), task).await;

    Ok(Json(StartScanResponse {
        scan_id: id,
        username: req.username,
        site_count,
    }))
}

/// Body for `POST /api/scan/:id/refilter`.
///
/// Mirrors [`StartScanRequest`] minus the `username` (carried over from
/// the existing scan). The active scan is cancelled and replaced with a
/// fresh one driven by the new filter; outcomes for sites that appear
/// in both the old and new site lists carry over unchanged, so the
/// operator pays only for newly-in-scope sites.
#[derive(Debug, Deserialize, Default)]
struct RefilterRequest {
    #[serde(default)]
    only: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    exclude_tag: Vec<String>,
    #[serde(default)]
    top: Option<u32>,
    #[serde(default)]
    nsfw: bool,
    #[serde(default)]
    concurrency: Option<std::num::NonZeroUsize>,
    #[serde(default)]
    deadline_secs: Option<u64>,
    #[serde(default)]
    egress_names: Vec<String>,
}

impl From<&RefilterRequest> for StartScanRequest {
    fn from(r: &RefilterRequest) -> Self {
        Self {
            username: String::new(), // filled in by caller; refilter reuses username from existing scan
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
struct RefilterResponse {
    /// Fresh scan id. The SPA switches its SSE stream over to this id.
    scan_id: ScanId,
    /// Predecessor whose outcomes were carried into the new scan.
    derived_from: ScanId,
    /// Number of outcomes pre-populated from the predecessor (the
    /// "overlap"). Zero when the new filter shares no completed sites
    /// with the old.
    carried_outcomes: usize,
    /// Total site count for the new scan (`carried_outcomes` already
    /// recorded + sites still to probe).
    site_count: usize,
}

/// Cancel an in-flight scan and replace it with a successor driven by
/// a new filter, carrying over outcomes for sites the two filters share.
///
/// Outcomes already on disk for the old scan stay there; nothing about
/// the historic record is rewritten. The new scan is a fresh entry in
/// `state.scans` with its own id. A finished scan can't be refiltered —
/// just call `POST /api/scan` to start a fresh one instead.
async fn refilter_scan(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RefilterRequest>,
) -> Result<Json<RefilterResponse>, ApiError> {
    let prev_id = ScanId::from(id);
    let prev_handle = state
        .get_scan(&prev_id)
        .await
        .ok_or_else(|| ApiError::not_found("scan_not_found", "no scan with that ID"))?;

    if prev_handle.is_finished_now() {
        return Err(ApiError::bad_request(
            "scan_finished",
            "scan has already finished; start a new one with POST /api/scan",
        ));
    }

    // Pre-validate egress subset against the live pool. Same boundary
    // check as `start_scan`; rejecting a typo before cancelling the
    // running scan avoids "operator clicks Apply, scan dies, then sees
    // the error" surprise.
    if !req.egress_names.is_empty() {
        let known: std::collections::HashSet<String> =
            state.client.egress_names().into_iter().collect();
        let bad: Vec<&String> = req
            .egress_names
            .iter()
            .filter(|n| !known.contains(n.as_str()))
            .collect();
        if !bad.is_empty() {
            let names: Vec<&str> = bad.iter().map(|s| s.as_str()).collect();
            return Err(ApiError::bad_request(
                "unknown_egress",
                format!("egress not in pool: {}", names.join(", ")),
            ));
        }
    }

    // Resolve new filter against the catalog.
    let start_shape = StartScanRequest::from(&req);
    let new_sites = filter_catalog(&state.sites, &start_shape);
    if new_sites.is_empty() {
        return Err(ApiError::bad_request(
            "empty_site_filter",
            "no sites match the requested filter",
        ));
    }

    // Snapshot the predecessor's outcomes; partition by whether the
    // site is still in the new filter. Sites in both → carried over;
    // sites in the new filter but not yet probed → spawn task probes
    // them; sites only in the old filter → dropped (the operator
    // narrowed scope deliberately).
    let prev_outcomes = prev_handle.outcomes_snapshot().await;
    let new_site_names: std::collections::HashSet<String> =
        new_sites.iter().map(|s| s.name.clone()).collect();
    let carried: Vec<adler_core::CheckOutcome> = prev_outcomes
        .into_iter()
        .filter(|o| new_site_names.contains(&o.site))
        .collect();
    let carried_names: std::collections::HashSet<String> =
        carried.iter().map(|o| o.site.clone()).collect();
    let sites_to_probe: Vec<Site> = new_sites
        .iter()
        .filter(|s| !carried_names.contains(&s.name))
        .cloned()
        .collect();

    // Abort the predecessor *after* the snapshot so a probe that
    // finishes between snapshot and abort can't sneak into the new
    // scan as a duplicate.
    state.abort_scan(&prev_id).await;

    // Apply per-scan executor knobs.
    let mut options = ExecutorOptions::default();
    if let Some(c) = req.concurrency {
        options = options.concurrency(c);
    }
    if let Some(d) = req.deadline_secs {
        options = options.deadline(Duration::from_secs(d));
    }

    let username_str = prev_handle.username().to_owned();
    let username = Username::new(username_str.clone())
        .map_err(|e| ApiError::bad_request("invalid_username", e.to_string()))?;

    let id = ScanId::new();
    let site_count = new_sites.len();
    let handle = ScanHandle::new(username_str.clone(), site_count, site_count.max(64));
    state.insert_scan(id.clone(), handle.clone()).await;

    // Pre-populate the new handle with the carried-over outcomes so a
    // subscriber that connects after the refilter sees them
    // immediately via the same `index N appended` events the
    // executor produces.
    handle.extend_outcomes(carried.clone()).await;

    let persist_ctx = state
        .scans_dir
        .as_ref()
        .map(|dir| crate::scan::PersistContext {
            scan_id: id.clone(),
            dir: dir.clone(),
        });

    let scan_client: Arc<adler_core::Client> = if req.egress_names.is_empty() {
        state.client.clone()
    } else {
        Arc::new(state.client.with_egress_subset(&req.egress_names))
    };

    let task = crate::scan::spawn(
        handle,
        scan_client,
        Arc::from(sites_to_probe.into_boxed_slice()),
        username,
        options,
        persist_ctx,
    );
    state.register_scan_task(id.clone(), task).await;

    Ok(Json(RefilterResponse {
        scan_id: id,
        derived_from: prev_id,
        carried_outcomes: carried.len(),
        site_count,
    }))
}

/// Snapshot returned by `GET /api/scan/:id`.
///
/// Both variants carry `username` and `site_count` so the UI can
/// render progress and breadcrumbs without cross-referencing the
/// history endpoint.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ScanSnapshot {
    /// Scan is still running. Outcomes recorded so far are included so
    /// a poller can render progress without holding an SSE stream open.
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

async fn get_scan(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ScanSnapshot>, ApiError> {
    let scan_id = ScanId::from(id);
    if let Some(scan) = state.get_scan(&scan_id).await {
        return Ok(match scan.finished().await {
            Some(finished) => Json(ScanSnapshot::Finished {
                username: scan.username().to_owned(),
                site_count: scan.site_count(),
                finished,
            }),
            None => Json(ScanSnapshot::Running {
                username: scan.username().to_owned(),
                site_count: scan.site_count(),
                elapsed_ms: u64::try_from(scan.elapsed().as_millis()).unwrap_or(u64::MAX),
                partial: scan.outcomes_snapshot().await,
            }),
        });
    }
    // Fall back to on-disk archive.
    if let Some(dir) = &state.scans_dir {
        if let Some(ps) = crate::persist::load(dir, &scan_id).await {
            return Ok(Json(ScanSnapshot::Finished {
                username: ps.username,
                site_count: ps.site_count,
                finished: crate::scan::FinishedScan {
                    summary: ps.summary,
                    outcomes: ps.outcomes,
                    elapsed_ms: ps.elapsed_ms,
                },
            }));
        }
    }
    Err(ApiError::not_found(
        "scan_not_found",
        "no scan with that ID",
    ))
}

/// Boxed alias used by [`stream_scan`] to unify two same-Item streams
/// (live broadcast vs. on-disk replay) under a single return type.
type SseStream = std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// `POST /api/scan/:id/retry` — re-probe a single site from a
/// finished scan and replace its outcome.
#[derive(Debug, Deserialize)]
struct RetryRequest {
    /// Name of the site to re-probe (must match `Site::name`).
    site: String,
}

#[derive(Serialize)]
struct RetryResponse {
    outcome: CheckOutcome,
}

async fn retry_site(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RetryRequest>,
) -> Result<Json<RetryResponse>, ApiError> {
    let scan_id = ScanId::from(id);

    // Locate the username for this scan — in-memory first, then disk.
    let username_raw: String = if let Some(handle) = state.get_scan(&scan_id).await {
        handle.username().to_owned()
    } else if let Some(dir) = &state.scans_dir {
        if let Some(ps) = crate::persist::load(dir, &scan_id).await {
            ps.username
        } else {
            return Err(ApiError::not_found(
                "scan_not_found",
                "no scan with that ID",
            ));
        }
    } else {
        return Err(ApiError::not_found(
            "scan_not_found",
            "no scan with that ID",
        ));
    };

    let site = state
        .sites
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(&req.site))
        .cloned()
        .ok_or_else(|| {
            ApiError::bad_request("site_not_in_catalog", "site not in current catalog")
        })?;

    let username = Username::new(username_raw.clone())
        .map_err(|e| ApiError::bad_request("invalid_username", e.to_string()))?;

    let new_outcome = state.client.check(&site, &username).await;

    // Update in-memory scan handle (if loaded) and re-persist.
    if let Some(handle) = state.get_scan(&scan_id).await {
        handle.replace_outcome(new_outcome.clone()).await;
        if let (Some(finished), Some(dir)) = (handle.finished().await, &state.scans_dir) {
            let snap = crate::persist::PersistedScan::from_finished(
                scan_id.clone(),
                handle.username().to_owned(),
                handle.site_count(),
                handle.created_at_ms(),
                finished,
            );
            if let Err(err) = crate::persist::save(dir, &snap).await {
                tracing::warn!(error = %err, scan_id = %scan_id, "failed to re-persist scan");
            }
        }
    } else if let Some(dir) = &state.scans_dir {
        // In-memory eviction already happened; patch the on-disk file.
        if let Some(mut ps) = crate::persist::load(dir, &scan_id).await {
            if let Some(slot) = ps.outcomes.iter_mut().find(|o| o.site == new_outcome.site) {
                *slot = new_outcome.clone();
            } else {
                ps.outcomes.push(new_outcome.clone());
            }
            ps.summary = crate::scan::Summary::from_outcomes(&ps.outcomes);
            if let Err(err) = crate::persist::save(dir, &ps).await {
                tracing::warn!(error = %err, scan_id = %scan_id, "failed to patch persisted scan");
            }
        }
    }

    Ok(Json(RetryResponse {
        outcome: new_outcome,
    }))
}

async fn stream_scan(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Sse<KeepAliveStream<SseStream>>, ApiError> {
    let scan_id = ScanId::from(id);
    if let Some(scan) = state.get_scan(&scan_id).await {
        let stream: SseStream = Box::pin(scan_event_stream(scan));
        return Ok(Sse::new(stream).keep_alive(KeepAlive::new()));
    }
    if let Some(dir) = &state.scans_dir {
        if let Some(ps) = crate::persist::load(dir, &scan_id).await {
            let stream: SseStream = Box::pin(persisted_event_stream(ps));
            return Ok(Sse::new(stream).keep_alive(KeepAlive::new()));
        }
    }
    Err(ApiError::not_found(
        "scan_not_found",
        "no scan with that ID",
    ))
}

/// Build an SSE stream that replays a [`PersistedScan`] all-at-once
/// then terminates. Mirrors [`scan_event_stream`]'s event types so the
/// client side handles both cases identically.
fn persisted_event_stream(
    ps: crate::persist::PersistedScan,
) -> impl Stream<Item = Result<Event, Infallible>> + Send {
    let username = ps.username.clone();
    let outcomes = ps.outcomes.clone();
    let finished = crate::scan::FinishedScan {
        summary: ps.summary,
        outcomes: ps.outcomes,
        elapsed_ms: ps.elapsed_ms,
    };
    stream! {
        yield Ok(Event::default()
            .event("start")
            .json_data(StartEvent { username })
            .unwrap_or_default());
        for o in &outcomes {
            yield Ok(outcome_event(o));
        }
        yield Ok(Event::default()
            .event("done")
            .json_data(&finished)
            .unwrap_or_default());
    }
}

/// Build the per-subscription SSE event stream.
///
/// Order: a `start` event, then every outcome already in history, then
/// each newly-broadcast outcome live, then a final `done` event with
/// the summary aggregate. The stream terminates after `done` so the
/// client's `EventSource` closes cleanly.
fn scan_event_stream(scan: ScanHandle) -> impl Stream<Item = Result<Event, Infallible>> {
    stream! {
        yield Ok(Event::default()
            .event("start")
            .json_data(StartEvent { username: scan.username().to_owned() })
            .unwrap_or_default());

        // Replay every outcome already recorded so a slightly late
        // subscriber still gets a full picture.
        let history = scan.outcomes_snapshot().await;
        let mut last_index = history.len();
        for outcome in &history {
            yield Ok(outcome_event(outcome));
        }

        // If the scan finished before we subscribed, the broadcast
        // channel is closed — skip the live loop and go straight to
        // emitting `done`.
        if scan.finished().await.is_none() {
            let mut rx = scan.subscribe();
            loop {
                tokio::select! {
                    biased;
                    () = scan.wait_done() => break,
                    recv = rx.recv() => match recv {
                        Ok(idx) => {
                            // Catch-up: deliver every outcome we haven't
                            // emitted yet (handles a Lagged broadcast as
                            // well — we re-snapshot the vec).
                            let snap = scan.outcomes_snapshot().await;
                            for outcome in &snap[last_index..=idx.min(snap.len().saturating_sub(1))] {
                                yield Ok(outcome_event(outcome));
                            }
                            last_index = idx + 1;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Re-snapshot and emit the gap.
                            let snap = scan.outcomes_snapshot().await;
                            for outcome in &snap[last_index..] {
                                yield Ok(outcome_event(outcome));
                            }
                            last_index = snap.len();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        // Emit any outcomes the live loop missed (shouldn't happen in
        // practice, but cheap insurance).
        let final_snap = scan.outcomes_snapshot().await;
        for outcome in &final_snap[last_index..] {
            yield Ok(outcome_event(outcome));
        }

        if let Some(finished) = scan.finished().await {
            yield Ok(Event::default()
                .event("done")
                .json_data(&finished)
                .unwrap_or_default());
        }
    }
}

fn outcome_event(outcome: &adler_core::CheckOutcome) -> Event {
    Event::default()
        .event("outcome")
        .json_data(outcome)
        .unwrap_or_default()
}

#[derive(Serialize)]
struct StartEvent {
    username: String,
}

/// JSON error envelope returned by failing handlers.
#[derive(Debug, Serialize)]
struct ApiError {
    #[serde(skip)]
    status: StatusCode,
    error: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: code,
            message: msg.into(),
        }
    }

    fn not_found(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: code,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::{Client, KnownPresent, Signal, UrlTemplate};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, header};
    use tower::ServiceExt;
    use wiremock::matchers::{any, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn site(name: &str, base: &str, segment: &str) -> Site {
        Site {
            name: name.into(),
            url: UrlTemplate::new(format!("{base}/{segment}/{{username}}")).unwrap(),
            signals: vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::StatusNotFound { codes: vec![404] },
            ],
            known_present: None::<KnownPresent>,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: adler_core::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            source: None,
            popularity: None,
            access: adler_core::AccessPolicy::default(),
        }
    }

    async fn test_app() -> (Router, MockServer) {
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        Mock::given(any())
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        (router(state), mock)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn list_sites_returns_summary() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["name"], "A");
        assert!(v[0]["url"].as_str().unwrap().contains("{username}"));
    }

    #[tokio::test]
    async fn list_access_empty_when_nothing_configured() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/access")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["egress"].as_array().unwrap().len(), 0);
        assert_eq!(v["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_access_surfaces_pool_and_sessions_without_secrets() {
        use adler_core::{EgressKind, EgressSpec, Session, SessionStore};
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a")];

        let pool = vec![
            EgressSpec {
                url: "http://corp-proxy.invalid:8080".into(),
                country: adler_core::CountryCode::new("de"),
                kind: EgressKind::Datacenter,
                name: Some("corp-de".into()),
            },
            EgressSpec {
                url: "socks5://user:hunter2@residential.invalid:1080".into(),
                country: adler_core::CountryCode::new("us"),
                kind: EgressKind::Residential,
                name: Some("us-residential".into()),
            },
        ];
        let mut sessions = SessionStore::new();
        let mut hdr = std::collections::BTreeMap::new();
        hdr.insert("Cookie".into(), "sessionid=secret-token-do-not-leak".into());
        sessions.insert("instagram", Session::from_headers(hdr));

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .sessions(sessions)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/access")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let raw = String::from_utf8(body.to_vec()).unwrap();
        // Negative assertions first — a regression here is the whole point
        // of the API design (no URLs, no header values reach the browser).
        assert!(
            !raw.contains("corp-proxy.invalid"),
            "proxy URLs must never leak into /api/access — got body: {raw}"
        );
        assert!(
            !raw.contains("residential.invalid"),
            "proxy URLs must never leak: {raw}"
        );
        assert!(
            !raw.contains("hunter2"),
            "proxy credentials must never leak: {raw}"
        );
        assert!(
            !raw.contains("secret-token-do-not-leak"),
            "session values must never leak: {raw}"
        );

        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let egress = v["egress"].as_array().unwrap();
        assert_eq!(egress.len(), 2);
        assert_eq!(egress[0]["name"], "corp-de");
        assert_eq!(egress[0]["country"], "de");
        assert_eq!(egress[0]["kind"], "datacenter");
        assert_eq!(egress[1]["name"], "us-residential");
        assert_eq!(egress[1]["country"], "us");
        assert_eq!(egress[1]["kind"], "residential");

        let sessions = v["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["name"], "instagram");
    }

    #[tokio::test]
    async fn start_scan_rejects_unknown_egress_name() {
        use adler_core::{EgressKind, EgressSpec};
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a")];
        let pool = vec![EgressSpec {
            url: "http://only-one.invalid:8080".into(),
            country: adler_core::CountryCode::new("de"),
            kind: EgressKind::Datacenter,
            name: Some("only-one".into()),
        }];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .build()
            .unwrap();
        let app = router(AppState::new(sites, client, 16));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","egress_names":["does-not-exist"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "unknown_egress");
        assert!(
            v["message"].as_str().unwrap().contains("does-not-exist"),
            "message should name the bad egress, got {}",
            v["message"]
        );
    }

    #[tokio::test]
    async fn start_scan_accepts_known_egress_name() {
        use adler_core::{EgressKind, EgressSpec};
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        let sites = vec![site("A", &mock.uri(), "a")];
        let pool = vec![EgressSpec {
            url: "http://corp-de.invalid:8080".into(),
            country: adler_core::CountryCode::new("de"),
            kind: EgressKind::Datacenter,
            name: Some("corp-de".into()),
        }];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .egress_pool(pool)
            .build()
            .unwrap();
        let app = router(AppState::new(sites, client, 16));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","egress_names":["corp-de"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Known egress name; the scan is accepted (the actual probe
        // outcome happens off-task and is checked elsewhere — this
        // assertion just covers the validation boundary).
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["scan_id"].is_string());
    }

    #[tokio::test]
    async fn start_scan_rejects_invalid_username() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":" bad "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "invalid_username");
    }

    #[tokio::test]
    async fn start_then_poll_finishes_with_expected_counts() {
        let (app, _mock) = test_app().await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();
        assert_eq!(v["site_count"], 2);

        // Poll until finished, max ~5s.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{scan_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            let body = to_bytes(r.into_body(), 16384).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if v["status"] == "finished" {
                assert_eq!(v["summary"]["found"], 1);
                assert_eq!(v["summary"]["not_found"], 1);
                assert_eq!(v["outcomes"].as_array().unwrap().len(), 2);
                return;
            }
        }
        panic!("scan did not finish within 5s");
    }

    #[tokio::test]
    async fn get_scan_404s_on_unknown_id() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scan/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "scan_not_found");
    }

    fn tagged_site(name: &str, base: &str, segment: &str, tags: &[&str]) -> Site {
        let mut s = site(name, base, segment);
        s.tags = tags.iter().map(|t| (*t).to_owned()).collect();
        s
    }

    #[test]
    fn filter_catalog_honours_only_exclude() {
        let sites = vec![
            site("GitHub", "http://x", "gh"),
            site("GitLab", "http://x", "gl"),
            site("Bitbucket", "http://x", "bb"),
        ];
        let only = StartScanRequest {
            only: vec!["git".into()],
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &only)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["GitHub", "GitLab"]);

        let exclude = StartScanRequest {
            exclude: vec!["lab".into()],
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &exclude)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["GitHub", "Bitbucket"]);
    }

    #[test]
    fn filter_catalog_honours_tags_and_nsfw() {
        let sites = vec![
            tagged_site("A", "http://x", "a", &["social"]),
            tagged_site("B", "http://x", "b", &["dev"]),
            tagged_site("C", "http://x", "c", &["social", "nsfw"]),
            tagged_site("D", "http://x", "d", &[]),
        ];
        let only_social = StartScanRequest {
            tag: vec!["social".into()],
            ..Default::default()
        };
        // C has `nsfw` so default `nsfw=false` excludes it.
        let names: Vec<_> = filter_catalog(&sites, &only_social)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A"]);

        let with_nsfw = StartScanRequest {
            tag: vec!["social".into()],
            nsfw: true,
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &with_nsfw)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A", "C"]);

        let exclude_dev = StartScanRequest {
            exclude_tag: vec!["dev".into()],
            ..Default::default()
        };
        // dev excluded → A, C (still no nsfw), D remain.
        let names: Vec<_> = filter_catalog(&sites, &exclude_dev)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["A", "D"]);
    }

    #[test]
    fn filter_catalog_top_sorts_by_popularity() {
        let mut a = site("A", "http://x", "a");
        a.popularity = Some(3);
        let mut b = site("B", "http://x", "b");
        b.popularity = Some(1);
        let mut c = site("C", "http://x", "c");
        c.popularity = Some(2);
        let d = site("D", "http://x", "d"); // no rank
        let sites = vec![a, b, c, d];
        let req = StartScanRequest {
            top: Some(2),
            ..Default::default()
        };
        let names: Vec<_> = filter_catalog(&sites, &req)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["B", "C"]);
    }

    #[tokio::test]
    async fn start_scan_with_tag_filter_only_runs_matching_sites() {
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;
        Mock::given(any())
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let sites = vec![
            tagged_site("A", &mock.uri(), "a", &["social"]),
            tagged_site("B", &mock.uri(), "b", &["dev"]),
        ];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice","tag":["social"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["site_count"], 1);
    }

    #[tokio::test]
    async fn empty_filter_returns_bad_request() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"alice","only":["definitely-not-a-site"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "empty_site_filter");
    }

    #[tokio::test]
    async fn retry_flips_outcome_when_response_changes() {
        // First call returns 404 (one-shot via `up_to_n_times`); second
        // and later calls hit the longer-lived 200 mock that follows it.
        let mock = MockServer::start().await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&mock)
            .await;
        Mock::given(any())
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let sites = vec![site("A", &mock.uri(), "a")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let app = router(state);

        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();

        // Wait for completion with NotFound for site A.
        let mut finished = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{scan_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = to_bytes(r.into_body(), 8192).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if v["status"] == "finished" {
                assert_eq!(v["summary"]["not_found"], 1);
                finished = true;
                break;
            }
        }
        assert!(finished, "scan did not finish");

        // Retry — should now hit the 200 mock and flip to found.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{scan_id}/retry"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"A"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["outcome"]["site"], "A");
        assert_eq!(v["outcome"]["kind"], "found");

        // Persistent scan state reflects the new outcome.
        let r = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/scan/{scan_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 16384).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["summary"]["found"], 1);
        assert_eq!(v["summary"]["not_found"], 0);
    }

    #[tokio::test]
    async fn retry_404s_unknown_site_or_scan() {
        let (app, _mock) = test_app().await;
        // Unknown scan.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan/nope/retry")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"A"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);

        // Start a scan, then ask to retry a site that isn't in the catalog.
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"alice"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(r.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let scan_id = v["scan_id"].as_str().unwrap().to_owned();
        let r = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{scan_id}/retry"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"site":"NoSuch"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(r.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "site_not_in_catalog");
    }

    #[tokio::test]
    async fn list_scans_returns_newest_first() {
        let (app, _mock) = test_app().await;
        // Kick off two scans.
        for _ in 0..2 {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/scan")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(r#"{"username":"alice"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            // Yield so SystemTime moves forward between insertions.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/scans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(
            arr[0]["started_at_ms"].as_u64() >= arr[1]["started_at_ms"].as_u64(),
            "scans must be newest-first",
        );
    }

    #[tokio::test]
    async fn refilter_404s_unknown_scan() {
        let (app, _mock) = test_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan/does-not-exist/refilter")
                    .header("content-type", "application/json")
                    .body(Body::from(r"{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn refilter_rejects_finished_scan() {
        // Start a scan, wait for it to finish naturally (both sites
        // resolve from the mock instantly), then refilter — must
        // return 400 scan_finished.
        let (app, _mock) = test_app().await;
        let id = start_and_wait(&app, "alice").await;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{id}/refilter"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"only":["A"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "scan_finished");
    }

    #[tokio::test]
    async fn refilter_rejects_empty_filter() {
        let (app, _mock) = test_app().await;
        let id = start_and_wait(&app, "alice").await;
        // Even with a finished predecessor, the empty-filter check
        // would fire before scan_finished — but here we get
        // scan_finished first. Use the live router with a custom
        // handle to actually exercise empty_site_filter. We instead
        // construct a fake running scan by inserting a never-ending
        // handle directly into AppState.
        let _ = id;
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);
        let prev_id = ScanId::new();
        let handle = ScanHandle::new("bob", 2, 16);
        state.insert_scan(prev_id.clone(), handle).await;
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{prev_id}/refilter"))
                    .header("content-type", "application/json")
                    // `only=Z` matches no site in the catalog (`A`, `B`).
                    .body(Body::from(r#"{"only":["Z"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "empty_site_filter");
    }

    #[tokio::test]
    async fn refilter_carries_overlap_and_returns_fresh_id() {
        // Synthesize a "running" predecessor whose handle already has
        // an outcome recorded for site A. Refiltering to `only=A`
        // means the new scan should carry A over (1 outcome) and have
        // 0 sites left to probe.
        let mock = MockServer::start().await;
        let sites = vec![site("A", &mock.uri(), "a"), site("B", &mock.uri(), "b")];
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap();
        let state = AppState::new(sites, client, 16);

        let prev_id = ScanId::new();
        let handle = ScanHandle::new("bob", 2, 16);
        // Inject a Found outcome for site A so the refilter has
        // something concrete to carry over.
        handle
            .extend_outcomes(vec![adler_core::CheckOutcome {
                site: "A".to_owned(),
                url: "https://a.test/bob".to_owned(),
                kind: adler_core::MatchKind::Found,
                reason: None,
                elapsed_ms: 12,
                evidence: Vec::new(),
                enrichment: std::collections::BTreeMap::new(),
                transport: None,
                escalations: 0,
            }])
            .await;
        state.insert_scan(prev_id.clone(), handle).await;
        let app = router(state.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/scan/{prev_id}/refilter"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"only":["A"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["carried_outcomes"], 1);
        assert_eq!(v["site_count"], 1);
        assert_eq!(v["derived_from"].as_str().unwrap(), prev_id.as_str());
        let new_id = v["scan_id"].as_str().unwrap();
        assert_ne!(new_id, prev_id.as_str(), "new scan must have a fresh id");

        // The successor handle should hold the carried-over outcome
        // already, even before the spawn task gets a chance to run.
        let new_handle = state
            .get_scan(&ScanId::from(new_id.to_owned()))
            .await
            .expect("new handle registered");
        let snap = new_handle.outcomes_snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].site, "A");
    }

    /// Test helper: start a scan and wait for it to finish. Returns
    /// the scan id as a string.
    async fn start_and_wait(app: &Router, username: &str) -> String {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/scan")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username": username}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["scan_id"].as_str().unwrap().to_owned();
        // Poll status until finished (test mocks resolve instantly).
        for _ in 0..50 {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/scan/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let b = to_bytes(r.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
            if v["status"] == "finished" {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("scan {id} did not finish within ~1s");
    }
}
