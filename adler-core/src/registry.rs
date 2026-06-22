//! Site registry — loading, validation, filtering.
//!
//! The default registry is embedded into the binary at compile time via
//! [`include_str!`]. Callers can override it with a file at runtime through
//! [`Registry::load_from_path`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::site::{Engine, Site};

const EMBEDDED_REGISTRY: &str = include_str!("../data/sites.json");

/// Supplementary registry derived from the `WhatsMyName` project
/// (`WebBreacher/WhatsMyName`, CC BY-SA 4.0). Kept as a separate
/// constant because its data license is incompatible with the
/// MIT-only [`EMBEDDED_REGISTRY`] above; callers opt in explicitly
/// via [`Registry::default_embedded_with_wmn`] to keep the default
/// MIT-clean for downstream redistribution.
const EMBEDDED_WMN_REGISTRY: &str = include_str!("../data/sites_wmn.json");

/// A loaded, validated collection of site definitions.
///
/// Engines (shared signature templates referenced by [`Site::engine`])
/// are resolved into sites at load time — by the time you call
/// [`Registry::sites`] every entry already has its inherited
/// `signals` / `request_headers` / `regex_check` materialised. The original
/// [`Engine`] objects are kept on the registry for re-export and
/// inspection via [`Registry::engines`].
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    #[serde(default)]
    engines: BTreeMap<String, Engine>,
    sites: Vec<Site>,
}

/// Reusable site-filter specification shared by CLI, server, and MCP surfaces.
///
/// Filtering semantics match [`Registry::filter`]: name include/exclude terms
/// are case-insensitive substrings, tag filters are case-insensitive exact
/// matches, disabled sites are always skipped, and `nsfw` sites are hidden
/// unless [`include_nsfw`](Self::include_nsfw) is true or the `nsfw` tag is
/// requested explicitly.
#[derive(Debug, Clone, Default)]
pub struct SiteFilter {
    /// Only keep sites whose name contains at least one term. Empty = no
    /// include filter.
    pub include: Vec<String>,
    /// Drop sites whose name contains any term.
    pub exclude: Vec<String>,
    /// Only keep sites carrying at least one requested tag. Empty = no tag
    /// include filter.
    pub tags: Vec<String>,
    /// Drop sites carrying any of these tags.
    pub exclude_tags: Vec<String>,
    /// Include sites tagged `nsfw`.
    pub include_nsfw: bool,
    /// Optional popularity-rank ceiling (`popularity <= top`). Sites
    /// without a popularity rank are dropped when this is set; returned
    /// sites are sorted by rank.
    pub top: Option<u32>,
}

impl SiteFilter {
    /// Apply this filter to a site slice, returning cloned sites in scan order
    /// (or popularity order when [`top`](Self::top) is set).
    pub fn apply(&self, sites: &[Site]) -> Vec<Site> {
        self.apply_inner(sites, DisabledMode::Exclude)
    }

    /// Apply this filter to a site slice without dropping disabled entries.
    /// Useful for catalogue/diagnostic surfaces that need to explain parked
    /// sites while scan paths continue to call [`apply`](Self::apply).
    pub fn apply_including_disabled(&self, sites: &[Site]) -> Vec<Site> {
        self.apply_inner(sites, DisabledMode::Include)
    }

    fn apply_inner(&self, sites: &[Site], disabled_mode: DisabledMode) -> Vec<Site> {
        let include: Vec<String> = self.include.iter().map(|s| s.to_lowercase()).collect();
        let exclude: Vec<String> = self.exclude.iter().map(|s| s.to_lowercase()).collect();
        let want_tags: Vec<String> = self.tags.iter().map(|s| s.to_lowercase()).collect();
        let mut drop_tags: Vec<String> =
            self.exclude_tags.iter().map(|s| s.to_lowercase()).collect();

        // NSFW gate: auto-exclude unless the caller explicitly opted in,
        // either via `include_nsfw` or by asking for the `nsfw` tag.
        let nsfw_tag = "nsfw".to_owned();
        let asking_for_nsfw = want_tags.contains(&nsfw_tag);
        if !self.include_nsfw && !asking_for_nsfw && !drop_tags.contains(&nsfw_tag) {
            drop_tags.push(nsfw_tag);
        }

        let mut filtered: Vec<Site> = sites
            .iter()
            .filter(|site| {
                match disabled_mode {
                    DisabledMode::Exclude if site.disabled => {
                        // Disabled sites are skipped unconditionally in
                        // the scan view — the bool is meant for parking
                        // known-broken entries with a reason comment
                        // instead of deleting them.
                        return false;
                    }
                    DisabledMode::Only if !site.disabled => return false,
                    DisabledMode::Exclude | DisabledMode::Only | DisabledMode::Include => {}
                }
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
            .collect();

        if let Some(n) = self.top {
            filtered.retain(|s| s.popularity.is_some_and(|p| p <= n));
            filtered.sort_by_key(|s| s.popularity.unwrap_or(u32::MAX));
        }
        filtered
    }
}

#[derive(Debug, Clone, Copy)]
enum DisabledMode {
    Exclude,
    Only,
    Include,
}

impl Registry {
    /// Load the default site list embedded into the crate at build time.
    pub fn default_embedded() -> Result<Self> {
        Self::from_json_str(EMBEDDED_REGISTRY)
    }

    /// Load the default site list *plus* the `WhatsMyName`-derived
    /// supplementary set. `WhatsMyName` data is licensed CC BY-SA 4.0
    /// (see `LICENSE-CC-BY-SA-4.0` at the repo root); enabling this
    /// path means downstream redistribution of the merged scan data
    /// must respect the `ShareAlike` obligation. Sites contributed by
    /// the `WhatsMyName` tranche carry the `source:wmn` tag for
    /// provenance.
    ///
    /// Engines from the WMN tranche merge with the MIT tranche;
    /// case-insensitive site-name collisions resolve in favour of the
    /// MIT-tranche entry (the hand-curated Sherlock/Maigret-derived
    /// signature wins; the WMN duplicate is dropped). Returns an
    /// error only if either tranche fails its own validation —
    /// engine references are checked across the merged set.
    pub fn default_embedded_with_wmn() -> Result<Self> {
        let mut base = Self::default_embedded()?;
        let wmn: Self = serde_json::from_str(EMBEDDED_WMN_REGISTRY)?;
        let existing_names: HashSet<String> =
            base.sites.iter().map(|s| s.name.to_lowercase()).collect();
        // URL-claim only counts enabled base entries — the dedup pattern
        // keeps disabled siblings at the canonical's URL, and a WMN
        // entry colliding with one of *those* is no worse than colliding
        // with the canonical.
        let claimed_urls: HashSet<String> = base
            .sites
            .iter()
            .filter(|s| !s.disabled)
            .map(|s| s.url.as_str().to_owned())
            .collect();
        for (name, engine) in wmn.engines {
            base.engines.entry(name).or_insert(engine);
        }
        for site in wmn.sites {
            if existing_names.contains(&site.name.to_lowercase()) {
                continue;
            }
            if !site.disabled && claimed_urls.contains(site.url.as_str()) {
                // Base already has an enabled site at this URL; WMN's
                // version would just produce a doubled probe, and
                // validate() would refuse the merged registry. Drop the
                // WMN entry; base canonical wins (same precedence rule
                // we apply for name collisions).
                continue;
            }
            base.sites.push(site);
        }
        base.resolve_engines()?;
        base.validate()?;
        Ok(base)
    }

    /// Parse and validate a registry from a JSON string. Engine
    /// references on each site are resolved before validation;
    /// a site that names an engine which doesn't exist in the
    /// `engines` block fails loading with [`Error::InvalidSite`].
    pub fn from_json_str(json: &str) -> Result<Self> {
        let mut registry: Self = serde_json::from_str(json)?;
        registry.resolve_engines()?;
        registry.apply_tag_derived_policy();
        registry.validate()?;
        Ok(registry)
    }

    /// Inheritable engine templates, keyed by name. Useful for
    /// introspection and for serialising the registry back out;
    /// detection paths read the resolved fields off the sites
    /// directly and don't need to consult this map.
    pub fn engines(&self) -> &BTreeMap<String, Engine> {
        &self.engines
    }

    /// Walk every site's tags for `region:XX` markers and fill
    /// [`AccessPolicy::prefer_geo`](crate::AccessPolicy::prefer_geo)
    /// with the matching country codes. **Soft** routing only — a
    /// site declaring `region:ru` *prefers* a Russian egress when one
    /// is configured but still works from anywhere else; the router
    /// falls back to the default egress on no match rather than
    /// reporting `Uncertain(GeoUnavailable)`.
    ///
    /// Skips sites that already declare a hard
    /// [`AccessPolicy::geo`](crate::AccessPolicy::geo) — explicit
    /// policy wins on conflict, same convention as engine inheritance.
    /// Existing `prefer_geo` entries are also preserved: tag-derived
    /// codes are *added*, not replaced, so a hand-tuned policy can
    /// stack on top of the tag. Invalid country codes (`region:xx` is
    /// not exactly two ASCII letters) are silently skipped — they
    /// shouldn't exist in the registry, and a parse error here would
    /// break the load for a tag the scanner already ignores in
    /// every other context.
    fn apply_tag_derived_policy(&mut self) {
        for site in &mut self.sites {
            if !site.access.geo.is_empty() {
                continue;
            }
            for tag in &site.tags {
                let Some(rest) = tag.strip_prefix("region:") else {
                    continue;
                };
                let Some(cc) = crate::access::CountryCode::new(rest) else {
                    continue;
                };
                if !site.access.prefer_geo.contains(&cc) {
                    site.access.prefer_geo.push(cc);
                }
            }
        }
    }

    /// Merge each engine's inheritable fields into the sites that
    /// reference it. After this call every site's `signals`,
    /// `request_headers` and `regex_check` reflect the effective
    /// values used by the scanner.
    ///
    /// Per-site fields are authoritative: anything declared
    /// explicitly on a site wins on conflict; only empty / unset
    /// fields are filled from the engine.
    fn resolve_engines(&mut self) -> Result<()> {
        for (name, engine) in &self.engines {
            engine.validate(name)?;
        }
        for site in &mut self.sites {
            let Some(name) = &site.engine else {
                continue;
            };
            let Some(engine) = self.engines.get(name) else {
                return Err(Error::InvalidSite {
                    reason: format!(
                        "site {:?}: references engine {name:?} which is not defined",
                        site.name
                    ),
                });
            };
            engine.merge_into(site);
        }
        Ok(())
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
        self.filter_with(&SiteFilter {
            include: include.to_vec(),
            exclude: exclude.to_vec(),
            tags: tags.to_vec(),
            exclude_tags: exclude_tags.to_vec(),
            include_nsfw,
            top: None,
        })
    }

    /// Apply a reusable [`SiteFilter`] to this registry.
    pub fn filter_with(&self, filter: &SiteFilter) -> Vec<Site> {
        filter.apply(&self.sites)
    }

    /// Apply a reusable [`SiteFilter`] without dropping disabled entries.
    /// This is the catalogue view: scans still call
    /// [`filter_with`](Self::filter_with), while UI/agent surfaces can keep
    /// parked entries visible with their reasons.
    pub fn matches_with(&self, filter: &SiteFilter) -> Vec<Site> {
        filter.apply_including_disabled(&self.sites)
    }

    /// Apply a reusable [`SiteFilter`] but return only disabled/parked
    /// entries. This is for diagnostics: scan surfaces still call
    /// [`filter_with`](Self::filter_with), while CLIs and UIs can use this
    /// to explain why an otherwise matching site is unavailable.
    pub fn disabled_matches_with(&self, filter: &SiteFilter) -> Vec<Site> {
        filter.apply_inner(&self.sites, DisabledMode::Only)
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
        // (URL, signals) uniqueness among ENABLED sites: each
        // (URL template, signal-set) pair should back exactly one live
        // entry. Disabled entries can legitimately share URLs with
        // their canonicals — that's how the `duplicate of <canonical>`
        // dedup pattern works. A second enabled hit at the same URL
        // *and* the same signal array is almost always an importer
        // re-introducing a known duplicate
        // (Sherlock/Maigret/WhatsMyName each name the same site
        // slightly differently); the doctor would otherwise
        // double-probe the URL for an identical verdict.
        //
        // Same URL with *distinct* signals is the legitimate-alias
        // shape — WordPress.com (Public/Private/Deleted) hit the same
        // API endpoint and disambiguate via their `body_present`
        // marker, which the doctor reads as three independent verdicts.
        let mut seen_url_sig: HashMap<(String, String), &str> = HashMap::new();
        for site in &self.sites {
            if site.disabled {
                continue;
            }
            // `serde_json` for the signal key gives a canonical
            // serialisation that doesn't depend on field-order or
            // `Debug` formatting, both of which could shift between
            // Rust releases or after a `#[derive(Debug)]` rearrange.
            // serde_json is already a workspace dep; the cost is
            // ~one allocation per enabled site at load time.
            let sigs_key = serde_json::to_string(&site.signals)
                .expect("Signal derives Serialize and contains no Map<_, _> with non-string keys");
            let key = (site.url.as_str().to_owned(), sigs_key);
            if let Some(prev) = seen_url_sig.insert(key, site.name.as_str()) {
                return Err(Error::InvalidSite {
                    reason: format!(
                        "duplicate (URL, signals) among enabled sites: {:?} and {:?} both back \
                         {:?} with identical signals. Mark one `disabled: true` with \
                         `disabled_reason: \"duplicate of {prev}\"` (or, if the two entries are \
                         supposed to disambiguate via different markers, give each a distinct \
                         signal set).",
                        prev,
                        site.name,
                        site.url.as_str(),
                    ),
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
        // Spot-check a couple of well-known entries. (HackerNews used
        // to be here but was pruned 2026-05-26 — its Sherlock-side
        // known_present went stale and the imported signature
        // doctor-failed; can be restored via OVERRIDES in
        // import_sherlock.py with a working account.)
        let names: Vec<&str> = registry.sites().iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"GitHub"));
        assert!(names.contains(&"Reddit"));
        assert!(names.contains(&"Telegram"));
    }

    #[test]
    fn wmn_embedded_registry_loads_and_supersets_default() {
        let base = Registry::default_embedded().unwrap();
        let merged = Registry::default_embedded_with_wmn().expect("WMN-merged registry must load");
        assert!(
            merged.len() > base.len(),
            "WMN merge must add sites: base={} merged={}",
            base.len(),
            merged.len()
        );
        // Every base-tranche name survives the merge; case-insensitive
        // collisions resolve in favour of the MIT-tranche entry.
        let merged_names: HashSet<String> = merged
            .sites()
            .iter()
            .map(|s| s.name.to_lowercase())
            .collect();
        for s in base.sites() {
            assert!(
                merged_names.contains(&s.name.to_lowercase()),
                "merge dropped base-tranche site {:?}",
                s.name
            );
        }
        // At least one WMN-only site carries the provenance tag.
        let has_wmn_tag = merged
            .sites()
            .iter()
            .any(|s| s.tags.iter().any(|t| t == "source:wmn"));
        assert!(has_wmn_tag, "no site carries the source:wmn tag");
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
    fn rejects_duplicate_enabled_urls() {
        // Two enabled sites at the same URL is almost always an importer
        // re-introducing a known duplicate. Reject at load time with a
        // message naming both entries.
        let json = r#"{
            "sites": [
                { "name": "Hub Code", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "HubCode", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] }
            ]
        }"#;
        let err = Registry::from_json_str(json).unwrap_err();
        assert!(matches!(err, Error::InvalidSite { .. }));
        let msg = err.to_string();
        assert!(msg.contains("duplicate (URL, signals)"), "msg: {msg}");
        assert!(msg.contains("Hub Code"), "msg: {msg}");
        assert!(msg.contains("HubCode"), "msg: {msg}");
    }

    #[test]
    fn allows_duplicate_urls_with_distinct_signals() {
        // Same URL, distinct signal sets — this is the legitimate-alias
        // shape (e.g. WordPress.com (Public/Private/Deleted) hit one
        // endpoint and disambiguate via the body marker). Must NOT
        // trigger the URL-uniqueness rule.
        let json = r#"{
            "sites": [
                { "name": "Site Public", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "Site Private", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [403] }] }
            ]
        }"#;
        let registry = Registry::from_json_str(json).expect("distinct-signal alias must validate");
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn allows_duplicate_urls_when_one_side_is_disabled() {
        // The dedup pattern that the v0.14 hygiene pass established:
        // canonical stays enabled, the surplus entry gets
        // `disabled: true` + `disabled_reason: "duplicate of <canonical>"`.
        // This shape must continue loading cleanly.
        let json = r#"{
            "sites": [
                { "name": "Hub Code", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "HubCode", "url": "https://example.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "disabled": true,
                  "disabled_reason": "duplicate of Hub Code" }
            ]
        }"#;
        let registry = Registry::from_json_str(json).expect("dedup pattern must validate");
        assert_eq!(registry.len(), 2);
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
        let baseline = registry.filter(&[], &[], &[], &[], true);
        let without_github = registry.filter(&[], &["github".into()], &[], &[], true);
        assert!(without_github.iter().all(|s| s.name != "GitHub"));
        // Asserting against the baseline (filtered) count rather than
        // `registry.len()` so this test is robust to changes in the
        // disabled-site count — `len()` includes disabled entries,
        // `filter()` does not.
        assert_eq!(without_github.len(), baseline.len() - 1);
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

    #[test]
    fn disabled_sites_are_skipped_by_filter() {
        let json = r#"{
            "sites": [
                { "name": "Alive", "url": "https://alive.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "Parked", "url": "https://parked.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "disabled": true }
            ]
        }"#;
        let registry = Registry::from_json_str(json).unwrap();
        // sites() returns everything including disabled — it's the
        // serialisation view. filter() is the scan view and drops
        // disabled entries.
        assert_eq!(registry.sites().len(), 2);
        let scanned = registry.filter(&[], &[], &[], &[], false);
        let names: Vec<&str> = scanned.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Alive"]);
    }

    #[test]
    fn disabled_matches_with_explains_parked_filter_hits() {
        let json = r#"{
            "sites": [
                { "name": "Alive", "url": "https://alive.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }] },
                { "name": "Parked Social", "url": "https://parked.example/@{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "disabled": true,
                  "disabled_reason": "Honest Limits: parked",
                  "tags": ["social"] }
            ]
        }"#;
        let registry = Registry::from_json_str(json).unwrap();
        let filter = SiteFilter {
            include: vec!["parked".into()],
            tags: vec!["social".into()],
            ..SiteFilter::default()
        };

        assert!(registry.filter_with(&filter).is_empty());
        let disabled = registry.disabled_matches_with(&filter);
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].name, "Parked Social");
        assert_eq!(
            disabled[0].disabled_reason.as_deref(),
            Some("Honest Limits: parked")
        );
    }

    #[test]
    fn parked_login_wall_entries_declare_user_auth_protection() {
        let registry = Registry::default_embedded().unwrap();
        for name in ["Facebook", "Threads"] {
            let site = registry
                .sites()
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| {
                    panic!("{name} entry should document the login-wall limitation")
                });

            assert!(site.disabled, "{name} must not be probed by default");
            let reason = site
                .disabled_reason
                .as_deref()
                .unwrap_or_else(|| panic!("disabled {name} entry should explain why it is parked"));
            assert!(
                reason.contains("Honest Limits") && reason.contains("login wall"),
                "unexpected {name} disabled_reason: {reason}"
            );
            assert!(
                site.protection
                    .iter()
                    .any(|p| matches!(p, super::super::site::ProtectionKind::UserAuth)),
                "{name} login-wall entry should declare user-auth protection"
            );

            let scanned = registry.filter(&[name.to_ascii_lowercase()], &[], &[], &[], true);
            assert!(
                scanned.is_empty(),
                "disabled {name} entry must not leak into scan filters"
            );
        }
    }

    #[test]
    fn reddit_uses_oauth_endpoint_and_requires_session() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let reddit_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Reddit")
            .collect();

        assert_eq!(
            reddit_entries.len(),
            1,
            "WMN merge must not reintroduce a second Reddit probe"
        );
        let reddit = reddit_entries[0];
        assert!(!reddit.disabled, "Reddit OAuth probe should remain enabled");
        assert_eq!(
            reddit.url.as_str(),
            "https://oauth.reddit.com/user/{username}/about"
        );
        assert_eq!(reddit.access.session.as_deref(), Some("reddit"));
        assert!(
            reddit
                .protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::UserAuth)),
            "Reddit should be classified as requiring user auth"
        );
        assert!(
            reddit.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::JsonUsername { pointer }
                    if pointer == "/data/name"
            )),
            "Reddit should expose exact username evidence from OAuth JSON"
        );
        assert!(
            reddit.tags.iter().any(|t| t == "reddit-oauth"),
            "Reddit should be discoverable as an OAuth-gated site"
        );
        assert!(
            reddit
                .tags
                .iter()
                .all(|t| !t.eq_ignore_ascii_case("bot-protected")),
            "Reddit OAuth should use HTTP session headers, not browser routing"
        );

        let scanned = registry.filter(&["reddit".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "Reddit").count(),
            1,
            "enabled Reddit OAuth entry should be scan-filterable"
        );
    }

    #[test]
    fn tiktok_uses_public_oembed_endpoint() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let tiktok_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "TikTok")
            .collect();

        assert_eq!(
            tiktok_entries.len(),
            1,
            "WMN merge must keep one TikTok probe"
        );
        let tiktok = tiktok_entries[0];
        assert!(
            !tiktok.disabled,
            "TikTok's public oEmbed endpoint is probeable by default"
        );
        assert!(
            tiktok
                .url
                .as_str()
                .contains("/oembed?url=https://www.tiktok.com/@{username}"),
            "TikTok should use the public oEmbed endpoint, not the JS-only profile shell"
        );
        assert!(
            tiktok.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyUsername { text }
                    if text == "\"embed_product_id\":\"{username}\""
            )),
            "TikTok should expose exact username evidence from oEmbed"
        );
        assert!(
            tiktok.protection.is_empty(),
            "oEmbed path should not require browser/captcha routing"
        );

        let scanned = registry.filter(&["tiktok".into()], &[], &[], &[], true);
        assert!(
            scanned.iter().any(|s| s.name == "TikTok"),
            "enabled TikTok oEmbed entry should be scan-filterable"
        );
    }

    #[test]
    fn pinterest_uses_oembed_instead_of_js_shell() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let pinterest_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Pinterest")
            .collect();

        assert_eq!(
            pinterest_entries.len(),
            1,
            "WMN merge must not reintroduce Pinterest's canonical JS shell"
        );
        let pinterest = pinterest_entries[0];
        assert!(
            !pinterest.disabled,
            "Pinterest oEmbed probe should remain enabled"
        );
        assert!(
            pinterest.url.as_str().contains("/oembed.json"),
            "Pinterest should use the oEmbed endpoint, got {}",
            pinterest.url.as_str()
        );
        assert!(
            pinterest.url.as_str() != "https://www.pinterest.com/{username}/",
            "Pinterest must not fall back to the canonical JS shell"
        );
        assert!(
            pinterest.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyUsername { text }
                    if text == "\"author_url\":\"https://www.pinterest.com/{username}/\""
            )),
            "Pinterest should expose exact username evidence from oEmbed"
        );

        let scanned = registry.filter(&["pinterest".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "Pinterest").count(),
            1,
            "enabled Pinterest oEmbed entry should be scan-filterable"
        );
    }

    #[test]
    fn patreon_remains_status_only_until_stable_metadata_signal_exists() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let patreon_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Patreon")
            .collect();

        assert_eq!(
            patreon_entries.len(),
            1,
            "WMN merge must not reintroduce a second Patreon probe"
        );
        let patreon = patreon_entries[0];
        assert!(
            !patreon.disabled,
            "Patreon status probe should remain enabled"
        );
        assert_eq!(patreon.url.as_str(), "https://www.patreon.com/{username}");
        assert!(
            patreon.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::StatusFound { codes } if codes == &[200]
            )),
            "Patreon should retain its live-verified HTTP 200 found signal"
        );
        assert!(
            patreon.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::StatusNotFound { codes } if codes == &[404]
            )),
            "Patreon should retain its live-verified HTTP 404 missing-user signal"
        );
        assert!(
            patreon.signals.iter().all(|signal| !matches!(
                signal,
                super::super::site::Signal::BodyUsername { .. }
                    | super::super::site::Signal::JsonUsername { .. }
            )),
            "Patreon must not infer exact username evidence from the generic HTML shell"
        );
        assert!(
            patreon.tags.iter().any(|t| t == "bot-protected"),
            "Patreon should stay classified as a bot-protected profile surface"
        );

        let scanned = registry.filter(&["patreon".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "Patreon").count(),
            1,
            "enabled Patreon status probe should be scan-filterable"
        );
    }

    #[test]
    fn stackoverflow_uses_api_with_exact_username_signal() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let stackoverflow_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "StackOverflow")
            .collect();

        assert_eq!(
            stackoverflow_entries.len(),
            1,
            "WMN merge must keep one StackOverflow probe"
        );
        let stackoverflow = stackoverflow_entries[0];
        assert!(
            !stackoverflow.disabled,
            "StackOverflow API probe should remain enabled"
        );
        assert!(
            stackoverflow
                .url
                .as_str()
                .starts_with("https://api.stackexchange.com/2.3/users?"),
            "StackOverflow should use the StackExchange API, got {}",
            stackoverflow.url.as_str()
        );
        assert!(
            stackoverflow.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyUsername { text }
                    if text == "\"display_name\":\"{username}\""
            )),
            "StackOverflow should require exact display_name username evidence"
        );
        assert!(
            stackoverflow.signals.iter().all(|signal| !matches!(
                signal,
                super::super::site::Signal::StatusFound { .. }
                    | super::super::site::Signal::BodyPresent { .. }
            )),
            "StackOverflow must not infer Found from HTTP 200 or non-empty partial search results"
        );

        let scanned = registry.filter(&["stackoverflow".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "StackOverflow").count(),
            1,
            "enabled StackOverflow API entry should be scan-filterable"
        );
    }

    #[test]
    fn pypi_uses_strict_profile_marker_not_status_only() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let pypi_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "pypi")
            .collect();

        assert_eq!(pypi_entries.len(), 1, "WMN merge must keep one PyPI probe");
        let pypi = pypi_entries[0];
        assert!(!pypi.disabled, "PyPI profile probe should remain enabled");
        assert_eq!(pypi.url.as_str(), "https://pypi.org/user/{username}/");
        assert!(
            pypi.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyUsername { text }
                    if text == "Profile of {username}"
            )),
            "PyPI should require an exact profile username marker"
        );
        assert!(
            pypi.signals.iter().all(|signal| !matches!(
                signal,
                super::super::site::Signal::StatusFound { .. }
                    | super::super::site::Signal::BodyPresent { .. }
            )),
            "PyPI must not infer Found from HTTP 200 or a generic profile shell"
        );
        assert!(
            pypi.tags.iter().any(|tag| tag == "bot-protected")
                && pypi.tags.iter().any(|tag| tag == "protection:other"),
            "PyPI should document its current JS challenge surface"
        );
        assert!(
            pypi.protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::ClientChallenge)),
            "PyPI should expose its browser client challenge as structured protection"
        );

        let scanned = registry.filter(&["pypi".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "pypi").count(),
            1,
            "enabled PyPI entry should be scan-filterable"
        );
    }

    #[test]
    fn npm_uses_registry_search_with_exact_maintainer_signal() {
        for (label, registry) in [
            ("default", Registry::default_embedded().unwrap()),
            (
                "default+wmn",
                Registry::default_embedded_with_wmn().unwrap(),
            ),
        ] {
            let npm_entries: Vec<&Site> = registry
                .sites()
                .iter()
                .filter(|s| s.name == "npm")
                .collect();

            assert_eq!(npm_entries.len(), 1, "{label} should keep one npm probe");
            let npm = npm_entries[0];
            assert!(!npm.disabled, "{label} npm probe should remain enabled");
            assert_eq!(
                npm.url.as_str(),
                "https://registry.npmjs.org/-/v1/search?text=maintainer%3A{username}&size=3"
            );
            assert!(
                npm.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyUsername { text }
                        if text == "\"username\":\"{username}\""
                )),
                "{label} npm should require exact maintainer username evidence"
            );
            assert!(
                npm.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyAbsent { text } if text == "\"total\":0"
                )),
                "{label} npm should classify an empty registry-search result as NotFound"
            );
            assert!(
                npm.signals.iter().all(|signal| !matches!(
                    signal,
                    super::super::site::Signal::StatusFound { .. }
                        | super::super::site::Signal::StatusNotFound { .. }
                        | super::super::site::Signal::BodyPresent { .. }
                )),
                "{label} npm must not use status-only or generic body markers"
            );
            assert!(
                npm.tags.iter().any(|tag| tag == "api")
                    && npm.tags.iter().all(|tag| tag != "bot-protected"),
                "{label} npm registry-search path should stay on the raw HTTP API path"
            );
        }
    }

    #[test]
    fn replit_requires_session_and_exact_username_marker() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let replit_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Replit")
            .collect();

        assert_eq!(
            replit_entries.len(),
            1,
            "WMN merge must keep one Replit probe"
        );
        let replit = replit_entries[0];
        assert!(
            !replit.disabled,
            "Replit profile probe should remain enabled"
        );
        assert_eq!(replit.url.as_str(), "https://replit.com/@{username}");
        assert_eq!(replit.access.session.as_deref(), Some("replit"));
        assert!(
            replit
                .protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::UserAuth)),
            "Replit should document that profile probes need user auth"
        );
        assert!(
            replit.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyUsername { text }
                    if text == "\"username\":\"{username}\""
            )),
            "Replit should require exact username evidence when a session is supplied"
        );
        assert!(
            replit.signals.iter().all(|signal| !matches!(
                signal,
                super::super::site::Signal::StatusFound { .. }
                    | super::super::site::Signal::BodyPresent { .. }
            )),
            "Replit must not infer Found from HTTP 200 or a generic login/user shell"
        );

        let scanned = registry.filter(&["replit".into()], &[], &[], &[], true);
        assert_eq!(
            scanned.iter().filter(|s| s.name == "Replit").count(),
            1,
            "enabled Replit entry should be scan-filterable"
        );
    }

    #[test]
    fn weibo_requires_session_and_has_no_embedded_cookie() {
        for (label, registry) in [
            ("default", Registry::default_embedded().unwrap()),
            (
                "default+wmn",
                Registry::default_embedded_with_wmn().unwrap(),
            ),
        ] {
            let weibo_entries: Vec<&Site> = registry
                .sites()
                .iter()
                .filter(|s| s.name == "Weibo")
                .collect();

            assert_eq!(
                weibo_entries.len(),
                1,
                "{label} should keep one Weibo probe"
            );
            let weibo = weibo_entries[0];
            assert!(!weibo.disabled, "{label} Weibo probe should remain enabled");
            assert_eq!(
                weibo.url.as_str(),
                "https://weibo.com/ajax/profile/info?custom={username}"
            );
            assert_eq!(weibo.access.session.as_deref(), Some("weibo"));
            assert!(
                weibo
                    .protection
                    .iter()
                    .any(|p| matches!(p, super::super::site::ProtectionKind::UserAuth)),
                "{label} Weibo should document that profile probes need user auth"
            );
            assert!(
                weibo
                    .request_headers
                    .keys()
                    .all(|key| !key.eq_ignore_ascii_case("cookie")),
                "{label} Weibo must not embed operator cookies"
            );
            assert!(
                weibo.signals.iter().all(|signal| !matches!(
                    signal,
                    super::super::site::Signal::StatusFound { .. }
                )),
                "{label} Weibo must not infer Found from HTTP 200"
            );
            assert!(
                weibo.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyPresent { text } if text == "\"user\":"
                )),
                "{label} Weibo should require the authenticated profile API user object"
            );
        }
    }

    #[test]
    fn x_and_twitter_use_username_availability_reason_without_status_200_conflict() {
        let registry = Registry::default_embedded_with_wmn().unwrap();

        for (name, url, found_marker, absent_marker) in [
            (
                "X",
                "https://api.x.com/i/users/username_available.json?username={username}",
                "\"reason\":\"taken\"",
                "\"reason\":\"available\"",
            ),
            (
                "Twitter",
                "https://api.x.com/i/users/username_available.json?username={username}&suggest=0",
                "\"valid\":false,\"reason\":\"taken\"",
                "\"valid\":true,\"reason\":\"available\"",
            ),
        ] {
            let entries: Vec<&Site> = registry.sites().iter().filter(|s| s.name == name).collect();

            assert_eq!(entries.len(), 1, "registry must keep one {name} probe");
            let site = entries[0];
            assert!(
                !site.disabled,
                "{name} username availability API probe should remain enabled"
            );
            assert_eq!(
                site.url.as_str(),
                url,
                "{name} should use the X username availability API"
            );
            assert!(
                site.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyPresent { text } if text == found_marker
                )),
                "{name} should report Found only when the API says the username is taken"
            );
            assert!(
                site.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyAbsent { text } if text == absent_marker
                )),
                "{name} should report NotFound only when the API says the username is available"
            );
            assert!(
                site.signals.iter().all(|signal| !matches!(
                    signal,
                    super::super::site::Signal::StatusFound { .. }
                        | super::super::site::Signal::StatusNotFound { .. }
                )),
                "{name} must not use HTTP 200 as both Found and NotFound"
            );
            assert!(
                site.tags.iter().any(|tag| tag == "api")
                    && site
                        .tags
                        .iter()
                        .all(|tag| tag != crate::client::BOT_PROTECTED_TAG),
                "{name} should stay on the raw HTTP API path"
            );
        }
    }

    #[test]
    fn instagram_uses_web_profile_info_api_with_exact_username_marker() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let instagram_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Instagram")
            .collect();

        assert_eq!(
            instagram_entries.len(),
            1,
            "WMN merge must keep one Instagram probe"
        );
        let instagram = instagram_entries[0];
        assert!(
            !instagram.disabled,
            "Instagram API probe should remain enabled"
        );
        assert_eq!(
            instagram.url.as_str(),
            "https://www.instagram.com/api/v1/users/web_profile_info/?username={username}"
        );
        assert_eq!(
            instagram
                .request_headers
                .get("X-IG-App-ID")
                .map(String::as_str),
            Some("936619743392459"),
            "Instagram web_profile_info requires the public app id header"
        );
        assert_eq!(
            instagram.access.session.as_deref(),
            Some("instagram"),
            "Instagram API should be operator-session gated to avoid unstable unauthenticated 401s"
        );
        assert!(
            instagram
                .protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::UserAuth)),
            "Instagram should document its user-auth requirement"
        );
        assert!(
            instagram.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::JsonUsername { pointer }
                    if pointer == "/data/user/username"
            )),
            "Instagram should require exact username evidence from top-level API JSON"
        );
        assert!(
            instagram.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::StatusNotFound { codes } if codes == &[404]
            )),
            "Instagram API should classify HTTP 404 as NotFound"
        );
        assert!(
            instagram.signals.iter().all(|signal| !matches!(
                signal,
                super::super::site::Signal::StatusFound { .. }
                    | super::super::site::Signal::BodyPresent { .. }
                    | super::super::site::Signal::BodyUsername { .. }
                    | super::super::site::Signal::BodyAbsent { .. }
            )),
            "Instagram must not infer Found or NotFound from the generic HTML shell"
        );
        assert!(
            instagram.tags.iter().any(|tag| tag == "api")
                && instagram
                    .tags
                    .iter()
                    .all(|tag| tag != crate::client::BOT_PROTECTED_TAG),
            "Instagram web_profile_info should stay on the raw HTTP session API path"
        );
    }

    #[test]
    fn vk_uses_exact_canonical_marker_without_redirect_conflict() {
        for (label, registry) in [
            ("default", Registry::default_embedded().unwrap()),
            (
                "default+wmn",
                Registry::default_embedded_with_wmn().unwrap(),
            ),
        ] {
            let vk_entries: Vec<&Site> =
                registry.sites().iter().filter(|s| s.name == "VK").collect();

            assert_eq!(vk_entries.len(), 1, "{label} should keep one VK probe");
            let vk = vk_entries[0];
            assert!(!vk.disabled, "{label} VK probe should remain enabled");
            assert_eq!(vk.url.as_str(), "https://vk.com/{username}");
            assert!(
                vk.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::BodyUsername { text }
                        if text == "data-canonical=\"https://vk.com/{username}\""
                )),
                "{label} VK should require exact canonical profile evidence"
            );
            assert!(
                vk.signals.iter().any(|signal| matches!(
                    signal,
                    super::super::site::Signal::StatusNotFound { codes } if codes == &[404]
                )),
                "{label} VK should still classify direct HTTP 404 as NotFound"
            );
            assert!(
                vk.signals.iter().all(|signal| !matches!(
                    signal,
                    super::super::site::Signal::StatusFound { .. }
                        | super::super::site::Signal::RedirectAbsent { .. }
                )),
                "{label} VK must not combine a generic 200 Found rule with redirect absence"
            );
        }
    }

    #[test]
    fn kofi_requires_profile_marker_not_status_only() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let kofi_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "Ko-Fi")
            .collect();

        assert_eq!(kofi_entries.len(), 1, "WMN merge must keep one Ko-Fi probe");
        let kofi = kofi_entries[0];
        assert!(!kofi.disabled, "Ko-Fi probe should remain enabled");
        assert_eq!(kofi.url.as_str(), "https://ko-fi.com/{username}");
        assert!(
            kofi.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyPresent { text }
                    if text == "id=\"profile-header\""
            )),
            "Ko-Fi should require the real profile header marker"
        );
        assert!(
            kofi.signals
                .iter()
                .all(|signal| !matches!(signal, super::super::site::Signal::StatusFound { .. })),
            "Ko-Fi must not infer Found from a generic HTTP 200 or challenge shell"
        );
        assert!(
            kofi.protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::Cloudflare))
                && kofi.tags.iter().any(|t| t == "bot-protected"),
            "Ko-Fi should stay routed as a Cloudflare-protected profile surface"
        );
    }

    #[test]
    fn codepen_requires_profile_marker_not_status_only() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let codepen_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "CodePen")
            .collect();

        assert_eq!(
            codepen_entries.len(),
            1,
            "WMN merge must keep one CodePen probe"
        );
        let codepen = codepen_entries[0];
        assert!(!codepen.disabled, "CodePen probe should remain enabled");
        assert_eq!(codepen.url.as_str(), "https://codepen.io/{username}");
        assert!(
            codepen.signals.iter().any(|signal| matches!(
                signal,
                super::super::site::Signal::BodyPresent { text }
                    if text == "property=\"og:url\""
            )),
            "CodePen should require profile metadata rather than status only"
        );
        assert!(
            codepen
                .signals
                .iter()
                .all(|signal| !matches!(signal, super::super::site::Signal::StatusFound { .. })),
            "CodePen must not infer Found from a generic HTTP 200 or challenge shell"
        );
        assert!(
            codepen
                .protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::Cloudflare))
                && codepen.tags.iter().any(|t| t == "bot-protected"),
            "CodePen should stay routed as a Cloudflare-protected profile surface"
        );
    }

    #[test]
    fn deviantart_is_classified_as_cloudfront_protected() {
        let registry = Registry::default_embedded_with_wmn().unwrap();
        let deviantart_entries: Vec<&Site> = registry
            .sites()
            .iter()
            .filter(|s| s.name == "DeviantArt")
            .collect();

        assert_eq!(
            deviantart_entries.len(),
            1,
            "WMN merge must keep one DeviantArt probe"
        );
        let deviantart = deviantart_entries[0];
        assert!(
            deviantart.tags.iter().any(|t| t == "bot-protected"),
            "DeviantArt should route through browser-capable transports when configured"
        );
        assert!(
            deviantart
                .protection
                .iter()
                .any(|p| matches!(p, super::super::site::ProtectionKind::Cloudfront)),
            "DeviantArt should document its current CloudFront edge block"
        );
    }

    #[test]
    fn source_field_round_trips() {
        let json = r#"{
            "sites": [
                { "name": "Nitter", "url": "https://nitter.example/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "source": "Twitter" }
            ]
        }"#;
        let registry = Registry::from_json_str(json).unwrap();
        assert_eq!(registry.sites()[0].source.as_deref(), Some("Twitter"));
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
    fn engine_inheritance_fills_empty_site_signals() {
        // Site has no `signals` block — should inherit the engine's.
        let json = r#"{
            "engines": {
                "Discourse": {
                    "signals": [
                        { "kind": "status_found", "codes": [200] },
                        { "kind": "body_absent", "text": "Oops! That page doesn't exist" }
                    ]
                }
            },
            "sites": [
                { "name": "Mozilla Forum", "url": "https://discourse.mozilla.org/u/{username}",
                  "engine": "Discourse" }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        let site = &r.sites()[0];
        assert_eq!(site.signals.len(), 2);
        assert_eq!(site.engine.as_deref(), Some("Discourse"));
        // engines map preserved
        assert!(r.engines().contains_key("Discourse"));
    }

    #[test]
    fn site_overrides_engine_signals_on_conflict() {
        // Site declares its own `signals` — engine's must NOT replace them.
        let json = r#"{
            "engines": {
                "Discourse": {
                    "signals": [{ "kind": "status_found", "codes": [200] }]
                }
            },
            "sites": [
                { "name": "Custom", "url": "https://example.com/{username}",
                  "engine": "Discourse",
                  "signals": [
                    { "kind": "status_found", "codes": [200] },
                    { "kind": "status_not_found", "codes": [404] }
                  ] }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        // The site-declared 2 signals win over the engine's 1 signal.
        assert_eq!(r.sites()[0].signals.len(), 2);
    }

    #[test]
    fn engine_headers_merge_with_site_headers_per_key() {
        // Engine declares one header; site declares another. Resolved
        // site should carry both. On per-key conflict the site wins.
        let json = r#"{
            "engines": {
                "Foo": {
                    "signals": [{ "kind": "status_found", "codes": [200] }],
                    "request_headers": {
                        "X-Engine": "engine-value",
                        "User-Agent": "engine-ua"
                    }
                }
            },
            "sites": [
                { "name": "S", "url": "https://example.com/{username}",
                  "engine": "Foo",
                  "request_headers": { "User-Agent": "site-ua" } }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        let h = &r.sites()[0].request_headers;
        assert_eq!(h.get("X-Engine").map(String::as_str), Some("engine-value"));
        assert_eq!(h.get("User-Agent").map(String::as_str), Some("site-ua"));
    }

    #[test]
    fn missing_engine_reference_fails_load() {
        let json = r#"{
            "engines": {},
            "sites": [
                { "name": "Mock", "url": "https://example.com/{username}",
                  "engine": "DoesNotExist" }
            ]
        }"#;
        let err = Registry::from_json_str(json).unwrap_err();
        assert!(
            err.to_string()
                .contains("references engine \"DoesNotExist\""),
            "expected missing-engine error, got: {err}"
        );
    }

    #[test]
    fn engine_regex_check_inherited_when_site_has_none() {
        let json = r#"{
            "engines": {
                "Bounded": {
                    "signals": [{ "kind": "status_found", "codes": [200] }],
                    "regex_check": "^[a-z]{3,16}$"
                }
            },
            "sites": [
                { "name": "S", "url": "https://example.com/{username}",
                  "engine": "Bounded" }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        assert_eq!(r.sites()[0].regex_check.as_deref(), Some("^[a-z]{3,16}$"));
    }

    #[test]
    fn region_tag_auto_populates_prefer_geo() {
        let json = r#"{
            "sites": [
                { "name": "vk.com", "url": "https://vk.com/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["region:ru", "social"] }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        let prefer = &r.sites()[0].access.prefer_geo;
        assert_eq!(prefer.len(), 1);
        assert_eq!(prefer[0].as_str(), "ru");
        // Hard geo stays empty — the tag is soft.
        assert!(r.sites()[0].access.geo.is_empty());
    }

    #[test]
    fn multiple_region_tags_stack() {
        let json = r#"{
            "sites": [
                { "name": "Pan-Slavic", "url": "https://example.test/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["region:ru", "region:by", "region:ua"] }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        let codes: Vec<&str> = r.sites()[0]
            .access
            .prefer_geo
            .iter()
            .map(super::super::access::CountryCode::as_str)
            .collect();
        assert_eq!(codes, vec!["ru", "by", "ua"]);
    }

    #[test]
    fn explicit_hard_geo_suppresses_tag_derived_soft() {
        // A site with hard `access.geo = ["pl"]` AND a `region:ru` tag:
        // the explicit hard policy wins, prefer_geo stays empty.
        // Otherwise tag-derived soft would silently re-route a probe
        // that the maintainer deliberately pinned to PL.
        let json = r#"{
            "sites": [
                { "name": "PL-only", "url": "https://example.test/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["region:ru"],
                  "access": { "geo": ["pl"] } }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        assert_eq!(r.sites()[0].access.geo[0].as_str(), "pl");
        assert!(r.sites()[0].access.prefer_geo.is_empty());
    }

    #[test]
    fn malformed_region_tag_is_ignored() {
        // `region:` followed by something that isn't a 2-letter code:
        // skip it silently rather than reject the whole load. The tag
        // already had no routing semantics in older versions.
        let json = r#"{
            "sites": [
                { "name": "Weird", "url": "https://example.test/{username}",
                  "signals": [{ "kind": "status_found", "codes": [200] }],
                  "tags": ["region:eurasia", "region:r", "region:RU"] }
            ]
        }"#;
        let r = Registry::from_json_str(json).unwrap();
        // Only the valid 2-letter "RU" survives (lowercased to "ru").
        let codes: Vec<&str> = r.sites()[0]
            .access
            .prefer_geo
            .iter()
            .map(super::super::access::CountryCode::as_str)
            .collect();
        assert_eq!(codes, vec!["ru"]);
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
