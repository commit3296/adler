use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use adler_core::{ExecutorOptions, Site, Username};
use async_stream::stream;
use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use futures::Stream;

use super::dto::{
    AccessSummary, DisabledSiteSummary, Health, RefilterRequest, RefilterResponse, RetryRequest,
    RetryResponse, ScanListEntry, ScanSnapshot, SessionName, SiteSummary, SitesResponse,
    StartEvent, StartScanRequest, StartScanResponse,
};
use super::error::ApiError;
use super::filter::filter_catalog;
use crate::persist::{PersistedDisabledMatch, PersistedScan, ScanRequestContext};
use crate::scan::{ScanHandle, ScanId};
use crate::state::AppState;

pub(super) async fn health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub(super) async fn list_sites(State(state): State<AppState>) -> Json<SitesResponse> {
    let sites = state
        .sites
        .iter()
        .map(SiteSummary::from)
        .collect::<Vec<_>>();
    let disabled = state
        .catalog
        .iter()
        .filter(|s| s.disabled)
        .map(DisabledSiteSummary::from)
        .collect::<Vec<_>>();
    Json(SitesResponse { sites, disabled })
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
pub(super) async fn list_access(State(state): State<AppState>) -> Json<AccessSummary> {
    let egress = state.client.egress_summary();
    let sessions = state
        .client
        .session_names()
        .into_iter()
        .map(|name| SessionName { name })
        .collect();
    Json(AccessSummary { egress, sessions })
}

pub(super) async fn list_scans(State(state): State<AppState>) -> Json<Vec<ScanListEntry>> {
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

pub(super) async fn list_scan_timeline(
    State(state): State<AppState>,
    AxumPath(username): AxumPath<String>,
) -> Result<Json<crate::persist::ScanTimeline>, ApiError> {
    let username = Username::new(username)
        .map_err(|e| ApiError::bad_request("invalid_username", e.to_string()))?;
    let scans = load_finished_scans_for_username(&state, username.as_str()).await;
    let mut timeline = crate::persist::build_scan_timeline(&scans);
    if timeline.username.is_empty() {
        username.as_str().clone_into(&mut timeline.username);
    }
    Ok(Json(timeline))
}

pub(super) async fn start_scan(
    State(state): State<AppState>,
    Json(req): Json<StartScanRequest>,
) -> Result<Json<StartScanResponse>, ApiError> {
    let username = Username::new(req.username.clone())
        .map_err(|e| ApiError::bad_request("invalid_username", e.to_string()))?;

    let sites = filter_catalog(&state.sites, &req);
    if sites.is_empty() {
        let disabled = disabled_matches(&state.catalog, &req);
        return Err(ApiError::bad_request(
            "empty_site_filter",
            empty_filter_message(disabled.is_empty()),
        )
        .with_disabled_matches(disabled));
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
            request_context: request_context(&req, &state.catalog, None),
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

/// Cancel an in-flight scan and replace it with a successor driven by
/// a new filter, carrying over outcomes for sites the two filters share.
///
/// Outcomes already on disk for the old scan stay there; nothing about
/// the historic record is rewritten. The new scan is a fresh entry in
/// `state.scans` with its own id. A finished scan can't be refiltered —
/// just call `POST /api/scan` to start a fresh one instead.
pub(super) async fn refilter_scan(
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
    let mut start_shape = StartScanRequest::from(&req);
    let new_sites = filter_catalog(&state.sites, &start_shape);
    if new_sites.is_empty() {
        let disabled = disabled_matches(&state.catalog, &start_shape);
        return Err(ApiError::bad_request(
            "empty_site_filter",
            empty_filter_message(disabled.is_empty()),
        )
        .with_disabled_matches(disabled));
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
    start_shape.username = username_str.clone();
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
            request_context: request_context(&start_shape, &state.catalog, Some(prev_id.clone())),
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

fn disabled_matches(catalog: &[Site], req: &StartScanRequest) -> Vec<DisabledSiteSummary> {
    super::filter::disabled_matches(catalog, req)
        .iter()
        .map(DisabledSiteSummary::from)
        .collect()
}

fn request_context(
    req: &StartScanRequest,
    catalog: &[Site],
    derived_from: Option<ScanId>,
) -> ScanRequestContext {
    ScanRequestContext {
        username: req.username.clone(),
        derived_from,
        only: req.only.clone(),
        exclude: req.exclude.clone(),
        tag: req.tag.clone(),
        exclude_tag: req.exclude_tag.clone(),
        top: req.top,
        nsfw: req.nsfw,
        concurrency: req.concurrency.map(std::num::NonZeroUsize::get),
        deadline_secs: req.deadline_secs,
        egress_names: req.egress_names.clone(),
        disabled_matches: super::filter::disabled_matches(catalog, req)
            .iter()
            .map(PersistedDisabledMatch::from)
            .collect(),
    }
}

fn empty_filter_message(disabled_empty: bool) -> &'static str {
    if disabled_empty {
        "no sites match the requested filter"
    } else {
        "no enabled sites match the requested filter"
    }
}

pub(super) async fn get_scan(
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
                    identity_clusters: ps.identity_clusters,
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

pub(super) async fn diff_scans(
    State(state): State<AppState>,
    AxumPath((from, to)): AxumPath<(String, String)>,
) -> Result<Json<crate::persist::ScanDiff>, ApiError> {
    let from_id = ScanId::from(from);
    let to_id = ScanId::from(to);
    let previous = load_finished_scan(&state, &from_id).await?;
    let current = load_finished_scan(&state, &to_id).await?;
    Ok(Json(crate::persist::diff_scans(&previous, &current)))
}

async fn load_finished_scans_for_username(state: &AppState, username: &str) -> Vec<PersistedScan> {
    let handles: Vec<(ScanId, ScanHandle)> = {
        let scans = state.scans.read().await;
        scans
            .iter()
            .map(|(id, handle)| (id.clone(), handle.clone()))
            .collect()
    };
    let mut by_id: HashMap<ScanId, PersistedScan> = HashMap::with_capacity(handles.len());

    for (id, handle) in handles {
        if handle.username() != username {
            continue;
        }
        if let Some(finished) = handle.finished().await {
            by_id.insert(
                id.clone(),
                PersistedScan::from_finished(
                    id,
                    handle.username().to_owned(),
                    handle.site_count(),
                    handle.created_at_ms(),
                    finished,
                ),
            );
        }
    }

    if let Some(dir) = &state.scans_dir {
        for scan in crate::persist::load_all(dir).await {
            if scan.username == username {
                by_id.entry(scan.scan_id.clone()).or_insert(scan);
            }
        }
    }

    by_id.into_values().collect()
}

async fn load_finished_scan(state: &AppState, scan_id: &ScanId) -> Result<PersistedScan, ApiError> {
    if let Some(scan) = state.get_scan(scan_id).await {
        if let Some(finished) = scan.finished().await {
            return Ok(PersistedScan::from_finished(
                scan_id.clone(),
                scan.username().to_owned(),
                scan.site_count(),
                scan.created_at_ms(),
                finished,
            ));
        }
        return Err(ApiError::bad_request(
            "scan_not_finished",
            "scan is still running",
        ));
    }
    if let Some(dir) = &state.scans_dir
        && let Some(scan) = crate::persist::load(dir, scan_id).await
    {
        return Ok(scan);
    }
    Err(ApiError::not_found(
        "scan_not_found",
        "no scan with that ID",
    ))
}

/// Boxed alias used by [`stream_scan`] to unify two same-Item streams
/// (live broadcast vs. on-disk replay) under a single return type.
type SseStream = std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

pub(super) async fn retry_site(
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
            let existing_context = crate::persist::load(dir, &scan_id)
                .await
                .and_then(|scan| scan.request_context);
            let mut snap = crate::persist::PersistedScan::from_finished(
                scan_id.clone(),
                handle.username().to_owned(),
                handle.site_count(),
                handle.created_at_ms(),
                finished,
            );
            snap.request_context = existing_context;
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
            ps.refresh_derived_fields();
            if let Err(err) = crate::persist::save(dir, &ps).await {
                tracing::warn!(error = %err, scan_id = %scan_id, "failed to patch persisted scan");
            }
        }
    }

    Ok(Json(RetryResponse {
        outcome: new_outcome,
    }))
}

pub(super) async fn stream_scan(
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
        identity_clusters: ps.identity_clusters,
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
