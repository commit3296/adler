//! Transport launchers.
//!
//! Two transports:
//!
//! - **stdio** ([`run_stdio`]) — JSON-RPC over the child process's
//!   stdin / stdout. Covers Claude Desktop, Cursor, and any other
//!   agent that spawns the server as a subprocess. Stdout is reserved
//!   for the protocol stream; tracing and banners go to stderr.
//! - **HTTP+SSE** ([`run_http`]) — Streamable HTTP transport per the
//!   2025-06-18 MCP spec. The server listens on a TCP address; the
//!   client POSTs JSON-RPC and the server responds either with a
//!   plain JSON body or an `Accept: text/event-stream` SSE stream
//!   depending on whether the call expects streaming.
//!
//! Both transports drive the same [`AdlerMcp`] service, so the surface
//! the agent sees is identical regardless of transport.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::StreamableHttpServerConfig;

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

/// Default MCP endpoint path under the HTTP server. Mounted via
/// `axum::Router::nest_service`, so a request to
/// `http://host:port/mcp` reaches the rmcp [`StreamableHttpService`].
pub const HTTP_ENDPOINT: &str = "/mcp";

/// Run the MCP server over HTTP+SSE on `bind`, blocking until the
/// process receives a shutdown signal (Ctrl-C) or the OS interrupts.
///
/// The endpoint is fixed at [`HTTP_ENDPOINT`] (currently `/mcp`) — the
/// agent connects to `http://<bind>/mcp` and speaks the Streamable
/// HTTP variant of MCP. rmcp's `StreamableHttpService` handles both
/// request modes: plain `application/json` responses for atomic
/// tool calls, `text/event-stream` SSE for streamed progress.
///
/// Security defaults match `--web`: the server is intended for
/// loopback / trusted-network binding. rmcp's `allowed_hosts`
/// default (`localhost`, `127.0.0.1`, `::1`) provides a DNS-rebind
/// guard out of the box; if you bind a non-loopback address you must
/// extend the allowed-hosts list yourself.
///
/// # Errors
///
/// Returns an error if the TCP listener can't bind, the axum runtime
/// fails, or the rmcp service factory fails to materialise a server.
pub async fn run_http(server: AdlerMcp, bind: SocketAddr) -> crate::Result<()> {
    let service_factory_server = server.clone();
    let service = StreamableHttpService::new(
        move || Ok(service_factory_server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service(HTTP_ENDPOINT, service);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
