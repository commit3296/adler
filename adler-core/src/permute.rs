//! Username permutation: expand one handle into plausible variants.
//!
//! People reuse the same identity with small spelling shifts —
//! `john_doe` / `johndoe` / `john.doe`, or leet like `j0hn`. Scanning a few
//! variants raises recall at the cost of more requests, so it's opt-in via
//! `--permute`.
//!
//! Levels:
//! - [`PermuteLevel::None`]: just the original.
//! - [`PermuteLevel::Basic`]: separator swaps (`_` `-` `.` and removal).
//! - [`PermuteLevel::Aggressive`]: basic, plus single-class leet
//!   substitutions and a couple of digit suffixes.
//!
//! Output is deduplicated, always leads with the original, contains only
//! strings that pass [`Username`] validation, and is capped at
//! [`MAX_VARIANTS`] to bound the request blow-up.

use std::collections::HashSet;

use crate::username::Username;

/// Hard cap on the number of variants returned (including the original).
pub const MAX_VARIANTS: usize = 64;

const SEPARATORS: [char; 3] = ['_', '-', '.'];
/// Single-character leet substitutions, applied one class at a time.
const LEET: [(char, char); 5] = [('o', '0'), ('i', '1'), ('e', '3'), ('a', '4'), ('s', '5')];
const SUFFIXES: [&str; 2] = ["1", "123"];

/// How aggressively to expand a username into variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermuteLevel {
    /// No expansion — just the original username.
    None,
    /// Separator swaps and removal.
    Basic,
    /// Basic, plus leet substitutions and digit suffixes.
    Aggressive,
}

/// Expand `username` into a deduplicated list of variants per `level`.
///
/// The original is always first. Variants that fail [`Username`] validation
/// are dropped. The result never exceeds [`MAX_VARIANTS`].
#[must_use]
pub fn permute(username: &Username, level: PermuteLevel) -> Vec<Username> {
    let base = username.as_str().to_owned();
    let mut out: Vec<Username> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    add(&mut out, &mut seen, base.clone());

    if level == PermuteLevel::None {
        return out;
    }

    // Basic: separator swaps + removal (only meaningful if a separator exists).
    if base.contains(SEPARATORS) {
        let stripped: String = base.chars().filter(|c| !SEPARATORS.contains(c)).collect();
        try_add(&mut out, &mut seen, stripped);
        for &sep in &SEPARATORS {
            let swapped: String = base
                .chars()
                .map(|c| if SEPARATORS.contains(&c) { sep } else { c })
                .collect();
            try_add(&mut out, &mut seen, swapped);
        }
    }

    if level == PermuteLevel::Basic {
        out.truncate(MAX_VARIANTS);
        return out;
    }

    // Aggressive: leet over every variant produced so far, one class at a time.
    let snapshot: Vec<String> = out.iter().map(|u| u.as_str().to_owned()).collect();
    for variant in &snapshot {
        for &(from, to) in &LEET {
            if variant.contains(from) {
                let leeted: String = variant
                    .chars()
                    .map(|c| if c == from { to } else { c })
                    .collect();
                try_add(&mut out, &mut seen, leeted);
            }
        }
    }
    // Digit suffixes on the original handle.
    for suffix in SUFFIXES {
        try_add(&mut out, &mut seen, format!("{base}{suffix}"));
    }

    out.truncate(MAX_VARIANTS);
    out
}

/// Insert a candidate that is already known to be a valid username.
fn add(out: &mut Vec<Username>, seen: &mut HashSet<String>, candidate: String) {
    if seen.insert(candidate.clone()) {
        if let Ok(u) = Username::new(candidate) {
            out.push(u);
        }
    }
}

/// Insert a candidate string, validating it as a username first.
fn try_add(out: &mut Vec<Username>, seen: &mut HashSet<String>, candidate: String) {
    if out.len() >= MAX_VARIANTS {
        return;
    }
    add(out, seen, candidate);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Username]) -> Vec<&str> {
        v.iter().map(Username::as_str).collect()
    }

    fn user(s: &str) -> Username {
        Username::new(s).unwrap()
    }

    #[test]
    fn none_returns_only_original() {
        let v = permute(&user("john_doe"), PermuteLevel::None);
        assert_eq!(names(&v), ["john_doe"]);
    }

    #[test]
    fn original_is_always_first() {
        for level in [PermuteLevel::Basic, PermuteLevel::Aggressive] {
            let v = permute(&user("john_doe"), level);
            assert_eq!(v[0].as_str(), "john_doe");
        }
    }

    #[test]
    fn basic_swaps_separators() {
        let v = permute(&user("john_doe"), PermuteLevel::Basic);
        let n = names(&v);
        for expected in ["john_doe", "johndoe", "john.doe", "john-doe"] {
            assert!(n.contains(&expected), "missing {expected:?} in {n:?}");
        }
    }

    #[test]
    fn basic_without_separator_is_just_original() {
        let v = permute(&user("johndoe"), PermuteLevel::Basic);
        assert_eq!(names(&v), ["johndoe"]);
    }

    #[test]
    fn aggressive_adds_leet_and_suffixes() {
        let v = permute(&user("bob"), PermuteLevel::Aggressive);
        let n = names(&v);
        assert!(n.contains(&"bob"));
        assert!(n.contains(&"b0b"), "leet o→0 missing in {n:?}");
        assert!(n.contains(&"bob1"));
        assert!(n.contains(&"bob123"));
    }

    #[test]
    fn results_are_deduplicated() {
        let v = permute(&user("aaa"), PermuteLevel::Aggressive);
        let n = names(&v);
        let mut sorted = n.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), n.len(), "duplicates in {n:?}");
    }

    #[test]
    fn all_variants_are_valid_usernames() {
        // Every emitted variant must round-trip through Username::new.
        let v = permute(&user("john.doe_x"), PermuteLevel::Aggressive);
        for u in &v {
            assert!(Username::new(u.as_str()).is_ok());
        }
    }

    #[test]
    fn never_exceeds_cap() {
        let v = permute(&user("a.b.c-d_e.o.i.e.a.s"), PermuteLevel::Aggressive);
        assert!(v.len() <= MAX_VARIANTS, "got {}", v.len());
    }

    proptest::proptest! {
        /// Invariants that must hold for any valid username and any level.
        #[test]
        fn permute_invariants(
            s in "[A-Za-z0-9._-]{1,64}",
            level_idx in 0usize..3,
        ) {
            let level = [
                PermuteLevel::None,
                PermuteLevel::Basic,
                PermuteLevel::Aggressive,
            ][level_idx];
            let variants = permute(&user(&s), level);

            proptest::prop_assert!(!variants.is_empty());
            // The original always leads.
            proptest::prop_assert_eq!(variants[0].as_str(), s.as_str());
            // Bounded request blow-up.
            proptest::prop_assert!(variants.len() <= MAX_VARIANTS);
            // Every variant is itself a valid username.
            for v in &variants {
                proptest::prop_assert!(Username::new(v.as_str()).is_ok());
            }
            // No duplicates.
            let unique: std::collections::HashSet<&str> =
                variants.iter().map(Username::as_str).collect();
            proptest::prop_assert_eq!(unique.len(), variants.len());
            // None means exactly the original.
            if level == PermuteLevel::None {
                proptest::prop_assert_eq!(variants.len(), 1);
            }
        }
    }
}
