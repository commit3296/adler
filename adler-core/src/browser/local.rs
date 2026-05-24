//! Local headless Chrome / Chromium backend.
//!
//! Launches a long-lived browser process via [`chromiumoxide`] and drives
//! it through the Chrome DevTools Protocol. Free to use; requires that
//! Chrome / Chromium is installed on the host. The user can pass a
//! [`LocalConfig::proxy_url`] which is forwarded to the child process as
//! `--proxy-server=<url>` so the browser inherits Adler's `--proxy` flag.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt as _;
use tokio::task::JoinHandle;
use url::Url;

use super::{BrowserBackend, RenderedPage};
use crate::{Error, Result};

/// Configuration for [`LocalBackend::launch`].
#[derive(Debug, Default, Clone)]
pub struct LocalConfig {
    /// Forwarded to Chrome as `--proxy-server=<url>` if set. Accepts
    /// `http://…`, `https://…`, or `socks5://…` (with embedded credentials).
    pub proxy_url: Option<String>,
}

/// A headless Chrome instance driven over CDP. Reused across many
/// [`fetch`](Self::fetch) calls until dropped.
pub struct LocalBackend {
    browser: Browser,
    // Kept alive for the lifetime of the backend — chromiumoxide commands
    // deadlock if this stream isn't drained.
    _handler: JoinHandle<()>,
}

impl LocalBackend {
    /// Launch a fresh headless Chrome process.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`] if Chrome can't be located or the
    /// process fails to start.
    pub async fn launch(cfg: LocalConfig) -> Result<Self> {
        // Default builder is already headless; just add proxy if present.
        let mut builder = BrowserConfig::builder();
        if let Some(proxy) = cfg.proxy_url.as_deref() {
            builder = builder.arg(format!("--proxy-server={proxy}"));
        }
        let config = builder
            .build()
            .map_err(|e| Error::BrowserSetup { message: e })?;
        let (browser, mut handler) =
            Browser::launch(config)
                .await
                .map_err(|e| Error::BrowserSetup {
                    message: format!("launch chrome: {e}"),
                })?;
        // Drain handler events for the lifetime of the backend; without
        // this, CDP commands made via `Page` block forever.
        let _handler = tokio::spawn(async move {
            while let Some(res) = handler.next().await {
                if res.is_err() {
                    break;
                }
            }
        });
        Ok(Self { browser, _handler })
    }
}

#[async_trait]
impl BrowserBackend for LocalBackend {
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

                // Pull the response for the navigation — gives us the real HTTP
                // status code and the final URL after redirects.
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

                // Best-effort close — even on failure we already have what we need.
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
