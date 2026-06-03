//! Escalation: when a cheap transport's `Uncertain` looks fixable by a
//! heavier transport, retry through it — bounded by a per-scan budget.
//!
//! The default routing in [`Client::probe_once`](crate::Client) picks one
//! transport per site based on its `protection` tags and the `bot-protected`
//! tag. That works when the registry tags the site correctly, but misses the
//! long tail of sites we haven't pre-tagged that nevertheless sit behind
//! Cloudflare or a rate-limit edge. For those, the HTTP / impersonate path
//! returns `Uncertain(CloudflareChallenge)` or `Uncertain(RateLimited)`; an
//! automatic retry through the browser backend flips the verdict to a real
//! `Found` / `NotFound` rather than the operator having to re-run with a
//! manual override.
//!
//! Escalation is bounded by [`EscalationBudget`]: the operator controls how
//! many extra browser fetches a single scan may consume, on top of the
//! [`BrowserBudget`](crate::BrowserBudget) cap that gates the pre-tagged
//! bot-protected subset. Defaults to 30; `--no-escalation` turns it off.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::check::UncertainReason;

/// Which transport actually produced an outcome.
///
/// Stamped on every [`CheckOutcome`](crate::CheckOutcome) so downstream
/// tools (the doctor, the bench harness, the web UI) can tell whether the
/// HTTP path was enough, whether impersonation was needed, or whether the
/// scan reached for the browser. `Option<TransportTier>` in the outcome
/// keeps older persisted JSON parseable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransportTier {
    /// Plain `reqwest` HTTP path (the default cheap transport).
    Http,
    /// `wreq` with Chrome 134 TLS-fingerprint emulation, behind the
    /// `impersonate` Cargo feature.
    Impersonate,
    /// Headless browser via [`BrowserBackend`](crate::BrowserBackend).
    Browser,
}

impl TransportTier {
    /// Short stable identifier for logs / JSON / explain output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Impersonate => "impersonate",
            Self::Browser => "browser",
        }
    }
}

impl core::fmt::Display for TransportTier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-scan ceiling on automatic escalation attempts.
///
/// Mirrors [`BrowserBudget`](crate::BrowserBudget) in shape but is a
/// distinct type so the two caps are independent: a `bot-protected` site
/// that goes straight to the browser consumes [`crate::BrowserBudget`]; a site
/// that tries HTTP first and falls back to the browser consumes one of
/// each. Cheap to share across tasks.
#[derive(Debug)]
pub struct EscalationBudget {
    used: AtomicUsize,
    cap: usize,
}

impl EscalationBudget {
    /// Allow up to `cap` consumes. `cap = 0` denies all escalations.
    #[must_use]
    pub const fn new(cap: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            cap,
        }
    }

    /// No ceiling — every `try_consume` succeeds.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Atomically reserve one unit of budget.
    ///
    /// Returns `true` if accepted, `false` once the cap is reached. The
    /// compare-exchange loop guarantees `used <= cap` under concurrent
    /// callers.
    pub fn try_consume(&self) -> bool {
        let mut cur = self.used.load(Ordering::Acquire);
        loop {
            if cur >= self.cap {
                return false;
            }
            match self
                .used
                .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Number of escalations the scan has consumed so far.
    #[must_use]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// Maximum the budget allows.
    #[must_use]
    pub const fn cap(&self) -> usize {
        self.cap
    }
}

/// Whether an `Uncertain` outcome from the cheap path is worth retrying
/// through the browser.
///
/// We escalate only on reasons that a real browser plausibly resolves —
/// Cloudflare interstitials and rate-limit / 429-style responses. Reasons
/// that no transport change can fix (the operator opted into robots-
/// disallowed, the username is locally invalid, the deadline elapsed, the
/// egress pool can't satisfy a geo requirement, a session is missing) stay
/// as-is so escalation doesn't waste budget on hopeless cases.
pub(crate) const fn should_escalate(reason: &UncertainReason) -> bool {
    matches!(
        reason,
        UncertainReason::CloudflareChallenge | UncertainReason::RateLimited
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalates_on_cloudflare_and_rate_limited_only() {
        assert!(should_escalate(&UncertainReason::CloudflareChallenge));
        assert!(should_escalate(&UncertainReason::RateLimited));

        assert!(!should_escalate(&UncertainReason::Captcha));
        assert!(!should_escalate(&UncertainReason::RobotsDisallowed));
        assert!(!should_escalate(&UncertainReason::Deadline));
        assert!(!should_escalate(&UncertainReason::SchedulerClosed));
        assert!(!should_escalate(&UncertainReason::Network(
            "refused".into()
        )));
        assert!(!should_escalate(&UncertainReason::BodyRead("eof".into())));
        assert!(!should_escalate(&UncertainReason::BrowserBudget));
        assert!(!should_escalate(&UncertainReason::UsernameNotAllowed));
        assert!(!should_escalate(&UncertainReason::BrowserFailed(
            "timeout".into()
        )));
        assert!(!should_escalate(&UncertainReason::GeoUnavailable));
        assert!(!should_escalate(&UncertainReason::SessionRequired));
        assert!(!should_escalate(&UncertainReason::Other("?".into())));
    }

    #[test]
    fn budget_consumes_up_to_cap() {
        let b = EscalationBudget::new(2);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume());
        assert_eq!(b.used(), 2);
        assert_eq!(b.cap(), 2);
    }

    #[test]
    fn budget_zero_denies_all() {
        let b = EscalationBudget::new(0);
        assert!(!b.try_consume());
    }

    #[test]
    fn budget_unlimited_never_denies() {
        let b = EscalationBudget::unlimited();
        for _ in 0..1024 {
            assert!(b.try_consume());
        }
    }

    #[test]
    fn transport_tier_as_str_matches_serde() {
        assert_eq!(TransportTier::Http.as_str(), "http");
        assert_eq!(TransportTier::Impersonate.as_str(), "impersonate");
        assert_eq!(TransportTier::Browser.as_str(), "browser");

        let json = serde_json::to_string(&TransportTier::Impersonate).unwrap();
        assert_eq!(json, r#""impersonate""#);
        let back: TransportTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TransportTier::Impersonate);
    }
}
