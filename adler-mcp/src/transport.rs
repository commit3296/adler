//! Transport launchers.
//!
//! Stdio is the only transport implemented in this first iteration —
//! it covers Claude Desktop, Cursor, and any other local agent that
//! pipes JSON-RPC over a child process's stdin/stdout. HTTP+SSE for
//! remote agents lands in a follow-up commit.

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::AdlerMcp;

/// Run the MCP server over stdio, blocking until the client closes
/// the connection (typically EOF on stdin).
///
/// Use this from `adler --mcp`. The CLI sets up `tracing` to write to
/// stderr only — stdout is reserved for the JSON-RPC protocol stream
/// and must stay clean.
///
/// # Errors
///
/// Returns an error if the transport handshake fails or the server
/// loop terminates abnormally.
pub async fn run_stdio(server: AdlerMcp) -> crate::Result<()> {
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| crate::Error::Service(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| crate::Error::Service(e.to_string()))?;
    Ok(())
}
