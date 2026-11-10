//! MCP client slice — the coordinator and per-connection actors.
//!
//! One [`coordinator::McpCoordinatorActor`] lives for the whole app; it keeps
//! exactly one [`connection::McpActor`] alive per (session × enabled-server)
//! pair, reconciling session lifecycle events against the session's enablement
//! set.
//!
//! Kernel dependency: the coordinator consumes shared application state
//! and session lifecycle events (`SessionCreated`, `SessionClosed`, …);
//! the asynchronous actor path is independent of synchronous slice actions.
//!
//! The wire contracts live in `jinn-slices::mcp_contracts` (single
//! definition — trouper dispatches by schema id); the kernel reaches the
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

#[cfg(all(test, unix))]
mod dispatch_roundtrip_tests;
#[cfg(test)]
mod header_expansion_tests;
#[cfg(test)]
mod mcp_picker_actions_tests;
#[cfg(test)]
mod mcp_picker_tests;
#[cfg(test)]
mod restart_mcp_tests;
#[cfg(test)]
mod session_cwd_tests;
#[cfg(test)]
mod singleton_connection_tests;
#[cfg(test)]
mod singleton_lifecycle_tests;
#[cfg(test)]
mod transport_routing_tests;

pub mod connection;
pub mod coordinator;
pub mod mcp_picker_actions;
pub mod mcp_picker_render;
pub mod mcp_picker_routes;

use jinn_mcp_msg::{McpCoordinatorHandle, McpRuntimeState, mcp_runtime_slot};
use jinn_slices::{Slices, TypedCell};
use std::sync::Arc;

/// Debug name for the minted handle (service-trait convention).
pub const HANDLE_NAME: &str = "mcp-coordinator";

/// Outer bound on the restart ask (matches the old tool-side `ASK_TIMEOUT`).
const RESTART_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// The mandatory trouper ask timeout for the restart round trip. The outer
/// [`RESTART_ASK_TIMEOUT`] remains authoritative; this inner bound is the
/// runtime's own lease deadline.
const RESTART_INNER_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// Resolves the MCP runtime-state write cell.
///
/// The cell is registered by the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`); this hands the coordinator
/// its handle at spawn time.
///
/// # Panics
///
/// Panics if the catalog has not run — the coordinator would otherwise
/// have no writer for the status and log events it consumes, and the
/// render pass would resolve `None` for every MCP reader.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate_runtime(slices: &Slices) -> TypedCell<McpRuntimeState> {
    slices
        .reader::<McpRuntimeState>(&mcp_runtime_slot())
        .expect("the cell catalog registers the mcp runtime slot before any slice activates")
}

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
                    let outcome = value.decode::<coordinator::McpRestartOutcome>().unwrap_or(
                        coordinator::McpRestartOutcome {
                            ok: false,
                            error: Some("Mailbox".to_owned()),
                        },
                    );
                    match (outcome.ok, outcome.error.as_deref()) {
                        (true, _) => Ok(()),
                        (false, Some("UnknownServer")) => {
                            Err(jinn_mcp_msg::RestartError::UnknownServer)
                        }
                        (false, Some("ConnectFailed")) => {
                            Err(jinn_mcp_msg::RestartError::ConnectFailed)
                        }
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

/// Registers the MCP server inspector: its overlay, its keys, and its
/// filter hook.
///
/// Split from [`activate_runtime`] because the inspector is a menu, not a
/// service: it spawns no actor and needs no services. Its cell comes from
/// the shared catalog, resolved here by slot key.
///
/// # Panics
///
/// Panics if the cell catalog has not run - the picker would render against
/// an absent cell and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate_picker(host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .slices()
        .reader::<jinn_mcp_msg::McpPickerState>(&jinn_mcp_msg::mcp_picker_slot())
        .expect("the cell catalog registers the mcp picker slot before any slice activates");

    let scope = jinn_mcp_msg::mcp_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(mcp_picker_render::mcp_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_mcp_msg::mcp_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(mcp_picker_render::render_mcp_picker),
    );

    // The inspector's keys, and the filter's input hook, are this slice's own.
    mcp_picker_routes::attach_mcp_picker_rows(host.key_routes(), &cell);
    mcp_picker_routes::register_mcp_picker_input_hook(host.key_routes(), &cell);
}
