//! Stable identity-clustering models built from structured profile evidence.
//!
//! This module intentionally lives alongside the older [`crate::correlate`]
//! report. The legacy report remains useful for compact CLI output, while
//! these serde-compatible types are the foundation for Web, MCP, and future
//! investigation reports.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::check::{CheckOutcome, MatchKind};
use crate::confidence::ConfidenceScore;
use crate::profile::{ProfileEvidence, ProfileEvidenceKind};

const LINK_THRESHOLD: u8 = 60;
const EXTERNAL_LINK_SCORE: u8 = 90;
const AVATAR_URL_SCORE: u8 = 85;
const DISPLAY_NAME_SCORE: u8 = 60;
const BIO_PHRASE_SCORE: u8 = 65;
const LOCATION_SCORE: u8 = 45;
const MAX_CLUSTER_CONFIDENCE: u8 = 95;
const MIN_DISPLAY_NAME_CHARS: usize = 8;
const MIN_DISPLAY_NAME_TOKENS: usize = 2;
const MIN_BIO_PHRASE_TOKENS: usize = 3;

const BIO_STOP_WORDS: &[&str] = &[
    "about", "and", "are", "for", "from", "has", "have", "into", "that", "the", "this", "was",
    "were", "with", "you", "your",
];

/// A positive profile observation suitable for identity-level reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedProfile {
    /// Site name that produced the positive result.
    pub site: String,
    /// Username or handle being investigated.
    pub username: String,
    /// Concrete profile URL that was observed.
    pub url: String,
    /// Structured profile facts extracted from the result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ProfileEvidence>,
    /// Per-profile verdict confidence.
    pub confidence: ConfidenceScore,
    /// Earliest observation timestamp among the profile evidence items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
}

impl ObservedProfile {
    /// Build an observed profile from a `Found` outcome with structured
    /// profile evidence.
    #[must_use]
    pub fn from_outcome(username: &str, outcome: &CheckOutcome) -> Option<Self> {
        if outcome.kind != MatchKind::Found || outcome.profile_evidence.is_empty() {
            return None;
        }

        let observed_at_ms = outcome
            .profile_evidence
            .iter()
            .filter_map(|evidence| evidence.source.observed_at_ms)
            .min();

        Some(Self {
            site: outcome.site.clone(),
            username: username.to_owned(),
            url: outcome.url.clone(),
            evidence: outcome.profile_evidence.clone(),
            confidence: outcome.confidence.clone(),
            observed_at_ms,
        })
    }
}

/// A deterministic candidate group of profiles that likely belong together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityCluster {
    /// Stable deterministic identifier within this clustering result.
    pub id: String,
    /// Observed profiles included in the cluster.
    pub members: Vec<ObservedProfile>,
    /// Cluster-level confidence in `0..=100`, separate from per-profile
    /// verdict confidence.
    pub confidence: u8,
    /// Evidence reasons that linked the cluster members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<ClusterReason>,
    /// Whether the cluster is linked only by weak or ambiguous evidence.
    pub uncertain: bool,
}

/// Deterministic reasons used to link observed profiles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClusterReason {
    /// Profiles share the same normalized display name.
    SharedDisplayName {
        /// Normalized shared display name.
        value: String,
    },
    /// Profiles share a normalized phrase from their biography text.
    SharedBioPhrase {
        /// Shared normalized biography phrase.
        phrase: String,
    },
    /// Profiles share the same normalized external link.
    SharedExternalLink {
        /// Normalized external URL or link value.
        value: String,
    },
    /// Profiles share the same normalized location.
    SharedLocation {
        /// Normalized shared location value.
        value: String,
    },
    /// Profiles share the same normalized avatar URL.
    SharedAvatarUrl {
        /// Normalized avatar URL.
        value: String,
    },
    /// Profiles historically appeared together in repeated scans.
    HistoricalCoOccurrence,
}

/// Build deterministic identity clusters from scan outcomes.
///
/// Only `Found` outcomes with structured `profile_evidence` are considered.
/// The supplied `username` is copied into each observed profile, but username
/// equality is never used as a linking signal.
#[must_use]
pub fn build_identity_clusters(username: &str, outcomes: &[CheckOutcome]) -> Vec<IdentityCluster> {
    let mut profiles: Vec<ObservedProfile> = outcomes
        .iter()
        .filter_map(|outcome| ObservedProfile::from_outcome(username, outcome))
        .collect();

    profiles.sort_by(|left, right| {
        left.site
            .cmp(&right.site)
            .then_with(|| left.url.cmp(&right.url))
    });

    cluster_observed_profiles(&profiles)
}

fn cluster_observed_profiles(profiles: &[ObservedProfile]) -> Vec<IdentityCluster> {
    if profiles.len() < 2 {
        return Vec::new();
    }

    let mut union_find = UnionFind::new(profiles.len());
    let mut links = Vec::new();

    for left in 0..profiles.len() {
        for right in (left + 1)..profiles.len() {
            if let Some(link) = profile_link(left, right, &profiles[left], &profiles[right]) {
                union_find.union(left, right);
                links.push(link);
            }
        }
    }

    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..profiles.len() {
        by_root
            .entry(union_find.find(index))
            .or_default()
            .push(index);
    }

    let mut clusters = Vec::new();
    for member_indices in by_root.values().filter(|members| members.len() >= 2) {
        let members_set: BTreeSet<usize> = member_indices.iter().copied().collect();
        let cluster_links: Vec<&ProfileLink> = links
            .iter()
            .filter(|link| members_set.contains(&link.left) && members_set.contains(&link.right))
            .collect();

        if cluster_links.is_empty() {
            continue;
        }

        let reasons = cluster_reasons(&cluster_links);
        let mut members: Vec<ObservedProfile> = member_indices
            .iter()
            .map(|&index| profiles[index].clone())
            .collect();
        members.sort_by(|left, right| {
            left.site
                .cmp(&right.site)
                .then_with(|| left.url.cmp(&right.url))
        });

        clusters.push(IdentityCluster {
            id: String::new(),
            members,
            confidence: cluster_confidence(&cluster_links),
            reasons,
            uncertain: cluster_links.iter().any(|link| !link.strong),
        });
    }

    clusters.sort_by(|left, right| {
        right
            .confidence
            .cmp(&left.confidence)
            .then_with(|| member_order(left).cmp(member_order(right)))
    });

    for (index, cluster) in clusters.iter_mut().enumerate() {
        cluster.id = format!("identity-{index:04}", index = index + 1);
    }

    clusters
}

fn cluster_reasons(links: &[&ProfileLink]) -> Vec<ClusterReason> {
    links
        .iter()
        .flat_map(|link| link.reasons.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn cluster_confidence(links: &[&ProfileLink]) -> u8 {
    let sum: u32 = links.iter().map(|link| u32::from(link.score)).sum();
    let count = u32::try_from(links.len()).unwrap_or(u32::MAX);
    let rounded = (sum + (count / 2)) / count;
    u8::try_from(rounded).unwrap_or(MAX_CLUSTER_CONFIDENCE)
}

fn member_order(cluster: &IdentityCluster) -> impl Iterator<Item = (&String, &String)> {
    cluster
        .members
        .iter()
        .map(|member| (&member.site, &member.url))
}

#[derive(Debug, Clone)]
struct ProfileLink {
    left: usize,
    right: usize,
    score: u8,
    reasons: Vec<ClusterReason>,
    strong: bool,
}

fn profile_link(
    left: usize,
    right: usize,
    left_profile: &ObservedProfile,
    right_profile: &ObservedProfile,
) -> Option<ProfileLink> {
    let mut signals = Vec::new();

    if let Some(value) = shared_value(
        left_profile,
        right_profile,
        ProfileEvidenceKind::ExternalLink,
        normalize_url,
    ) {
        signals.push(LinkSignal::strong(
            EXTERNAL_LINK_SCORE,
            ClusterReason::SharedExternalLink { value },
        ));
    }

    if let Some(value) = shared_value(
        left_profile,
        right_profile,
        ProfileEvidenceKind::AvatarUrl,
        normalize_url,
    ) {
        signals.push(LinkSignal::strong(
            AVATAR_URL_SCORE,
            ClusterReason::SharedAvatarUrl { value },
        ));
    }

    if let Some(value) = shared_value(
        left_profile,
        right_profile,
        ProfileEvidenceKind::DisplayName,
        normalize_text,
    )
    .filter(|value| conservative_display_name(value))
    {
        signals.push(LinkSignal::weak(
            DISPLAY_NAME_SCORE,
            ClusterReason::SharedDisplayName { value },
        ));
    }

    if let Some(phrase) = shared_bio_phrase(left_profile, right_profile) {
        signals.push(LinkSignal::weak(
            BIO_PHRASE_SCORE,
            ClusterReason::SharedBioPhrase { phrase },
        ));
    }

    if let Some(value) = shared_value(
        left_profile,
        right_profile,
        ProfileEvidenceKind::Location,
        normalize_text,
    ) {
        signals.push(LinkSignal::weak(
            LOCATION_SCORE,
            ClusterReason::SharedLocation { value },
        ));
    }

    if signals.is_empty() {
        return None;
    }

    let strong = signals.iter().any(|signal| signal.strong);
    let score = signals
        .iter()
        .map(|signal| signal.score)
        .fold(0_u8, u8::saturating_add)
        .min(MAX_CLUSTER_CONFIDENCE);
    if score < LINK_THRESHOLD {
        return None;
    }

    Some(ProfileLink {
        left,
        right,
        score,
        reasons: signals.into_iter().map(|signal| signal.reason).collect(),
        strong,
    })
}

#[derive(Debug, Clone)]
struct LinkSignal {
    score: u8,
    reason: ClusterReason,
    strong: bool,
}

impl LinkSignal {
    const fn strong(score: u8, reason: ClusterReason) -> Self {
        Self {
            score,
            reason,
            strong: true,
        }
    }

    const fn weak(score: u8, reason: ClusterReason) -> Self {
        Self {
            score,
            reason,
            strong: false,
        }
    }
}

fn shared_value(
    left_profile: &ObservedProfile,
    right_profile: &ObservedProfile,
    kind: ProfileEvidenceKind,
    normalize: fn(&str) -> String,
) -> Option<String> {
    let left_values = normalized_values(left_profile, kind, normalize);
    let right_values = normalized_values(right_profile, kind, normalize);
    left_values.intersection(&right_values).next().cloned()
}

fn normalized_values(
    profile: &ObservedProfile,
    kind: ProfileEvidenceKind,
    normalize: fn(&str) -> String,
) -> BTreeSet<String> {
    profile
        .evidence
        .iter()
        .filter(|evidence| evidence.kind == kind)
        .map(|evidence| normalize(&evidence.value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn shared_bio_phrase(
    left_profile: &ObservedProfile,
    right_profile: &ObservedProfile,
) -> Option<String> {
    let left_phrases = bio_phrases(left_profile);
    let right_phrases = bio_phrases(right_profile);
    left_phrases.intersection(&right_phrases).next().cloned()
}

fn bio_phrases(profile: &ObservedProfile) -> BTreeSet<String> {
    profile
        .evidence
        .iter()
        .filter(|evidence| evidence.kind == ProfileEvidenceKind::Bio)
        .flat_map(|evidence| phrase_windows(&evidence.value))
        .collect()
}

fn phrase_windows(value: &str) -> BTreeSet<String> {
    let tokens = significant_tokens(value);
    if tokens.len() < MIN_BIO_PHRASE_TOKENS {
        return BTreeSet::new();
    }
    tokens
        .windows(MIN_BIO_PHRASE_TOKENS)
        .map(|window| window.join(" "))
        .collect()
}

fn significant_tokens(value: &str) -> Vec<String> {
    normalize_text(value)
        .split_whitespace()
        .filter(|token| token.chars().count() >= 3)
        .filter(|token| !BIO_STOP_WORDS.contains(token))
        .map(str::to_owned)
        .collect()
}

fn conservative_display_name(value: &str) -> bool {
    value.split_whitespace().count() >= MIN_DISPLAY_NAME_TOKENS
        && value.chars().filter(|ch| ch.is_alphanumeric()).count() >= MIN_DISPLAY_NAME_CHARS
}

fn normalize_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                normalized.push(lower);
            }
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let Ok(parsed) = url::Url::parse(trimmed) else {
        return normalize_text(trimmed);
    };

    let Some(host) = parsed.host_str() else {
        return parsed.to_string().trim_end_matches('/').to_lowercase();
    };

    let scheme = parsed.scheme().to_lowercase();
    let host = host.to_lowercase();
    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = parsed.path().trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    let query = parsed
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();

    format!("{scheme}://{host}{port}{path}{query}")
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn find(&mut self, node: usize) -> usize {
        let mut root = node;
        while self.parent[root] != root {
            root = self.parent[root];
        }

        let mut current = node;
        while self.parent[current] != root {
            let parent = self.parent[current];
            self.parent[current] = root;
            current = parent;
        }

        root
    }

    fn union(&mut self, left: usize, right: usize) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root != right_root {
            self.parent[left_root] = right_root;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{ConfidenceScore, TransportTier};

    fn found(site: &str, fields: &[(&str, &str)]) -> CheckOutcome {
        let url = format!("https://{}.example/alice", site.to_lowercase());
        let profile_evidence = fields
            .iter()
            .map(|(field, value)| ProfileEvidence::from_enrichment(site, &url, field, value))
            .collect();
        let mut outcome = CheckOutcome {
            site: site.to_owned(),
            url,
            kind: MatchKind::Found,
            reason: None,
            elapsed_ms: 10,
            enrichment: BTreeMap::new(),
            evidence: vec!["HTTP 200 (status_found)".to_owned()],
            profile_evidence,
            confidence: ConfidenceScore::default(),
            transport: Some(TransportTier::Http),
            escalations: 0,
        };
        outcome.refresh_confidence();
        outcome
    }

    #[test]
    fn shared_external_link_clusters_profiles() {
        let clusters = build_identity_clusters(
            "alice",
            &[
                found(
                    "GitHub",
                    &[("name", "Alice Example"), ("website", "https://Alice.dev/")],
                ),
                found("GitLab", &[("website", "https://alice.dev")]),
            ],
        );

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].confidence, 90);
        assert!(!clusters[0].uncertain);
        assert_eq!(clusters[0].members[0].site, "GitHub");
        assert_eq!(clusters[0].members[1].site, "GitLab");
        assert!(
            clusters[0]
                .reasons
                .contains(&ClusterReason::SharedExternalLink {
                    value: "https://alice.dev/".to_owned(),
                })
        );
    }

    #[test]
    fn shared_display_name_clusters_only_above_conservative_threshold() {
        let strong_name = build_identity_clusters(
            "alice",
            &[
                found("GitHub", &[("name", "Alice Example")]),
                found("Mastodon", &[("name", "alice example")]),
            ],
        );
        assert_eq!(strong_name.len(), 1);
        assert!(strong_name[0].uncertain);
        assert_eq!(strong_name[0].confidence, 60);
        assert!(
            strong_name[0]
                .reasons
                .contains(&ClusterReason::SharedDisplayName {
                    value: "alice example".to_owned(),
                })
        );

        let weak_name = build_identity_clusters(
            "alice",
            &[
                found("GitHub", &[("name", "Alice")]),
                found("Mastodon", &[("name", "alice")]),
            ],
        );
        assert!(weak_name.is_empty());
    }

    #[test]
    fn username_only_matches_do_not_cluster() {
        let clusters =
            build_identity_clusters("alice", &[found("GitHub", &[]), found("GitLab", &[])]);

        assert!(clusters.is_empty());
    }

    #[test]
    fn username_evidence_only_matches_do_not_cluster() {
        let mut github = found("GitHub", &[]);
        github.profile_evidence = vec![ProfileEvidence::from_signal_username(
            "GitHub",
            &github.url,
            "alice",
            Some(100),
            None,
        )];
        github.refresh_confidence();

        let mut gitlab = found("GitLab", &[]);
        gitlab.profile_evidence = vec![ProfileEvidence::from_signal_username(
            "GitLab",
            &gitlab.url,
            "alice",
            Some(100),
            None,
        )];
        gitlab.refresh_confidence();

        let clusters = build_identity_clusters("alice", &[github, gitlab]);

        assert!(clusters.is_empty());
    }

    #[test]
    fn unrelated_profiles_remain_separate() {
        let clusters = build_identity_clusters(
            "alice",
            &[
                found(
                    "GitHub",
                    &[("name", "Alice Example"), ("website", "https://alice.dev")],
                ),
                found(
                    "Twitch",
                    &[("name", "Bob Example"), ("website", "https://bob.example")],
                ),
            ],
        );

        assert!(clusters.is_empty());
    }

    #[test]
    fn ambiguous_bio_phrase_links_are_marked_uncertain() {
        let clusters = build_identity_clusters(
            "alice",
            &[
                found("GitHub", &[("bio", "Rust systems researcher and builder")]),
                found("GitLab", &[("bio", "Rust systems researcher in Berlin")]),
            ],
        );

        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].uncertain);
        assert_eq!(clusters[0].confidence, 65);
        assert!(
            clusters[0]
                .reasons
                .contains(&ClusterReason::SharedBioPhrase {
                    phrase: "rust systems researcher".to_owned(),
                })
        );
    }

    #[test]
    fn cluster_with_any_weak_edge_is_uncertain() {
        let clusters = build_identity_clusters(
            "alice",
            &[
                found("GitHub", &[("website", "https://alice.dev")]),
                found(
                    "GitLab",
                    &[("website", "https://alice.dev"), ("name", "Alice Example")],
                ),
                found("Mastodon", &[("name", "Alice Example")]),
            ],
        );

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members.len(), 3);
        assert!(clusters[0].uncertain);
        assert!(
            clusters[0]
                .reasons
                .contains(&ClusterReason::SharedExternalLink {
                    value: "https://alice.dev/".to_owned(),
                })
        );
        assert!(
            clusters[0]
                .reasons
                .contains(&ClusterReason::SharedDisplayName {
                    value: "alice example".to_owned(),
                })
        );
    }

    #[test]
    fn observed_profile_uses_earliest_evidence_timestamp() {
        let mut outcome = found("GitHub", &[("name", "Alice Example"), ("bio", "Rust")]);
        outcome.profile_evidence[0].source.observed_at_ms = Some(200);
        outcome.profile_evidence[1].source.observed_at_ms = Some(100);

        let observed = ObservedProfile::from_outcome("alice", &outcome).unwrap();

        assert_eq!(observed.observed_at_ms, Some(100));
        assert_eq!(observed.username, "alice");
    }

    #[test]
    fn not_found_outcomes_are_ignored_even_with_profile_evidence() {
        let mut outcome = found("GitHub", &[("name", "Alice Example")]);
        outcome.kind = MatchKind::NotFound;

        assert!(build_identity_clusters("alice", &[outcome]).is_empty());
    }

    #[test]
    fn cluster_reason_serializes_as_snake_case_tagged_data() {
        let reason = ClusterReason::SharedAvatarUrl {
            value: "https://cdn.example/avatar.png".to_owned(),
        };
        let json = serde_json::to_value(reason).unwrap();

        assert_eq!(json["kind"], "shared_avatar_url");
        assert_eq!(json["value"], "https://cdn.example/avatar.png");
    }
}
