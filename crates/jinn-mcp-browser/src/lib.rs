//! The rmcp server handler exposing the browser daemon's tools.
//!
//! Eleven tools, all thin clients of the browser daemon's loopback HTTP API.
//! This server process holds no Chrome connection — it is stateless by
//! design, so a config-edit restart of this child (recorded jinn behavior)
//! never recycles the browser.

pub mod server;
#[cfg(test)]
mod server_tests;

use error_stack::Report;
use error_stack::ResultExt;
use wherror::Error;

/// Fatal errors while bringing the browser MCP server up.
#[derive(Debug, Error)]
#[error(debug)]
pub struct ServeError;

/// Serves the browser MCP server over stdio until the client disconnects.
///
/// The server is built on the caller's thread, *before* this function's
/// async context: `DaemonClient` wraps a `reqwest::blocking::Client`,
/// whose construction panics inside a tokio runtime. The dispatch path
/// runs `serve` under `block_on`, so a lazily-built client would kill the
/// child at startup (recorded failure mode of the first cut).
///
/// # Errors
///
/// Returns an error if the stdio handshake or transport fails.
pub fn serve() -> Result<(), Report<ServeError>> {
    // Blocking-client construction must stay off the runtime thread: the
    // server (and its reqwest blocking client) is built here, then the
    // serve future runs on a fresh runtime the dispatch thread never
    // enters as an async context.
    let server = server::BrowserMcpServer::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .change_context(ServeError)
        .attach("MCP runtime failed to start")?;
    runtime.block_on(self::serve_with(server))
}

/// Serves an already-built server over stdio until the client disconnects.
///
/// # Errors
///
/// Returns an error if the stdio handshake or transport fails.
async fn serve_with(server: server::BrowserMcpServer) -> Result<(), Report<ServeError>> {
    tracing::info!("browser MCP server listening on stdio");

    let transport = rmcp::transport::io::stdio();
    let running = rmcp::service::ServiceExt::serve(server, transport)
        .await
        .change_context(ServeError)
        .attach("MCP stdio handshake failed")?;

    // Completes when the transport closes (client disconnect / stdin EOF).
    let _ = running.waiting().await;

    tracing::info!("browser MCP server shutting down");
    Ok(())
}

#[cfg(test)]
mod test_only {
    /// The first cut's shape: `BrowserMcpServer::new` under `block_on`.
    ///
    /// # Errors
    ///
    /// Mirrors [`super::serve_with`]'s contract.
    pub(crate) async fn serve_built_inside_runtime() -> Result<(), super::Report<super::ServeError>>
    {
        super::serve_with(super::server::BrowserMcpServer::new()).await
    }
}
