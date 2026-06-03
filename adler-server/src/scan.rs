//! Per-scan state: the live broadcast channel + the final aggregate.
//!
//! A scan is started in the background via [`spawn`]. Outcomes flow
//! into [`ScanHandle::outcomes`] in append-only order; each push fans
//! out an index notification on [`ScanHandle::tx`] so SSE subscribers
//! can stream them as they arrive. When the executor finishes, the
//! aggregate is published in [`ScanHandle::finished`] and waiters
//! parked on [`ScanHandle::done`] are released.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use adler_core::{CheckOutcome, Client, ExecutorOptions, MatchKind, Site, Username, executor};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock, broadcast, mpsc};

use crate::persist::{self, PersistedScan};

/// Identifier for a running or finished scan.
///
/// Short alphanumeric token (12 chars, ~71 bits of entropy) suitable
/// for URLs. Not a cryptographic identifier — it is a *capability* in
/// the sense that knowing the ID lets you read scan results, so it is
/// random enough not to be guessable in a single-process session, but
/// no replacement for proper auth if the server is ever exposed
/// publicly (it isn't, by default — see [`crate::AppConfig`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScanId(String);

impl ScanId {
    /// Mint a fresh random ID using the workspace `fastrand` PRNG.
    #[must_use]
    pub fn new() -> Self {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut s = String::with_capacity(12);
        for _ in 0..12 {
            let idx = fastrand::usize(..ALPHABET.len());
            s.push(char::from(ALPHABET[idx]));
        }
        Self(s)
    }

    /// Borrow the ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ScanId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ScanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ScanId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Aggregate published once a scan finishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishedScan {
    /// Counts by verdict.
    pub summary: Summary,
    /// All outcomes, in completion order (same order as the live stream).
    pub outcomes: Vec<CheckOutcome>,
    /// Wall-clock duration of the whole scan, milliseconds.
    pub elapsed_ms: u64,
}

/// Verdict counts for a finished scan.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Summary {
    /// Sites where the account exists.
    pub found: usize,
    /// Sites where the account doesn't exist.
    pub not_found: usize,
    /// Sites with inconclusive verdicts.
    pub uncertain: usize,
}

impl Summary {
    /// Tally verdicts from a slice of outcomes.
    #[must_use]
    pub fn from_outcomes(outcomes: &[CheckOutcome]) -> Self {
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

    /// Total number of probed sites.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.found + self.not_found + self.uncertain
    }
}

/// Live state of one scan.
///
/// All fields are `Arc<…>` because handles are shared between the
/// background scan task and any number of HTTP request handlers.
#[derive(Debug, Clone)]
pub struct ScanHandle {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    username: String,
    site_count: usize,
    started_at: Instant,
    created_at_ms: u64,
    outcomes: RwLock<Vec<CheckOutcome>>,
    finished: RwLock<Option<FinishedScan>>,
    // Broadcast carries the *index* of a newly appended outcome rather
    // than the outcome itself — subscribers re-read from `outcomes` so
    // a slow subscriber that misses a notification can still resync by
    // re-snapshotting on the next event.
    tx: broadcast::Sender<usize>,
    done: Notify,
}

impl ScanHandle {
    /// Construct an empty handle ready to accept outcomes.
    ///
    /// `site_count` is the size of the site list this scan will run
    /// against — surfaced through [`Self::site_count`] so the UI can
    /// render `423 / 1890` progress without holding open an SSE
    /// stream. `outcome_buffer` sizes the broadcast ring buffer; a
    /// value substantially larger than `site_count` is fine — the cost
    /// is one `Arc<…>` slot per buffered notification.
    #[must_use]
    pub fn new(username: impl Into<String>, site_count: usize, outcome_buffer: usize) -> Self {
        let (tx, _) = broadcast::channel(outcome_buffer.max(1));
        let created_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Self {
            inner: Arc::new(Inner {
                username: username.into(),
                site_count,
                started_at: Instant::now(),
                created_at_ms,
                outcomes: RwLock::new(Vec::new()),
                finished: RwLock::new(None),
                tx,
                done: Notify::new(),
            }),
        }
    }

    /// Username being scanned (for display / debugging).
    #[must_use]
    pub fn username(&self) -> &str {
        &self.inner.username
    }

    /// Total number of sites this scan will / did probe.
    #[must_use]
    pub fn site_count(&self) -> usize {
        self.inner.site_count
    }

    /// Wall-clock time since the handle was created.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.inner.started_at.elapsed()
    }

    /// Unix epoch milliseconds when this handle was constructed. Used
    /// by the history endpoint so the UI can render relative times.
    #[must_use]
    pub fn created_at_ms(&self) -> u64 {
        self.inner.created_at_ms
    }

    /// Snapshot of outcomes recorded so far. Cheap clone — `Vec` deep-clones
    /// only the small number of strings inside each [`CheckOutcome`].
    pub async fn outcomes_snapshot(&self) -> Vec<CheckOutcome> {
        self.inner.outcomes.read().await.clone()
    }

    /// Final aggregate, once the scan has completed. `None` while running.
    pub async fn finished(&self) -> Option<FinishedScan> {
        self.inner.finished.read().await.clone()
    }

    /// Best-effort sync peek used by the eviction policy. Returns
    /// `false` if the lock is currently held — a momentarily-locked
    /// `finished` slot is, by construction, still being mutated.
    #[must_use]
    pub fn is_finished_now(&self) -> bool {
        self.inner.finished.try_read().is_ok_and(|g| g.is_some())
    }

    /// Subscribe to "new outcome appended at index N" notifications.
    /// Combine with [`Self::outcomes_snapshot`] for "replay then live" semantics.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<usize> {
        self.inner.tx.subscribe()
    }

    /// Wait until the scan finishes. Idempotent — fires for every
    /// caller registered before *or* after the scan completes (the
    /// `finished` field is the source of truth; this is just a wake-up).
    pub async fn wait_done(&self) {
        if self.inner.finished.read().await.is_some() {
            return;
        }
        self.inner.done.notified().await;
    }

    fn tx(&self) -> broadcast::Sender<usize> {
        self.inner.tx.clone()
    }

    async fn append(&self, outcome: CheckOutcome) {
        let mut buf = self.inner.outcomes.write().await;
        let idx = buf.len();
        buf.push(outcome);
        drop(buf);
        // Broadcast send is non-blocking; an `Err` means no live
        // subscribers, which is fine — `outcomes_snapshot` still works.
        let _ = self.inner.tx.send(idx);
    }

    /// Bulk-append outcomes carried over from a previous scan run
    /// (the overlap subset on a refilter). Used to pre-populate a
    /// handle before [`spawn`] starts probing the rest, so the SSE
    /// stream a subscriber attaches to surfaces the carried-over
    /// outcomes immediately. Acquires one write-lock and emits one
    /// broadcast per outcome so subscribers see them as ordinary
    /// `index N appended` events.
    // The write lock is held for the whole bulk-insert deliberately
    // so subscribers never see a half-populated buffer; the
    // "tighten the drop" nursery lint would defeat that.
    #[allow(clippy::significant_drop_tightening)]
    pub(crate) async fn extend_outcomes(&self, carried: Vec<CheckOutcome>) {
        if carried.is_empty() {
            return;
        }
        let mut buf = self.inner.outcomes.write().await;
        for outcome in carried {
            let idx = buf.len();
            buf.push(outcome);
            let _ = self.inner.tx.send(idx);
        }
    }

    pub(crate) async fn publish(&self, finished: FinishedScan) {
        *self.inner.finished.write().await = Some(finished);
        self.inner.done.notify_waiters();
    }

    /// Replace the outcome for `new.site` in the (finished) scan,
    /// recomputing the summary. No-op if the scan is still running.
    ///
    /// Used by the per-site retry endpoint to swap an `Uncertain`
    /// result with a fresh probe.
    // The whole function body operates on the write guard; the nursery
    // lint's "tighten the drop" suggestion would defeat the atomicity
    // we want between the mutation and the summary recompute.
    #[allow(clippy::significant_drop_tightening)]
    pub(crate) async fn replace_outcome(&self, new: CheckOutcome) {
        let mut guard = self.inner.finished.write().await;
        let Some(finished) = guard.as_mut() else {
            return;
        };
        if let Some(slot) = finished.outcomes.iter_mut().find(|o| o.site == new.site) {
            *slot = new;
        } else {
            finished.outcomes.push(new);
        }
        finished.summary = Summary::from_outcomes(&finished.outcomes);
    }
}

/// Optional persistence context handed to [`spawn`]: when present, the
/// finished scan is written to `<dir>/<scan_id>.json` before the `done`
/// event fires — so a UI refresh right after completion can reload the
/// scan from disk.
#[derive(Debug, Clone)]
pub(crate) struct PersistContext {
    pub scan_id: ScanId,
    pub dir: Arc<PathBuf>,
}

/// Spawn the background task that runs the scan and feeds the handle.
///
/// Returns immediately; the caller drives progress via SSE
/// ([`ScanHandle::subscribe`]) or polls completion via
/// [`ScanHandle::finished`].
pub(crate) fn spawn(
    handle: ScanHandle,
    client: Arc<Client>,
    sites: Arc<[Site]>,
    username: Username,
    options: ExecutorOptions,
    persist_ctx: Option<PersistContext>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run(handle, &client, &sites, &username, options, persist_ctx).await;
    })
}

async fn run(
    handle: ScanHandle,
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
    persist_ctx: Option<PersistContext>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CheckOutcome>();

    // The executor callback is sync FnMut — bridge to the async append
    // path via an unbounded mpsc so we never block the executor loop.
    let tx_for_cb = tx.clone();
    let scan_fut = async move {
        let outcomes = executor::run_with_progress(client, sites, username, options, move |o| {
            // Drop is fine: a receive end disappearing means the server
            // is shutting down, in which case losing notifications is
            // exactly what we want.
            let _ = tx_for_cb.send(o.clone());
        })
        .await;
        // Drop the original sender so the consumer loop terminates.
        drop(tx);
        outcomes
    };

    let handle_ref = handle.clone();
    let consume_fut = async move {
        while let Some(outcome) = rx.recv().await {
            handle_ref.append(outcome).await;
        }
    };

    let (all_outcomes, ()) = tokio::join!(scan_fut, consume_fut);

    let elapsed_ms = u64::try_from(handle.elapsed().as_millis()).unwrap_or(u64::MAX);
    let summary = Summary::from_outcomes(&all_outcomes);
    let finished = FinishedScan {
        summary,
        outcomes: all_outcomes,
        elapsed_ms,
    };

    // Persist before publishing the `done` event so a UI that refreshes
    // immediately after seeing `done` still finds the scan on disk.
    if let Some(ctx) = &persist_ctx {
        let snapshot = PersistedScan::from_finished(
            ctx.scan_id.clone(),
            handle.username().to_owned(),
            handle.site_count(),
            handle.created_at_ms(),
            finished.clone(),
        );
        if let Err(err) = persist::save(&ctx.dir, &snapshot).await {
            tracing::warn!(error = %err, scan_id = %ctx.scan_id, "failed to persist scan");
        } else {
            let removed = persist::prune(&ctx.dir, persist::MAX_PERSISTED_SCANS).await;
            if removed > 0 {
                tracing::debug!(removed, "pruned older persisted scans");
            }
        }
    }

    handle.publish(finished).await;
    drop(handle.tx()); // help the broadcast channel close cleanly
}

#[cfg(test)]
mod tests {
    use super::*;
    use adler_core::UncertainReason;

    fn outcome(name: &str, kind: MatchKind) -> CheckOutcome {
        CheckOutcome {
            site: name.into(),
            url: format!("https://{name}.example/u"),
            kind,
            reason: matches!(kind, MatchKind::Uncertain)
                .then_some(UncertainReason::Other("test".into())),
            elapsed_ms: 1,
            enrichment: std::collections::BTreeMap::new(),
            evidence: Vec::new(),
            transport: None,
            escalations: 0,
        }
    }

    #[test]
    fn summary_tallies_by_verdict() {
        let s = Summary::from_outcomes(&[
            outcome("a", MatchKind::Found),
            outcome("b", MatchKind::NotFound),
            outcome("c", MatchKind::NotFound),
            outcome("d", MatchKind::Uncertain),
        ]);
        assert_eq!(s.found, 1);
        assert_eq!(s.not_found, 2);
        assert_eq!(s.uncertain, 1);
        assert_eq!(s.total(), 4);
    }

    #[test]
    fn scan_id_is_url_safe_and_random() {
        let a = ScanId::new();
        let b = ScanId::new();
        assert_eq!(a.as_str().len(), 12);
        assert!(
            a.as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        );
        // Birthday-collision probability on two 71-bit IDs is negligible.
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn append_publishes_to_subscribers_and_history() {
        let handle = ScanHandle::new("alice", 2, 16);
        let mut rx = handle.subscribe();

        handle.append(outcome("GitHub", MatchKind::Found)).await;
        handle.append(outcome("GitLab", MatchKind::NotFound)).await;

        // History was recorded in order.
        let snap = handle.outcomes_snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].site, "GitHub");
        assert_eq!(snap[1].site, "GitLab");

        // Indices were broadcast in order.
        assert_eq!(rx.recv().await.unwrap(), 0);
        assert_eq!(rx.recv().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn publish_releases_wait_done_and_exposes_finished() {
        let handle = ScanHandle::new("alice", 1, 4);

        let waiter = {
            let h = handle.clone();
            tokio::spawn(async move { h.wait_done().await })
        };

        // Give the waiter a chance to park.
        tokio::task::yield_now().await;

        handle
            .publish(FinishedScan {
                summary: Summary {
                    found: 1,
                    not_found: 0,
                    uncertain: 0,
                },
                outcomes: vec![outcome("GitHub", MatchKind::Found)],
                elapsed_ms: 42,
            })
            .await;

        waiter.await.unwrap();
        let f = handle.finished().await.expect("finished");
        assert_eq!(f.summary.found, 1);
        assert_eq!(f.elapsed_ms, 42);
        assert_eq!(f.outcomes.len(), 1);
    }

    #[tokio::test]
    async fn wait_done_returns_immediately_if_already_finished() {
        let handle = ScanHandle::new("alice", 1, 4);
        handle
            .publish(FinishedScan {
                summary: Summary::default(),
                outcomes: Vec::new(),
                elapsed_ms: 0,
            })
            .await;
        // Should not deadlock — the fast path checks `finished` first.
        tokio::time::timeout(Duration::from_millis(100), handle.wait_done())
            .await
            .expect("wait_done must return immediately when already finished");
    }
}
