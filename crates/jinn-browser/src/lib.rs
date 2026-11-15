//! The browser daemon crate: seam, daemon, HTTP API, registry, shared client.
//!
//! This crate is the machinery behind the `browser` tool family. The daemon
//! process (the `daemon` feature) owns the lone headed Chrome instance through
//! the vendored chaser-oxide fork and serves the loopback HTTP control API;
//! everything else in the workspace reaches that API through the client in
//! [`client`], which discovers the daemon via the registry and auto-starts it
//! when none is live.
//!
//! Feature split: the `daemon` feature (default) builds the Chrome-owning
//! process side — [`backend`], [`daemon`], [`api`] — and pulls chaser-oxide,
//! axum, and tokio. Client-only consumers (the MCP server, the luau family)
//! depend on this crate with `default-features = false` and get only
//! [`client`] + [`registry`]: the discovery/auto-start/HTTP-client surface,
//! riding plain `reqwest`.
//!
//! Owning paths here are raw `PathBuf` by necessity: the resolved executable
//! crosses into chaser-oxide's config API (which owns raw paths) and the
//! daemon CLI is the argv boundary where jinn's typed-path machinery is not
//! carried — the same carve-out `jinn-mcp-smolvm` documents.
#![cfg_attr(
    feature = "daemon",
    expect(
        clippy::disallowed_types,
        reason = "daemon argv/config boundary: the resolved executable feeds chaser-oxide's config API (raw-path upstream); jinn-facing inputs are validated absolute before use"
    )
)]

#[cfg(feature = "daemon")]
pub mod api;
#[cfg(feature = "daemon")]
pub mod backend;
pub mod client;
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod registry;
pub mod surface;

pub use surface::{Snapshot, SnapshotRef, TabInfo, WaitCondition};

#[cfg(all(test, feature = "daemon"))]
mod backend_tests;

#[cfg(all(test, feature = "daemon"))]
mod daemon_tests;

/// Fatal errors while bringing the browser daemon up.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct DaemonStartError;
