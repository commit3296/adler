//! Per-site access policy and the egress (proxy) model.
//!
//! Access-engine phase 3: route the raw-HTTP probe path through a
//! geo / IP-type-appropriate egress. A site declares what it needs via
//! [`AccessPolicy`] (e.g. "only reachable from a Polish residential
//! IP"); the client matches that against a configured pool of
//! [`EgressSpec`]s. If the policy is unconstrained the request uses the
//! client's default egress (direct, or the global `--proxy`); if it's
//! constrained but nothing in the pool fits, the probe is reported as
//! `Uncertain(GeoUnavailable)` — **never** a false `NotFound`, since
//! "couldn't reach from the required location" is not "account absent".
//!
//! The browser transport keeps its backend's own egress; this phase
//! routes the HTTP path only.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::transport::HttpFetcher;

/// ISO-3166-1 alpha-2 country code, stored lowercased (e.g. `pl`, `de`).
/// A newtype so a geo requirement can't be confused with an arbitrary
/// string and is validated at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CountryCode([u8; 2]);

impl CountryCode {
    /// Parse a two-letter code, lowercasing ASCII. `None` for anything
    /// that isn't exactly two ASCII letters.
    #[must_use]
    pub fn new(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic() {
            Some(Self([b[0].to_ascii_lowercase(), b[1].to_ascii_lowercase()]))
        } else {
            None
        }
    }

    /// The lowercased two-letter code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Constructed only from ASCII letters, so this is always valid.
        std::str::from_utf8(&self.0).unwrap_or("??")
    }
}

impl TryFrom<String> for CountryCode {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s).ok_or_else(|| format!("invalid country code: {s:?}"))
    }
}

impl From<CountryCode> for String {
    fn from(c: CountryCode) -> Self {
        c.as_str().to_owned()
    }
}

/// The kind of network an egress exits from.
///
/// A site's `ip_type` requirement is matched against this. (`Direct`
/// isn't a kind here — the unproxied default egress is selected by an
/// *unconstrained* policy, not by requesting a kind.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EgressKind {
    /// A datacenter / hosting-provider IP (cheap, easily fingerprinted
    /// and blocked). The default when a config entry omits `kind`.
    #[default]
    Datacenter,
    /// A residential ISP IP (harder to block; what most "real users"
    /// look like).
    Residential,
    /// A mobile-carrier IP (shared CGNAT ranges; highest trust on many
    /// sites).
    Mobile,
    /// A Tor exit node.
    Tor,
}

/// A configured egress (proxy) the client can route through.
///
/// Produced from CLI / config; the live client pairs each spec with its
/// own HTTP client (reqwest bakes the proxy in at build time).
/// Deserialises from the `[[egress]]` entries of a proxy-pool config
/// file.
#[derive(Debug, Clone, Deserialize)]
pub struct EgressSpec {
    /// Proxy URL — `http://`, `https://`, `socks5://`, or `socks5h://`.
    pub url: String,
    /// Country this egress exits from, if known.
    #[serde(default)]
    pub country: Option<CountryCode>,
    /// Network kind this egress exits from (defaults to `datacenter`).
    #[serde(default)]
    pub kind: EgressKind,
}

/// What a site needs from its egress. The default (empty) means "no
/// special routing" — the request uses the client's default egress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPolicy {
    /// Require an egress in one of these countries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geo: Vec<CountryCode>,
    /// Require an egress of this network kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_type: Option<EgressKind>,
    /// Name of an operator-supplied session (see `--sessions`) whose
    /// headers (cookies / auth tokens) this site's probes must carry.
    /// The site is unreachable without it, so a missing session yields
    /// `Uncertain(SessionRequired)` rather than a login-wall false
    /// `NotFound`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl AccessPolicy {
    /// True when the policy imposes no constraint at all (the common
    /// case). Drives `skip_serializing_if` so existing `sites.json`
    /// entries serialise unchanged.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.geo.is_empty() && self.ip_type.is_none() && self.session.is_none()
    }
}

/// An operator-supplied authenticated session for a site: a bag of HTTP
/// headers (typically `Cookie`, sometimes `Authorization` / CSRF
/// tokens) applied to probes for sites whose `access.session` names it.
///
/// This is "use a real account", not evasion — the operator brings a
/// session they're entitled to. Header *values* are secrets: they're
/// redacted from `Debug` and are never logged or serialised.
#[derive(Clone, Default)]
pub struct Session {
    headers: BTreeMap<String, String>,
}

impl Session {
    /// Build a session from plain header name→value pairs (e.g. parsed
    /// from a `--sessions` config file).
    #[must_use]
    pub fn from_headers(headers: BTreeMap<String, String>) -> Self {
        Self { headers }
    }

    /// Merge this session's headers over `base` (the session wins on
    /// conflict), producing the header set for the outgoing request.
    pub(crate) fn apply(&self, base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut out = base.clone();
        for (k, v) in &self.headers {
            out.insert(k.clone(), v.clone());
        }
        out
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact values — session headers carry cookies / tokens.
        f.debug_struct("Session")
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Named-session store, indexed by the name a site references via
/// `access.session`. Empty by default → a no-op.
#[derive(Clone, Default, Debug)]
pub struct SessionStore {
    sessions: HashMap<String, Session>,
}

impl SessionStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a named session.
    pub fn insert(&mut self, name: impl Into<String>, session: Session) {
        self.sessions.insert(name.into(), session);
    }

    /// True when no session is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Number of configured sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Session> {
        self.sessions.get(name)
    }
}

/// One built egress: its match metadata plus the HTTP client that
/// routes through it.
struct EgressEntry {
    country: Option<CountryCode>,
    kind: EgressKind,
    fetcher: Arc<HttpFetcher>,
}

/// Runtime pool of built egresses. Empty by default → every site uses
/// the client's default egress, so an empty pool is a no-op.
pub(crate) struct EgressPool {
    entries: Vec<EgressEntry>,
}

/// Result of matching a site's [`AccessPolicy`] against the pool.
pub(crate) enum EgressChoice {
    /// Unconstrained policy → use the client's default egress.
    Default,
    /// Route through this egress's HTTP client.
    Use(Arc<HttpFetcher>),
    /// Constrained policy with no matching egress → honest
    /// `Uncertain(GeoUnavailable)` rather than a false `NotFound`.
    Unavailable,
}

impl EgressPool {
    pub(crate) fn new(entries: Vec<(Option<CountryCode>, EgressKind, Arc<HttpFetcher>)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(country, kind, fetcher)| EgressEntry {
                    country,
                    kind,
                    fetcher,
                })
                .collect(),
        }
    }

    /// Pick an egress for `policy`. Unconstrained → [`EgressChoice::Default`].
    /// Constrained → a random matching egress, or [`EgressChoice::Unavailable`]
    /// when none fit (geo and/or kind don't match any pool entry).
    pub(crate) fn select(&self, policy: &AccessPolicy) -> EgressChoice {
        // Only geo / IP-type constrain the egress; a session-only policy
        // (no geo, no ip_type) still uses the default egress.
        if policy.geo.is_empty() && policy.ip_type.is_none() {
            return EgressChoice::Default;
        }
        let matches: Vec<&EgressEntry> = self
            .entries
            .iter()
            .filter(|e| {
                let geo_ok = policy.geo.is_empty()
                    || e.country.as_ref().is_some_and(|c| policy.geo.contains(c));
                let kind_ok = policy.ip_type.is_none_or(|k| e.kind == k);
                geo_ok && kind_ok
            })
            .collect();
        match matches.len() {
            0 => EgressChoice::Unavailable,
            n => EgressChoice::Use(Arc::clone(&matches[fastrand::usize(0..n)].fetcher)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpFetcher;

    fn cc(s: &str) -> CountryCode {
        CountryCode::new(s).expect("valid country code")
    }

    fn dummy_fetcher() -> Arc<HttpFetcher> {
        Arc::new(HttpFetcher::new(reqwest::Client::new()))
    }

    fn pool() -> EgressPool {
        EgressPool::new(vec![
            (Some(cc("pl")), EgressKind::Residential, dummy_fetcher()),
            (Some(cc("de")), EgressKind::Datacenter, dummy_fetcher()),
        ])
    }

    #[test]
    fn country_code_normalises_and_rejects() {
        assert_eq!(CountryCode::new("PL").unwrap().as_str(), "pl");
        assert!(CountryCode::new("p").is_none());
        assert!(CountryCode::new("pol").is_none());
        assert!(CountryCode::new("p1").is_none());
    }

    #[test]
    fn unconstrained_policy_uses_default_egress() {
        let choice = pool().select(&AccessPolicy::default());
        assert!(matches!(choice, EgressChoice::Default));
    }

    #[test]
    fn geo_match_picks_an_egress() {
        let policy = AccessPolicy {
            geo: vec![cc("pl")],
            ip_type: None,
            session: None,
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Use(_)));
    }

    #[test]
    fn ip_type_match_picks_an_egress() {
        let policy = AccessPolicy {
            geo: Vec::new(),
            ip_type: Some(EgressKind::Datacenter),
            session: None,
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Use(_)));
    }

    #[test]
    fn geo_present_but_wrong_kind_is_unavailable() {
        // PL exists in the pool, but only as Residential — asking for a
        // PL *Mobile* egress must fail rather than fall back.
        let policy = AccessPolicy {
            geo: vec![cc("pl")],
            ip_type: Some(EgressKind::Mobile),
            session: None,
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn unknown_geo_is_unavailable() {
        let policy = AccessPolicy {
            geo: vec![cc("jp")],
            ip_type: None,
            session: None,
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn empty_pool_with_constraint_is_unavailable() {
        let empty = EgressPool::new(Vec::new());
        let policy = AccessPolicy {
            geo: vec![cc("pl")],
            ip_type: None,
            session: None,
        };
        assert!(matches!(empty.select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn session_apply_overrides_base_headers() {
        let mut base = BTreeMap::new();
        base.insert("X-IG-App-ID".to_string(), "936".to_string());
        base.insert("Cookie".to_string(), "old".to_string());
        let mut sh = BTreeMap::new();
        sh.insert("Cookie".to_string(), "sessionid=real".to_string());
        let merged = Session::from_headers(sh).apply(&base);
        // Session wins on conflict; non-conflicting base header preserved.
        assert_eq!(merged.get("Cookie").unwrap(), "sessionid=real");
        assert_eq!(merged.get("X-IG-App-ID").unwrap(), "936");
    }

    #[test]
    fn session_store_insert_and_lookup() {
        let mut store = SessionStore::new();
        assert!(store.is_empty());
        store.insert("ig", Session::from_headers(BTreeMap::new()));
        assert!(!store.is_empty());
        assert!(store.get("ig").is_some());
        assert!(store.get("missing").is_none());
    }
}
