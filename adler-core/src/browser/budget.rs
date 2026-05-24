//! Per-scan ceiling on how many browser-routed probes are allowed.
//!
//! Browser fetches are slower and (for [`BrowserbaseBackend`]) cost money.
//! A cap lets the user avoid accidentally piping the whole registry
//! through the browser if a flag is misconfigured. When exhausted the
//! [`Client`] returns `Uncertain` with a `BrowserBudget` reason instead
//! of doing the fetch.
//!
//! [`BrowserbaseBackend`]: super::BrowserbaseBackend
//! [`Client`]: crate::Client

use std::sync::atomic::{AtomicUsize, Ordering};

/// Atomic counter against a fixed cap. Cheap to share across tasks.
#[derive(Debug)]
pub struct BrowserBudget {
    used: AtomicUsize,
    cap: usize,
}

impl BrowserBudget {
    /// Allow up to `cap` consumes. `cap = 0` denies everything.
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
    /// Returns `true` if accepted, `false` once the cap is reached.
    /// Multiple callers may race — the compare-exchange loop guarantees
    /// `used <= cap` at all times.
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

    /// Current consumed count.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumes_up_to_cap_then_denies() {
        let b = BrowserBudget::new(3);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume(), "fourth must be denied");
        assert!(!b.try_consume());
        assert_eq!(b.used(), 3);
    }

    #[test]
    fn zero_cap_denies_everything() {
        let b = BrowserBudget::new(0);
        assert!(!b.try_consume());
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn unlimited_never_denies() {
        let b = BrowserBudget::unlimited();
        for _ in 0..1000 {
            assert!(b.try_consume());
        }
        assert_eq!(b.cap(), usize::MAX);
    }

    #[test]
    fn parallel_consumes_never_exceed_cap() {
        use std::sync::Arc;
        use std::thread;
        let b = Arc::new(BrowserBudget::new(50));
        let mut handles = vec![];
        for _ in 0..10 {
            let b = Arc::clone(&b);
            handles.push(thread::spawn(move || {
                let mut won = 0;
                for _ in 0..20 {
                    if b.try_consume() {
                        won += 1;
                    }
                }
                won
            }));
        }
        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 50);
        assert_eq!(b.used(), 50);
    }
}
