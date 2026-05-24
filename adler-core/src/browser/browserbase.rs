//! Browserbase cloud backend.
//!
//! Creates a remote browser session via the Browserbase REST API and
//! drives it through the CDP WebSocket the service returns. Pays per
//! session-minute (see Browserbase pricing); the pool comes with a
//! residential / mobile IP and anti-fingerprint baked in.
//!
//! ## Why a raw CDP client and not `chromiumoxide` / `headless_chrome`
//!
//! Both maintained Rust CDP libraries assume the target-attach semantics
//! of a *locally launched* Chrome (which auto-emits `Target.attachedToTarget`
//! immediately after `Target.createTarget`) and deadlock against
//! Browserbase, whose remote browser is quieter on that front. Issue #5
//! has the full diagnosis. Instead we drive CDP directly through our
//! [`CdpClient`](super::cdp::CdpClient) and request the attach explicitly
//! via `Target.attachToTarget` with `flatten: true`.
//!
//! ## Session lifecycle
//!
//! One Browserbase session is opened per backend instance and reused
//! across every fetch — keeps cost low and the egress IP stable across a
//! scan. Each fetch creates a fresh target, navigates it, reads
//! `document.documentElement.outerHTML`, and closes the target.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use url::Url;

use super::cdp::{CdpClient, CdpError, CdpEvent};
use super::{BrowserBackend, RenderedPage};
use crate::{Error, Result};

const API_BASE: &str = "https://api.browserbase.com/v1";
/// Per-call CDP timeout. Generous to absorb cold-cache fetches; the
/// trait-level `fetch` timeout wraps the whole sequence so a stalled
/// individual call still surfaces.
const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(45);

/// Credentials and target project for [`BrowserbaseBackend::connect`].
#[derive(Debug, Clone)]
pub struct BrowserbaseConfig {
    /// API key from <https://browserbase.com/settings>. Wrapped in
    /// [`SecretString`] so it doesn't leak into `Debug` output or logs.
    pub api_key: SecretString,
    /// Project id (UUID) the session is created under.
    pub project_id: String,
}

/// Cloud browser session against Browserbase, reused across fetches.
pub struct BrowserbaseBackend {
    cdp: CdpClient,
    /// Serializes fetches — Browserbase's session is a single browser,
    /// and we want predictable session-id allocation order in tests.
    fetch_lock: Mutex<()>,
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct CreateSessionResponse {
    id: String,
    #[serde(rename = "connectUrl")]
    connect_url: String,
}

#[derive(Debug, Deserialize)]
struct CreateTargetResult {
    #[serde(rename = "targetId")]
    target_id: String,
}

#[derive(Debug, Deserialize)]
struct AttachToTargetResult {
    #[serde(rename = "sessionId")]
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct NavigateResult {
    #[serde(rename = "frameId")]
    frame_id: String,
    #[serde(rename = "errorText", default)]
    error_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EvaluateResult {
    result: RemoteObject,
    #[serde(rename = "exceptionDetails", default)]
    exception_details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RemoteObject {
    #[serde(default)]
    value: Option<serde_json::Value>,
}

impl BrowserbaseBackend {
    /// Create a session via the Browserbase REST API and attach a raw
    /// [`CdpClient`] to the CDP WebSocket it returns.
    ///
    /// # Errors
    /// [`Error::BrowserSetup`] on REST / authentication / WebSocket /
    /// TLS / CDP-handshake failure.
    pub async fn connect(cfg: BrowserbaseConfig) -> Result<Self> {
        let session = create_session(&cfg).await?;
        let cdp = CdpClient::connect(&session.connect_url)
            .await
            .map_err(|e| Error::BrowserSetup {
                message: format!("connect CDP: {e}"),
            })?;
        tracing::info!(session_id = %session.id, "browserbase session opened");
        Ok(Self {
            cdp,
            fetch_lock: Mutex::new(()),
            session_id: session.id,
        })
    }

    /// The Browserbase session id, useful in logs / billing correlation.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Test-only: assemble a backend directly from a pre-connected
    /// [`CdpClient`], skipping the live Browserbase REST handshake.
    /// Used by the in-tree mock-CDP integration tests.
    #[cfg(test)]
    pub(crate) fn from_parts(cdp: CdpClient, session_id: String) -> Self {
        Self {
            cdp,
            fetch_lock: Mutex::new(()),
            session_id,
        }
    }
}

async fn create_session(cfg: &BrowserbaseConfig) -> Result<CreateSessionResponse> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::BrowserSetup {
            message: format!("http client: {e}"),
        })?;
    let resp = http
        .post(format!("{API_BASE}/sessions"))
        .header("x-bb-api-key", cfg.api_key.expose_secret())
        .header("content-type", "application/json")
        .body(json!({ "projectId": cfg.project_id }).to_string())
        .send()
        .await
        .map_err(|e| Error::BrowserSetup {
            message: format!("create session: {e}"),
        })?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(Error::BrowserSetup {
            message: format!("create session: HTTP {status}: {detail}"),
        });
    }
    resp.json::<CreateSessionResponse>()
        .await
        .map_err(|e| Error::BrowserSetup {
            message: format!("decode session response: {e}"),
        })
}

#[async_trait]
impl BrowserBackend for BrowserbaseBackend {
    // The fetch sequence is a flat list of CDP commands that have to run in
    // a specific order; splitting them across helper fns hides the order
    // and adds nothing.
    #[allow(clippy::too_many_lines)]
    async fn fetch(
        &self,
        url: &Url,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<RenderedPage> {
        let start = Instant::now();
        let cdp = &self.cdp;

        // One fetch at a time per session — keeps the CDP message
        // ordering legible and matches Browserbase's "one browser per
        // session" model.
        let _guard = self.fetch_lock.lock().await;

        let work = async {
            // 1. Open a fresh target. The url is `about:blank` first;
            // we navigate explicitly below so we can capture the response.
            let CreateTargetResult { target_id } = cdp
                .execute(
                    "Target.createTarget",
                    json!({ "url": "about:blank" }),
                    None,
                    CDP_CALL_TIMEOUT,
                )
                .await
                .map_err(|e| browser_err(&e))?;

            // 2. Attach to it with `flatten: true` — all subsequent
            // messages for this target carry our sessionId on the same
            // socket.
            let AttachToTargetResult { session_id: sid } = cdp
                .execute(
                    "Target.attachToTarget",
                    json!({ "targetId": target_id, "flatten": true }),
                    None,
                    CDP_CALL_TIMEOUT,
                )
                .await
                .map_err(|e| browser_err(&e))?;

            // 3. Enable the Page + Network domains so we get load /
            // responseReceived events for this session.
            let _: serde_json::Value = cdp
                .execute("Page.enable", json!({}), Some(&sid), CDP_CALL_TIMEOUT)
                .await
                .map_err(|e| browser_err(&e))?;
            let _: serde_json::Value = cdp
                .execute("Network.enable", json!({}), Some(&sid), CDP_CALL_TIMEOUT)
                .await
                .map_err(|e| browser_err(&e))?;

            // 3a. Per-site request headers (e.g. Instagram needs
            // `X-IG-App-ID` + a matching `User-Agent` to unlock its
            // `web_profile_info` JSON endpoint — Chrome's default UA gets
            // a `useragent mismatch` reject). Applied before navigation so
            // they cover the main document request too.
            //
            // `User-Agent` is special-cased: CDP wants it via
            // `Network.setUserAgentOverride`, not `setExtraHTTPHeaders`.
            // Splitting keeps us compatible across Chrome builds.
            if !headers.is_empty() {
                let mut ua: Option<&str> = None;
                let mut extras = serde_json::Map::new();
                for (k, v) in headers {
                    if k.eq_ignore_ascii_case("user-agent") {
                        ua = Some(v.as_str());
                    } else {
                        extras.insert(k.clone(), serde_json::Value::String(v.clone()));
                    }
                }
                if let Some(ua) = ua {
                    let _: serde_json::Value = cdp
                        .execute(
                            "Network.setUserAgentOverride",
                            json!({ "userAgent": ua }),
                            Some(&sid),
                            CDP_CALL_TIMEOUT,
                        )
                        .await
                        .map_err(|e| browser_err(&e))?;
                }
                if !extras.is_empty() {
                    let _: serde_json::Value = cdp
                        .execute(
                            "Network.setExtraHTTPHeaders",
                            json!({ "headers": extras }),
                            Some(&sid),
                            CDP_CALL_TIMEOUT,
                        )
                        .await
                        .map_err(|e| browser_err(&e))?;
                }
            }

            // 4. Subscribe BEFORE navigation so neither the
            // `Network.responseReceived` for the main document nor the
            // `Page.frameStoppedLoading` we wait on later can fire
            // between command and subscribe. Two independent
            // subscriptions — the collector consumes one, the wait
            // drains the other.
            let captured = Arc::new(Mutex::new(None::<(u16, String)>));
            let captured_clone = Arc::clone(&captured);
            let sid_for_collector = sid.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_clone = Arc::clone(&stop);
            let mut collector_rx = cdp.subscribe_events();
            let mut wait_rx = cdp.subscribe_events();
            let collector = tokio::spawn(async move {
                while !stop_clone.load(Ordering::Acquire) {
                    let Ok(evt) = collector_rx.recv().await else {
                        return;
                    };
                    if evt.session_id.as_deref() == Some(&sid_for_collector)
                        && evt.method == "Network.responseReceived"
                    {
                        if let Some((status, url)) = extract_document_response(&evt) {
                            let mut g = captured_clone.lock().await;
                            if g.is_none() {
                                *g = Some((status, url));
                            }
                        }
                    }
                }
            });

            // 5. Navigate. We need the returned `frameId` to scope the
            // load-wait below — without it we'd accept the
            // `Page.frameStoppedLoading` of the initial `about:blank`
            // and exit before the real navigation finishes.
            let nav: NavigateResult = cdp
                .execute(
                    "Page.navigate",
                    json!({ "url": url.as_str() }),
                    Some(&sid),
                    CDP_CALL_TIMEOUT,
                )
                .await
                .map_err(|e| browser_err(&e))?;
            if let Some(err) = nav.error_text.as_deref().filter(|s| !s.is_empty()) {
                return Err(Error::BrowserSetup {
                    message: format!("Page.navigate {url}: {err}"),
                });
            }

            // 6. Wait for the *main* frame to stop loading. Pinning to
            // `nav.frame_id` avoids both the about:blank load event
            // (different frame) and any iframe loads (different frames
            // too).
            let target_frame = nav.frame_id.clone();
            let sid_for_wait = sid.clone();
            let _ = CdpClient::wait_for_event_on(
                &mut wait_rx,
                move |e| {
                    e.session_id.as_deref() == Some(&sid_for_wait)
                        && e.method == "Page.frameStoppedLoading"
                        && e.params.get("frameId").and_then(|v| v.as_str()) == Some(&target_frame)
                },
                CDP_CALL_TIMEOUT,
                "Page.frameStoppedLoading",
            )
            .await
            .map_err(|e| browser_err(&e))?;

            // 7. Read the post-render DOM via Runtime.evaluate.
            let eval: EvaluateResult = cdp
                .execute(
                    "Runtime.evaluate",
                    json!({
                        "expression": "document.documentElement.outerHTML",
                        "returnByValue": true,
                    }),
                    Some(&sid),
                    CDP_CALL_TIMEOUT,
                )
                .await
                .map_err(|e| browser_err(&e))?;
            if let Some(exc) = eval.exception_details {
                return Err(Error::BrowserSetup {
                    message: format!("Runtime.evaluate threw: {exc}"),
                });
            }
            let body = eval
                .result
                .value
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();

            // Stop the collector. Pending captured value (if any) wins
            // over the URL we navigated to.
            stop.store(true, Ordering::Release);
            collector.abort();

            let (status, final_url) = {
                let g = captured.lock().await;
                g.clone().map_or_else(
                    || (0_u16, url.clone()),
                    |(s, u)| (s, Url::parse(&u).unwrap_or_else(|_| url.clone())),
                )
            };

            // 8. Best-effort cleanup. The session will GC closed targets
            // on its own; doing it ourselves keeps memory low across
            // many fetches.
            let _: std::result::Result<serde_json::Value, _> = cdp
                .execute(
                    "Target.closeTarget",
                    json!({ "targetId": target_id }),
                    None,
                    CDP_CALL_TIMEOUT,
                )
                .await;

            Ok::<_, Error>(RenderedPage {
                status,
                final_url,
                body,
                elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            })
        };

        tokio::time::timeout(timeout, work)
            .await
            .map_err(|_| Error::BrowserSetup {
                message: format!("browser fetch timeout after {}s", timeout.as_secs()),
            })?
    }
}

fn browser_err(e: &CdpError) -> Error {
    Error::BrowserSetup {
        message: e.to_string(),
    }
}

/// Pull `(status, url)` out of a `Network.responseReceived` event if it's
/// the main document (`type == "Document"`). Returns `None` for
/// sub-resources (XHR, images, etc.).
fn extract_document_response(evt: &CdpEvent) -> Option<(u16, String)> {
    let kind = evt.params.get("type")?.as_str()?;
    if kind != "Document" {
        return None;
    }
    let response = evt.params.get("response")?;
    let status = response.get("status")?.as_u64()?;
    let url = response.get("url")?.as_str()?.to_owned();
    Some((u16::try_from(status).unwrap_or(0), url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::mock_cdp::{FrameOut, MockCdpServer};

    #[test]
    fn extract_document_response_filters_non_documents() {
        let xhr = CdpEvent {
            method: "Network.responseReceived".into(),
            params: json!({
                "type": "XHR",
                "response": { "status": 200, "url": "https://example.com/api" },
            }),
            session_id: Some("S".into()),
        };
        assert!(extract_document_response(&xhr).is_none());
    }

    #[test]
    fn extract_document_response_picks_main_document() {
        let doc = CdpEvent {
            method: "Network.responseReceived".into(),
            params: json!({
                "type": "Document",
                "response": { "status": 404, "url": "https://example.com/missing" },
            }),
            session_id: Some("S".into()),
        };
        assert_eq!(
            extract_document_response(&doc),
            Some((404_u16, "https://example.com/missing".into()))
        );
    }

    /// Canonical CDP fetch handler used by the integration tests.
    /// Replays the minimum sequence the real Browserbase backend
    /// expects: createTarget → attachToTarget → enable Page+Network →
    /// navigate → frameStoppedLoading + Network.responseReceived →
    /// Runtime.evaluate returning a canned body.
    ///
    /// `body` parameterises what `document.documentElement.outerHTML`
    /// returns. `status` parameterises the navigation HTTP status,
    /// which the backend reads from the synthetic
    /// `Network.responseReceived` event.
    fn happy_path_handler(
        body: &'static str,
        status: u16,
    ) -> impl Fn(&str, &serde_json::Value, Option<&str>) -> Vec<FrameOut> + Send + Sync + 'static
    {
        move |method, params, _sid| match method {
            "Target.createTarget" => vec![FrameOut::Response(json!({ "targetId": "T1" }))],
            "Target.attachToTarget" => vec![FrameOut::Response(json!({ "sessionId": "S1" }))],
            "Page.navigate" => {
                let url = params
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("about:blank")
                    .to_owned();
                vec![
                    FrameOut::Response(json!({ "frameId": "F1" })),
                    FrameOut::Event {
                        method: "Network.responseReceived".into(),
                        params: json!({
                            "type": "Document",
                            "response": { "status": status, "url": url },
                        }),
                        session_id: Some("S1".into()),
                    },
                    FrameOut::Event {
                        method: "Page.frameStoppedLoading".into(),
                        params: json!({ "frameId": "F1" }),
                        session_id: Some("S1".into()),
                    },
                ]
            }
            "Runtime.evaluate" => vec![FrameOut::Response(json!({
                "result": { "type": "string", "value": body },
            }))],
            // Everything else (Page.enable / Network.enable / header
            // configures / Target.closeTarget …) just needs an empty
            // ack. The mock loop records the request regardless, so
            // tests that assert on those commands still see them.
            _ => vec![FrameOut::Response(json!({}))],
        }
    }

    #[tokio::test]
    async fn fetch_returns_status_url_and_body_on_happy_path() {
        let server =
            MockCdpServer::start(happy_path_handler("<html><body>hello</body></html>", 200)).await;
        let cdp = CdpClient::connect(&server.ws_url())
            .await
            .expect("cdp connect to mock");
        let backend = BrowserbaseBackend::from_parts(cdp, "test-session".into());
        assert_eq!(backend.session_id(), "test-session");

        let url = Url::parse("https://example.com/u/torvalds").unwrap();
        let headers = BTreeMap::new();
        let page = backend
            .fetch(&url, &headers, Duration::from_secs(5))
            .await
            .expect("fetch ok");

        assert_eq!(page.status, 200);
        assert_eq!(page.final_url.as_str(), "https://example.com/u/torvalds");
        assert!(page.body.contains("hello"), "body: {}", page.body);
    }

    #[tokio::test]
    async fn fetch_propagates_404_status_from_navigation_response() {
        let server =
            MockCdpServer::start(happy_path_handler("<html><body>404</body></html>", 404)).await;
        let cdp = CdpClient::connect(&server.ws_url()).await.unwrap();
        let backend = BrowserbaseBackend::from_parts(cdp, "test-session".into());

        let url = Url::parse("https://example.com/u/nobody").unwrap();
        let page = backend
            .fetch(&url, &BTreeMap::new(), Duration::from_secs(5))
            .await
            .expect("fetch ok");

        assert_eq!(page.status, 404);
    }

    #[tokio::test]
    async fn fetch_sends_per_site_headers_via_extra_headers_and_ua_override() {
        let server = MockCdpServer::start(happy_path_handler("<html></html>", 200)).await;
        let cdp = CdpClient::connect(&server.ws_url()).await.unwrap();
        let backend = BrowserbaseBackend::from_parts(cdp, "test-session".into());

        let mut headers = BTreeMap::new();
        headers.insert("X-IG-App-ID".into(), "936619743392459".into());
        headers.insert("User-Agent".into(), "Mozilla/5.0 (test)".into());

        backend
            .fetch(
                &Url::parse("https://example.com/u/torvalds").unwrap(),
                &headers,
                Duration::from_secs(5),
            )
            .await
            .expect("fetch ok");

        let log = server.received().await;
        let ua = log
            .iter()
            .find(|r| r.method == "Network.setUserAgentOverride")
            .expect("setUserAgentOverride was sent");
        assert_eq!(
            ua.params
                .get("userAgent")
                .and_then(serde_json::Value::as_str),
            Some("Mozilla/5.0 (test)"),
            "UA override params: {:?}",
            ua.params
        );

        let extras = log
            .iter()
            .find(|r| r.method == "Network.setExtraHTTPHeaders")
            .expect("setExtraHTTPHeaders was sent");
        let map = extras
            .params
            .get("headers")
            .and_then(serde_json::Value::as_object)
            .expect("headers object");
        assert_eq!(
            map.get("X-IG-App-ID").and_then(serde_json::Value::as_str),
            Some("936619743392459")
        );
        // User-Agent must be routed via setUserAgentOverride, not
        // duplicated into setExtraHTTPHeaders.
        assert!(
            !map.contains_key("User-Agent"),
            "User-Agent leaked into setExtraHTTPHeaders: {map:?}"
        );

        // Navigation must happen *after* both header configurations.
        let nav_idx = log
            .iter()
            .position(|r| r.method == "Page.navigate")
            .unwrap();
        let ua_idx = log
            .iter()
            .position(|r| r.method == "Network.setUserAgentOverride")
            .unwrap();
        let extras_idx = log
            .iter()
            .position(|r| r.method == "Network.setExtraHTTPHeaders")
            .unwrap();
        assert!(
            ua_idx < nav_idx && extras_idx < nav_idx,
            "headers must be set before navigate; got order: \
             ua={ua_idx} extras={extras_idx} nav={nav_idx}"
        );
    }

    #[tokio::test]
    async fn fetch_skips_header_commands_when_no_headers_given() {
        let server = MockCdpServer::start(happy_path_handler("<html></html>", 200)).await;
        let cdp = CdpClient::connect(&server.ws_url()).await.unwrap();
        let backend = BrowserbaseBackend::from_parts(cdp, "test-session".into());

        backend
            .fetch(
                &Url::parse("https://example.com/u/x").unwrap(),
                &BTreeMap::new(),
                Duration::from_secs(5),
            )
            .await
            .expect("fetch ok");

        let methods: Vec<String> = server
            .received()
            .await
            .into_iter()
            .map(|r| r.method)
            .collect();
        assert!(
            !methods.iter().any(|m| m == "Network.setExtraHTTPHeaders"),
            "setExtraHTTPHeaders should not fire on empty headers; saw {methods:?}"
        );
        assert!(
            !methods.iter().any(|m| m == "Network.setUserAgentOverride"),
            "setUserAgentOverride should not fire on empty headers; saw {methods:?}"
        );
    }
}
