//! HTTP client wrapping `reqwest`, plus the per-site probe entry point.
//!
//! The wrapper exists to keep `reqwest` out of Adler's public API surface.
//! All knobs that future modules need (timeouts, redirect policy, user agent)
//! are configured through [`ClientBuilder`]; per-request transient failures
//! never bubble up as errors — they become
//! [`MatchKind::Uncertain`](crate::MatchKind::Uncertain) on the returned
//! outcome.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::access::{EgressPool, SessionStore};
use crate::browser::{BrowserBackend, BrowserBudget};
use crate::retry::RetryPolicy;
use crate::robots::RobotsCache;
use crate::throttle::HostThrottle;
use crate::transport::HttpFetcher;
#[cfg(feature = "impersonate")]
use crate::transport::ImpersonateFetcher;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_REDIRECT_LIMIT: usize = 8;
const DEFAULT_PER_HOST_INTERVAL: Duration = Duration::from_millis(100);
/// Single fixed key for the global rate limiter (it gates all hosts).
const GLOBAL_THROTTLE_KEY: &str = "*global*";

/// HTTP client used to probe sites.
///
/// Cheap to clone — the underlying `reqwest::Client` is reference-counted
/// internally, and the throttle is `Arc`-backed, so cloning is the
/// recommended way to share a client between tasks. Cloned clients share
/// throttle state, which is what you want: a fan-out scan must not
/// accidentally exceed a per-host budget by spawning more clients.
#[derive(Clone)]
pub struct Client {
    http: Arc<HttpFetcher>,
    /// Geo / IP-type egress pool for sites whose `access` policy needs a
    /// specific proxy. Empty by default → every site uses `http`.
    egress: Arc<EgressPool>,
    /// Operator-supplied sessions, keyed by the name a site references
    /// via `access.session`. Empty by default.
    sessions: Arc<SessionStore>,
    throttle: HostThrottle,
    /// Global RPS cap applied across all hosts. `None` → uncapped.
    global_throttle: Option<HostThrottle>,
    retry: RetryPolicy,
    /// Optional rotation pool. Empty → use the client's fixed User-Agent.
    /// `Arc<[String]>` so cloning a client per task stays cheap.
    user_agents: Arc<[String]>,
    /// Extract profile fields from `Found` pages that declare extractors.
    enrich: bool,
    /// When set, skip probes disallowed by the host's `robots.txt`.
    robots: Option<RobotsCache>,
    /// Browser backend used for `bot-protected` sites. `None` → those sites
    /// stay on the raw HTTP path and typically end up `Uncertain`.
    browser: Option<Arc<dyn BrowserBackend>>,
    /// TLS-fingerprint-impersonating HTTP client (`wreq`). Built when
    /// the `impersonate` Cargo feature is on; routes sites whose
    /// `protection` is exactly `TlsFingerprint`.
    #[cfg(feature = "impersonate")]
    impersonate: Option<Arc<ImpersonateFetcher>>,
    /// Per-scan cap on browser fetches. Shared across `Client::check` calls
    /// for a single scan, so several tasks compete for the same budget.
    browser_budget: Arc<BrowserBudget>,
    /// Per-scan cap on *automatic escalations* from a cheap transport to
    /// the browser when the cheap path returns
    /// `Uncertain(CloudflareChallenge | RateLimited)`. Independent of
    /// `browser_budget` so the pre-tagged `bot-protected` subset and the
    /// long-tail escalation subset don't fight over the same number.
    escalation_budget: Arc<crate::escalation::EscalationBudget>,
    /// Whether automatic escalation runs at all. `false` keeps the cheap
    /// transport's outcome verbatim — useful for benchmarking the raw
    /// signals without the access-engine lift on top.
    escalation_enabled: bool,
}

impl Client {
    /// Start configuring a new client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Read-only view of the configured egress pool — `(country, kind)`
    /// for every registered proxy, in the order they were declared.
    /// Proxy URLs are not surfaced (they typically carry credentials),
    /// so this is safe to serialise to a JSON response.
    #[must_use]
    pub fn egress_summary(&self) -> Vec<crate::access::EgressSummary> {
        self.egress.summary()
    }

    /// Names of the configured sessions (sorted lexicographically),
    /// without any header values. Useful for a UI listing which session
    /// keys an operator can reference via `access.session` on a site.
    #[must_use]
    pub fn session_names(&self) -> Vec<String> {
        self.sessions.names()
    }

    /// Names of the configured egresses (in registration order, only
    /// those that supplied a name). Used by the server to validate
    /// per-scan `egress_names` against the loaded pool.
    #[must_use]
    pub fn egress_names(&self) -> Vec<String> {
        self.egress.names()
    }

    /// Returns a new client identical to this one except its egress
    /// pool is restricted to entries whose `name` matches one of
    /// `names`. An empty `names` slice is treated as "no filter" and
    /// returns a clone of the full pool.
    ///
    /// Cheap to call repeatedly: all shared state (HTTP clients,
    /// throttle, sessions, budgets, browser backend, …) is
    /// `Arc`-cloned so the returned client shares the parent's
    /// per-scan caps (browser budget, escalation budget, throttle
    /// state) rather than each subset getting a fresh one. This is the
    /// right behaviour for a single web-server instance handing out
    /// per-request clients.
    #[must_use]
    pub fn with_egress_subset(&self, names: &[String]) -> Self {
        Self {
            http: Arc::clone(&self.http),
            egress: Arc::new(self.egress.subset(names)),
            sessions: Arc::clone(&self.sessions),
            throttle: self.throttle.clone(),
            global_throttle: self.global_throttle.clone(),
            retry: self.retry.clone(),
            user_agents: Arc::clone(&self.user_agents),
            enrich: self.enrich,
            robots: self.robots.clone(),
            browser: self.browser.clone(),
            #[cfg(feature = "impersonate")]
            impersonate: self.impersonate.clone(),
            browser_budget: Arc::clone(&self.browser_budget),
            escalation_budget: Arc::clone(&self.escalation_budget),
            escalation_enabled: self.escalation_enabled,
        }
    }
}

/// Raw response data returned by [`Client::fetch`] for diagnostics.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP status code.
    pub status: u16,
    /// Final URL after redirects.
    pub final_url: String,
    /// Decoded response body.
    pub body: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("throttle", &self.throttle)
            .field("global_throttle", &self.global_throttle)
            .field("retry", &self.retry)
            .field("user_agents", &self.user_agents)
            .field("enrich", &self.enrich)
            .field("robots", &self.robots.is_some())
            .field("browser", &self.browser.is_some())
            .field("browser_budget", &self.browser_budget)
            .field("escalation_budget", &self.escalation_budget)
            .field("escalation_enabled", &self.escalation_enabled)
            .finish_non_exhaustive()
    }
}

/// Registry tag marking a site as bot-protected.
///
/// Set on sites behind Cloudflare, `PerimeterX`, datadome,
/// `hCaptcha`, etc. The routing layer treats it as a hint that
/// residential egress is likely required; the doctor and
/// registry-summary surfaces use it to annotate honest-limit audits.
/// Tags are compared with [`str::eq_ignore_ascii_case`].
pub const BOT_PROTECTED_TAG: &str = "bot-protected";

mod builder;
mod probe;
mod util;
pub use builder::{ClientBuilder, DEFAULT_BROWSER_BUDGET, DEFAULT_ESCALATION_BUDGET};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::RenderedPage;
    use crate::check::{MatchKind, UncertainReason};
    use crate::error::{Error, Result};
    use crate::site::{HttpMethod, ProtectionKind, Signal, Site, UrlTemplate};
    use crate::username::Username;
    use std::time::Instant;
    use wiremock::matchers::{any, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::test_fixtures::{default_site, test_client};

    fn build_client() -> Client {
        test_client()
    }

    fn site_with(server: &MockServer, signals: Vec<Signal>) -> Site {
        let mut s = default_site("Mock", &format!("{}/{{username}}", server.uri()));
        s.signals = signals;
        s
    }

    fn user() -> Username {
        Username::new("alice").unwrap()
    }

    #[tokio::test]
    async fn regex_check_short_circuits_before_any_request() {
        // Stand up a mock that would 200 on *anything* — if probe_once
        // failed to short-circuit on regex mismatch, the username
        // "alice" (5 chars) would resolve to Found here.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // The site only accepts usernames of 8+ chars; "alice" is 5.
        site.regex_check = Some("^[A-Za-z]{8,}$".into());
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(
            matches!(outcome.reason, Some(UncertainReason::UsernameNotAllowed)),
            "expected UsernameNotAllowed, got {:?}",
            outcome.reason,
        );
        // No request should have hit the mock — assert by counting
        // received_requests on the wiremock server.
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            recvd.len(),
            0,
            "regex_check mismatch must skip the HTTP request entirely"
        );
    }

    #[tokio::test]
    async fn geo_constrained_site_with_no_egress_is_geo_unavailable() {
        // A mock that would 200 on anything — if the geo gate failed to
        // short-circuit, "alice" would resolve to Found here.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // Require a Polish egress; the default client has no egress pool,
        // so nothing can satisfy it.
        site.access = crate::access::AccessPolicy {
            geo: vec![crate::access::CountryCode::new("pl").unwrap()],
            ..crate::access::AccessPolicy::default()
        };
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(
            matches!(outcome.reason, Some(UncertainReason::GeoUnavailable)),
            "expected GeoUnavailable, got {:?}",
            outcome.reason,
        );
        // The site must NOT have been probed — an unreachable geo is not
        // evidence of absence, and we don't fetch from the wrong location.
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            recvd.len(),
            0,
            "geo-unavailable must skip the HTTP request entirely"
        );
    }

    #[tokio::test]
    async fn session_headers_are_sent_on_probe() {
        // Only respond 200 when the request carries the session cookie,
        // so a Found verdict proves the header was actually applied.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(wiremock::matchers::header("cookie", "sessionid=real"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Cookie".to_string(), "sessionid=real".to_string());
        let mut store = SessionStore::new();
        store.insert("acct", crate::access::Session::from_headers(headers));
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .sessions(store)
            .build()
            .expect("client builds");
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        site.access.session = Some("acct".to_string());
        let outcome = client.check(&site, &user()).await;
        assert_eq!(
            outcome.kind,
            MatchKind::Found,
            "session cookie should unlock the 200 (got {:?})",
            outcome.reason,
        );
    }

    #[tokio::test]
    async fn missing_named_session_is_session_required() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // Names a session the (empty) store doesn't have.
        site.access.session = Some("not-configured".to_string());
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(
            matches!(outcome.reason, Some(UncertainReason::SessionRequired)),
            "expected SessionRequired, got {:?}",
            outcome.reason,
        );
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            recvd.len(),
            0,
            "a missing session must skip the request, not probe unauthenticated"
        );
    }

    #[cfg(feature = "impersonate")]
    #[tokio::test]
    async fn impersonate_routes_pure_tls_fingerprint_site() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .build()
            .expect("client builds with impersonate");
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // Pure TLS-fingerprint protection — exactly the shape that
        // routes to the impersonate fetcher.
        site.protection = vec![crate::site::ProtectionKind::TlsFingerprint];
        let outcome = client.check(&site, &user()).await;
        assert_eq!(
            outcome.kind,
            MatchKind::Found,
            "expected Found (reason {:?})",
            outcome.reason,
        );
        // wreq's Chrome-134 emulation sets a Chrome-shaped User-Agent —
        // observable proof that the request came from the impersonate
        // path and not the default `adler/<version>` HTTP fetcher.
        let recvd = server.received_requests().await.expect("received requests");
        assert_eq!(recvd.len(), 1, "expected exactly one request");
        let ua = recvd[0]
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ua.contains("Chrome/"),
            "expected Chrome-shaped UA from wreq, got {ua:?}"
        );
    }

    #[tokio::test]
    async fn regex_check_pass_proceeds_to_probe() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // Pattern that matches "alice".
        site.regex_check = Some("^[a-z]{3,}$".into());
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
    }

    #[tokio::test]
    async fn status_signal_reports_found_on_match() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        assert!(outcome.url.ends_with("/alice"));
        assert!(outcome.reason.is_none());
        assert_eq!(outcome.evidence, ["HTTP 200 (status_found)"]);
    }

    #[tokio::test]
    async fn status_signal_pair_reports_not_found_on_404() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::StatusNotFound { codes: vec![404] },
            ],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::NotFound);
        // Only the NotFound-voting signal is cited as evidence.
        assert_eq!(outcome.evidence, ["HTTP 404 (status_not_found)"]);
    }

    #[tokio::test]
    async fn body_absent_signal_detects_missing_account() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<h1>Profile not found</h1>"))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![Signal::BodyAbsent {
                text: "Profile not found".into(),
            }],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::NotFound);
    }

    #[tokio::test]
    async fn body_absent_alone_yields_uncertain_when_marker_missing() {
        // Phase 2 semantics: absence of an absence-marker is not evidence
        // of presence — it just means we have no signal that fired.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<h1>Welcome alice</h1>"))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![Signal::BodyAbsent {
                text: "Profile not found".into(),
            }],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
    }

    #[tokio::test]
    async fn body_present_plus_absent_resolve_to_found() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<div class="profile-card">alice</div>"#),
            )
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![
                Signal::BodyPresent {
                    text: "profile-card".into(),
                },
                Signal::BodyAbsent {
                    text: "Profile not found".into(),
                },
            ],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
    }

    #[tokio::test]
    async fn redirect_absent_signal_detects_missing_account() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "/login?next=/alice"),
            )
            .mount(&server)
            .await;
        Mock::given(any())
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("login page"))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![Signal::RedirectAbsent {
                fragment: "/login".into(),
            }],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::NotFound);
    }

    #[tokio::test]
    async fn negative_signal_wins_over_positive() {
        // StatusFound votes Found (200 matches); BodyAbsent votes NotFound
        // (error marker appears). Negative-priority aggregation → NotFound.
        // This is the canonical Sherlock "message" pattern: a site that
        // returns 200 for everyone and differentiates via an error string.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Profile not found"))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::BodyAbsent {
                    text: "Profile not found".into(),
                },
            ],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::NotFound);
    }

    #[tokio::test]
    async fn network_failure_yields_uncertain() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let site = Site {
            name: "Dead".into(),
            url: UrlTemplate::new(format!("http://127.0.0.1:{port}/{{username}}")).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        let client = Client::builder()
            .timeout(Duration::from_millis(500))
            .connect_timeout(Duration::from_millis(500))
            .max_retries(0)
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(outcome.reason.is_some());
    }

    #[tokio::test]
    async fn throttle_spaces_consecutive_calls_to_same_host() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // Interval is intentionally much larger than typical wiremock latency
        // (≤10 ms locally, can spike under heavy parallel test load). Any
        // value too close to HTTP latency would let the first request burn
        // through the throttle window and make the assertion flaky.
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::from_millis(300))
            .build()
            .unwrap();

        client.check(&site, &user()).await;
        let started = Instant::now();
        client.check(&site, &user()).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(200),
            "second probe to the same host should wait ≥200 ms, got {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn builder_overrides_user_agent() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .and(wiremock::matchers::header("user-agent", "adler-test/1.0"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let client = Client::builder()
            .user_agent("adler-test/1.0")
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
    }

    #[tokio::test]
    async fn rate_limit_429_yields_uncertain_with_note() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert_eq!(outcome.reason, Some(UncertainReason::RateLimited));
    }

    #[tokio::test]
    async fn cloudflare_server_header_yields_uncertain() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(503).insert_header("server", "cloudflare"))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert_eq!(outcome.reason, Some(UncertainReason::CloudflareChallenge));
    }

    #[tokio::test]
    async fn cloudflare_interstitial_in_body_yields_uncertain() {
        // Body-based ban detection only runs when a signal already needs
        // the body — this site uses BodyAbsent so the body is read.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><head><title>Just a moment...</title></head></html>"),
            )
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![Signal::BodyAbsent {
                text: "Profile not found".into(),
            }],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert_eq!(outcome.reason, Some(UncertainReason::CloudflareChallenge));
    }

    #[tokio::test]
    async fn ban_detection_does_not_fire_on_legitimate_403() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![
                Signal::StatusFound { codes: vec![200] },
                Signal::StatusNotFound { codes: vec![403] },
            ],
        );
        let outcome = build_client().check(&site, &user()).await;
        // 403 is ambiguous for bans; site explicitly maps it to NotFound.
        assert_eq!(outcome.kind, MatchKind::NotFound);
        assert!(outcome.reason.is_none());
    }

    #[tokio::test]
    async fn retry_recovers_after_transient_429() {
        let server = MockServer::start().await;
        // First request: 429. Subsequent: 200.
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .max_retries(2)
            .base_backoff_delay(Duration::from_millis(20))
            .max_backoff_delay(Duration::from_millis(100))
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        assert!(outcome.reason.is_none());
    }

    #[tokio::test]
    async fn retry_exhausts_and_returns_uncertain() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .min_request_interval(Duration::ZERO)
            .max_retries(2)
            .base_backoff_delay(Duration::from_millis(10))
            .max_backoff_delay(Duration::from_millis(50))
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert_eq!(outcome.reason, Some(UncertainReason::RateLimited));
    }

    #[tokio::test]
    async fn retry_does_not_fire_on_network_error() {
        // Connection refused → Uncertain note starts with "request:", not a
        // ban marker. We must NOT retry — otherwise a single dead site
        // burns the full backoff budget before reporting.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let site = Site {
            name: "Dead".into(),
            url: UrlTemplate::new(format!("http://127.0.0.1:{port}/{{username}}")).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        let client = Client::builder()
            .timeout(Duration::from_millis(500))
            .connect_timeout(Duration::from_millis(500))
            .min_request_interval(Duration::ZERO)
            .max_retries(3)
            .base_backoff_delay(Duration::from_secs(60))
            .build()
            .unwrap();
        let started = Instant::now();
        let outcome = client.check(&site, &user()).await;
        // If retry fired, we'd be sleeping minutes; instead this returns
        // promptly with an Uncertain.
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(
            matches!(outcome.reason, Some(UncertainReason::Network(_))),
            "got {:?}",
            outcome.reason,
        );
    }

    #[tokio::test]
    async fn rotates_user_agent_per_request() {
        // The mock only matches when the request carries one of the pooled
        // UAs; if rotation weren't applied, the default adler/x.y UA would
        // miss and the verdict would be NotFound.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .and(wiremock::matchers::header("user-agent", "RotatorUA/9.9"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .rotate_user_agents(vec!["RotatorUA/9.9".into()])
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
    }

    #[test]
    fn invalid_proxy_url_fails_build() {
        let err = Client::builder().proxy("not a url").build().unwrap_err();
        assert!(matches!(err, Error::HttpSetup { .. }));
    }

    #[test]
    fn schemeless_proxy_is_rejected_up_front() {
        // reqwest would silently treat this as a host; we require a scheme.
        let err = Client::builder().proxy("not-a-url").build().unwrap_err();
        let Error::HttpSetup { message } = err else {
            panic!("expected HttpSetup, got {err:?}");
        };
        assert!(message.contains("must start with"), "{message}");
    }

    #[test]
    fn socks5_proxy_scheme_is_accepted() {
        // Valid scheme + endpoint builds fine (no connection is attempted).
        assert!(
            Client::builder()
                .proxy("socks5://127.0.0.1:9050")
                .build()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn global_rps_cap_spaces_requests_across_hosts() {
        // Two distinct host paths; per-host throttle is disabled, so any
        // spacing must come from the global RPS cap. 5 RPS → 200 ms apart.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site_a = Site {
            name: "A".into(),
            url: UrlTemplate::new(format!("{}/a/{{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        let site_b = Site {
            name: "B".into(),
            url: UrlTemplate::new(format!("{}/b/{{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        // 2 RPS → ~500 ms between requests. A large interval keeps the
        // assertion robust even when the first probe's own duration (which
        // eats into the measured gap) is inflated by test instrumentation
        // such as coverage tooling.
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .max_rps(std::num::NonZeroU32::new(2).unwrap())
            .build()
            .unwrap();
        // First request consumes the slot at t≈0; second waits ~500 ms even
        // though it targets a different host.
        client.check(&site_a, &user()).await;
        let started = Instant::now();
        client.check(&site_b, &user()).await;
        assert!(
            started.elapsed() >= Duration::from_millis(350),
            "global cap should space cross-host requests, got {:?}",
            started.elapsed(),
        );
    }

    #[tokio::test]
    async fn respect_robots_skips_disallowed_paths() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/robots.txt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /no"),
            )
            .mount(&server)
            .await;
        Mock::given(any())
            .and(path("/no/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(any())
            .and(path("/yes/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .respect_robots(true)
            .build()
            .unwrap();

        let disallowed = Site {
            name: "No".into(),
            url: UrlTemplate::new(format!("{}/no/{{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        let allowed = Site {
            name: "Yes".into(),
            url: UrlTemplate::new(format!("{}/yes/{{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: crate::site::HttpMethod::Get,
            request_body: None,
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };

        let no = client.check(&disallowed, &user()).await;
        assert_eq!(no.kind, MatchKind::Uncertain);
        assert_eq!(no.reason, Some(UncertainReason::RobotsDisallowed));

        let yes = client.check(&allowed, &user()).await;
        assert_eq!(yes.kind, MatchKind::Found);
    }

    #[tokio::test]
    async fn body_read_skipped_when_no_body_signal_needed() {
        // Mock returns body that would fail a body_absent check — but since
        // we only have a status signal, body is never read.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Profile not found"))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
    }

    // ===== Browser routing =====

    /// Test backend that returns a canned page and counts calls. Lets the
    /// routing tests assert "Client did/did not invoke the browser" without
    /// involving a real Chrome process.
    #[derive(Debug)]
    struct RecordingBackend {
        page: RenderedPage,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl RecordingBackend {
        fn with_page(page: RenderedPage) -> Self {
            Self {
                page,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl BrowserBackend for RecordingBackend {
        async fn fetch(
            &self,
            _url: &url::Url,
            _headers: &std::collections::BTreeMap<String, String>,
            _timeout: Duration,
        ) -> Result<RenderedPage> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.page.clone())
        }
    }

    fn site_bot_protected(server: &MockServer) -> Site {
        let mut s = site_with(server, vec![Signal::StatusFound { codes: vec![200] }]);
        s.tags = vec![BOT_PROTECTED_TAG.into()];
        s
    }

    #[tokio::test]
    async fn browser_routes_bot_protected_sites() {
        // wiremock would *not* fire (raw HTTP path is skipped) — the backend
        // returns its canned page directly.
        let server = MockServer::start().await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse("https://example.com/alice").unwrap(),
            body: "<html></html>".into(),
            elapsed_ms: 42,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(backend.clone())
            .build()
            .unwrap();
        let outcome = client.check(&site_bot_protected(&server), &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        assert_eq!(backend.call_count(), 1, "browser invoked exactly once");
    }

    #[tokio::test]
    async fn non_bot_protected_sites_skip_browser() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 500, // would make wiremock case fail if browser was taken
            final_url: url::Url::parse("https://x/").unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(backend.clone())
            .build()
            .unwrap();
        // site WITHOUT bot-protected tag → must go via raw HTTP (wiremock).
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        assert_eq!(backend.call_count(), 0, "browser must not be touched");
    }

    #[tokio::test]
    async fn browser_budget_exhaust_yields_uncertain() {
        let server = MockServer::start().await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse("https://x/").unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(backend.clone())
            .browser_budget(1)
            .build()
            .unwrap();
        let site = site_bot_protected(&server);
        // First call consumes the only slot.
        let first = client.check(&site, &user()).await;
        assert_eq!(first.kind, MatchKind::Found);
        // Second call hits the cap → Uncertain(BrowserBudget), backend NOT invoked.
        let second = client.check(&site, &user()).await;
        assert_eq!(second.kind, MatchKind::Uncertain);
        assert!(matches!(
            second.reason,
            Some(UncertainReason::BrowserBudget)
        ));
        assert_eq!(
            backend.call_count(),
            1,
            "second call must not invoke backend"
        );
    }

    #[tokio::test]
    async fn browser_failure_surfaces_as_uncertain_browser_failed() {
        struct FailingBackend;
        #[async_trait::async_trait]
        impl BrowserBackend for FailingBackend {
            async fn fetch(
                &self,
                _url: &url::Url,
                _headers: &std::collections::BTreeMap<String, String>,
                _timeout: Duration,
            ) -> Result<RenderedPage> {
                Err(Error::BrowserSetup {
                    message: "simulated crash".into(),
                })
            }
        }
        impl std::fmt::Debug for FailingBackend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("FailingBackend")
            }
        }

        let server = MockServer::start().await;
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(Arc::new(FailingBackend))
            .build()
            .unwrap();
        let outcome = client.check(&site_bot_protected(&server), &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        match outcome.reason {
            Some(UncertainReason::BrowserFailed(msg)) => {
                assert!(msg.contains("simulated crash"), "got: {msg}");
            }
            other => panic!("expected BrowserFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_only_site_uses_head_request() {
        // Site with only status signals (no body markers, no enrichment)
        // should be probed with HEAD — saves the body download on
        // ~30% of the registry.
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(recvd.len(), 1);
        assert_eq!(recvd[0].method.as_str(), "HEAD");
    }

    #[tokio::test]
    async fn body_signal_site_uses_get_request() {
        // Same baseline plus a body-marker signal — must still GET so
        // the body actually arrives for matching.
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello alice"))
            .mount(&server)
            .await;
        let site = site_with(
            &server,
            vec![Signal::BodyPresent {
                text: "hello".into(),
            }],
        );
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(recvd[0].method.as_str(), "GET");
    }

    #[tokio::test]
    async fn protection_field_routes_through_browser_like_bot_protected_tag() {
        // A site that declares `protection: [Cloudflare]` but doesn't
        // carry the legacy `bot-protected` tag should still route
        // through the browser backend — the new structured field is
        // an additional signal, not a tag replacement.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        site.protection = vec![crate::site::ProtectionKind::Cloudflare];
        // No bot-protected tag — pure structured-field test.
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse(&format!("{}/alice", server.uri())).unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(backend)
            .build()
            .unwrap();
        let outcome = client.check(&site, &user()).await;
        // The recording backend always returns a synthetic 200, so
        // Found means we went through the browser path.
        assert_eq!(outcome.kind, MatchKind::Found);
        // No raw HTTP probe should have hit the mock server.
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            recvd.len(),
            0,
            "structured protection must skip the raw HTTP path"
        );
    }

    #[tokio::test]
    async fn user_auth_protection_alone_uses_http_session_path() {
        let server = MockServer::start().await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 500,
            final_url: url::Url::parse("https://x/").unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(backend.clone())
            .build()
            .unwrap();
        let mut site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        site.protection = vec![ProtectionKind::UserAuth];

        let outcome = client.check(&site, &user()).await;

        assert_eq!(outcome.kind, MatchKind::Found);
        assert_eq!(
            backend.call_count(),
            0,
            "user-auth alone must not invoke browser"
        );
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(recvd.len(), 1, "user-auth alone should use raw HTTP");
    }

    #[tokio::test]
    async fn post_method_sends_body_with_username_substituted() {
        // A POST-probed site (e.g. Anilist GraphQL) — the username
        // goes in the body, not the URL. Adler should substitute
        // `{username}` and send a POST with the rendered payload.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // URL substitution still requires the `{username}` placeholder,
        // even for POST sites where the username also lives in the
        // body. Most real POST endpoints encode the username in both
        // (e.g. query string + body); we mirror that.
        let site = Site {
            name: "ApiPost".into(),
            url: UrlTemplate::new(format!("{}/api?_={{username}}", server.uri())).unwrap(),
            signals: vec![Signal::StatusFound { codes: vec![200] }],
            known_present: None,
            known_absent: None,
            extract: Vec::new(),
            tags: Vec::new(),
            request_headers: std::collections::BTreeMap::new(),
            regex_check: None,
            engine: None,
            strip_bad_char: None,
            request_method: HttpMethod::Post,
            request_body: Some(r#"{"name":"{username}"}"#.into()),
            protection: Vec::new(),
            disabled: false,
            disabled_reason: None,
            source: None,
            popularity: None,
            access: crate::AccessPolicy::default(),
        };
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(recvd.len(), 1);
        assert_eq!(recvd[0].method.as_str(), "POST");
        let body = String::from_utf8_lossy(&recvd[0].body).to_string();
        assert!(body.contains("\"name\":\"alice\""), "body was: {body}");
    }

    #[tokio::test]
    async fn head_405_falls_back_to_get() {
        // A server that rejects HEAD with 405 — Adler should silently
        // retry with GET so the optimisation can never cost accuracy.
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        Mock::given(any())
            .and(path("/alice"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        let recvd = server.received_requests().await.unwrap_or_default();
        assert_eq!(recvd.len(), 2);
        assert_eq!(recvd[0].method.as_str(), "HEAD");
        assert_eq!(recvd[1].method.as_str(), "GET");
    }

    // ------------------------------------------------------------------
    // Phase 4 — automatic escalation when the cheap transport hits a
    // Cloudflare / rate-limit Uncertain that the browser could resolve.
    // ------------------------------------------------------------------

    /// Mocked HTTP that always responds with a Cloudflare 503 (server
    /// header + 503 status — what the pre-body ban detector turns into
    /// `Uncertain(CloudflareChallenge)`).
    async fn cloudflare_503_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(503).insert_header("server", "cloudflare"))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn http_success_stamps_http_transport_no_escalations() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = build_client().check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Found);
        assert_eq!(
            outcome.transport,
            Some(crate::escalation::TransportTier::Http),
            "successful HTTP probe must stamp Http transport"
        );
        assert_eq!(outcome.escalations, 0, "no escalation on the happy path");
    }

    #[tokio::test]
    async fn escalates_cloudflare_uncertain_to_browser_and_stamps_one() {
        let server = cloudflare_503_server().await;
        // Browser returns a 200 that the StatusFound signal turns into Found.
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse(&format!("{}/alice", server.uri())).unwrap(),
            body: String::new(),
            elapsed_ms: 5,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(Arc::clone(&backend) as Arc<dyn BrowserBackend>)
            .build()
            .unwrap();
        // Non-bot-protected site — HTTP path runs first, hits Cloudflare,
        // escalation routes to the browser, browser's 200 → Found.
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = client.check(&site, &user()).await;
        assert_eq!(
            outcome.kind,
            MatchKind::Found,
            "escalation should flip CF challenge to Found via browser (reason {:?})",
            outcome.reason
        );
        assert_eq!(
            outcome.transport,
            Some(crate::escalation::TransportTier::Browser),
            "escalated outcome must be stamped Browser"
        );
        assert_eq!(
            outcome.escalations, 1,
            "exactly one escalation should have fired"
        );
        assert_eq!(backend.call_count(), 1, "browser invoked exactly once");
    }

    #[tokio::test]
    async fn disable_escalation_leaves_cloudflare_uncertain_untouched() {
        let server = cloudflare_503_server().await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse(&format!("{}/alice", server.uri())).unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(Arc::clone(&backend) as Arc<dyn BrowserBackend>)
            .disable_escalation()
            .build()
            .unwrap();
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(matches!(
            outcome.reason,
            Some(UncertainReason::CloudflareChallenge)
        ));
        assert_eq!(
            outcome.transport,
            Some(crate::escalation::TransportTier::Http),
            "primary transport must still be stamped"
        );
        assert_eq!(outcome.escalations, 0);
        assert_eq!(
            backend.call_count(),
            0,
            "browser must not be touched when --no-escalation"
        );
    }

    #[tokio::test]
    async fn escalation_budget_zero_keeps_browser_untouched() {
        let server = cloudflare_503_server().await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse(&format!("{}/alice", server.uri())).unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(Arc::clone(&backend) as Arc<dyn BrowserBackend>)
            .escalation_budget(0)
            .build()
            .unwrap();
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        let outcome = client.check(&site, &user()).await;
        assert_eq!(outcome.kind, MatchKind::Uncertain);
        assert!(matches!(
            outcome.reason,
            Some(UncertainReason::CloudflareChallenge)
        ));
        assert_eq!(outcome.escalations, 0);
        assert_eq!(
            backend.call_count(),
            0,
            "zero budget must deny every escalation"
        );
    }

    #[tokio::test]
    async fn escalation_consumes_budget_then_stops() {
        let server = cloudflare_503_server().await;
        let backend = Arc::new(RecordingBackend::with_page(RenderedPage {
            status: 200,
            final_url: url::Url::parse(&format!("{}/alice", server.uri())).unwrap(),
            body: String::new(),
            elapsed_ms: 0,
        }));
        let client = Client::builder()
            .min_request_interval(Duration::ZERO)
            .max_retries(0)
            .browser(Arc::clone(&backend) as Arc<dyn BrowserBackend>)
            .escalation_budget(1)
            .build()
            .unwrap();
        let site = site_with(&server, vec![Signal::StatusFound { codes: vec![200] }]);
        // First call burns the only escalation slot.
        let first = client.check(&site, &user()).await;
        assert_eq!(first.kind, MatchKind::Found);
        assert_eq!(first.escalations, 1);
        // Second call's escalation is denied → cheap-path Uncertain survives.
        let second = client.check(&site, &user()).await;
        assert_eq!(second.kind, MatchKind::Uncertain);
        assert!(matches!(
            second.reason,
            Some(UncertainReason::CloudflareChallenge)
        ));
        assert_eq!(second.escalations, 0);
        assert_eq!(backend.call_count(), 1, "browser called exactly once total");
    }
}
