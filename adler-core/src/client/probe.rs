//! Per-site probe path: routing, ban-retry, escalation, finish.
//!
//! Hosts the methods on [`Client`] that turn one `(site, username)`
//! pair into a [`CheckOutcome`]: the public entry point
//! [`Client::check`], the request-issuing path [`Client::probe_once`]
//! (browser routing → impersonate-fingerprint → egress selection →
//! HTTP fetch), the HTTP→browser escalation in [`Client::maybe_escalate`]
//! when a cheap-path response merits a second look, and the
//! signal-evaluation [`Client::finish`] that turns a raw response
//! into a final outcome. Also hosts the two diagnostic fetch helpers
//! (`fetch`, `fetch_for_doctor`) used by `adler --doctor --fix`.
//!
//! Construction lives in `client::builder`; accessors and
//! [`Client::with_egress_subset`] stay in `client::mod`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::access::EgressChoice;
use crate::check::{CheckOutcome, MatchKind, UncertainReason};
use crate::escalation::TransportTier;
use crate::retry;
use crate::site::{HttpMethod, Probe, ProtectionKind, Signal, SignalVerdict, Site, aggregate};
use crate::transport::{
    BROWSER_TIMEOUT, BrowserFetcher, FetchError, FetchRequest, Fetcher, HttpFetcher,
};
use crate::username::Username;

use super::util::{host_of, origin_and_path, outcome, uncertain};
use super::{BOT_PROTECTED_TAG, Client, GLOBAL_THROTTLE_KEY, RawResponse};

fn routes_through_browser(site: &Site) -> bool {
    site.tags
        .iter()
        .any(|t| t.eq_ignore_ascii_case(BOT_PROTECTED_TAG))
        || site
            .protection
            .iter()
            .any(|p| !matches!(p, ProtectionKind::UserAuth))
}

#[derive(Debug, Clone, Copy)]
struct ProbeEvidenceContext {
    transport: TransportTier,
    escalations: u8,
    authenticated: bool,
}

impl Client {
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
        let mut request = self.http.client().get(url);
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
            if routes_through_browser(site) {
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

        // Resolve an operator session if the site's access policy names
        // one, and fold its headers (cookies / tokens) over the site's
        // own. A named-but-missing session is reported rather than sent
        // unauthenticated into a login wall — which reads identically
        // for an existing and a missing account. Applies to both the
        // HTTP and browser transports.
        let (session_headers, authenticated): (Cow<'_, BTreeMap<String, String>>, bool) =
            match &site.access.session {
                None => (Cow::Borrowed(&site.request_headers), false),
                Some(name) => match self.sessions.get(name) {
                    Some(session) => (Cow::Owned(session.apply(&site.request_headers)), true),
                    None => {
                        return uncertain(
                            &site.name,
                            url,
                            Instant::now(),
                            UncertainReason::SessionRequired,
                        );
                    }
                },
            };
        let headers: &BTreeMap<String, String> = &session_headers;

        // Auto-route bot-protected sites through the browser backend when
        // one is configured. Raw HTTP can't see past their JS/login wall,
        // so this is the only way they ever produce a Found verdict.
        // A site is "bot-protected" in the routing sense if it carries
        // the legacy tag OR declares any specific protection mechanism
        // via the new `protection` field — either signal is enough.
        if let Some(backend) = &self.browser {
            if routes_through_browser(site) {
                if self.browser_budget.try_consume() {
                    let started = Instant::now();
                    let req = FetchRequest {
                        method: site.request_method,
                        url: &url,
                        body: None,
                        user_agent: None,
                        headers,
                        want_body: true,
                    };
                    let fetcher = BrowserFetcher::new(Arc::clone(backend));
                    let mut outcome = match fetcher.fetch(&req).await {
                        Ok(resp) => self.finish(
                            site,
                            url,
                            started,
                            &resp,
                            ProbeEvidenceContext {
                                transport: TransportTier::Browser,
                                escalations: 0,
                                authenticated,
                            },
                        ),
                        Err(FetchError(reason)) => uncertain(&site.name, url, started, reason),
                    };
                    outcome.transport = Some(TransportTier::Browser);
                    return outcome;
                }
                tracing::warn!(site = %site.name, "browser budget exhausted");
                let mut outcome = uncertain(
                    &site.name,
                    url,
                    Instant::now(),
                    UncertainReason::BrowserBudget,
                );
                outcome.transport = Some(TransportTier::Browser);
                return outcome;
            }
        }

        // Phase 2: route pure-`TlsFingerprint` sites through the
        // impersonating transport — a real BoringSSL TLS handshake from
        // `wreq` matches Chrome's JA3/JA4 fingerprint that triggered the
        // protection tag, at a fraction of the cost of a real browser.
        // Mixed-protection sites (TLS-fingerprint + Cloudflare, etc.)
        // keep going through the browser path above, where they were.
        #[cfg(feature = "impersonate")]
        if let Some(fetcher) = &self.impersonate {
            let pure_tls = site.protection.len() == 1
                && site.protection[0] == crate::site::ProtectionKind::TlsFingerprint
                && !site
                    .tags
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(BOT_PROTECTED_TAG));
            if pure_tls {
                let started = Instant::now();
                let req = FetchRequest {
                    method: site.request_method,
                    url: &url,
                    body: None,
                    user_agent: self.pick_user_agent(),
                    headers,
                    want_body: true,
                };
                let mut primary = match fetcher.fetch(&req).await {
                    Ok(resp) => self.finish(
                        site,
                        url.clone(),
                        started,
                        &resp,
                        ProbeEvidenceContext {
                            transport: TransportTier::Impersonate,
                            escalations: 0,
                            authenticated,
                        },
                    ),
                    Err(FetchError(reason)) => uncertain(&site.name, url.clone(), started, reason),
                };
                primary.transport = Some(TransportTier::Impersonate);
                return self
                    .maybe_escalate(site, &url, headers, authenticated, primary)
                    .await;
            }
        }

        // Egress selection: route the HTTP path through a geo / IP-type
        // matching proxy when the site's access policy demands one. An
        // unconstrained policy uses the default egress; a constrained
        // policy with no matching egress is reported `GeoUnavailable`
        // rather than fetched from the wrong location (a false
        // `NotFound` would be worse than an honest `Uncertain`).
        let egress: Arc<HttpFetcher> = match self.egress.select(&site.access) {
            EgressChoice::Default => Arc::clone(&self.http),
            EgressChoice::Use(fetcher) => fetcher,
            EgressChoice::Unavailable => {
                return uncertain(
                    &site.name,
                    url,
                    Instant::now(),
                    UncertainReason::GeoUnavailable,
                );
            }
        };

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

        // Read the body only if a signal needs it, or enrichment is on
        // and the site declares extractor rules (extraction needs it).
        let want_enrich = self.enrich && !site.extract.is_empty();
        let needs_body = want_enrich || site.signals.iter().any(crate::site::Signal::needs_body);

        // POST sites carry their own body payload (the username goes in
        // the body, not the URL — e.g. Anilist's GraphQL endpoint).
        // `{username}` in `Site::request_body` is substituted here,
        // mirroring URL substitution.
        let body_for_post: Option<String> = if matches!(site.request_method, HttpMethod::Post) {
            const USERNAME_PH: &str = "{username}";
            site.request_body
                .as_deref()
                .map(|t| t.replace(USERNAME_PH, username.as_str()))
        } else {
            None
        };

        let req = FetchRequest {
            method: site.request_method,
            url: &url,
            body: body_for_post.as_deref(),
            user_agent: self.pick_user_agent(),
            headers,
            want_body: needs_body,
        };
        let mut primary = match egress.fetch(&req).await {
            Ok(resp) => self.finish(
                site,
                url.clone(),
                started,
                &resp,
                ProbeEvidenceContext {
                    transport: TransportTier::Http,
                    escalations: 0,
                    authenticated,
                },
            ),
            Err(FetchError(reason)) => uncertain(&site.name, url.clone(), started, reason),
        };
        primary.transport = Some(TransportTier::Http);
        self.maybe_escalate(site, &url, headers, authenticated, primary)
            .await
    }

    /// If the cheap transport returned an `Uncertain` reason a browser
    /// fetch could plausibly resolve, retry through the browser backend
    /// and stamp the new outcome as escalated. Bounded by
    /// [`escalation_budget`](ClientBuilder::escalation_budget).
    async fn maybe_escalate(
        &self,
        site: &Site,
        url: &str,
        headers: &BTreeMap<String, String>,
        authenticated: bool,
        primary: CheckOutcome,
    ) -> CheckOutcome {
        if !self.escalation_enabled || primary.kind != MatchKind::Uncertain {
            return primary;
        }
        let Some(reason) = &primary.reason else {
            return primary;
        };
        if !crate::escalation::should_escalate(reason) {
            return primary;
        }
        let Some(backend) = &self.browser else {
            return primary;
        };
        if !self.escalation_budget.try_consume() {
            tracing::debug!(site = %site.name, "escalation budget exhausted");
            return primary;
        }

        tracing::debug!(site = %site.name, reason = %reason, "escalating to browser");
        let started = Instant::now();
        let req = FetchRequest {
            method: site.request_method,
            url,
            body: None,
            user_agent: None,
            headers,
            want_body: true,
        };
        let fetcher = BrowserFetcher::new(Arc::clone(backend));
        let mut escalated = match fetcher.fetch(&req).await {
            Ok(resp) => self.finish(
                site,
                url.to_owned(),
                started,
                &resp,
                ProbeEvidenceContext {
                    transport: TransportTier::Browser,
                    escalations: 1,
                    authenticated,
                },
            ),
            Err(FetchError(r)) => uncertain(&site.name, url.to_owned(), started, r),
        };
        escalated.transport = Some(TransportTier::Browser);
        escalated.escalations = 1;
        escalated
    }

    /// Evaluate a fetched response against the site's signals and build
    /// the outcome. Shared by the HTTP and browser transports so the
    /// verdict / evidence / enrichment logic lives in exactly one place.
    fn finish(
        &self,
        site: &Site,
        url: String,
        started: Instant,
        resp: &crate::transport::FetchResponse,
        context: ProbeEvidenceContext,
    ) -> CheckOutcome {
        let probe = Probe {
            status: resp.status,
            final_url: &resp.final_url,
            body: &resp.body,
        };
        let votes: Vec<(&Signal, SignalVerdict)> = site
            .signals
            .iter()
            .map(|s| (s, s.evaluate(&probe)))
            .collect();
        let kind = aggregate(votes.iter().map(|(_, v)| *v));
        let mut result = outcome(&site.name, url, started, kind);
        result.transport = Some(context.transport);
        result.escalations = context.escalations;
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
        if self.enrich && kind == MatchKind::Found && !site.extract.is_empty() {
            result.enrichment = crate::enrich::extract(&resp.body, &site.extract);
            let observed_at_ms = unix_epoch_ms();
            let access_path = crate::EvidenceAccessPath::new(
                context.transport,
                context.escalations,
                context.authenticated,
            );
            result.profile_evidence = result
                .enrichment
                .iter()
                .map(|(field, value)| {
                    crate::ProfileEvidence::from_enrichment_with_source(
                        &result.site,
                        &result.url,
                        field,
                        value,
                        Some(observed_at_ms),
                        Some(access_path.clone()),
                    )
                })
                .collect();
        }
        result.refresh_confidence();
        result
    }
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}
