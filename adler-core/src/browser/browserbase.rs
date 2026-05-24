//! Browserbase cloud backend.
//!
//! Creates a remote browser session on <https://browserbase.com> and
//! drives it via the CDP WebSocket the service exposes. Pays per
//! session-minute (see Browserbase pricing); pool comes with a residential
//! / mobile IP and anti-fingerprint baked in.
//!
//! One session is created per backend instance and reused across all
//! fetches for the lifetime of the scan — keeps cost low and IP stable.
//! For per-site fresh sessions, instantiate a new backend each time.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use futures::StreamExt as _;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use url::Url;

use super::{BrowserBackend, RenderedPage};
use crate::{Error, Result};

const API_BASE: &str = "https://api.browserbase.com/v1";

/// Credentials and target project for [`BrowserbaseBackend::connect`].
#[derive(Debug, Clone)]
pub struct BrowserbaseConfig {
    /// API key from <https://browserbase.com/settings>. Stored as
    /// [`SecretString`] so it doesn't leak into `Debug` output or logs.
    pub api_key: SecretString,
    /// Project id (UUID) the session is created under.
    pub project_id: String,
}

/// Cloud browser session against Browserbase, reused across fetches.
///
/// All fetches share the single tab that the session opens with — we don't
/// call `Browser::new_page`. `chromiumoxide` 0.9's `Browser::connect` has a
/// known race that panics ("Created target not present") on `new_page` when
/// attached to a remote browser, so we sidestep it entirely by reusing the
/// session's pre-existing tab. The `Mutex<Page>` serializes navigations,
/// which is fine — a single page can't handle two `goto` calls in flight.
pub struct BrowserbaseBackend {
    _browser: Browser,
    _handler: JoinHandle<()>,
    session_id: String,
    page: Mutex<Page>,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    id: String,
    #[serde(rename = "connectUrl")]
    connect_url: String,
}

impl BrowserbaseBackend {
    /// Create a session via the Browserbase REST API and attach a
    /// chromiumoxide [`Browser`] to its CDP WebSocket.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`] on authentication, network, or
    /// CDP handshake failure.
    pub async fn connect(cfg: BrowserbaseConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::BrowserSetup {
                message: format!("http client: {e}"),
            })?;

        let body = serde_json::json!({ "projectId": cfg.project_id });
        let resp = http
            .post(format!("{API_BASE}/sessions"))
            .header("x-bb-api-key", cfg.api_key.expose_secret())
            .header("content-type", "application/json")
            .body(body.to_string())
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

        let session: SessionResponse = resp.json().await.map_err(|e| Error::BrowserSetup {
            message: format!("decode session response: {e}"),
        })?;

        let (mut browser, mut handler) =
            Browser::connect(&session.connect_url)
                .await
                .map_err(|e| Error::BrowserSetup {
                    message: format!("connect CDP: {e}"),
                })?;
        // The handler stream MUST be draining before any CDP command
        // (including `fetch_targets` below), or the response is dropped.
        let _handler = tokio::spawn(async move {
            while let Some(res) = handler.next().await {
                if res.is_err() {
                    break;
                }
            }
        });
        // Prime the targets map with the pre-existing tab the session
        // already has, then grab a handle to that tab. Reusing it avoids
        // the chromiumoxide `new_page`-on-connect bug entirely.
        browser
            .fetch_targets()
            .await
            .map_err(|e| Error::BrowserSetup {
                message: format!("fetch_targets: {e}"),
            })?;
        let pages = browser.pages().await.map_err(|e| Error::BrowserSetup {
            message: format!("enumerate pages: {e}"),
        })?;
        let page = pages
            .into_iter()
            .next()
            .ok_or_else(|| Error::BrowserSetup {
                message:
                    "Browserbase session opened with no initial page, so we can't reuse a tab; \
                 creating one ourselves via chromiumoxide::Browser::new_page hits a known \
                 race in chromiumoxide 0.9 (\"Created target not present\" panic on remote \
                 connect). Workaround: use `--browser-backend local` with Chrome installed. \
                 Tracking issue: https://github.com/commit3296/adler/issues — search 'browserbase'."
                        .into(),
            })?;

        tracing::info!(session_id = %session.id, "browserbase session opened");
        Ok(Self {
            _browser: browser,
            _handler,
            session_id: session.id,
            page: Mutex::new(page),
        })
    }

    /// The Browserbase session id (for logs / billing correlation).
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[async_trait]
impl BrowserBackend for BrowserbaseBackend {
    async fn fetch(&self, url: &Url, timeout: Duration) -> Result<RenderedPage> {
        let start = Instant::now();
        let url_str = url.as_str().to_owned();

        let work = async {
            // Serialize navigations on the shared session tab — a single
            // page can't service two goto's in parallel anyway.
            let page = self.page.lock().await;
            page.goto(&url_str).await.map_err(|e| Error::BrowserSetup {
                message: format!("goto {url_str}: {e}"),
            })?;
            let nav =
                page.wait_for_navigation_response()
                    .await
                    .map_err(|e| Error::BrowserSetup {
                        message: format!("wait_for_navigation: {e}"),
                    })?;
            let (status, final_url) = nav.as_ref().map_or((0_u16, url.clone()), |req| {
                let st = req
                    .response
                    .as_ref()
                    .and_then(|r| u16::try_from(r.status).ok())
                    .unwrap_or(0);
                let fu = req
                    .url
                    .as_deref()
                    .and_then(|s| Url::parse(s).ok())
                    .unwrap_or_else(|| url.clone());
                (st, fu)
            });
            let body = page.content().await.map_err(|e| Error::BrowserSetup {
                message: format!("content: {e}"),
            })?;
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
