//! [FlareSolverr][1] backend — a self-hosted HTTP service that runs
//! Chrome internally and exposes a REST API for fetching
//! Cloudflare-protected pages.
//!
//! Trade-off vs. the other two backends:
//!
//! - [`LocalBackend`](super::local::LocalBackend): local Chrome
//!   process you maintain. Free, but each scan boots Chrome which
//!   adds ~1 s setup latency and the local IP is fingerprintable
//!   so big CF sites can still block it.
//! - [`BrowserbaseBackend`](super::browserbase::BrowserbaseBackend):
//!   cloud sessions with residential IPs. Reliable but pays per
//!   session-minute; cost matters when probing 200+ CF-tagged
//!   sites in one scan.
//! - **`FlareSolverrBackend`** *(this module)*: long-running
//!   `FlareSolverr` instance — typically in Docker — that
//!   maintains warm browser sessions and answers HTTP requests in
//!   seconds. Self-hosted, free, no residential IP. Suitable for
//!   the Cloudflare-WAF subset (`protection: ["cloudflare"]`)
//!   where operator-provided browser execution is enough; for CF
//!   Firewall / TLS-fingerprint sites you still want the residential
//!   backend.
//!
//! Adler treats `FlareSolverr` as an operator-provided browser backend.
//! It does not embed stealth scripts, CAPTCHA solving, or fingerprint
//! evasion logic in core.
//!
//! ## Setup
//!
//! Run the official image: `docker run -d -p 8191:8191
//! ghcr.io/flaresolverr/flaresolverr:latest`. Then point Adler at
//! the service:
//!
//! ```bash
//! adler --flaresolverr http://localhost:8191 alice
//! ```
//!
//! [1]: https://github.com/FlareSolverr/FlareSolverr

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::{BrowserBackend, RenderedPage};
use crate::error::{Error, Result};

/// A [`FlareSolverr`][1] backend pointed at a running instance.
///
/// Cheap to clone — the underlying [`reqwest::Client`] is
/// reference-counted internally.
///
/// [1]: https://github.com/FlareSolverr/FlareSolverr
#[derive(Clone)]
pub struct FlareSolverrBackend {
    endpoint: Url,
    client: reqwest::Client,
}

impl std::fmt::Debug for FlareSolverrBackend {
    // reqwest::Client isn't Debug-friendly; we expose only the
    // endpoint, which is the operationally interesting field.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlareSolverrBackend")
            .field("endpoint", &self.endpoint.as_str())
            .finish_non_exhaustive()
    }
}

impl FlareSolverrBackend {
    /// Build a backend that POSTs to `<endpoint>/v1` for each
    /// fetch. The endpoint should be the *base* URL of the
    /// `FlareSolverr` service — e.g. `http://localhost:8191` —
    /// without the `/v1` suffix; this method appends it.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`] when `endpoint` is not a
    /// valid `http(s)` URL or when the inner reqwest client can't
    /// be built.
    pub fn new(endpoint: &str) -> Result<Self> {
        let original = endpoint.to_owned();
        let mut parsed = Url::parse(endpoint).map_err(|e| Error::BrowserSetup {
            message: format!("flaresolverr endpoint {original:?}: {e}"),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::BrowserSetup {
                message: format!("flaresolverr endpoint {original:?}: must be http(s)"),
            });
        }
        // Always POST to /v1 — ensure the base path ends with a
        // slash so URL composition lands at `<endpoint>/v1`.
        if !parsed.path().ends_with('/') {
            let new_path = format!("{}/", parsed.path());
            parsed.set_path(&new_path);
        }
        let client = reqwest::Client::builder()
            // FlareSolverr already enforces its own maxTimeout;
            // we add a small ceiling so a hung service doesn't
            // wedge the whole scan.
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| Error::BrowserSetup {
                message: format!("flaresolverr reqwest client: {e}"),
            })?;
        Ok(Self {
            endpoint: parsed,
            client,
        })
    }

    /// Lightweight health/capability probe for diagnostics.
    ///
    /// Uses `FlareSolverr`'s `sessions.list` command and returns only a
    /// session count, not session IDs. Failing health checks should be
    /// reported to the operator but do not imply every later fetch must
    /// fail — callers may choose to warn and continue.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`] if the service is unreachable,
    /// returns non-2xx HTTP, or reports a non-`ok` API status.
    pub async fn health(&self) -> Result<FlareSolverrHealth> {
        let resp = self
            .client
            .post(self.v1_endpoint()?)
            .json(&FlareCommand {
                cmd: "sessions.list",
            })
            .send()
            .await
            .map_err(|e| Error::BrowserSetup {
                message: format!("flaresolverr health POST: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::BrowserSetup {
                message: format!(
                    "flaresolverr health returned HTTP {}",
                    resp.status().as_u16()
                ),
            });
        }
        let body: FlareHealthResponse = resp.json().await.map_err(|e| Error::BrowserSetup {
            message: format!("flaresolverr health body parse: {e}"),
        })?;
        let status = body.status.unwrap_or_else(|| "ok".to_owned());
        if status != "ok" {
            return Err(Error::BrowserSetup {
                message: format!(
                    "flaresolverr health non-ok status: {status} ({})",
                    body.message
                ),
            });
        }
        Ok(FlareSolverrHealth {
            status,
            message: body.message,
            version: body.version,
            session_count: body.sessions.len(),
        })
    }

    fn v1_endpoint(&self) -> Result<Url> {
        self.endpoint.join("v1").map_err(|e| Error::BrowserSetup {
            message: format!("flaresolverr v1 URL join failed: {e}"),
        })
    }
}

/// Sanitized `FlareSolverr` health data safe to log or show in CLI output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlareSolverrHealth {
    /// `FlareSolverr` API status, normally `ok`.
    pub status: String,
    /// Optional service message. Usually empty on success.
    pub message: String,
    /// `FlareSolverr` version when the service includes it.
    pub version: Option<String>,
    /// Number of active sessions. Session IDs are intentionally not exposed.
    pub session_count: usize,
}

#[async_trait]
impl BrowserBackend for FlareSolverrBackend {
    async fn fetch(
        &self,
        url: &Url,
        // FlareSolverr v1's request.get accepts a `headers` field
        // but it's tied to a session-id, not the one-shot request
        // form Adler uses. Custom headers therefore go *unused* in
        // this backend — sites that need them (Instagram's
        // X-IG-App-ID etc.) should keep using LocalBackend /
        // Browserbase. CF-WAF sites — the main use case — don't
        // need custom headers.
        _headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<RenderedPage> {
        let started = Instant::now();
        let request = FlareRequest {
            cmd: "request.get",
            url: url.as_str(),
            // FlareSolverr expects milliseconds; honor the caller's
            // budget but clamp to at least 5 s (less is pointless
            // since Chrome boot alone takes a second).
            max_timeout: u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .max(5_000),
        };
        let resp = self
            .client
            .post(self.v1_endpoint()?)
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::BrowserSetup {
                message: format!("flaresolverr POST: {e}"),
            })?;
        if !resp.status().is_success() {
            return Err(Error::BrowserSetup {
                message: format!("flaresolverr returned HTTP {}", resp.status().as_u16()),
            });
        }
        let body: FlareResponse = resp.json().await.map_err(|e| Error::BrowserSetup {
            message: format!("flaresolverr body parse: {e}"),
        })?;
        if body.status != "ok" {
            return Err(Error::BrowserSetup {
                message: format!(
                    "flaresolverr non-ok status: {} ({})",
                    body.status, body.message
                ),
            });
        }
        let solution = body.solution.ok_or_else(|| Error::BrowserSetup {
            message: "flaresolverr ok status with no `solution` field".into(),
        })?;
        let final_url = Url::parse(&solution.url).map_err(|e| Error::BrowserSetup {
            message: format!("flaresolverr solution.url parse: {e}"),
        })?;
        Ok(RenderedPage {
            status: solution.status,
            final_url,
            body: solution.response,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }
}

#[derive(Serialize)]
struct FlareCommand<'a> {
    cmd: &'a str,
}

#[derive(Serialize)]
struct FlareRequest<'a> {
    cmd: &'a str,
    url: &'a str,
    #[serde(rename = "maxTimeout")]
    max_timeout: u64,
}

#[derive(Deserialize)]
struct FlareResponse {
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    solution: Option<FlareSolution>,
}

#[derive(Deserialize)]
struct FlareHealthResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sessions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct FlareSolution {
    url: String,
    status: u16,
    response: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetch_parses_ok_solution_into_rendered_page() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "message": "",
                "solution": {
                    "url": "https://example.com/u/alice",
                    "status": 200,
                    "response": "<html>profile of alice</html>",
                },
                "startTimestamp": 0,
                "endTimestamp": 0,
                "version": "test"
            })))
            .mount(&mock)
            .await;

        let backend = FlareSolverrBackend::new(&mock.uri()).unwrap();
        let page = backend
            .fetch(
                &Url::parse("https://example.com/u/alice").unwrap(),
                &BTreeMap::new(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.final_url.as_str(), "https://example.com/u/alice");
        assert!(page.body.contains("profile of alice"));
    }

    #[tokio::test]
    async fn fetch_surfaces_non_ok_status_as_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "error",
                "message": "Could not solve the challenge",
                "solution": null
            })))
            .mount(&mock)
            .await;

        let backend = FlareSolverrBackend::new(&mock.uri()).unwrap();
        let err = backend
            .fetch(
                &Url::parse("https://example.com").unwrap(),
                &BTreeMap::new(),
                Duration::from_secs(10),
            )
            .await
            .unwrap_err();
        match err {
            Error::BrowserSetup { message } => {
                assert!(message.contains("non-ok"), "got: {message}");
                assert!(message.contains("Could not solve"), "got: {message}");
            }
            other => panic!("expected Error::BrowserSetup, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_reports_version_and_sanitized_session_count() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "message": "",
                "version": "test-version",
                "sessions": ["session-a", "session-b"]
            })))
            .mount(&mock)
            .await;

        let backend = FlareSolverrBackend::new(&mock.uri()).unwrap();
        let health = backend.health().await.unwrap();
        assert_eq!(health.status, "ok");
        assert_eq!(health.version.as_deref(), Some("test-version"));
        assert_eq!(health.session_count, 2);
    }

    #[tokio::test]
    async fn health_surfaces_non_ok_status_as_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "error",
                "message": "service is warming up"
            })))
            .mount(&mock)
            .await;

        let backend = FlareSolverrBackend::new(&mock.uri()).unwrap();
        let err = backend.health().await.unwrap_err();
        match err {
            Error::BrowserSetup { message } => {
                assert!(message.contains("health non-ok"), "got: {message}");
                assert!(message.contains("warming up"), "got: {message}");
            }
            other => panic!("expected Error::BrowserSetup, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_http_endpoint() {
        let err = FlareSolverrBackend::new("ftp://localhost").unwrap_err();
        assert!(matches!(err, Error::BrowserSetup { .. }));
    }
}
