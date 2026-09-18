//! jinn-mcp — MCP client support for jinn.
//!
//! Wraps the `rmcp` crate to connect to an MCP server over a stdio child
//! process, list its tools, and invoke them. This crate is transport-level:
//! it knows nothing about the actor system or `AppState`. The
//! `jinn-mcp-slice` drives it from inside the actor
//! system.

pub mod client;
pub mod tool_mapping;
pub mod transport;

/// Installs the process-wide rustls crypto provider (ring) in this crate's
/// test binary. reqwest is built with `rustls-no-provider` (see the workspace
/// `Cargo.toml`), so without a default provider every `reqwest::Client` panics
/// with "No provider set" at construction. Test binaries never run `main()`.
/// `install_default` errors on the second call; the result is deliberately
/// ignored.
#[cfg(test)]
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub use client::{
    HalfOpenHttp, LivenessProbe, McpClient, McpClientError, McpStderrBuffer, ServerCommand,
};
pub use tool_mapping::provider_prefix;

/// Re-exported so the `McpActor` child watcher can cancel the transport without
/// jinn-domain taking a direct rmcp dependency.
pub use rmcp::service::RunningServiceCancellationToken;

// Re-export rmcp model types so downstream crates (jinn-domain) can pattern-
// match on tool results without taking a direct rmcp dependency.
pub use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};

// Test-only constructors for rmcp types that are `#[non_exhaustive]` and so
// cannot be built with a struct literal from outside the rmcp crate.
// Gated behind the `testkit` feature so production builds never pull these in.
#[cfg(feature = "testkit")]
pub mod testkit;

// A reusable stub MCP server for downstream integration tests.
// Gated behind `server-testkit` so production builds never pull in rmcp's
// server implementation.
#[cfg(feature = "server-testkit")]
pub mod server_testkit;
