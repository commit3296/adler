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
use futures::StreamExt as _;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
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
pub struct BrowserbaseBackend {
    browser: Browser,
    _handler: JoinHandle<()>,
    session_id: String,
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

        let (browser, mut handler) =
            Browser::connect(&session.connect_url)
                .await
                .map_err(|e| Error::BrowserSetup {
                    message: format!("connect CDP: {e}"),
                })?;
        let _handler = tokio::spawn(async move {
            while let Some(res) = handler.next().await {
                if res.is_err() {
                    break;
                }
            }
        });

        tracing::info!(session_id = %session.id, "browserbase session opened");
        Ok(Self {
            browser,
            _handler,
            session_id: session.id,
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

        let work =
            async {
                let page = self.browser.new_page("about:blank").await.map_err(|e| {
                    Error::BrowserSetup {
                        message: format!("new_page: {e}"),
                    }
                })?;
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
                let _ = page.close().await;
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
