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
    /// Operator-supplied identifier for this egress — used by the web
    /// UI's per-scan egress subset selection (and by any other call
    /// site that needs to refer to a specific egress by stable name).
    /// Optional: an unnamed egress still participates in policy-based
    /// matching, it just can't be selected by name.
    #[serde(default)]
    pub name: Option<String>,
}

/// What a site needs from its egress. The default (empty) means "no
/// special routing" — the request uses the client's default egress.
///
/// Two flavours of geo constraint co-exist:
///
/// - [`geo`](Self::geo) — **hard**. A site that won't answer from
///   anywhere else (e.g. a country-locked profile). No matching egress
///   in the pool → `Uncertain(GeoUnavailable)`, never a false `NotFound`.
/// - [`prefer_geo`](Self::prefer_geo) — **soft**. A site that *prefers*
///   a local egress (better recall, less aggressive bot filtering) but
///   still works from anywhere. No matching egress → fall back to the
///   default egress and probe normally. Auto-populated at registry-load
///   time from `region:XX` tags when the site doesn't already declare
///   a hard `geo` constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPolicy {
    /// Require an egress in one of these countries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geo: Vec<CountryCode>,
    /// Prefer an egress in one of these countries — fall back to the
    /// default if the pool has no match. Soft counterpart to [`geo`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer_geo: Vec<CountryCode>,
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
        self.geo.is_empty()
            && self.prefer_geo.is_empty()
            && self.ip_type.is_none()
            && self.session.is_none()
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

    /// Names of the configured sessions, sorted lexicographically for a
    /// stable display order. Values stay private — by design the public
    /// surface only ever leaks the keys an operator referenced via
    /// `access.session`, never the cookie/token bytes themselves.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.sessions.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Read-only metadata for one configured egress, surfaced via
/// [`Client::egress_summary`](crate::Client::egress_summary).
///
/// Carries only the match-relevant facets (name + country + kind); the
/// proxy URL is *deliberately omitted* — those typically embed
/// credentials (`socks5://user:pass@host:1080`) that have no business
/// landing in a JSON response served to a browser.
#[derive(Debug, Clone, Serialize)]
pub struct EgressSummary {
    /// Operator-supplied name, if any. Used by per-scan egress subset
    /// selection (`POST /api/scan` with `egress_names`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Country this egress exits from, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<CountryCode>,
    /// Network kind (`datacenter` / `residential` / `mobile` / `tor`).
    pub kind: EgressKind,
}

/// One built egress: its match metadata plus the HTTP client that
/// routes through it.
struct EgressEntry {
    name: Option<String>,
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

/// Constructor tuple for [`EgressPool`]: one row per configured proxy
/// carries its operator-supplied `name` (if any), its country and
/// kind, and the already-built `reqwest`-backed fetcher.
pub(crate) type EgressEntryTuple = (
    Option<String>,
    Option<CountryCode>,
    EgressKind,
    Arc<HttpFetcher>,
);

impl EgressPool {
    pub(crate) fn new(entries: Vec<EgressEntryTuple>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(name, country, kind, fetcher)| EgressEntry {
                    name,
                    country,
                    kind,
                    fetcher,
                })
                .collect(),
        }
    }

    /// Read-only view of the pool — `(name, country, kind)` for every
    /// configured egress, in the order they were registered. Used by the
    /// `GET /api/access` endpoint so the SPA can show what's configured
    /// without ever touching proxy URLs.
    pub(crate) fn summary(&self) -> Vec<EgressSummary> {
        self.entries
            .iter()
            .map(|e| EgressSummary {
                name: e.name.clone(),
                country: e.country.clone(),
                kind: e.kind,
            })
            .collect()
    }

    /// Return a new pool containing only entries whose `name` matches
    /// one of `names`. Entries without a name are excluded (they can't
    /// be referenced by name). `names` being empty is treated as "no
    /// filter" and a clone of the full pool is returned — that
    /// preserves the policy-driven default for callers who didn't ask
    /// for an explicit subset.
    pub(crate) fn subset(&self, names: &[String]) -> Self {
        if names.is_empty() {
            return Self {
                entries: self
                    .entries
                    .iter()
                    .map(|e| EgressEntry {
                        name: e.name.clone(),
                        country: e.country.clone(),
                        kind: e.kind,
                        fetcher: Arc::clone(&e.fetcher),
                    })
                    .collect(),
            };
        }
        let wanted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
        Self {
            entries: self
                .entries
                .iter()
                .filter(|e| e.name.as_deref().is_some_and(|n| wanted.contains(n)))
                .map(|e| EgressEntry {
                    name: e.name.clone(),
                    country: e.country.clone(),
                    kind: e.kind,
                    fetcher: Arc::clone(&e.fetcher),
                })
                .collect(),
        }
    }

    /// Names of egresses configured in this pool, in registration
    /// order. Used by the server to validate `egress_names` on
    /// `POST /api/scan`.
    pub(crate) fn names(&self) -> Vec<String> {
        self.entries.iter().filter_map(|e| e.name.clone()).collect()
    }

    /// Pick an egress for `policy`. Three outcomes:
    ///
    /// - Unconstrained policy (no hard `geo`, no `prefer_geo`, no
    ///   `ip_type`) → [`EgressChoice::Default`].
    /// - Hard constraint with no match → [`EgressChoice::Unavailable`].
    /// - Soft `prefer_geo` with no match → falls back to
    ///   [`EgressChoice::Default`] (the probe still happens, just via
    ///   the unproxied / default egress).
    pub(crate) fn select(&self, policy: &AccessPolicy) -> EgressChoice {
        // Session-only policy (no geo / no ip_type / no prefer_geo) →
        // default egress.
        if policy.geo.is_empty() && policy.prefer_geo.is_empty() && policy.ip_type.is_none() {
            return EgressChoice::Default;
        }

        // Hard path: explicit `geo` (and optional `ip_type`) — when
        // present, this is authoritative and prefer_geo is ignored.
        if !policy.geo.is_empty() {
            return self
                .pick_matching(&policy.geo, policy.ip_type)
                .map_or(EgressChoice::Unavailable, EgressChoice::Use);
        }

        // Soft path: only `prefer_geo` (and optional `ip_type`). Match
        // → route through it; no match → fall back to the default
        // egress rather than emit Unavailable. The site is *expected*
        // to be reachable from anywhere; the egress preference is a
        // recall optimisation, not a correctness constraint.
        if !policy.prefer_geo.is_empty() {
            return self
                .pick_matching(&policy.prefer_geo, policy.ip_type)
                .map_or(EgressChoice::Default, EgressChoice::Use);
        }

        // Only `ip_type` constrained — keep the hard semantics: a site
        // that asks for a residential IP and the pool has none is
        // Unavailable, not silently downgraded to datacenter.
        self.pick_matching(&[], policy.ip_type)
            .map_or(EgressChoice::Unavailable, EgressChoice::Use)
    }

    /// Internal: pick a random matching entry for the given geo and
    /// optional `ip_type`. `geo` empty means "any country". Returns
    /// `None` when nothing fits.
    fn pick_matching(
        &self,
        geo: &[CountryCode],
        ip_type: Option<EgressKind>,
    ) -> Option<Arc<HttpFetcher>> {
        let matches: Vec<&EgressEntry> = self
            .entries
            .iter()
            .filter(|e| {
                let geo_ok = geo.is_empty() || e.country.as_ref().is_some_and(|c| geo.contains(c));
                let kind_ok = ip_type.is_none_or(|k| e.kind == k);
                geo_ok && kind_ok
            })
            .collect();
        match matches.len() {
            0 => None,
            n => Some(Arc::clone(&matches[fastrand::usize(0..n)].fetcher)),
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
            (
                None,
                Some(cc("pl")),
                EgressKind::Residential,
                dummy_fetcher(),
            ),
            (
                None,
                Some(cc("de")),
                EgressKind::Datacenter,
                dummy_fetcher(),
            ),
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
            ..AccessPolicy::default()
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Use(_)));
    }

    #[test]
    fn ip_type_match_picks_an_egress() {
        let policy = AccessPolicy {
            ip_type: Some(EgressKind::Datacenter),
            ..AccessPolicy::default()
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
            ..AccessPolicy::default()
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn unknown_geo_is_unavailable() {
        let policy = AccessPolicy {
            geo: vec![cc("jp")],
            ..AccessPolicy::default()
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn empty_pool_with_constraint_is_unavailable() {
        let empty = EgressPool::new(Vec::new());
        let policy = AccessPolicy {
            geo: vec![cc("pl")],
            ..AccessPolicy::default()
        };
        assert!(matches!(empty.select(&policy), EgressChoice::Unavailable));
    }

    #[test]
    fn soft_prefer_match_routes_through_it() {
        // prefer_geo = pl, pool has a PL residential → use it.
        let policy = AccessPolicy {
            prefer_geo: vec![cc("pl")],
            ..AccessPolicy::default()
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Use(_)));
    }

    #[test]
    fn soft_prefer_no_match_falls_back_to_default() {
        // prefer_geo = jp, pool has no JP egress → Default, NOT Unavailable.
        // This is the whole point of soft routing: the probe still goes
        // out, just via the unproxied default — the site is reachable
        // from anywhere, the preference was a recall optimisation.
        let policy = AccessPolicy {
            prefer_geo: vec![cc("jp")],
            ..AccessPolicy::default()
        };
        assert!(matches!(pool().select(&policy), EgressChoice::Default));
    }

    #[test]
    fn hard_geo_wins_over_soft_prefer() {
        // When both are set, hard `geo` is authoritative — prefer_geo
        // is ignored. Asking for hard PL with no match in the JP-only
        // prefer is still Unavailable.
        let empty_pl = EgressPool::new(vec![(
            None,
            Some(cc("jp")),
            EgressKind::Datacenter,
            dummy_fetcher(),
        )]);
        let policy = AccessPolicy {
            geo: vec![cc("pl")],
            prefer_geo: vec![cc("jp")],
            ..AccessPolicy::default()
        };
        assert!(matches!(
            empty_pl.select(&policy),
            EgressChoice::Unavailable
        ));
    }

    #[test]
    fn ip_type_only_is_still_hard() {
        // Asking for residential when the pool has none must remain
        // Unavailable. We only soften geo via prefer_geo — kind
        // requirements are still load-bearing.
        let dc_only = EgressPool::new(vec![(None, None, EgressKind::Datacenter, dummy_fetcher())]);
        let policy = AccessPolicy {
            ip_type: Some(EgressKind::Residential),
            ..AccessPolicy::default()
        };
        assert!(matches!(dc_only.select(&policy), EgressChoice::Unavailable));
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
