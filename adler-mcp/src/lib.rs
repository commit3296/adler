//! Model Context Protocol server for Adler.
//!
//! Exposes Adler's OSINT capabilities to AI assistants over the MCP
//! protocol (JSON-RPC). Three surfaces:
//!
//! - **Tools** — callable agent actions: `list_sites`, `scan_username`
//!   (with streaming progress notifications), `scan_batch`,
//!   `doctor_check`, `get_scan_history`, `diff_scans`.
//! - **Resources** — browsable data: `adler://registry/sites`,
//!   `adler://registry/tags`, `adler://registry/disabled` (audit
//!   surface for the `disabled_reason` annotations),
//!   `adler://scans/recent`, and the templated `adler://scans/{id}`
//!   / `adler://scans/{from}/diff/{to}`.
//! - **Prompts** — templated OSINT workflows: `investigate_username`,
//!   `audit_registry_health`, `correlate_accounts`.
//!
//! Two transports: stdio (Claude Desktop / local agents) via
//! [`run_stdio`] and HTTP+SSE (remote agents, multi-client) via
//! [`run_http`]. Both drive the same [`AdlerMcp`] service. Launched by
//! `adler --mcp` (stdio) / `adler --mcp-http <addr>` (HTTP).

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
