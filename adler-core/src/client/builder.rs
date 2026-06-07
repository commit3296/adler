//! `ClientBuilder` — public configuration surface for [`Client`].
//!
//! Every CLI flag that affects HTTP behaviour (timeout / retries /
//! proxy / Tor / UA rotation / egress pool / sessions / browser
//! backend / escalation budget) maps onto a method here. The builder
//! pattern lets `.build()` enforce derived invariants (the impersonate
//! transport must initialise before the client is returned) without
//! exposing them as fallible setters.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use reqwest::redirect;

use crate::access::{EgressPool, EgressSpec, SessionStore};
use crate::browser::{BrowserBackend, BrowserBudget};
use crate::error::{Error, Result};
use crate::retry::RetryPolicy;
use crate::robots::RobotsCache;
use crate::throttle::HostThrottle;
use crate::transport::HttpFetcher;
#[cfg(feature = "impersonate")]
use crate::transport::ImpersonateFetcher;

use super::util::default_user_agent;
use super::{
    Client, DEFAULT_CONNECT_TIMEOUT, DEFAULT_PER_HOST_INTERVAL, DEFAULT_REDIRECT_LIMIT,
    DEFAULT_TIMEOUT,
};

/// Builder for [`Client`].
#[derive(Clone)]
#[must_use = "ClientBuilder does nothing until `.build()` is called"]
// A configuration builder accumulates many small flags; the four bool
// fields here are semantically independent (redirect / enrich /
// respect-robots / escalation), so collapsing them into a state machine
// or enum would obscure rather than clarify.
#[allow(clippy::struct_excessive_bools)]
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
    egress: Vec<EgressSpec>,
    sessions: SessionStore,
    escalation_budget: usize,
    escalation_enabled: bool,
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
            egress: Vec::new(),
            sessions: SessionStore::new(),
            escalation_budget: DEFAULT_ESCALATION_BUDGET,
            escalation_enabled: true,
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

    /// Per-scan cap on automatic escalations from the cheap transport
    /// (HTTP / impersonate) to the browser when the cheap path returns
    /// `Uncertain(CloudflareChallenge | RateLimited)`. Independent of
    /// [`browser_budget`](Self::browser_budget). Defaults to
    /// [`DEFAULT_ESCALATION_BUDGET`]. `cap = 0` is equivalent to
    /// [`disable_escalation`](Self::disable_escalation).
    pub const fn escalation_budget(mut self, cap: usize) -> Self {
        self.escalation_budget = cap;
        self
    }

    /// Disable automatic escalation entirely — the cheap transport's
    /// outcome is returned verbatim, even when its `Uncertain` reason is
    /// one a browser fetch would resolve. Useful for benchmarking the
    /// raw HTTP signals without the access-engine lift on top.
    pub const fn disable_escalation(mut self) -> Self {
        self.escalation_enabled = false;
        self
    }

    /// Configure the egress pool: proxies tagged by country / IP type
    /// that sites with an `access` policy can require. Sites without a
    /// policy are unaffected (they use the default egress / `--proxy`).
    /// Replaces any previously set pool.
    pub fn egress_pool(mut self, egress: Vec<EgressSpec>) -> Self {
        self.egress = egress;
        self
    }

    /// Supply operator authenticated sessions. A site whose `access`
    /// policy names a session has that session's headers (cookies /
    /// tokens) applied to its probe; a named-but-missing session yields
    /// `Uncertain(SessionRequired)` rather than a login-wall false
    /// negative. Replaces any previously set store.
    pub fn sessions(mut self, sessions: SessionStore) -> Self {
        self.sessions = sessions;
        self
    }

    /// Build a [`Client`].
    pub fn build(self) -> Result<Client> {
        let inner = build_reqwest(
            &self.user_agent,
            self.timeout,
            self.connect_timeout,
            self.follow_redirects,
            self.redirect_limit,
            self.proxy.as_deref(),
        )?;

        // One HTTP client per configured egress — `reqwest` bakes the
        // proxy in at build time, so geo / IP-type routing means a
        // distinct client per proxy, paired with its match metadata.
        let mut egress_entries = Vec::with_capacity(self.egress.len());
        for spec in &self.egress {
            let client = build_reqwest(
                &self.user_agent,
                self.timeout,
                self.connect_timeout,
                self.follow_redirects,
                self.redirect_limit,
                Some(&spec.url),
            )?;
            egress_entries.push((
                spec.name.clone(),
                spec.country.clone(),
                spec.kind,
                Arc::new(HttpFetcher::new(client)),
            ));
        }

        let global_throttle = self.max_rps.map(|rps| {
            // Min spacing between any two requests = 1s / rps.
            let interval = Duration::from_secs(1) / rps.get();
            HostThrottle::new(interval)
        });
        let robots = self
            .respect_robots
            .then(|| RobotsCache::new(inner.clone(), "adler"));
        // Build the impersonate fetcher up front when the feature is on;
        // surface a wreq init failure as `HttpSetup` so the caller sees
        // it the same way they'd see a bad `--proxy` URL.
        #[cfg(feature = "impersonate")]
        let impersonate = Some(Arc::new(ImpersonateFetcher::new()?));
        Ok(Client {
            http: Arc::new(HttpFetcher::new(inner)),
            egress: Arc::new(EgressPool::new(egress_entries)),
            sessions: Arc::new(self.sessions),
            throttle: HostThrottle::new(self.min_request_interval),
            global_throttle,
            retry: self.retry,
            user_agents: Arc::from(self.user_agents),
            enrich: self.enrich,
            robots,
            browser: self.browser,
            browser_budget: Arc::new(BrowserBudget::new(self.browser_budget)),
            escalation_budget: Arc::new(crate::escalation::EscalationBudget::new(
                self.escalation_budget,
            )),
            escalation_enabled: self.escalation_enabled,
            #[cfg(feature = "impersonate")]
            impersonate,
        })
    }
}

/// Build a configured `reqwest::Client`, optionally routed through a
/// proxy. Shared by the default client and every egress in the pool so
/// they get identical timeout / redirect / User-Agent settings.
fn build_reqwest(
    user_agent: &str,
    timeout: Duration,
    connect_timeout: Duration,
    follow_redirects: bool,
    redirect_limit: usize,
    proxy: Option<&str>,
) -> Result<reqwest::Client> {
    let redirect_policy = if follow_redirects {
        redirect::Policy::limited(redirect_limit)
    } else {
        redirect::Policy::none()
    };
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .redirect(redirect_policy);
    if let Some(proxy_url) = proxy {
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
    builder.build().map_err(|e| Error::HttpSetup {
        message: e.to_string(),
    })
}

/// Default ceiling on browser-backed probes per scan when no other value
/// is specified.
///
/// Sized as ~5× the typical `bot-protected` registry subset — comfortable
/// headroom while still being a guardrail against a misconfigured flag
/// burning a whole Browserbase quota.
pub const DEFAULT_BROWSER_BUDGET: usize = 50;

/// Default ceiling on *automatic escalation* fetches per scan (HTTP /
/// impersonate → browser when the cheap path returns
/// `Uncertain(CloudflareChallenge | RateLimited)`).
///
/// Independent of [`DEFAULT_BROWSER_BUDGET`]: a `bot-protected` site that
/// goes straight to the browser consumes browser budget; a non-pre-tagged
/// site that escalates from HTTP to browser consumes one of each. Sized so
/// a few-percent escalation rate across a typical registry stays under the
/// cap without thinking about it.
pub const DEFAULT_ESCALATION_BUDGET: usize = 30;

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
            .field("egress", &self.egress)
            .field("sessions", &self.sessions)
            .field("escalation_budget", &self.escalation_budget)
            .field("escalation_enabled", &self.escalation_enabled)
            .finish()
    }
}
