//! Browser backend for pages that are unusable from raw HTTP.
//!
//! A handful of sites (`bot-protected` tag — `Facebook`, `Threads`, `Snapchat`,
//! `Ko-Fi`, `CodePen`, `DeviantArt`, …) refuse to render anything useful
//! to a plain `reqwest` call: they ship a JavaScript login wall, a
//! Cloudflare challenge, or a TLS-fingerprint check. From Adler's signal
//! perspective the response looks identical for an existing account and a
//! missing one, so the verdict is always `Uncertain`.
//!
//! This module adds a thin abstraction over a *real* browser that can
//! execute JS, accept cookies, present a residential / mobile IP, and
//! return the final post-JS DOM. The existing detection signals
//! (`status_found`, `body_*`, `redirect_absent`) then work on the rendered
//! page exactly as they do on a raw HTTP response.
//!
//! ## Backends
//!
//! - [`local::LocalBackend`] launches a headless Chrome/Chromium process
//!   via [`chromiumoxide`]. Free, runs on the user's machine, requires
//!   Chrome to be installed.
//! - [`browserbase::BrowserbaseBackend`] creates a remote session on
//!   <https://browserbase.com> and connects to it via the CDP WebSocket
//!   the service exposes. Pays per session-minute, no local setup, comes
//!   with a residential / mobile proxy pool out of the box.
//!
//! Both backends drive Chrome through the same chromiumoxide [`Browser`]
//! handle — only the transport (process vs. WebSocket) differs.
//!
//! [`Browser`]: chromiumoxide::Browser

pub mod browserbase;
pub mod budget;
pub mod cdp;
pub mod flaresolverr;
pub mod local;

#[cfg(test)]
pub(crate) mod mock_cdp;

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use url::Url;

use crate::Result;

pub use browserbase::{BrowserbaseBackend, BrowserbaseConfig};
pub use budget::BrowserBudget;
pub use flaresolverr::FlareSolverrBackend;
pub use local::{LocalBackend, LocalConfig};

/// Page state captured after the backend finished loading and JS
/// settled. Fed into the same `Signal` pipeline as a raw HTTP response.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RenderedPage {
    /// Final HTTP response status (after redirects).
    pub status: u16,
    /// Final URL the browser ended up on (after redirects + any
    /// client-side navigation).
    pub final_url: Url,
    /// Outer HTML of the document at the end of the wait.
    pub body: String,
    /// Wall-clock time from `fetch` entry to `Ok`/`Err`, in milliseconds.
    pub elapsed_ms: u64,
}

/// Abstraction over a real browser. Implemented by [`LocalBackend`] and
/// [`BrowserbaseBackend`].
///
/// Backends are reused across many fetches for the lifetime of a scan —
/// they own a long-lived [`chromiumoxide::Browser`] internally. Drop the
/// backend to release the underlying resources (kill the local process or
/// close the remote session).
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    /// Render `url` and return the final page state.
    ///
    /// `headers` are applied to *every* request the page issues (sent via
    /// `Network.setExtraHTTPHeaders` before navigation). The map is keyed
    /// by header name; empty means "no overrides, use defaults". Used by
    /// sites whose JSON APIs require app-id or custom UA — e.g.
    /// Instagram's `web_profile_info` endpoint needs `X-IG-App-ID`.
    ///
    /// Failures (timeout, navigation error, JS crash, etc.) should be
    /// returned as `Err`; the caller will convert them into a
    /// per-site `Uncertain` verdict so a single flaky site can't abort the
    /// scan.
    ///
    /// # Errors
    /// Returns [`Error::BrowserSetup`](crate::Error::BrowserSetup) on
    /// connection / lifecycle problems and a generic browser error string
    /// on per-fetch failures.
    async fn fetch(
        &self,
        url: &Url,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<RenderedPage>;
}
