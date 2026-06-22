//! Local headless Chrome / Chromium backend.
//!
//! Launches a long-lived browser process via [`chromiumoxide`] and drives
//! it through the Chrome `DevTools` Protocol. Free to use; requires that
//! Chrome / Chromium is installed on the host. The user can pass a
//! [`LocalConfig::proxy_url`] which is forwarded to the child process as
//! `--proxy-server=<url>` so the browser inherits Adler's `--proxy` flag.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{Headers, SetExtraHttpHeadersParams};
use futures::StreamExt as _;
use serde_json::Value as JsonValue;
use tempfile::TempDir;
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
    _profile_dir: TempDir,
    // Kept alive for the lifetime of the backend — chromiumoxide commands
    // deadlock if this stream isn't drained.
    handler: JoinHandle<()>,
}

impl LocalBackend {
    /// Launch a fresh headless Chrome process.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`] if Chrome can't be located or the
    /// process fails to start.
    pub async fn launch(cfg: LocalConfig) -> Result<Self> {
        let profile_dir = tempfile::Builder::new()
            .prefix("adler-chrome-")
            .tempdir()
            .map_err(|e| Error::BrowserSetup {
                message: format!("create temporary Chrome profile: {e}"),
            })?;
        let config = browser_config(&cfg, profile_dir.path())?;
        let (browser, mut handler) =
            Browser::launch(config)
                .await
                .map_err(|e| Error::BrowserSetup {
                    message: format!("launch chrome: {e}"),
                })?;
        // Drain handler events for the lifetime of the backend; without
        // this, CDP commands made via `Page` block forever.
        let handler_task = tokio::spawn(async move {
            while let Some(res) = handler.next().await {
                if res.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            browser,
            _profile_dir: profile_dir,
            handler: handler_task,
        })
    }
}

fn browser_config(cfg: &LocalConfig, profile_dir: &std::path::Path) -> Result<BrowserConfig> {
    // Default builder is already headless; use an isolated profile so
    // repeated or parallel Adler runs never contend on Chrome SingletonLock.
    let mut builder = BrowserConfig::builder().user_data_dir(profile_dir);
    if let Some(proxy) = cfg.proxy_url.as_deref() {
        builder = builder.arg(format!("--proxy-server={proxy}"));
    }
    builder
        .build()
        .map_err(|e| Error::BrowserSetup { message: e })
}

impl Drop for LocalBackend {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

#[async_trait]
impl BrowserBackend for LocalBackend {
    async fn fetch(
        &self,
        url: &Url,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<RenderedPage> {
        let start = Instant::now();
        let url_str = url.as_str().to_owned();

        let work =
            async {
                let page = self.browser.new_page("about:blank").await.map_err(|e| {
                    Error::BrowserSetup {
                        message: format!("new_page: {e}"),
                    }
                })?;

                // Per-site overrides (e.g. Instagram's `X-IG-App-ID` +
                // matching `User-Agent`). UA goes through the dedicated
                // override command; the rest via Network.setExtraHTTPHeaders.
                if !headers.is_empty() {
                    let mut ua: Option<&str> = None;
                    let mut extras = serde_json::Map::new();
                    for (k, v) in headers {
                        if k.eq_ignore_ascii_case("user-agent") {
                            ua = Some(v.as_str());
                        } else {
                            extras.insert(k.clone(), JsonValue::String(v.clone()));
                        }
                    }
                    if let Some(ua) = ua {
                        page.set_user_agent(ua)
                            .await
                            .map_err(|e| Error::BrowserSetup {
                                message: format!("set_user_agent: {e}"),
                            })?;
                    }
                    if !extras.is_empty() {
                        page.execute(SetExtraHttpHeadersParams::new(Headers::new(
                            JsonValue::Object(extras),
                        )))
                        .await
                        .map_err(|e| Error::BrowserSetup {
                            message: format!("setExtraHTTPHeaders: {e}"),
                        })?;
                    }
                }

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

                let (status, final_url) = nav.as_ref().map_or_else(
                    || (0_u16, url.clone()),
                    |req| {
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
                    },
                );

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_config_uses_supplied_isolated_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = browser_config(&LocalConfig::default(), dir.path()).unwrap();

        assert_eq!(config.user_data_dir.as_deref(), Some(dir.path()));
    }

    #[test]
    fn browser_config_accepts_proxy_with_isolated_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = LocalConfig {
            proxy_url: Some("socks5://127.0.0.1:9050".into()),
        };
        let config = browser_config(&cfg, dir.path()).unwrap();

        assert_eq!(config.user_data_dir.as_deref(), Some(dir.path()));
    }
}
