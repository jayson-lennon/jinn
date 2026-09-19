//! MCP client slice — the coordinator and per-connection actors.
//!
//! One [`coordinator::McpCoordinatorActor`] lives for the whole app; it keeps
//! exactly one [`connection::McpActor`] alive per (session × enabled-server)
//! pair, reconciling session lifecycle events against the session's enablement
//! set. Both actors moved here verbatim from the kernel's
//! `feat/{mcp_actor,mcp_coordinator_actor}` modules.
//!
//! Kernel dep (Cargo.toml): the coordinator writes
//! `ChatSession.mcp_server_status` through a `SessionCap` and consumes
//! session lifecycle events (`SessionCreated`, `SessionClosed`, …) — the
//! sync `SliceActionState` capability pattern does not fit async kameo
//! actors (sidebar precedent).
//!
//! The wire contracts live in `jinn-slices::mcp_contracts` (single
//! definition — kameo dispatches by `TypeId`); the kernel reaches the
//! coordinator through `jinn_mcp_msg::McpCoordinatorHandle`, minted by
//! [`mcp_coordinator_handle`] at spawn.

/// Installs the process-wide rustls crypto provider (ring) in this crate's
/// test binary — reqwest is built with `rustls-no-provider`, so without this
/// every `reqwest::Client` panics with "No provider set" (jinn-mcp's own ctor
/// only covers its test binary). `install_default` errors on the second call;
/// the result is deliberately ignored.
#[cfg(test)]
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod dispatch_roundtrip_tests;
#[cfg(test)]
mod header_expansion_tests;
#[cfg(test)]
mod restart_mcp_tests;
#[cfg(test)]
mod transport_routing_tests;

pub mod connection;
pub mod coordinator;

use jinn_mcp_msg::McpCoordinatorHandle;
use std::sync::Arc;

/// Debug name for the minted handle (service-trait convention).
pub const HANDLE_NAME: &str = "mcp-coordinator";

/// Outer bound on the restart ask (matches the old tool-side `ASK_TIMEOUT`).
const RESTART_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// The mandatory trouper ask timeout for the restart round trip. The outer
/// [`RESTART_ASK_TIMEOUT`] remains authoritative; this inner bound is the
/// runtime's own lease deadline.
const RESTART_INNER_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// Mint the kernel-side handle from the spawned coordinator actor.
///
/// Composition calls this after the coordinator spawn handshake and stores
/// the result in `Services.mcp_coordinator`; an unset handle keeps the
/// `restart_mcp` tool's graceful-error path.
#[must_use]
pub fn mcp_coordinator_handle(
    system: trouper::system::ActorSystem,
    actor_path: trouper::actor::ActorPath,
) -> Arc<dyn McpCoordinatorHandle> {
    struct Impl(trouper::system::ActorSystem, trouper::actor::ActorPath);

    #[async_trait::async_trait]
    impl McpCoordinatorHandle for Impl {
        async fn restart(
            &self,
            session_id: jinn_core_types::SessionId,
            server: String,
        ) -> Result<(), jinn_mcp_msg::RestartError> {
            // Outer bound so a hung coordinator yields Timeout instead of
            // hanging the tool caller (the old tool-side ASK_TIMEOUT
            // semantics, moved into the seam). The trouper ask has its own
            // MANDATORY timeout — this outer timeout wraps the whole round
            // trip and stays the authoritative bound.
            match tokio::time::timeout(
                RESTART_ASK_TIMEOUT,
                self.0.ask(
                    self.1.clone(),
                    jinn_mcp_msg::RestartMcpServer { session_id, server },
                    RESTART_INNER_ASK_TIMEOUT,
                ),
            )
            .await
            {
                Ok(Ok(value)) => {
                    let outcome = serde_json::from_value::<coordinator::RestartOutcome>(value)
                        .unwrap_or(coordinator::RestartOutcome {
                            ok: false,
                            error: Some("Mailbox".to_owned()),
                        });
                    match (outcome.ok, outcome.error.as_deref()) {
                        (true, _) => Ok(()),
                        (false, Some("UnknownServer")) => Err(jinn_mcp_msg::RestartError::UnknownServer),
                        (false, Some("ConnectFailed")) => Err(jinn_mcp_msg::RestartError::ConnectFailed),
                        (false, Some("Timeout")) => Err(jinn_mcp_msg::RestartError::Timeout),
                        _ => Err(jinn_mcp_msg::RestartError::Mailbox),
                    }
                }
                Ok(Err(_report)) => Err(jinn_mcp_msg::RestartError::Mailbox),
                Err(_) => Err(jinn_mcp_msg::RestartError::Timeout),
            }
        }

        fn name(&self) -> &'static str {
            HANDLE_NAME
        }
    }

    Arc::new(Impl(system, actor_path))
}
