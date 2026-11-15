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
/// # Errors
///
/// Returns an error if the stdio handshake or transport fails.
pub async fn serve() -> Result<(), Report<ServeError>> {
    let server = server::BrowserMcpServer::new();
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
