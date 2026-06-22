//! Transport layer: how a single probe actually reaches a site.
//!
//! [`Client`](crate::Client) is the *router* — it owns cross-cutting
//! concerns (regex gate, robots, throttle, browser budget, retry) and
//! signal evaluation. A [`Fetcher`] owns only the transport: given a
//! [`FetchRequest`], produce a normalised [`FetchResponse`] (or a
//! [`FetchError`] carrying the [`UncertainReason`] the outcome should
//! report).
//!
//! Phase 1 ships two transports — [`HttpFetcher`] (raw `reqwest`, the
//! default) and [`BrowserFetcher`] (adapts a
//! [`BrowserBackend`](crate::browser::BrowserBackend)). The seam exists
//! so later phases can add fingerprint-impersonating transports,
//! operator-provided browser services, and an egress (proxy) dimension
//! without growing the router into a monster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::ban;
use crate::browser::BrowserBackend;
use crate::check::UncertainReason;
use crate::site::HttpMethod;

/// Per-fetch timeout for the browser transport. Browser fetches (JS
/// execution + waits) are inherently slower than raw HTTP, so this is
/// generous on purpose. Also used by `Client::fetch_for_doctor`.
pub(crate) const BROWSER_TIMEOUT: Duration = Duration::from_secs(60);

/// Everything a fetcher needs for one request. A superset: each
/// transport reads the subset it cares about (e.g. `want_body` and
/// `method` are HTTP-only; the browser transport uses `url` + `headers`).
pub(crate) struct FetchRequest<'a> {
    pub method: HttpMethod,
    pub url: &'a str,
    /// POST body (already `{username}`-substituted). HTTP-only.
    pub body: Option<&'a str>,
    /// Resolved User-Agent for this request (rotation handled upstream).
    pub user_agent: Option<&'a str>,
    /// Per-site/session extra headers. Applied by both raw HTTP and
    /// browser transports.
    pub headers: &'a BTreeMap<String, String>,
    /// Whether the response body is needed (signals / enrichment). When
    /// `false`, the HTTP transport may issue a HEAD.
    pub want_body: bool,
}

/// Normalised response, transport-agnostic. `body` is empty when the
/// caller didn't request it (HTTP HEAD path).
pub(crate) struct FetchResponse {
    pub status: u16,
    pub final_url: String,
    pub body: String,
}

/// A fetch that didn't yield a usable response. Wraps the
/// [`UncertainReason`] the resulting [`CheckOutcome`](crate::CheckOutcome)
/// should carry, so the router maps every error uniformly to
/// `Uncertain(reason)` — preserving the exact reason taxonomy the raw
/// HTTP / browser paths produced before this seam existed.
pub(crate) struct FetchError(pub UncertainReason);

#[async_trait]
pub(crate) trait Fetcher: Send + Sync {
    async fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, FetchError>;
}

/// Raw-HTTP transport over a `reqwest::Client`. Owns the client so a
/// later egress-pool phase can hold one fetcher per proxy.
pub(crate) struct HttpFetcher {
    inner: reqwest::Client,
}

impl HttpFetcher {
    pub(crate) fn new(inner: reqwest::Client) -> Self {
        Self { inner }
    }

    /// Borrow the underlying client for non-probe diagnostics
    /// (`Client::fetch`).
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.inner
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, FetchError> {
        // Method dispatch mirrors the pre-seam probe path: POST always
        // POST (carries the username in its body); GET reads the body
        // only when needed, otherwise HEAD with a transparent 405→GET
        // retry (some servers reject HEAD).
        let sent = match req.method {
            HttpMethod::Post => {
                send(
                    &self.inner,
                    reqwest::Method::POST,
                    req.url,
                    req.user_agent,
                    req.headers,
                    req.body,
                )
                .await
            }
            HttpMethod::Get if req.want_body => {
                send(
                    &self.inner,
                    reqwest::Method::GET,
                    req.url,
                    req.user_agent,
                    req.headers,
                    None,
                )
                .await
            }
            HttpMethod::Get => {
                match send(
                    &self.inner,
                    reqwest::Method::HEAD,
                    req.url,
                    req.user_agent,
                    req.headers,
                    None,
                )
                .await
                {
                    Ok(r) if r.status().as_u16() == 405 => {
                        send(
                            &self.inner,
                            reqwest::Method::GET,
                            req.url,
                            req.user_agent,
                            req.headers,
                            None,
                        )
                        .await
                    }
                    other => other,
                }
            }
        };

        let response = match sent {
            Ok(r) => r,
            Err(err) => {
                tracing::debug!(url = %req.url, error = %err, "request failed");
                return Err(FetchError(UncertainReason::Network(err.to_string())));
            }
        };

        let status = response.status().as_u16();
        let final_url = response.url().to_string();

        if let Some(reason) = ban::detect_pre_body(status, response.headers()) {
            tracing::warn!(url = %req.url, status, %reason, "ban-like response");
            return Err(FetchError(reason));
        }

        let body = if req.want_body {
            match response.text().await {
                Ok(b) => b,
                Err(err) => return Err(FetchError(UncertainReason::BodyRead(err.to_string()))),
            }
        } else {
            String::new()
        };

        if !body.is_empty() {
            if let Some(reason) = ban::detect_in_body(&body) {
                tracing::warn!(url = %req.url, %reason, "ban-like body");
                return Err(FetchError(reason));
            }
        }

        Ok(FetchResponse {
            status,
            final_url,
            body,
        })
    }
}

/// Browser transport: renders through a
/// [`BrowserBackend`](crate::browser::BrowserBackend) and normalises the
/// [`RenderedPage`](crate::browser::RenderedPage). Uses only `url` and
/// `headers` from the request (timeout is the fixed `BROWSER_TIMEOUT`) —
/// the backend always returns a full body and manages its own method /
/// User-Agent.
pub(crate) struct BrowserFetcher {
    backend: Arc<dyn BrowserBackend>,
}

impl BrowserFetcher {
    pub(crate) fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Fetcher for BrowserFetcher {
    async fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, FetchError> {
        let parsed = match url::Url::parse(req.url) {
            Ok(u) => u,
            Err(err) => {
                return Err(FetchError(UncertainReason::Other(format!(
                    "invalid url: {err}"
                ))));
            }
        };
        match self
            .backend
            .fetch(&parsed, req.headers, BROWSER_TIMEOUT)
            .await
        {
            Ok(page) => {
                if let Some(reason) = ban::detect_in_body(&page.body) {
                    tracing::warn!(url = %req.url, %reason, "ban-like browser body");
                    return Err(FetchError(reason));
                }
                Ok(FetchResponse {
                    status: page.status,
                    final_url: page.final_url.as_str().to_owned(),
                    body: page.body,
                })
            }
            Err(err) => {
                tracing::warn!(url = %req.url, error = %err, "browser fetch failed");
                Err(FetchError(UncertainReason::BrowserFailed(err.to_string())))
            }
        }
    }
}

/// Issue one request, applying the per-site / session `headers` and an
/// optional User-Agent override and body. A `User-Agent` in `headers`
/// wins over `ua`; a `Content-Type` in `headers` wins over the POST
/// default of `application/json`.
async fn send(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    ua: Option<&str>,
    headers: &BTreeMap<String, String>,
    body: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    let mut request = client.request(method, url);
    let has = |name: &str| headers.keys().any(|k| k.eq_ignore_ascii_case(name));
    // Rotation/default UA only when the headers don't set their own.
    if let Some(ua) = ua {
        if !has("user-agent") {
            request = request.header(reqwest::header::USER_AGENT, ua);
        }
    }
    for (k, v) in headers {
        request = request.header(k, v);
    }
    if let Some(b) = body {
        if !has("content-type") {
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        request = request.body(b.to_owned());
    }
    request.send().await
}

#[cfg(feature = "impersonate")]
pub(crate) use impersonate::ImpersonateFetcher;

/// Browser-impersonating HTTP transport (`wreq` + `BoringSSL`), gated by
/// the `impersonate` Cargo feature. A site whose protection list is
/// *only* [`ProtectionKind::TlsFingerprint`](crate::ProtectionKind) is
/// routed here instead of the heavyweight browser backend — much
/// cheaper, since a real TLS handshake from `wreq` matches Chrome's
/// JA3/JA4 fingerprint without launching a browser process.
#[cfg(feature = "impersonate")]
mod impersonate {
    use super::{
        FetchError, FetchRequest, FetchResponse, Fetcher, HttpMethod, UncertainReason, ban,
    };
    use async_trait::async_trait;
    use std::collections::BTreeMap;

    /// Chrome version we impersonate. Picked from
    /// [`wreq_util::Profile`]; bump as Chrome moves so the JA3/JA4
    /// fingerprint stays "current Chrome" — old emulations get filtered
    /// out by WAFs over time.
    const EMULATION: wreq_util::Profile = wreq_util::Profile::Chrome134;

    pub(crate) struct ImpersonateFetcher {
        inner: wreq::Client,
    }

    impl ImpersonateFetcher {
        pub(crate) fn new() -> crate::error::Result<Self> {
            let inner = wreq::Client::builder()
                .emulation(EMULATION)
                .build()
                .map_err(|e| crate::error::Error::HttpSetup {
                    message: format!("wreq client init: {e}"),
                })?;
            Ok(Self { inner })
        }
    }

    #[async_trait]
    impl Fetcher for ImpersonateFetcher {
        async fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, FetchError> {
            // Method dispatch mirrors `HttpFetcher`: POST always POST;
            // GET reads the body only when needed, otherwise HEAD with
            // a transparent 405 → GET retry.
            let sent = match req.method {
                HttpMethod::Post => {
                    send(
                        &self.inner,
                        wreq::Method::POST,
                        req.url,
                        req.user_agent,
                        req.headers,
                        req.body,
                    )
                    .await
                }
                HttpMethod::Get if req.want_body => {
                    send(
                        &self.inner,
                        wreq::Method::GET,
                        req.url,
                        req.user_agent,
                        req.headers,
                        None,
                    )
                    .await
                }
                HttpMethod::Get => {
                    match send(
                        &self.inner,
                        wreq::Method::HEAD,
                        req.url,
                        req.user_agent,
                        req.headers,
                        None,
                    )
                    .await
                    {
                        Ok(r) if r.status().as_u16() == 405 => {
                            send(
                                &self.inner,
                                wreq::Method::GET,
                                req.url,
                                req.user_agent,
                                req.headers,
                                None,
                            )
                            .await
                        }
                        other => other,
                    }
                }
            };

            let response = match sent {
                Ok(r) => r,
                Err(err) => {
                    tracing::debug!(url = %req.url, error = %err, "impersonate request failed");
                    return Err(FetchError(UncertainReason::Network(err.to_string())));
                }
            };

            let status = response.status().as_u16();
            let final_url = response.uri().to_string();

            if let Some(reason) = ban::detect_pre_body(status, response.headers()) {
                tracing::warn!(url = %req.url, status, %reason, "ban-like response");
                return Err(FetchError(reason));
            }

            let body = if req.want_body {
                match response.text().await {
                    Ok(b) => b,
                    Err(err) => {
                        return Err(FetchError(UncertainReason::BodyRead(err.to_string())));
                    }
                }
            } else {
                String::new()
            };

            if !body.is_empty() {
                if let Some(reason) = ban::detect_in_body(&body) {
                    tracing::warn!(url = %req.url, %reason, "ban-like body");
                    return Err(FetchError(reason));
                }
            }

            Ok(FetchResponse {
                status,
                final_url,
                body,
            })
        }
    }

    async fn send(
        client: &wreq::Client,
        method: wreq::Method,
        url: &str,
        ua: Option<&str>,
        headers: &BTreeMap<String, String>,
        body: Option<&str>,
    ) -> wreq::Result<wreq::Response> {
        let mut request = client.request(method, url);
        let has = |name: &str| headers.keys().any(|k| k.eq_ignore_ascii_case(name));
        // wreq's emulation already sets a Chrome User-Agent on every
        // request; only override when we have a rotation UA AND the
        // caller hasn't put their own UA in headers.
        if let Some(ua) = ua {
            if !has("user-agent") {
                request = request.header(wreq::header::USER_AGENT, ua);
            }
        }
        for (k, v) in headers {
            request = request.header(k, v);
        }
        if let Some(b) = body {
            if !has("content-type") {
                request = request.header(wreq::header::CONTENT_TYPE, "application/json");
            }
            request = request.body(b.to_owned());
        }
        request.send().await
    }
}
