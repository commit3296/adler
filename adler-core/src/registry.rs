//! Site registry — loading, validation, filtering.
//!
//! The default registry is embedded into the binary at compile time via
//! [`include_str!`]. Callers can override it with a file at runtime through
//! [`Registry::load_from_path`].

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::site::Site;

const EMBEDDED_REGISTRY: &str = include_str!("../data/sites.json");

/// A loaded, validated collection of site definitions.
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    sites: Vec<Site>,
}

impl Registry {
    /// Load the default site list embedded into the crate at build time.
    pub fn default_embedded() -> Result<Self> {
        Self::from_json_str(EMBEDDED_REGISTRY)
    }

    /// Parse and validate a registry from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self> {
        let registry: Self = serde_json::from_str(json)?;
        registry.validate()?;
        Ok(registry)
    }

    /// Read a registry from a JSON file.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let json = std::str::from_utf8(&bytes).map_err(|e| Error::InvalidSite {
            reason: format!("registry file is not valid UTF-8: {e}"),
        })?;
        Self::from_json_str(json)
    }

    /// Borrow all sites in load order.
    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    /// Number of sites.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// True if the registry has no sites (always false for a valid load,
    /// since we'd already have rejected it).
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// Apply include/exclude name filters and a tag filter.
    ///
    /// - If `include` is non-empty, only sites whose name contains at least
    ///   one include term (case-insensitive substring) are kept.
    /// - Sites whose name contains any exclude term are dropped.
    /// - If `tags` is non-empty, only sites carrying at least one of the
    ///   requested tags are kept (case-insensitive). A site with no tags is
    ///   therefore dropped by a tag filter — asking for `--tag social` means
    ///   "only social-tagged sites".
    /// - Sites carrying any tag in `exclude_tags` are dropped (e.g.
    ///   `--exclude-tag bot-protected` for a fast clean run).
    /// - **NSFW sites are auto-excluded** (the `nsfw` tag) unless
    ///   `include_nsfw` is `true` or `tags` explicitly asks for `nsfw`.
    ///   This matches Sherlock's `--nsfw` opt-in pattern and prevents
    ///   the default `adler <username>` from surfacing adult-site URLs
    ///   the user didn't ask for.
    /// - Sites are returned by value (cloned) so the result is independent
    ///   of the registry's lifetime — convenient for handing to the executor.
    pub fn filter(
        &self,
        include: &[String],
        exclude: &[String],
        tags: &[String],
        exclude_tags: &[String],
        include_nsfw: bool,
    ) -> Vec<Site> {
        let include: Vec<String> = include.iter().map(|s| s.to_lowercase()).collect();
        let exclude: Vec<String> = exclude.iter().map(|s| s.to_lowercase()).collect();
        let want_tags: Vec<String> = tags.iter().map(|s| s.to_lowercase()).collect();
        let mut drop_tags: Vec<String> = exclude_tags.iter().map(|s| s.to_lowercase()).collect();

        // NSFW gate: auto-exclude unless the caller explicitly opted in,
        // either via `include_nsfw` or by asking for the `nsfw` tag.
        let nsfw_tag = "nsfw".to_owned();
        let asking_for_nsfw = want_tags.contains(&nsfw_tag);
        if !include_nsfw && !asking_for_nsfw && !drop_tags.contains(&nsfw_tag) {
            drop_tags.push(nsfw_tag);
        }

        self.sites
            .iter()
            .filter(|site| {
                let name = site.name.to_lowercase();
                let included = include.is_empty() || include.iter().any(|i| name.contains(i));
                let excluded = exclude.iter().any(|x| name.contains(x));
                let lower_tags: Vec<String> = site.tags.iter().map(|t| t.to_lowercase()).collect();
                let tagged =
                    want_tags.is_empty() || lower_tags.iter().any(|t| want_tags.contains(t));
                let tag_excluded = lower_tags.iter().any(|t| drop_tags.contains(t));
                included && !excluded && tagged && !tag_excluded
            })
            .cloned()
            .collect()
    }

    /// Distinct tags across all sites, sorted, with the count of sites
    /// carrying each. Powers `--list-tags`.
    pub fn tag_counts(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for site in &self.sites {
            for tag in &site.tags {
                *counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
        counts.into_iter().collect()
    }

    fn validate(&self) -> Result<()> {
        if self.sites.is_empty() {
            return Err(Error::InvalidSite {
                reason: "registry has no sites".into(),
            });
        }
        for site in &self.sites {
            site.validate()?;
        }
        let mut seen: HashSet<String> = HashSet::new();
        for site in &self.sites {
            let key = site.name.to_lowercase();
            if !seen.insert(key) {
                return Err(Error::InvalidSite {
                    reason: format!("duplicate site name: {:?}", site.name),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_registry_loads_and_validates() {
        let registry = Registry::default_embedded().expect("embedded registry must load");
        // The registry is imported from Sherlock (~450 sites); a floor well
        // above the old hand-written 15 guards against accidental truncation.
        assert!(
            registry.len() >= 100,
            "imported registry should have ≥100 sites, got {}",
            registry.len()
        );
        // Spot-check a couple of well-known entries.
        let names: Vec<&str> = registry.sites().iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"GitHub"));
        assert!(names.contains(&"HackerNews"));
        assert!(names.contains(&"Reddit"));
    }

    #[test]
    fn rejects_empty_registry() {
        let err = Registry::from_json_str(r#"{ "sites": [] }"#).unwrap_err();
        assert!(matches!(err, Error::InvalidSite { .. }));
    }

    #[test]
    fn rejects_duplicate_site_names() {
        let json = r#"{
            "sites": [
                { "name": "GitHub", "url": "https://github.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "github", "url": "https://github.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] }
            ]
        }"#;
        let err = Registry::from_json_str(json).unwrap_err();
        assert!(matches!(err, Error::InvalidSite { .. }));
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_invalid_site_definition() {
        // Missing {username} placeholder.
        let json = r#"{
            "sites": [
                { "name": "Bad", "url": "https://example.com/",
                  "signals": [{ "kind": "status_found", "codes": [200] }] }
            ]
        }"#;
        assert!(Registry::from_json_str(json).is_err());
    }

    #[test]
    fn rejects_malformed_json() {
        let err = Registry::from_json_str("{").unwrap_err();
        assert!(matches!(err, Error::Json(_)));
    }

    #[test]
    fn filter_include_is_case_insensitive_substring() {
        let registry = Registry::default_embedded().unwrap();
        let only_github = registry.filter(&["github".into()], &[], &[], &[], false);
        assert_eq!(only_github.len(), 1);
        assert_eq!(only_github[0].name, "GitHub");

        let many = registry.filter(&["e".into()], &[], &[], &[], false); // matches anything with "e"
        assert!(many.len() > 1);
    }

    #[test]
    fn filter_exclude_drops_matches() {
        let registry = Registry::default_embedded().unwrap();
        // Include NSFW to keep the test focused on the name-exclude
        // path; the NSFW auto-exclusion is exercised separately.
        let without_github = registry.filter(&[], &["github".into()], &[], &[], true);
        assert!(without_github.iter().all(|s| s.name != "GitHub"));
        assert_eq!(without_github.len(), registry.len() - 1);
    }

    #[test]
    fn filter_include_and_exclude_compose() {
        let registry = Registry::default_embedded().unwrap();
        // Include "git", then exclude "lab" → keep GitHub, drop GitLab.
        let filtered = registry.filter(&["git".into()], &["lab".into()], &[], &[], false);
        let names: Vec<&str> = filtered.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"GitHub"));
        assert!(!names.contains(&"GitLab"));
        // Exclude wins over include for sites containing both terms (none here).
    }

    #[test]
    fn filter_with_no_matches_returns_empty() {
        let registry = Registry::default_embedded().unwrap();
        let filtered = registry.filter(&["does-not-exist-xyz".into()], &[], &[], &[], false);
        assert!(filtered.is_empty());
    }

    fn tagged_registry() -> Registry {
        let json = r#"{
            "sites": [
                { "name": "Soc", "url": "https://soc.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["social", "region:ru"] },
                { "name": "Dev", "url": "https://dev.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["dev"] },
                { "name": "Plain", "url": "https://plain.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] }
            ]
        }"#;
        Registry::from_json_str(json).unwrap()
    }

    #[test]
    fn tag_filter_keeps_only_matching_tags_and_drops_untagged() {
        let r = tagged_registry();
        let social = r.filter(&[], &[], &["social".into()], &[], false);
        let names: Vec<&str> = social.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Soc"], "tag filter should keep only tagged matches");
    }

    #[test]
    fn tag_filter_is_or_within_requested_tags_and_case_insensitive() {
        let r = tagged_registry();
        let either = r.filter(&[], &[], &["DEV".into(), "social".into()], &[], false);
        let names: Vec<&str> = either.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Soc", "Dev"]);
    }

    #[test]
    fn no_tag_filter_includes_untagged_sites() {
        let r = tagged_registry();
        assert_eq!(r.filter(&[], &[], &[], &[], false).len(), 3);
    }

    #[test]
    fn exclude_tag_drops_matching_sites() {
        let r = tagged_registry();
        let kept = r.filter(&[], &[], &[], &["social".into()], false);
        let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
        // Soc carries "social" → dropped; Dev and untagged Plain remain.
        assert_eq!(names, ["Dev", "Plain"], "{names:?}");
    }

    fn nsfw_registry() -> Registry {
        let json = r#"{
            "sites": [
                { "name": "Family", "url": "https://family.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["social"] },
                { "name": "Adult", "url": "https://adult.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["nsfw"] }
            ]
        }"#;
        Registry::from_json_str(json).unwrap()
    }

    #[test]
    fn nsfw_sites_excluded_by_default() {
        let r = nsfw_registry();
        let kept = r.filter(&[], &[], &[], &[], false);
        let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Family"], "nsfw site must be excluded by default");
    }

    #[test]
    fn nsfw_sites_included_when_flag_set() {
        let r = nsfw_registry();
        let kept = r.filter(&[], &[], &[], &[], true);
        assert_eq!(kept.len(), 2, "both sites present with include_nsfw=true");
    }

    #[test]
    fn nsfw_sites_included_when_tag_asked_for_explicitly() {
        // `--tag nsfw` is an explicit opt-in; should bypass the default
        // auto-exclusion even with include_nsfw=false.
        let r = nsfw_registry();
        let kept = r.filter(&[], &[], &["nsfw".into()], &[], false);
        let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Adult"]);
    }

    #[test]
    fn tag_counts_are_sorted_with_per_tag_totals() {
        let r = tagged_registry();
        assert_eq!(
            r.tag_counts(),
            vec![
                ("dev".to_owned(), 1),
                ("region:ru".to_owned(), 1),
                ("social".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn load_from_path_round_trips_via_tempfile() {
        let mut path = std::env::temp_dir();
        path.push(format!("adler-test-registry-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{
                "sites": [
                    { "name": "Mock", "url": "https://example.com/{username}",
                      "signals": [{ "kind": "status_found", "codes": [200] }] }
                ]
            }"#,
        )
        .unwrap();
        let result = Registry::load_from_path(&path);
        let _ = std::fs::remove_file(&path);
        let registry = result.unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.sites()[0].name, "Mock");
    }
}
