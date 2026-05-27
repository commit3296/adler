//! Opt-in `robots.txt` compliance.
//!
//! When enabled (`--respect-robots`), each probe URL is checked against the
//! host's `robots.txt` first; a disallowed path is skipped (reported
//! `Uncertain` with note `robots_disallowed`) rather than requested.
//!
//! The parser is intentionally minimal but errs toward *more* restriction:
//! it honors `Disallow` (prefix match) for the group matching our product
//! token `adler`, falling back to the `*` group, and **ignores `Allow`**.
//! Ignoring `Allow` can only cause us to skip a path a site would have
//! permitted — the safe direction for a "respect robots" switch. A missing,
//! unreadable, or empty `robots.txt` allows everything (the standard
//! default).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OnceCell};

/// Parsed `Disallow` rules for the user-agent group that applies to us.
#[derive(Debug, Clone, Default)]
pub(crate) struct Rules {
    disallow: Vec<String>,
}

impl Rules {
    fn allow_all() -> Self {
        Self::default()
    }

    /// True if `path` is not blocked by any non-empty `Disallow` prefix.
    pub(crate) fn is_allowed(&self, path: &str) -> bool {
        !self
            .disallow
            .iter()
            .any(|rule| !rule.is_empty() && path.starts_with(rule.as_str()))
    }

    /// Parse `robots.txt`, returning the rules for `ua_token` (preferred) or
    /// the `*` group.
    pub(crate) fn parse(body: &str, ua_token: &str) -> Self {
        let ua_token = ua_token.to_ascii_lowercase();
        let mut groups: Vec<(Vec<String>, Vec<String>)> = Vec::new();
        let mut agents: Vec<String> = Vec::new();
        let mut disallow: Vec<String> = Vec::new();
        let mut saw_rule = false;

        for raw in body.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            match key.as_str() {
                "user-agent" => {
                    // A user-agent line after rules begins a new group.
                    if saw_rule {
                        groups.push((std::mem::take(&mut agents), std::mem::take(&mut disallow)));
                        saw_rule = false;
                    }
                    agents.push(value.to_ascii_lowercase());
                }
                "disallow" => {
                    disallow.push(value);
                    saw_rule = true;
                }
                _ => {} // Allow / Crawl-delay / Sitemap / unknown → ignored
            }
        }
        if !agents.is_empty() || !disallow.is_empty() {
            groups.push((agents, disallow));
        }

        let mut wildcard: Option<Vec<String>> = None;
        for (agents, rules) in groups {
            if agents.iter().any(|a| a == &ua_token) {
                return Self { disallow: rules };
            }
            if wildcard.is_none() && agents.iter().any(|a| a == "*") {
                wildcard = Some(rules);
            }
        }
        Self {
            disallow: wildcard.unwrap_or_default(),
        }
    }
}

/// Per-origin `robots.txt` cache. One fetch per origin, deduplicated across
/// concurrent probes; cheap to clone (`Arc`-backed).
#[derive(Debug, Clone)]
pub(crate) struct RobotsCache {
    client: reqwest::Client,
    ua_token: String,
    cells: Arc<Mutex<HashMap<String, Arc<OnceCell<Rules>>>>>,
}

impl RobotsCache {
    pub(crate) fn new(client: reqwest::Client, ua_token: impl Into<String>) -> Self {
        Self {
            client,
            ua_token: ua_token.into(),
            cells: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether `path` may be requested on `origin` (e.g. `https://x.com`).
    pub(crate) async fn allowed(&self, origin: &str, path: &str) -> bool {
        let cell = {
            let mut cells = self.cells.lock().await;
            cells
                .entry(origin.to_owned())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let rules = cell.get_or_init(|| self.fetch(origin.to_owned())).await;
        rules.is_allowed(path)
    }

    async fn fetch(&self, origin: String) -> Rules {
        let url = format!("{origin}/robots.txt");
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                Rules::parse(&body, &self.ua_token)
            }
            _ => Rules::allow_all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_group_disallow_prefix() {
        let r = Rules::parse("User-agent: *\nDisallow: /private", "adler");
        assert!(!r.is_allowed("/private/x"));
        assert!(r.is_allowed("/public"));
    }

    #[test]
    fn specific_group_preferred_over_wildcard() {
        let body = "User-agent: adler\nDisallow: /\n\nUser-agent: *\nDisallow:";
        let r = Rules::parse(body, "adler");
        assert!(!r.is_allowed("/anything"));
    }

    #[test]
    fn falls_back_to_wildcard_when_no_specific_group() {
        let body = "User-agent: googlebot\nDisallow: /g\n\nUser-agent: *\nDisallow: /w";
        let r = Rules::parse(body, "adler");
        assert!(!r.is_allowed("/w/x"));
        assert!(r.is_allowed("/g/x")); // googlebot rule doesn't apply to us
    }

    #[test]
    fn empty_disallow_allows_everything() {
        let r = Rules::parse("User-agent: *\nDisallow:", "adler");
        assert!(r.is_allowed("/anything"));
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let body = "# a comment\n\nUser-agent: *   # us\nDisallow: /no  # nope\n";
        let r = Rules::parse(body, "adler");
        assert!(!r.is_allowed("/no"));
        assert!(r.is_allowed("/ok"));
    }

    #[test]
    fn allow_lines_are_ignored_conservatively() {
        // Allow does not loosen Disallow in our minimal parser.
        let body = "User-agent: *\nDisallow: /u\nAllow: /u/public";
        let r = Rules::parse(body, "adler");
        assert!(!r.is_allowed("/u/public"));
    }

    #[test]
    fn missing_robots_allows_all() {
        assert!(Rules::allow_all().is_allowed("/anything"));
    }

    #[tokio::test]
    async fn cache_fetches_and_applies_rules() {
        use wiremock::matchers::{any, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /no"),
            )
            .mount(&server)
            .await;
        let cache = RobotsCache::new(reqwest::Client::new(), "adler");
        let origin = server.uri();
        assert!(!cache.allowed(&origin, "/no/alice").await);
        assert!(cache.allowed(&origin, "/yes/alice").await);
    }

    #[tokio::test]
    async fn missing_robots_txt_allows() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let cache = RobotsCache::new(reqwest::Client::new(), "adler");
        assert!(cache.allowed(&server.uri(), "/anything").await);
    }
}
