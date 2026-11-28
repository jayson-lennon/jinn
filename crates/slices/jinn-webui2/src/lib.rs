//! The web UI slice — the message-driven mirror of the terminal UI.
//!
//! A live instance serves the web UI at the root of its own listener,
//! bound per the `[webui]` config section, that mirrors the terminal: it shares the
//! instance's single active session, streams the transcript, and sends,
//! queues, cancels, switches, closes and archives sessions, switches models,
//! and renames — each by publishing the same bus messages the terminal
//! publishes. The address
//! the page is served on is noted on the slice's dashboard row.
//!
//! The design invariant: **the browser owns all view state.** The server
//! renders no HTML fragments and reads no client state back; it answers
//! `ClientMsg`s by reading `State` and cells and by publishing existing
//! commands, and it pushes `ServerMsg`s — data, never markup — as the
//! facts change. Every byte across the socket is one of the two enums
//! in `jinn-webui2-msg`.

use error_stack::ResultExt as _;
use jinn_config::ConfigLayer;
use jinn_kernel::{State, common::services::Services};
use jinn_slices::{SliceHost, bus::BusService};

pub use jinn_webui2_msg::WEBUI2_PROTOCOL_VERSION;

mod actions;
mod attachment;
mod connections;
mod frames;
pub(crate) mod health;
mod listener;
mod market;
mod outbound;
mod page;
mod protocol;
mod revision;
mod server;
mod settle;
mod snapshot;

#[cfg(test)]
mod tests;

/// Why the web UI's activation failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum WebUi2ActivationError {
    /// The `[webui]` section failed to read or the bind failed to parse.
    #[error(debug)]
    Startup,
    /// The configured address could not be bound.
    #[error(debug)]
    Bind,
}

/// Installs the web UI: spawns its outbound actor and binds its listener.
///
/// Activation is fail-fast: a broken bind cannot serve anything. The
/// bind is read **once**; a live `[webui]` edit applies at the next
/// restart.
///
/// # Errors
///
/// Returns [`WebUi2ActivationError::Startup`] when the `[webui]` section
/// is unreadable or the bind unparseable, and
/// [`WebUi2ActivationError::Bind`] when the address is taken.
///
/// # Panics
///
/// Panics when the cell catalog has not registered the provider
/// selection slot. That is a boot-order bug, not a runtime condition:
/// the catalog runs before any slice activates.
pub async fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &ConfigLayer,
    bus: BusService,
    state: State,
    services: &Services,
) -> Result<(), error_stack::Report<WebUi2ActivationError>> {
    let webui2_config = config
        .get::<jinn_preferences_config::schemas::WebUi2Config>()
        .change_context(WebUi2ActivationError::Startup)
        .attach("failed to read the [webui] config section")?;
    let bind = webui2_config
        .parse_bind()
        .map_err(|report| report.change_context(WebUi2ActivationError::Startup))?
        .to_string();

    // The provider cell, for the model picker's rows. Resolved from the
    // shared catalog the way the terminal's picker resolves it.
    let provider_cell = resolve_cell::<jinn_provider_selection_msg::ProviderCell>(
        host,
        &jinn_provider_selection_msg::provider_state_slot(),
    );

    // The persona catalog and the MCP runtime projection, for the persona
    // and MCP pickers' rows. Resolved the same way, from the same catalog.
    let personas_cell =
        resolve_cell::<jinn_persona_msg::Personas>(host, &jinn_persona_msg::personas_slot());
    let mcp_runtime_cell =
        resolve_cell::<jinn_mcp_msg::McpRuntimeState>(host, &jinn_mcp_msg::mcp_runtime_slot());

    // The applets catalog and the applets picker, for the applets modal's
    // rows and the server's serving phase. Resolved the same way.
    let applets_catalog_cell = resolve_cell::<jinn_applets_msg::AppletsCatalog>(
        host,
        &jinn_applets_msg::applets_catalog_slot(),
    );
    let applets_picker_cell = resolve_cell::<jinn_applets_msg::AppletsPickerState>(
        host,
        &jinn_applets_msg::applets_picker_slot(),
    );

    let work_time_cell = resolve_cell::<jinn_work_time_msg::WorkingTimeState>(
        host,
        &jinn_work_time_msg::work_time_slot(),
    );

    // The model-market data cell, for the market modal's rows and detail.
    // Resolved from the shared catalog the way the terminal popup does.
    let market_cell = resolve_cell::<jinn_model_market_msg::MarketDataState>(
        host,
        &jinn_model_market_msg::market_data_slot(),
    );
    // The session store slice's picker cell, for the Archive picker's rows:
    // the store actor writes them, and the outbound actor reads them when the
    // write is announced.
    let session_picker_cell = resolve_cell::<jinn_session_store_msg::SessionPickerState>(
        host,
        &jinn_session_store_msg::session_picker_slot(),
    );

    let outbound = outbound::spawn(
        host.system(),
        outbound::OutboundActorDeps {
            state,
            bus: bus.clone(),
            connections: connections::ConnectionRegistry::default(),
            revisions: revision::RevisionTracker::default(),
            settles: settle::SettleTracker::default(),
            services: services.clone(),
            provider_cell,
            work_time_cell,
            personas_cell,
            mcp_runtime_cell,
            applets_catalog_cell,
            applets_picker_cell,
            market_cell,
            session_picker_cell,
            config: services.config.clone(),
        },
    );

    let shared = std::sync::Arc::new(server::ServerShared {
        outbound: outbound.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        health: health::WebUi2Health::new(bus.clone()),
    });

    let (bound, _outbound) = listener::spawn(
        host.system(),
        &listener::ListenerDeps {
            bind,
            shared,
            bus: bus.clone(),
        },
    )
    .await
    .map_err(|report| report.change_context(WebUi2ActivationError::Bind))?;

    tracing::info!(url = %bound.connection_note(), "web UI listening");
    Ok(())
}

/// Resolves one read-only cell from the shared catalog.
///
/// # Panics
///
/// Panics when the catalog has not registered the slot. That is a
/// boot-order bug, not a runtime condition: the catalog runs before any
/// slice activates, so a missing slot means the boot is broken and must
/// abort rather than continue degraded.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: a missing cell slot is a broken boot order and must abort launch, not continue degraded"
)]
fn resolve_cell<T: Send + Sync + 'static>(
    host: &SliceHost<'_, jinn_slices::RenderFacts>,
    slot: &jinn_slices::SlotKey,
) -> jinn_slices::TypedCell<T> {
    host.slices()
        .reader::<T>(slot)
        .expect("the cell catalog registers every web UI slot before any slice activates")
}
