//! Concurrent fan-out runner for site probes.
//!
//! Spawns one task per site and bounds the maximum in-flight count with a
//! [`Semaphore`]. Tasks are independent — a panic or hang in one site never
//! blocks results from the rest. Each task self-aborts when the global
//! deadline (if any) is reached; remaining sites surface as
//! [`MatchKind::Uncertain`].

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Instant as TokioInstant, timeout_at};

use crate::check::{CheckOutcome, MatchKind};
use crate::client::Client;
use crate::site::Site;
use crate::username::Username;

/// Default concurrency for [`run`].
///
/// Most sites are distinct hosts, so the per-host throttle rarely serialises;
/// the bottleneck is network round-trips, and 32 in-flight probes keeps the
/// pipe full without hammering any single host.
const DEFAULT_CONCURRENCY: NonZeroUsize = match NonZeroUsize::new(32) {
    Some(n) => n,
    None => unreachable!(),
};

/// Tunables for [`run`].
#[derive(Debug, Clone)]
#[must_use = "ExecutorOptions does nothing until passed to executor::run"]
pub struct ExecutorOptions {
    /// Maximum number of in-flight site probes.
    pub concurrency: NonZeroUsize,
    /// Total wall-clock deadline for the entire scan. Sites still in flight
    /// when this elapses produce [`MatchKind::Uncertain`] outcomes.
    pub deadline: Option<Duration>,
}

impl Default for ExecutorOptions {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            deadline: None,
        }
    }
}

impl ExecutorOptions {
    /// Override [`Self::concurrency`].
    pub fn concurrency(mut self, n: NonZeroUsize) -> Self {
        self.concurrency = n;
        self
    }

    /// Set a total scan deadline.
    pub fn deadline(mut self, d: Duration) -> Self {
        self.deadline = Some(d);
        self
    }
}

/// Run a fan-out scan over `sites`, returning one outcome per site.
///
/// Results come back in completion order (not input order) — sort by name
/// for stable presentation. A panicking site task is logged at `error` and
/// silently dropped; transient HTTP failures already become
/// [`MatchKind::Uncertain`] inside `Client::check`.
pub async fn run(
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
) -> Vec<CheckOutcome> {
    run_with_progress(client, sites, username, options, |_| {}).await
}

/// Variant of [`run`] that invokes `on_outcome` for each completed probe.
///
/// Useful for driving a live progress indicator or for emitting streaming
/// output before the full scan finishes. The callback runs on the executor
/// task between completions; long work inside it will throttle the loop.
pub async fn run_with_progress<F>(
    client: &Client,
    sites: &[Site],
    username: &Username,
    options: ExecutorOptions,
    mut on_outcome: F,
) -> Vec<CheckOutcome>
where
    F: FnMut(&CheckOutcome),
{
    let semaphore = Arc::new(Semaphore::new(options.concurrency.get()));
    let deadline_at = options.deadline.map(|d| TokioInstant::now() + d);
    let mut set: JoinSet<CheckOutcome> = JoinSet::new();

    for site in sites {
        let site = site.clone();
        let username = username.clone();
        let client = client.clone();
        let permits = Arc::clone(&semaphore);
        set.spawn(async move {
            let permit = match permits.acquire_owned().await {
                Ok(p) => p,
                Err(_closed) => {
                    return CheckOutcome {
                        site: site.name.clone(),
                        url: site.url_for(&username),
                        kind: MatchKind::Uncertain,
                        reason: Some(crate::check::UncertainReason::SchedulerClosed),
                        elapsed_ms: 0,
                        enrichment: std::collections::BTreeMap::new(),
                        evidence: Vec::new(),
                    };
                }
            };
            let probe = client.check(&site, &username);
            let outcome = match deadline_at {
                None => probe.await,
                Some(at) => match timeout_at(at, probe).await {
                    Ok(o) => o,
                    Err(_elapsed) => CheckOutcome {
                        site: site.name.clone(),
                        url: site.url_for(&username),
                        kind: MatchKind::Uncertain,
                        reason: Some(crate::check::UncertainReason::Deadline),
                        elapsed_ms: 0,
                        enrichment: std::collections::BTreeMap::new(),
                        evidence: Vec::new(),
                    },
                },
            };
            drop(permit);
            outcome
        });
    }

    let mut results = Vec::with_capacity(sites.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(outcome) => {
                on_outcome(&outcome);
                results.push(outcome);
            }
            Err(err) if err.is_cancelled() => {
                tracing::warn!(error = %err, "check task cancelled");
            }
            Err(err) => {
                tracing::error!(error = %err, "check task panicked");
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{Signal, UrlTemplate};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Test sites are uniformly defined with a Found/NotFound status pair,
    /// matching how production sites.json migrates from Phase 1.
    fn site(server: &MockServer, name: &str, segment: &str) -> Site {
        Site {
            name: name.into(),
            url: UrlTemplate::new(format!("{}/{}/{{username}}", server.uri(), segment)).unwrap(),
            signals: vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::StatusNotFound { codes: vec![404] },
            ],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn fast_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(5))
            // Tests share host 127.0.0.1 — disable throttling so concurrency
            // assertions actually exercise the executor.
            .min_request_interval(Duration::ZERO)
            .build()
            .unwrap()
    }

    fn opts_with_concurrency(n: usize) -> ExecutorOptions {
        ExecutorOptions::default().concurrency(NonZeroUsize::new(n).unwrap())
    }

    #[tokio::test]
    async fn runs_all_sites_concurrently() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/c/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let sites = vec![
            site(&server, "A", "a"),
            site(&server, "B", "b"),
            site(&server, "C", "c"),
        ];
        let user = Username::new("alice").unwrap();
        let mut out = run(&fast_client(), &sites, &user, opts_with_concurrency(4)).await;
        out.sort_by(|a, b| a.site.cmp(&b.site));

        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, MatchKind::Found);
        assert_eq!(out[1].kind, MatchKind::NotFound);
        assert_eq!(out[2].kind, MatchKind::Found);
    }

    #[tokio::test]
    async fn respects_concurrency_limit() {
        let server = MockServer::start().await;
        for i in 0..6 {
            Mock::given(method("GET"))
                .and(path(format!("/{i}/alice")))
                .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(50)))
                .mount(&server)
                .await;
        }
        let sites: Vec<Site> = (0..6)
            .map(|i| site(&server, &format!("S{i}"), &i.to_string()))
            .collect();
        let user = Username::new("alice").unwrap();
        let started = std::time::Instant::now();
        let out = run(&fast_client(), &sites, &user, opts_with_concurrency(2)).await;
        let elapsed = started.elapsed();
        assert_eq!(out.len(), 6);
        // 6 sites / 2 concurrent * 50 ms = 150 ms floor.
        assert!(
            elapsed >= Duration::from_millis(120),
            "expected ≥120 ms, got {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn empty_input_returns_empty() {
        let user = Username::new("alice").unwrap();
        let out = run(&fast_client(), &[], &user, opts_with_concurrency(4)).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn run_with_progress_invokes_callback_per_outcome() {
        use std::sync::Mutex;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let sites = vec![site(&server, "A", "a"), site(&server, "B", "b")];
        let user = Username::new("alice").unwrap();
        let calls = Mutex::new(0);
        let outcomes = run_with_progress(
            &fast_client(),
            &sites,
            &user,
            opts_with_concurrency(4),
            |_| *calls.lock().unwrap() += 1,
        )
        .await;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn deadline_marks_slow_sites_uncertain() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow/alice"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fast/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let sites = vec![site(&server, "Slow", "slow"), site(&server, "Fast", "fast")];
        let user = Username::new("alice").unwrap();
        let options = ExecutorOptions::default()
            .concurrency(NonZeroUsize::new(4).unwrap())
            .deadline(Duration::from_millis(200));
        let started = std::time::Instant::now();
        let mut out = run(&fast_client(), &sites, &user, options).await;
        let elapsed = started.elapsed();
        out.sort_by(|a, b| a.site.cmp(&b.site));

        assert_eq!(out.len(), 2);
        // Fast site completed; slow one hit the deadline.
        let fast = out.iter().find(|o| o.site == "Fast").unwrap();
        let slow = out.iter().find(|o| o.site == "Slow").unwrap();
        assert_eq!(fast.kind, MatchKind::Found);
        assert_eq!(slow.kind, MatchKind::Uncertain);
        assert_eq!(slow.reason, Some(crate::check::UncertainReason::Deadline));
        assert!(
            elapsed < Duration::from_millis(800),
            "scan should abort near the deadline, got {elapsed:?}",
        );
    }
}
