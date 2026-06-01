//! Retry policy for ban-like responses.
//!
//! The Phase 2 ban detector classifies certain responses
//! (`rate_limited`, `cloudflare_challenge`) as transient. This module decides
//! whether to retry such an outcome and computes the delay before the next
//! attempt. The delay grows exponentially with each attempt and is jittered
//! by ±25 % to avoid synchronising retries across concurrent scans.

use std::time::Duration;

use crate::check::{CheckOutcome, MatchKind, UncertainReason};

/// Retry tuning. Cheap to clone — held on the [`crate::Client`].
#[derive(Debug, Clone)]
pub(crate) struct RetryPolicy {
    /// Max retry attempts beyond the initial try. `0` disables retry.
    pub(crate) max_retries: u32,
    /// First backoff delay before jitter.
    pub(crate) base_delay: Duration,
    /// Upper bound on a single backoff delay (pre-jitter).
    pub(crate) max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

/// True if a probe outcome looks transient (rate-limited / challenged) and
/// the policy still permits another attempt.
pub(crate) fn should_retry(outcome: &CheckOutcome, attempt: u32, policy: &RetryPolicy) -> bool {
    if attempt >= policy.max_retries {
        return false;
    }
    if outcome.kind != MatchKind::Uncertain {
        return false;
    }
    matches!(
        outcome.reason,
        Some(UncertainReason::RateLimited | UncertainReason::CloudflareChallenge)
    )
}

/// Compute the delay before retry `attempt`. `attempt` is 0-based: 0 for the
/// delay between the first and second tries, 1 for second→third, etc.
///
/// The delay is `base_delay * 2^attempt`, capped at `max_delay`, then
/// jittered by ±25 % to break synchrony across parallel tasks. Jitter uses
/// the process-wide `fastrand` PRNG; deterministic delays in tests come
/// from seeding `fastrand::seed`.
pub(crate) fn backoff_delay(attempt: u32, policy: &RetryPolicy) -> Duration {
    let shift = attempt.min(20);
    let raw = policy.base_delay.saturating_mul(1u32 << shift);
    let capped = raw.min(policy.max_delay);
    let jitter = fastrand::f64().mul_add(0.5, -0.25); // -0.25 .. +0.25
    let secs = capped.as_secs_f64() * (1.0 + jitter);
    if secs <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome_with_reason(reason: Option<UncertainReason>) -> CheckOutcome {
        CheckOutcome {
            site: "S".into(),
            url: "u".into(),
            kind: MatchKind::Uncertain,
            reason,
            elapsed_ms: 0,
            enrichment: std::collections::BTreeMap::new(),
            evidence: Vec::new(),
            transport: None,
            escalations: 0,
        }
    }

    #[test]
    fn rate_limited_uncertain_retries_while_attempts_remain() {
        let policy = RetryPolicy::default();
        let outcome = outcome_with_reason(Some(UncertainReason::RateLimited));
        assert!(should_retry(&outcome, 0, &policy));
        assert!(should_retry(&outcome, 1, &policy));
        assert!(!should_retry(&outcome, 2, &policy));
    }

    #[test]
    fn cloudflare_challenge_retries() {
        let policy = RetryPolicy::default();
        let outcome = outcome_with_reason(Some(UncertainReason::CloudflareChallenge));
        assert!(should_retry(&outcome, 0, &policy));
    }

    #[test]
    fn other_uncertain_reasons_do_not_retry() {
        let policy = RetryPolicy::default();
        let outcome = outcome_with_reason(Some(UncertainReason::Network("refused".into())));
        assert!(!should_retry(&outcome, 0, &policy));
        let outcome = outcome_with_reason(Some(UncertainReason::BodyRead("eof".into())));
        assert!(!should_retry(&outcome, 0, &policy));
    }

    #[test]
    fn found_and_not_found_do_not_retry() {
        let policy = RetryPolicy::default();
        let mut outcome = outcome_with_reason(Some(UncertainReason::RateLimited));
        outcome.kind = MatchKind::Found;
        assert!(!should_retry(&outcome, 0, &policy));
        outcome.kind = MatchKind::NotFound;
        assert!(!should_retry(&outcome, 0, &policy));
    }

    #[test]
    fn zero_max_retries_never_retries() {
        let policy = RetryPolicy {
            max_retries: 0,
            ..Default::default()
        };
        let outcome = outcome_with_reason(Some(UncertainReason::RateLimited));
        assert!(!should_retry(&outcome, 0, &policy));
    }

    #[test]
    fn backoff_grows_exponentially_within_cap() {
        // Use a deterministic seed so jitter doesn't poison the bounds.
        fastrand::seed(42);
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
        };
        let d0 = backoff_delay(0, &policy);
        let d1 = backoff_delay(1, &policy);
        let d2 = backoff_delay(2, &policy);
        // d0 around 100 ms ±25 %, d1 around 200 ms ±25 %, d2 around 400 ms ±25 %.
        assert!(d0 >= Duration::from_millis(70) && d0 <= Duration::from_millis(130));
        assert!(d1 >= Duration::from_millis(140) && d1 <= Duration::from_millis(260));
        assert!(d2 >= Duration::from_millis(280) && d2 <= Duration::from_millis(520));
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        fastrand::seed(7);
        let policy = RetryPolicy {
            max_retries: 20,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        // Attempt 10 would otherwise be 100 ms * 2^10 = ~102 s; capped at 500 ms.
        let d = backoff_delay(10, &policy);
        // 500 ms ±25 % = [375, 625] ms.
        assert!(d <= Duration::from_millis(625), "got {d:?}");
        assert!(d >= Duration::from_millis(375), "got {d:?}");
    }
}
