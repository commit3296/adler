//! Per-host minimum-interval throttle.
//!
//! Phase 1 ships a simple spacing-based throttle: every request to the same
//! host must be at least `min_interval` apart from the previous reservation
//! for that host. Concurrent callers serialise naturally — each one
//! atomically claims the next slot under a short-held mutex. This is enough
//! to avoid hammering any single site during a fan-out scan.
//!
//! Phase 2 will swap this for a real token-bucket (`governor`) when burst
//! handling and per-site quotas come into play.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Reservation-based per-host throttle.
///
/// Cheap to clone — internally `Arc<Mutex<…>>`, so sharing one throttle
/// across all probe tasks is correct (and intended).
#[derive(Debug, Clone)]
pub(crate) struct HostThrottle {
    state: Arc<Mutex<HashMap<String, Instant>>>,
    min_interval: Duration,
}

impl HostThrottle {
    /// Build a throttle that enforces at least `min_interval` between
    /// successive reservations for the same host.
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            min_interval,
        }
    }

    /// Reserve the next slot for `host` and wait until it is due.
    ///
    /// The reservation is taken under the lock, so N concurrent callers
    /// for the same host wake up at `now`, `now + interval`,
    /// `now + 2 * interval`, … rather than all bypassing the throttle.
    pub(crate) async fn wait(&self, host: &str) {
        let due = {
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let earliest = state.get(host).copied().filter(|t| *t > now).unwrap_or(now);
            state.insert(host.to_owned(), earliest + self.min_interval);
            earliest
        };
        let now = Instant::now();
        if due > now {
            tokio::time::sleep(due - now).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_call_returns_immediately() {
        let throttle = HostThrottle::new(Duration::from_millis(50));
        let started = Instant::now();
        throttle.wait("example.com").await;
        assert!(started.elapsed() < Duration::from_millis(30));
    }

    #[tokio::test]
    async fn second_call_to_same_host_waits_min_interval() {
        let throttle = HostThrottle::new(Duration::from_millis(80));
        throttle.wait("example.com").await;
        let started = Instant::now();
        throttle.wait("example.com").await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(70),
            "second call should wait, got {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn different_hosts_do_not_interfere() {
        let throttle = HostThrottle::new(Duration::from_millis(80));
        throttle.wait("a.example.com").await;
        let started = Instant::now();
        throttle.wait("b.example.com").await;
        assert!(started.elapsed() < Duration::from_millis(30));
    }

    #[tokio::test]
    async fn concurrent_calls_serialise() {
        // Three concurrent callers for the same host should fan out to
        // ~0 ms / interval / 2 * interval.
        let throttle = HostThrottle::new(Duration::from_millis(60));
        let started = Instant::now();
        let throttle = Arc::new(throttle);
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let t = Arc::clone(&throttle);
                tokio::spawn(async move { t.wait("example.com").await })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(110),
            "three serialised calls at 60 ms spacing should take ~120 ms, got {elapsed:?}",
        );
    }
}
