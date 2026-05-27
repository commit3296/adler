//! HTTP client wrapping `reqwest`, plus the per-site probe entry point.
//!
//! The wrapper exists to keep `reqwest` out of Adler's public API surface.
//! All knobs that future modules need (timeouts, redirect policy, user agent)
//! are configured through [`ClientBuilder`]; per-request transient failures
//! never bubble up as errors — they become
//! [`MatchKind::Uncertain`](crate::MatchKind::Uncertain) on the returned
//! outcome.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::redirect;

use crate::ban;
use crate::browser::{BrowserBackend, BrowserBudget, RenderedPage};
use crate::check::{CheckOutcome, MatchKind, UncertainReason};
use crate::error::{Error, Result};
use crate::retry::{self, RetryPolicy};
use crate::robots::RobotsCache;
use crate::site::{HttpMethod, Probe, Signal, SignalVerdict, Site, aggregate};
use crate::throttle::HostThrottle;
use crate::username::Username;

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
    inner: reqwest::Client,
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
    /// Per-scan cap on browser fetches. Shared across `Client::check` calls
    /// for a single scan, so several tasks compete for the same budget.
    browser_budget: Arc<BrowserBudget>,
}

impl Client {
    /// Start configuring a new client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Probe a single site for `username`, retrying on transient bans.
    ///
    /// Network failures, timeouts, and unexpected response shapes all yield
    /// [`MatchKind::Uncertain`] with a descriptive note. The method never
    /// returns an error: at the executor level we want a partial result for
    /// every site, not abort-on-first-failure semantics.
    ///
    /// When ban detection classifies a response as `rate_limited` /
    /// `cloudflare_challenge`, the call is retried with jittered exponential
    /// backoff (configurable via [`ClientBuilder::max_retries`]). Non-ban
    /// Uncertain (network errors, body read failures) is **not** retried —
    /// those failures rarely fix themselves in the seconds-to-minutes window
    /// we'd block for.
    #[tracing::instrument(skip(self), fields(site = %site.name, user = %username))]
    pub async fn check(&self, site: &Site, username: &Username) -> CheckOutcome {
        let mut attempt: u32 = 0;
        loop {
            let outcome = self.probe_once(site, username).await;
            if !retry::should_retry(&outcome, attempt, &self.retry) {
                return outcome;
            }
            let delay = retry::backoff_delay(attempt, &self.retry);
            tracing::info!(
                site = %site.name,
                attempt = attempt + 1,
                reason = outcome.reason.as_ref().map(ToString::to_string).unwrap_or_default(),
                ?delay,
                "transient ban, retrying",
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// Fetch a URL and return raw response data (status, final URL, body)
    /// with the same throttle / User-Agent / proxy machinery as `check`,
    /// but without signal evaluation or retry.
    ///
    /// Returns `None` on any network/transport error. Intended for
    /// diagnostics such as `adler --doctor --fix`, which diffs the
    /// responses for a known-present and a nonsense user to derive a
    /// signature.
    pub async fn fetch(&self, url: &str) -> Option<RawResponse> {
        let host = host_of(url);
        if let Some(global) = &self.global_throttle {
            global.wait(GLOBAL_THROTTLE_KEY).await;
        }
        self.throttle.wait(&host).await;
        let mut request = self.inner.get(url);
        if let Some(ua) = self.pick_user_agent() {
            request = request.header(reqwest::header::USER_AGENT, ua);
        }
        let response = request.send().await.ok()?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let body = response.text().await.unwrap_or_default();
        Some(RawResponse {
            status,
            final_url,
            body,
        })
    }

    /// Same as [`Self::fetch`] but routes through the configured browser
    /// backend when the site is tagged `bot-protected` and a backend is
    /// available. Used by [`doctor::suggest_fix`](crate::doctor::suggest_fix)
    /// so that the diff-derivation works against the JS-rendered page
    /// (login wall vs. real profile) rather than two identical raw-HTTP
    /// shells.
    ///
    /// Falls back to raw HTTP if (a) no browser is configured, (b) the
    /// site isn't `bot-protected`, or (c) the browser fetch fails — so
    /// callers get the same `Option<RawResponse>` shape either way.
    pub async fn fetch_for_doctor(&self, site: &Site, url: &str) -> Option<RawResponse> {
        if let Some(backend) = self.browser.as_deref() {
            let has_tag = site
                .tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case(BOT_PROTECTED_TAG));
            if has_tag || !site.protection.is_empty() {
                let parsed = url::Url::parse(url).ok()?;
                match backend
                    .fetch(&parsed, &site.request_headers, BROWSER_TIMEOUT)
                    .await
                {
                    Ok(page) => {
                        return Some(RawResponse {
                            status: page.status,
                            final_url: page.final_url.to_string(),
                            body: page.body,
                        });
                    }
                    Err(err) => {
                        tracing::warn!(
                            site = %site.name, %url, error = %err,
                            "browser fetch failed in doctor; falling back to raw HTTP",
                        );
                    }
                }
            }
        }
        self.fetch(url).await
    }

    /// Pick a User-Agent for the next request from the rotation pool, or
    /// `None` to fall back on the client's fixed header.
    fn pick_user_agent(&self) -> Option<&str> {
        match self.user_agents.len() {
            0 => None,
            1 => Some(&self.user_agents[0]),
            n => Some(&self.user_agents[fastrand::usize(0..n)]),
        }
    }

    // Splitting probe_once into helpers would scatter the request/response
    // flow that has to read top-to-bottom; one long function reads better.
    #[allow(clippy::too_many_lines)]
    async fn probe_once(&self, site: &Site, username: &Username) -> CheckOutcome {
        let url = site.url_for(username);

        // Site-level username constraint (Sherlock's `regexCheck`).
        // Mismatch → skip the probe entirely. Saves a request and
        // sidesteps the false-positive class where a site 404s on
        // illegal usernames in a way our signal can't distinguish
        // from a missing account. If the pattern fails to compile
        // (Sherlock occasionally uses lookarounds, which our `regex`
        // crate can't express), we let validate's warn-log stand
        // and silently fall through — the rest of the probe still
        // works.
        if let Some(pat) = &site.regex_check {
            if let Ok(re) = regex::Regex::new(pat) {
                if !re.is_match(username.as_str()) {
                    return uncertain(
                        &site.name,
                        url,
                        Instant::now(),
                        UncertainReason::UsernameNotAllowed,
                    );
                }
            }
        }

        // Auto-route bot-protected sites through the browser backend when
        // one is configured. Raw HTTP can't see past their JS/login wall,
        // so this is the only way they ever produce a Found verdict.
        // A site is "bot-protected" in the routing sense if it carries
        // the legacy tag OR declares any specific protection mechanism
        // via the new `protection` field — either signal is enough.
        if let Some(backend) = self.browser.as_deref() {
            let has_tag = site
                .tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case(BOT_PROTECTED_TAG));
            if has_tag || !site.protection.is_empty() {
                if self.browser_budget.try_consume() {
                    return self.probe_with_browser(site, &url, backend).await;
                }
                tracing::warn!(site = %site.name, "browser budget exhausted");
                return uncertain(
                    &site.name,
                    url,
                    Instant::now(),
                    UncertainReason::BrowserBudget,
                );
            }
        }

        let host = host_of(&url);

        // robots.txt gate, before consuming a throttle slot or probing.
        if let Some(robots) = &self.robots {
            if let Some((origin, path)) = origin_and_path(&url) {
                if !robots.allowed(&origin, &path).await {
                    tracing::debug!(%url, "skipped by robots.txt");
                    return uncertain(
                        &site.name,
                        url,
                        Instant::now(),
                        UncertainReason::RobotsDisallowed,
                    );
                }
            }
        }

        // Global cap first (gates every request), then per-host spacing.
        if let Some(global) = &self.global_throttle {
            global.wait(GLOBAL_THROTTLE_KEY).await;
        }
        self.throttle.wait(&host).await;
        let started = Instant::now();
        tracing::debug!(%url, %host, "probing");

        // Read the body if a signal needs it, or if enrichment is on and the
        // site has extractor rules (extraction needs the body).
        let want_enrich = self.enrich && !site.extract.is_empty();
        let needs_body = want_enrich || site.signals.iter().any(crate::site::Signal::needs_body);

        // POST sites carry their own body payload (the username goes in
        // the body, not the URL — e.g. Anilist's GraphQL endpoint).
        // HEAD optimisation only applies to GET-probed sites: a HEAD
        // for a POST endpoint would defeat its purpose. Body
        // substitution mirrors URL substitution: `{username}` in
        // `Site::request_body` is replaced before sending.
        let body_for_post: Option<String> = if matches!(site.request_method, HttpMethod::Post) {
            const USERNAME_PH: &str = "{username}";
            site.request_body
                .as_deref()
                .map(|t| t.replace(USERNAME_PH, username.as_str()))
        } else {
            None
        };

        // For status-only sites (only StatusFound / StatusNotFound /
        // RedirectAbsent signals, no enrichment), HEAD avoids the body
        // download entirely — saving bandwidth and time on the
        // ~30% of the registry that doesn't need a body marker.
        // Some servers reject HEAD with 405; we transparently retry
        // with GET so the optimisation never costs accuracy. POST
        // probes always go out as POST regardless of body needs.
        let response = match site.request_method {
            HttpMethod::Post => {
                send_request_with_body(
                    &self.inner,
                    reqwest::Method::POST,
                    &url,
                    self.pick_user_agent(),
                    body_for_post.as_deref(),
                )
                .await
            }
            HttpMethod::Get if needs_body => {
                send_request(
                    &self.inner,
                    reqwest::Method::GET,
                    &url,
                    self.pick_user_agent(),
                )
                .await
            }
            HttpMethod::Get => {
                match send_request(
                    &self.inner,
                    reqwest::Method::HEAD,
                    &url,
                    self.pick_user_agent(),
                )
                .await
                {
                    Ok(r) if r.status().as_u16() == 405 => {
                        send_request(
                            &self.inner,
                            reqwest::Method::GET,
                            &url,
                            self.pick_user_agent(),
                        )
                        .await
                    }
                    other => other,
                }
            }
        };
        let response = match response {
            Ok(r) => r,
            Err(err) => {
                tracing::debug!(error = %err, "request failed");
                return uncertain(
                    &site.name,
                    url,
                    started,
                    UncertainReason::Network(err.to_string()),
                );
            }
        };

        let status = response.status().as_u16();
        let final_url = response.url().to_string();

        if let Some(reason) = ban::detect_pre_body(status, response.headers()) {
            tracing::warn!(%host, status, %reason, "ban-like response");
            return uncertain(&site.name, url, started, reason);
        }
        let body = if needs_body {
            match response.text().await {
                Ok(b) => b,
                Err(err) => {
                    return uncertain(
                        &site.name,
                        url,
                        started,
                        UncertainReason::BodyRead(err.to_string()),
                    );
                }
            }
        } else {
            String::new()
        };

        if !body.is_empty() {
            if let Some(reason) = ban::detect_in_body(&body) {
                tracing::warn!(%host, %reason, "ban-like body");
                return uncertain(&site.name, url, started, reason);
            }
        }

        let probe = Probe {
            status,
            final_url: &final_url,
            body: &body,
        };
        let votes: Vec<(&Signal, SignalVerdict)> = site
            .signals
            .iter()
            .map(|s| (s, s.evaluate(&probe)))
            .collect();
        let kind = aggregate(votes.iter().map(|(_, v)| *v));
        let mut result = outcome(&site.name, url, started, kind);
        // Record which signals produced the verdict (the winning polarity).
        let winning = match kind {
            MatchKind::Found => Some(SignalVerdict::Found),
            MatchKind::NotFound => Some(SignalVerdict::NotFound),
            MatchKind::Uncertain => None,
        };
        if let Some(want) = winning {
            result.evidence = votes
                .iter()
                .filter(|(_, v)| *v == want)
                .map(|(s, _)| s.describe_match(&probe))
                .collect();
        }
        if want_enrich && kind == MatchKind::Found {
            result.enrichment = crate::enrich::extract(&body, &site.extract);
        }
        result
    }

    /// Render `url` through the configured [`BrowserBackend`] and run the
    /// same signal pipeline on the result. Per-fetch failures (timeout,
    /// navigation error, etc.) surface as `Uncertain(BrowserFailed)` so
    /// one flaky bot-protected site can't abort the scan.
    async fn probe_with_browser(
        &self,
        site: &Site,
        url: &str,
        backend: &dyn BrowserBackend,
    ) -> CheckOutcome {
        let started = Instant::now();
        let parsed = match url::Url::parse(url) {
            Ok(u) => u,
            Err(err) => {
                return uncertain(
                    &site.name,
                    url.to_owned(),
                    started,
                    UncertainReason::Other(format!("invalid url: {err}")),
                );
            }
        };

        let page: RenderedPage = match backend
            .fetch(&parsed, &site.request_headers, BROWSER_TIMEOUT)
            .await
        {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(site = %site.name, %url, error = %err, "browser fetch failed");
                return uncertain(
                    &site.name,
                    url.to_owned(),
                    started,
                    UncertainReason::BrowserFailed(err.to_string()),
                );
            }
        };

        let final_url_str = page.final_url.as_str().to_owned();
        let probe = Probe {
            status: page.status,
            final_url: &final_url_str,
            body: &page.body,
        };
        let votes: Vec<(&Signal, SignalVerdict)> = site
            .signals
            .iter()
            .map(|s| (s, s.evaluate(&probe)))
            .collect();
        let kind = aggregate(votes.iter().map(|(_, v)| *v));
        let mut result = outcome(&site.name, url.to_owned(), started, kind);
        let winning = match kind {
            MatchKind::Found => Some(SignalVerdict::Found),
            MatchKind::NotFound => Some(SignalVerdict::NotFound),
            MatchKind::Uncertain => None,
        };
        if let Some(want) = winning {
            result.evidence = votes
                .iter()
                .filter(|(_, v)| *v == want)
                .map(|(s, _)| s.describe_match(&probe))
                .collect();
        }
        if self.enrich && kind == MatchKind::Found && !site.extract.is_empty() {
            result.enrichment = crate::enrich::extract(&page.body, &site.extract);
        }
        result
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

/// Builder for [`Client`].
#[derive(Clone)]
#[must_use = "ClientBuilder does nothing until `.build()` is called"]
pub struct ClientBuilder {
    timeout: Duration,
    connect_timeout: Duration,
    user_agent: String,
    follow_redirects: bool,
    redirect_limit: usize,
    min_request_interval: Duration,
    max_rps: Option<NonZeroU32>,
    retry: RetryPolicy,
    proxy: Option<String>,
    user_agents: Vec<String>,
    enrich: bool,
    respect_robots: bool,
    browser: Option<Arc<dyn BrowserBackend>>,
    browser_budget: usize,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            user_agent: default_user_agent(),
            follow_redirects: true,
            redirect_limit: DEFAULT_REDIRECT_LIMIT,
            min_request_interval: DEFAULT_PER_HOST_INTERVAL,
            max_rps: None,
            retry: RetryPolicy::default(),
            proxy: None,
            user_agents: Vec::new(),
            enrich: false,
            respect_robots: false,
            browser: None,
            browser_budget: DEFAULT_BROWSER_BUDGET,
        }
    }
}

impl ClientBuilder {
    /// Per-request timeout (covers connect, headers, and body read).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// TCP-connect timeout, applied independently of the request timeout.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the `User-Agent` header sent on every request.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Toggle automatic redirect following. Defaults to `true`; disable when
    /// using [`crate::Signal::RedirectAbsent`] is undesirable for a run.
    pub fn follow_redirects(mut self, follow: bool) -> Self {
        self.follow_redirects = follow;
        self
    }

    /// Minimum time between consecutive requests to the same host.
    ///
    /// Defaults to 100 ms (≈ 10 RPS per host) — enough headroom to avoid
    /// rate-limit responses on common OSINT targets while keeping fan-out
    /// across many sites fast.
    pub fn min_request_interval(mut self, interval: Duration) -> Self {
        self.min_request_interval = interval;
        self
    }

    /// Cap the total request rate across *all* hosts to `rps` requests per
    /// second. Independent of (and composed with) the per-host interval —
    /// useful on a metered connection or behind a shared-quota proxy.
    /// Uncapped by default.
    pub fn max_rps(mut self, rps: NonZeroU32) -> Self {
        self.max_rps = Some(rps);
        self
    }

    /// Maximum retry attempts after a transient ban response. Defaults to 2
    /// (so up to 3 total tries). Set to `0` to disable retry entirely.
    pub fn max_retries(mut self, n: u32) -> Self {
        self.retry.max_retries = n;
        self
    }

    /// Base delay for the first retry. Subsequent retries double until
    /// reaching [`Self::max_backoff_delay`]. Defaults to 500 ms.
    pub fn base_backoff_delay(mut self, d: Duration) -> Self {
        self.retry.base_delay = d;
        self
    }

    /// Cap on a single backoff delay (pre-jitter). Defaults to 30 s.
    pub fn max_backoff_delay(mut self, d: Duration) -> Self {
        self.retry.max_delay = d;
        self
    }

    /// Route all requests through a proxy. Accepts `http://`, `https://`,
    /// and `socks5://` URLs. For Tor, pass `socks5://127.0.0.1:9050`.
    pub fn proxy(mut self, url: impl Into<String>) -> Self {
        self.proxy = Some(url.into());
        self
    }

    /// Rotate the `User-Agent` header per request, picking uniformly at
    /// random from `agents`. An empty list (the default) keeps the single
    /// fixed User-Agent. Useful for reducing trivial fingerprinting.
    pub fn rotate_user_agents(mut self, agents: Vec<String>) -> Self {
        self.user_agents = agents;
        self
    }

    /// Extract profile fields (per [`crate::Site::extract`]) from `Found`
    /// pages. Off by default; enables an extra body read for matching sites.
    pub fn enrich(mut self, enrich: bool) -> Self {
        self.enrich = enrich;
        self
    }

    /// Honor each host's `robots.txt`: probes to disallowed paths are
    /// skipped (reported `Uncertain`, note `robots_disallowed`). Off by
    /// default. Adds one cached `robots.txt` fetch per origin.
    pub fn respect_robots(mut self, respect: bool) -> Self {
        self.respect_robots = respect;
        self
    }

    /// Attach a browser backend. Sites tagged `bot-protected` will be
    /// routed through it instead of the raw HTTP path, up to the
    /// [`browser_budget`](Self::browser_budget) cap.
    pub fn browser(mut self, backend: Arc<dyn BrowserBackend>) -> Self {
        self.browser = Some(backend);
        self
    }

    /// Per-scan cap on how many `bot-protected` sites are allowed to use
    /// the browser backend. Once exhausted, the rest fall back to
    /// `Uncertain(BrowserBudget)`. Defaults to
    /// [`DEFAULT_BROWSER_BUDGET`].
    pub const fn browser_budget(mut self, cap: usize) -> Self {
        self.browser_budget = cap;
        self
    }

    /// Build a [`Client`].
    pub fn build(self) -> Result<Client> {
        let redirect_policy = if self.follow_redirects {
            redirect::Policy::limited(self.redirect_limit)
        } else {
            redirect::Policy::none()
        };
        let mut builder = reqwest::Client::builder()
            .user_agent(self.user_agent)
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .redirect(redirect_policy);
        if let Some(proxy_url) = &self.proxy {
            // reqwest treats a schemeless string (e.g. "not-a-url") as a host
            // and silently defaults it to http://, so every probe would fail
            // confusingly. Require an explicit, supported scheme up front.
            const SCHEMES: [&str; 4] = ["http://", "https://", "socks5://", "socks5h://"];
            if !SCHEMES.iter().any(|s| proxy_url.starts_with(s)) {
                return Err(Error::HttpSetup {
                    message: format!(
                        "invalid proxy {proxy_url:?}: must start with one of {}",
                        SCHEMES.join(", ")
                    ),
                });
            }
            let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| Error::HttpSetup {
                message: format!("invalid proxy {proxy_url:?}: {e}"),
            })?;
            builder = builder.proxy(proxy);
        }
        let inner = builder.build().map_err(|e| Error::HttpSetup {
            message: e.to_string(),
        })?;
        let global_throttle = self.max_rps.map(|rps| {
            // Min spacing between any two requests = 1s / rps.
            let interval = Duration::from_secs(1) / rps.get();
            HostThrottle::new(interval)
        });
        let robots = self
            .respect_robots
            .then(|| RobotsCache::new(inner.clone(), "adler"));
        Ok(Client {
            inner,
            throttle: HostThrottle::new(self.min_request_interval),
            global_throttle,
            retry: self.retry,
            user_agents: Arc::from(self.user_agents),
            enrich: self.enrich,
            robots,
            browser: self.browser,
            browser_budget: Arc::new(BrowserBudget::new(self.browser_budget)),
        })
    }
}

/// Default ceiling on browser-backed probes per scan when no other value
/// is specified.
///
/// Sized as ~5× the typical `bot-protected` registry subset — comfortable
/// headroom while still being a guardrail against a misconfigured flag
/// burning a whole Browserbase quota.
pub const DEFAULT_BROWSER_BUDGET: usize = 50;

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
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("user_agent", &self.user_agent)
            .field("follow_redirects", &self.follow_redirects)
            .field("redirect_limit", &self.redirect_limit)
            .field("min_request_interval", &self.min_request_interval)
            .field("max_rps", &self.max_rps)
            .field("retry", &self.retry)
            .field("proxy", &self.proxy)
            .field("user_agents", &self.user_agents)
            .field("enrich", &self.enrich)
            .field("respect_robots", &self.respect_robots)
            .field("browser", &self.browser.is_some())
            .field("browser_budget", &self.browser_budget)
            .finish()
    }
}

/// Per-fetch timeout passed to [`BrowserBackend::fetch`]. Browser fetches
/// (JS execution + waits) are inherently slower than raw HTTP, so this is
/// generous on purpose.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(60);

const BOT_PROTECTED_TAG: &str = "bot-protected";

fn default_user_agent() -> String {
    format!("adler/{}", env!("CARGO_PKG_VERSION"))
}

/// Issue a single HTTP request with the configured client, an optional
/// User-Agent override, and the given method. Centralised so the probe
/// path can transparently swap HEAD for GET (and retry on 405) without
/// duplicating the request-build logic.
async fn send_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    ua: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    send_request_with_body(client, method, url, ua, None).await
}

/// Same as [`send_request`] but with an optional request body — used
/// for POST probes against API endpoints (GraphQL, login form, …).
/// When `body` is `Some`, the request is sent with a `application/json`
/// content type by default; sites that need a different content type
/// declare it through [`Site::request_headers`].
async fn send_request_with_body(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    ua: Option<&str>,
    body: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    let mut request = client.request(method, url);
    if let Some(ua) = ua {
        request = request.header(reqwest::header::USER_AGENT, ua);
    }
    if let Some(b) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(b.to_owned());
    }
    request.send().await
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// Split a URL into its origin (`scheme://host[:port]`) and path-with-query,
/// for `robots.txt` lookup. `None` if the URL won't parse or lacks a host.
fn origin_and_path(url: &str) -> Option<(String, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port().map_or_else(String::new, |p| format!(":{p}"));
    let origin = format!("{}://{host}{port}", parsed.scheme());
    let path = parsed.query().map_or_else(
        || parsed.path().to_owned(),
        |q| format!("{}?{q}", parsed.path()),
    );
    Some((origin, path))
}

fn outcome(site: &str, url: String, started: Instant, kind: MatchKind) -> CheckOutcome {
    CheckOutcome {
        site: site.to_owned(),
        url,
        kind,
        reason: None,
        elapsed_ms: elapsed_ms(started),
        enrichment: std::collections::BTreeMap::new(),
        evidence: Vec::new(),
    }
}

fn uncertain(site: &str, url: String, started: Instant, reason: UncertainReason) -> CheckOutcome {
    CheckOutcome {
        site: site.to_owned(),
        url,
        kind: MatchKind::Uncertain,
        reason: Some(reason),
        elapsed_ms: elapsed_ms(started),
        enrichment: std::collections::BTreeMap::new(),
        evidence: Vec::new(),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{Signal, UrlTemplate};
    use wiremock::matchers::{any, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn build_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(2))
            // Tests share `127.0.0.1` as host — keep throttle out of the
            // way for everything but the dedicated throttle test below.
            .min_request_interval(Duration::ZERO)
            // Default retry would re-hit ban-test mocks; tests opt in
            // explicitly when they want to exercise the retry path.
            .max_retries(0)
            .build()
            .expect("client builds")
    }

    fn site_with(server: &MockServer, signals: Vec<Signal>) -> Site {
        Site {
            name: "Mock".into(),
            url: UrlTemplate::new(format!("{}/{{username}}", server.uri())).unwrap(),
            signals,
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
            source: None,
            popularity: None,
        }
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
            source: None,
            popularity: None,
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
            source: None,
            popularity: None,
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
            source: None,
            popularity: None,
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
            source: None,
            popularity: None,
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
            source: None,
            popularity: None,
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
            source: None,
            popularity: None,
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
        s.tags = vec!["bot-protected".into()];
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
            source: None,
            popularity: None,
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
}
