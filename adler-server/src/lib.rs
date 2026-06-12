//! HTTP server for the [Adler](https://github.com/commit3296/adler)
//! OSINT username-search engine.
//!
//! This crate hosts the JSON API and embedded `SolidJS` web UI for
//! Adler. It is a thin shell around [`adler_core`]: scans run through
//! the same [`adler_core::executor`] the CLI uses, and the same
//! [`adler_core::Client`] is shared across all in-process scans.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::net::SocketAddr;
//! use adler_core::{Client, Registry};
//! use adler_server::{AppConfig, serve};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = Registry::default_embedded()?;
//! // Use the caller's filtering rules — the CLI already exposes
//! // --only/--tag/--exclude, so the server just runs whatever site
//! // list it's handed.
//! let filter = adler_core::SiteFilter::default();
//! let sites = registry.filter_with(&filter);
//! let catalog = registry.matches_with(&filter);
//! let client = Client::builder().build()?;
//! let config = AppConfig {
//!     bind: "127.0.0.1:8765".parse::<SocketAddr>()?,
//!     scan_capacity: 32,
//!     scans_dir: None, // or Some(adler_server::default_scans_dir())
//! };
//! serve(sites, catalog, client, config).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Routes
//!
//! | Route                              | Method | Purpose                              |
//! |------------------------------------|--------|--------------------------------------|
//! | `/api/health`                      | GET    | liveness                             |
//! | `/api/sites`                       | GET    | site catalogue                       |
//! | `/api/scan`                        | POST   | start a scan, returns a `scan_id`    |
//! | `/api/scan/{id}`                   | GET    | poll status / final aggregate        |
//! | `/api/scan/{id}/stream`            | GET    | Server-Sent Events                   |
//! | `/api/scan/{id}/retry`             | POST   | retry one site in a scan             |
//! | `/api/scan/{id}/refilter`          | POST   | cancel and restart with new filters  |
//! | `/api/scans`                       | GET    | recent scan history                  |
//! | `/api/access`                      | GET    | read-only access-engine summary      |
//! | `/`                                | GET    | embedded `SolidJS` SPA               |
//!
//! ## Threading and shutdown
//!
//! [`serve`] binds the TCP listener, installs a `SIGINT` / `SIGTERM`
//! graceful-shutdown signal, and runs until the listener closes. All
//! state (registry, client, in-flight scans) lives in an [`AppState`]
//! cloned into each handler — no global mutables.

#![warn(missing_docs)]

use std::net::SocketAddr;
use std::path::PathBuf;

use adler_core::{Client, Site};
use tokio::net::TcpListener;
use tokio::signal;

mod api;
mod assets;
mod error;
mod persist;
mod scan;
mod state;

pub use api::router;
pub use error::{Error, Result};
pub use persist::{
    EvidenceChange, PersistedScan, ScanDiff, ScanTimeline, TimelineEvent, TimelineEventKind,
    TimelineProfile, VerdictChange, apply_historical_confidence_overlay, build_scan_timeline,
    default_dir as default_scans_dir, diff_scans,
};
pub use scan::{FinishedScan, ScanHandle, ScanId, Summary};
pub use state::AppState;

/// Server configuration.
///
/// `bind` is the TCP socket the server listens on; defaults are
/// imposed by the caller (the CLI binds `127.0.0.1:8765` and refuses
/// to bind a non-loopback address unless explicitly told to — there
/// is no authentication on the API).
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Address to bind the HTTP listener.
    pub bind: SocketAddr,
    /// Maximum number of recent scans retained in memory.
    pub scan_capacity: usize,
    /// Directory for on-disk scan history. `None` disables persistence.
    /// The CLI defaults to [`default_scans_dir`].
    pub scans_dir: Option<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8765)),
            scan_capacity: 32,
            scans_dir: None,
        }
    }
}

/// Run the server until the listener closes or a shutdown signal arrives.
///
/// `sites` is the pre-filtered enabled site list every scan dispatched
/// through this server runs against. `catalog` is the same startup filter
/// including disabled/parked entries so API/UI surfaces can explain why a
/// site is unavailable. `client` is the pre-built HTTP client (so
/// configuration like proxy, throttle, and browser backend flows from the
/// CLI flags through here unchanged).
pub async fn serve(
    sites: Vec<Site>,
    catalog: Vec<Site>,
    client: Client,
    config: AppConfig,
) -> Result<()> {
    let mut state = AppState::with_catalog(sites, catalog, client, config.scan_capacity);
    if let Some(dir) = config.scans_dir.clone() {
        state = state.with_scans_dir(dir);
    }
    let app = assets::attach(router(state));

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|source| Error::Bind {
            addr: config.bind.to_string(),
            source,
        })?;
    tracing::debug!(bind = %config.bind, "adler-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::Server)?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            tracing::warn!(error = %err, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => tracing::warn!(error = %err, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");
}
