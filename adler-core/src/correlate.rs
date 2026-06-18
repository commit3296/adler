//! Cross-account correlation from enrichment fields.
//!
//! A username search returns many `Found` accounts for the *same handle* —
//! but the same handle can belong to different people on different sites.
//! This module groups accounts that share enough profile signal (name, bio)
//! to plausibly be the same identity, so an analyst can tell
//! "all these are clearly one person" from "this handle is just popular".
//!
//! Signals are text-only by design in this legacy CLI correlation report.
//! The newer `IdentityCluster` model handles typed evidence such as avatar
//! URL equality and opt-in avatar perceptual hashes. Each pair of accounts
//! that both carry profile data is scored 0..1:
//!
//! - **name**: 1.0 if normalised-equal, else token Jaccard.
//! - **bio**: token Jaccard.
//! - combined = mean of the signals present in both.
//!
//! Pairs at or above [`LINK_THRESHOLD`] are linked; connected accounts form
//! a cluster (union-find). Cluster confidence is the mean linking score.
//! Confidence is a heuristic triage aid, not proof.

// All `usize as f64` casts here are over small counts (token-set sizes,
// cluster/edge counts) used to form ratios; the 52-bit mantissa is never a
// concern at these magnitudes.
#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeSet;

use crate::check::{CheckOutcome, MatchKind};

/// Minimum pairwise score to link two accounts.
pub const LINK_THRESHOLD: f64 = 0.5;
/// Drop tokens shorter than this when building word sets (cuts noise).
const MIN_TOKEN_LEN: usize = 2;

/// A group of accounts that likely belong to the same person.
#[derive(Debug, Clone)]
pub struct Cluster {
    /// Site names of the member accounts.
    pub members: Vec<String>,
    /// Mean linking score across the cluster's edges, in `0..=1`.
    pub confidence: f64,
    /// A normalised name shared by the whole cluster, if any.
    pub shared_name: Option<String>,
}

/// Result of correlating a scan's outcomes.
#[derive(Debug, Clone, Default)]
pub struct CorrelationReport {
    /// Clusters of size ≥ 2 (actual cross-site links).
    pub clusters: Vec<Cluster>,
    /// Found accounts that carry profile data but linked to nothing.
    pub unlinked: Vec<String>,
    /// Found accounts with no profile data to correlate on.
    pub without_profile: Vec<String>,
}

struct Node<'a> {
    site: &'a str,
    name: Option<String>,
    name_tokens: BTreeSet<String>,
    bio_tokens: BTreeSet<String>,
}

/// Correlate `Found` accounts by their enrichment fields.
#[must_use]
pub fn correlate(outcomes: &[CheckOutcome]) -> CorrelationReport {
    let mut report = CorrelationReport::default();

    let mut nodes: Vec<Node<'_>> = Vec::new();
    for outcome in outcomes.iter().filter(|o| o.kind == MatchKind::Found) {
        let name = outcome.enrichment.get("name");
        let bio = outcome.enrichment.get("bio");
        if name.is_none() && bio.is_none() {
            report.without_profile.push(outcome.site.clone());
            continue;
        }
        nodes.push(Node {
            site: &outcome.site,
            name: name.map(|n| normalize(n)),
            name_tokens: name.map(|n| tokenize(n)).unwrap_or_default(),
            bio_tokens: bio.map(|b| tokenize(b)).unwrap_or_default(),
        });
    }

    let mut uf = UnionFind::new(nodes.len());
    // Accumulate edge scores per root so we can average per cluster.
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    for a in 0..nodes.len() {
        for b in (a + 1)..nodes.len() {
            let score = pair_score(&nodes[a], &nodes[b]);
            if score >= LINK_THRESHOLD {
                uf.union(a, b);
                edges.push((a, b, score));
            }
        }
    }

    // Group node indices by union-find root.
    let mut by_root: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..nodes.len() {
        by_root.entry(uf.find(i)).or_default().push(i);
    }

    for (root, members) in by_root {
        if members.len() < 2 {
            // Singleton with profile data → unlinked.
            for &i in &members {
                report.unlinked.push(nodes[i].site.to_owned());
            }
            continue;
        }
        let scores: Vec<f64> = edges
            .iter()
            .filter(|(a, _, _)| uf_root_eq(&mut uf, *a, root))
            .map(|(_, _, s)| *s)
            .collect();
        let confidence = if scores.is_empty() {
            0.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        };
        let mut member_sites: Vec<String> =
            members.iter().map(|&i| nodes[i].site.to_owned()).collect();
        member_sites.sort_unstable();
        report.clusters.push(Cluster {
            members: member_sites,
            confidence,
            shared_name: shared_name(&members, &nodes),
        });
    }

    report.clusters.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.members.cmp(&b.members))
    });
    report.unlinked.sort_unstable();
    report.without_profile.sort_unstable();
    report
}

fn uf_root_eq(uf: &mut UnionFind, node: usize, root: usize) -> bool {
    uf.find(node) == root
}

fn shared_name(members: &[usize], nodes: &[Node<'_>]) -> Option<String> {
    let first = nodes[members[0]].name.clone()?;
    if first.is_empty() {
        return None;
    }
    members
        .iter()
        .all(|&i| nodes[i].name.as_deref() == Some(first.as_str()))
        .then_some(first)
}

fn pair_score(a: &Node<'_>, b: &Node<'_>) -> f64 {
    let mut signals: Vec<f64> = Vec::new();
    if a.name.is_some() && b.name.is_some() {
        let name_sim = if a.name == b.name {
            1.0
        } else {
            jaccard(&a.name_tokens, &b.name_tokens)
        };
        signals.push(name_sim);
    }
    if !a.bio_tokens.is_empty() && !b.bio_tokens.is_empty() {
        signals.push(jaccard(&a.bio_tokens, &b.bio_tokens));
    }
    if signals.is_empty() {
        0.0
    } else {
        signals.iter().sum::<f64>() / signals.len() as f64
    }
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn tokenize(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TOKEN_LEN)
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn found(site: &str, fields: &[(&str, &str)]) -> CheckOutcome {
        let mut enrichment = BTreeMap::new();
        for (k, v) in fields {
            enrichment.insert((*k).to_owned(), (*v).to_owned());
        }
        CheckOutcome {
            site: site.into(),
            url: format!("https://{site}.example/u"),
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 1,
            enrichment,
            evidence: Vec::new(),
            profile_evidence: Vec::new(),
            confidence: crate::ConfidenceScore::default(),
            transport: None,
            escalations: 0,
        }
    }

    #[test]
    fn links_accounts_with_matching_name_and_bio() {
        let outcomes = vec![
            found(
                "GitHub",
                &[("name", "Alice Liddell"), ("bio", "Rust systems hacker")],
            ),
            found(
                "GitLab",
                &[("name", "Alice Liddell"), ("bio", "systems hacker, Rust")],
            ),
        ];
        let report = correlate(&outcomes);
        assert_eq!(report.clusters.len(), 1);
        let c = &report.clusters[0];
        assert_eq!(c.members, ["GitHub", "GitLab"]);
        assert!(c.confidence > 0.7, "confidence {}", c.confidence);
        assert_eq!(c.shared_name.as_deref(), Some("alice liddell"));
    }

    #[test]
    fn does_not_link_different_people_sharing_a_handle() {
        let outcomes = vec![
            found(
                "GitHub",
                &[("name", "Alice Liddell"), ("bio", "Rust systems hacker")],
            ),
            found(
                "Twitch",
                &[("name", "Bob Jones"), ("bio", "pro gamer and streamer")],
            ),
        ];
        let report = correlate(&outcomes);
        assert!(report.clusters.is_empty(), "should not link: {report:?}");
        assert_eq!(report.unlinked.len(), 2);
    }

    #[test]
    fn accounts_without_profile_are_separated() {
        let outcomes = vec![
            found("GitHub", &[("name", "Alice Liddell")]),
            found("Vimeo", &[]),
            found("HackerNews", &[]),
        ];
        let report = correlate(&outcomes);
        assert!(report.clusters.is_empty());
        assert_eq!(report.unlinked, ["GitHub"]);
        assert_eq!(report.without_profile, ["HackerNews", "Vimeo"]);
    }

    #[test]
    fn transitive_links_form_one_cluster() {
        let outcomes = vec![
            found("A", &[("bio", "loves rust and coffee")]),
            found("B", &[("bio", "rust and coffee enthusiast")]),
            found("C", &[("bio", "coffee and rust forever")]),
        ];
        let report = correlate(&outcomes);
        assert_eq!(report.clusters.len(), 1);
        assert_eq!(report.clusters[0].members.len(), 3);
    }

    #[test]
    fn ignores_not_found_outcomes() {
        let mut nf = found("GitLab", &[("name", "Alice Liddell")]);
        nf.kind = MatchKind::NotFound;
        let outcomes = vec![found("GitHub", &[("name", "Alice Liddell")]), nf];
        let report = correlate(&outcomes);
        // Only one Found-with-profile node → no cluster, one unlinked.
        assert!(report.clusters.is_empty());
        assert_eq!(report.unlinked, ["GitHub"]);
    }

    #[test]
    fn jaccard_basics() {
        let a = tokenize("rust and coffee");
        let b = tokenize("rust and tea");
        // tokens (len>=2): {rust, and, coffee} vs {rust, and, tea}
        // inter {rust, and}=2, union 4 → 0.5
        assert!((jaccard(&a, &b) - 0.5).abs() < 1e-9);
    }
}
