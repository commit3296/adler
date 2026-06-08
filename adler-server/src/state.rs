//! Shared application state: registry, sites cache, HTTP client, scans.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use adler_core::{Client, Registry, Site, SiteFilter};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::scan::{ScanHandle, ScanId};

/// State shared across all axum handlers.
///
/// Cheap to clone — every field is an [`Arc`] or a small primitive.
/// axum requires `State<T>` to be `Clone`, hence this design.
#[derive(Clone)]
pub struct AppState {
    /// Pre-filtered site list (registry + workspace flags applied at
    /// startup). Held as an `Arc<[Site]>` to avoid re-cloning the
    /// 2.5k-entry vector on every scan dispatch.
    pub sites: Arc<[Site]>,
    /// Startup-filtered catalogue including disabled/parked entries.
    /// Scan handlers use [`Self::sites`]; catalogue and error-diagnostic
    /// surfaces use this to explain why an otherwise matching site is not
    /// currently scannable.
    pub catalog: Arc<[Site]>,
    /// Shared HTTP client (connection pool, throttle, etc.).
    pub client: Arc<Client>,
    /// In-flight + recently-finished scans, keyed by ID.
    pub scans: Arc<RwLock<HashMap<ScanId, ScanHandle>>>,
    /// Running-scan task handles, keyed by [`ScanId`]. Lets the
    /// refilter endpoint cancel an in-flight scan via
    /// [`JoinHandle::abort`] before spawning a successor with the new
    /// filter. Entries are removed when their scan finishes naturally
    /// (the task's last act before returning) or when the eviction
    /// policy reaps them alongside the [`ScanHandle`].
    pub scan_tasks: Arc<RwLock<HashMap<ScanId, JoinHandle<()>>>>,
    /// Maximum number of scans retained in memory. Beyond this, the
    /// oldest finished scan is evicted on the next insertion (a tiny
    /// LRU — we never need more than ~dozens of recent scans in a
    /// human-driven web session).
    pub scan_capacity: usize,
    /// Directory where finished scans are persisted as JSON. `None`
    /// disables persistence (used by tests and ephemeral runs).
    pub scans_dir: Option<Arc<PathBuf>>,
}

impl AppState {
    /// Build initial state from a registry + a pre-built HTTP client.
    ///
    /// The full registry is filtered with the supplied predicate; the
    /// result is materialised into an `Arc<[Site]>` once so handler
    /// dispatch is a pointer copy. Persistence is off by default —
    /// chain [`Self::with_scans_dir`] to enable.
    #[must_use]
    pub fn new(sites: Vec<Site>, client: Client, scan_capacity: usize) -> Self {
        Self::with_catalog(sites.clone(), sites, client, scan_capacity)
    }

    /// Build initial state with separate scan and catalogue views.
    /// `sites` must contain enabled entries only; `catalog` may include
    /// disabled entries for diagnostics.
    #[must_use]
    pub fn with_catalog(
        sites: Vec<Site>,
        catalog: Vec<Site>,
        client: Client,
        scan_capacity: usize,
    ) -> Self {
        Self {
            sites: Arc::from(sites.into_boxed_slice()),
            catalog: Arc::from(catalog.into_boxed_slice()),
            client: Arc::new(client),
            scans: Arc::new(RwLock::new(HashMap::new())),
            scan_tasks: Arc::new(RwLock::new(HashMap::new())),
            scan_capacity: scan_capacity.max(1),
            scans_dir: None,
        }
    }

    /// Convenience: build state from a [`Registry`] using the
    /// "no filter, NSFW excluded" default. The web UI exposes
    /// per-scan filters anyway, so the initial site list is the full
    /// non-NSFW set.
    #[must_use]
    pub fn from_registry(registry: &Registry, client: Client, scan_capacity: usize) -> Self {
        let filter = SiteFilter::default();
        let sites = registry.filter_with(&filter);
        let catalog = registry.matches_with(&filter);
        Self::with_catalog(sites, catalog, client, scan_capacity)
    }

    /// Enable on-disk persistence of finished scans under `dir`. Files
    /// are written as `<scan_id>.json` after each scan completes;
    /// startup reads them back so history survives server restarts.
    #[must_use]
    pub fn with_scans_dir(mut self, dir: PathBuf) -> Self {
        self.scans_dir = Some(Arc::new(dir));
        self
    }

    /// Insert a fresh scan handle, evicting the oldest finished entry
    /// (or the oldest entry overall, if none has finished) when we are
    /// at capacity.
    pub async fn insert_scan(&self, id: ScanId, handle: ScanHandle) {
        let mut evicted: Option<ScanId> = None;
        let mut scans = self.scans.write().await;
        if scans.len() >= self.scan_capacity {
            let mut finished_candidate: Option<(ScanId, std::time::Duration)> = None;
            let mut any_candidate: Option<(ScanId, std::time::Duration)> = None;
            for (k, v) in scans.iter() {
                let age = v.elapsed();
                if v.is_finished_now()
                    && finished_candidate
                        .as_ref()
                        .is_none_or(|(_, prev)| age > *prev)
                {
                    finished_candidate = Some((k.clone(), age));
                }
                if any_candidate.as_ref().is_none_or(|(_, prev)| age > *prev) {
                    any_candidate = Some((k.clone(), age));
                }
            }
            if let Some((victim, _)) = finished_candidate.or(any_candidate) {
                scans.remove(&victim);
                evicted = Some(victim);
            }
        }
        scans.insert(id, handle);
        drop(scans);
        if let Some(v) = evicted {
            self.scan_tasks.write().await.remove(&v);
        }
    }

    /// Register an in-flight scan task. The handle is stored so the
    /// refilter endpoint can abort it before starting a successor.
    pub async fn register_scan_task(&self, id: ScanId, task: JoinHandle<()>) {
        self.scan_tasks.write().await.insert(id, task);
    }

    /// Remove an in-flight scan task entry. Used at the end of
    /// `crate::scan::run` so the map doesn't accumulate completed tasks.
    pub async fn forget_scan_task(&self, id: &ScanId) {
        self.scan_tasks.write().await.remove(id);
    }

    /// Abort the running task for `id` (if any). Returns true when an
    /// abort signal was actually sent; false when no live task was
    /// recorded (already finished, or never started). Doesn't wait for
    /// the task to observe the abort — `JoinHandle::abort` is
    /// non-blocking and the caller continues immediately.
    pub async fn abort_scan(&self, id: &ScanId) -> bool {
        let task = self.scan_tasks.write().await.remove(id);
        task.is_some_and(|t| {
            t.abort();
            true
        })
    }

    /// Look up a scan by ID, cloning the handle (cheap — `Arc` inside).
    pub async fn get_scan(&self, id: &ScanId) -> Option<ScanHandle> {
        self.scans.read().await.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{FinishedScan, Summary};

    fn client() -> Client {
        Client::builder().build().expect("default client")
    }

    #[tokio::test]
    async fn evicts_oldest_finished_when_over_capacity() {
        let state = AppState::new(Vec::new(), client(), 2);

        let id_a = ScanId::from("aaaaaaaaaaaa".to_owned());
        let handle_a = ScanHandle::new("a", 0, 4);
        handle_a
            .publish(FinishedScan {
                summary: Summary::default(),
                outcomes: Vec::new(),
                elapsed_ms: 0,
            })
            .await;
        state.insert_scan(id_a.clone(), handle_a).await;

        let id_b = ScanId::from("bbbbbbbbbbbb".to_owned());
        state
            .insert_scan(id_b.clone(), ScanHandle::new("b", 0, 4))
            .await;

        // Capacity is 2; both fit.
        assert!(state.get_scan(&id_a).await.is_some());
        assert!(state.get_scan(&id_b).await.is_some());

        // Inserting a third evicts the finished one (a) over the
        // running one (b).
        let id_c = ScanId::from("cccccccccccc".to_owned());
        state
            .insert_scan(id_c.clone(), ScanHandle::new("c", 0, 4))
            .await;

        assert!(
            state.get_scan(&id_a).await.is_none(),
            "finished scan should be evicted first"
        );
        assert!(state.get_scan(&id_b).await.is_some());
        assert!(state.get_scan(&id_c).await.is_some());
    }
}
