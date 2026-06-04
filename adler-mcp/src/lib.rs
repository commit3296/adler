//! Model Context Protocol server for Adler.
//!
//! Exposes Adler's OSINT capabilities to AI assistants over the MCP
//! protocol (JSON-RPC). Three surfaces are planned:
//!
//! - **Tools** — callable agent actions (scan a username, batch scan,
//!   list sites, doctor-check a site, fetch scan history). This first
//!   iteration ships `list_sites` only; the scan / doctor / history
//!   tools land in follow-up commits.
//! - **Resources** — browsable data (live registry, available tags,
//!   disabled entries with reasons, recent scans).
//! - **Prompts** — templated workflows for typical OSINT tasks
//!   (investigate a username, audit registry health, correlate
//!   accounts).
//!
//! Two transports are planned: stdio (Claude Desktop / local agents,
//! shipped now) and HTTP+SSE (remote agents, multi-client, shipped in
//! a follow-up). Launched by `adler --mcp` which forwards into
//! [`run_stdio`].

#![forbid(unsafe_code)]

mod server;
mod transport;

pub use server::AdlerMcp;
pub use transport::HTTP_ENDPOINT;
pub use transport::run_http;
pub use transport::run_stdio;

/// Crate-level result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during MCP server setup or runtime.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// I/O error from a transport or filesystem operation.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialisation/deserialisation error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Adler core error surfaced through the MCP boundary.
    #[error("adler-core: {0}")]
    Core(#[from] adler_core::Error),
    /// MCP service error (transport startup, peer handshake, etc.).
    #[error("mcp service: {0}")]
    Service(String),
}
