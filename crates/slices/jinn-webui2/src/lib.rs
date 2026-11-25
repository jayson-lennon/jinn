//! The second web UI slice — the message-driven mirror of the terminal UI.
//!
//! A live instance serves a browser page at `/web2`, bound per the
//! `[webui2]` config section, that mirrors the terminal: it shares the
//! instance's single active session, streams the transcript, and sends,
//! queues, cancels, switches sessions and models, and renames — each by
//! publishing the same bus messages the terminal publishes.
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
mod connections;
mod frames;
mod listener;
mod outbound;
mod protocol;
mod revision;
mod server;
mod snapshot;

#[cfg(test)]
mod tests;

/// Why the second web UI's activation failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum WebUi2ActivationError {
    /// The `[webui2]` section failed to read or the bind failed to parse.
    #[error(debug)]
    Startup,
    /// The configured address could not be bound.
    #[error(debug)]
    Bind,
}

/// Installs the second web UI: spawns its outbound actor and binds `/web2`.
///
/// Activation is fail-fast, like the original web UI: a broken bind cannot
/// serve anything. The bind is read **once**; a live `[webui2]` edit
/// applies at the next restart.
///
/// # Errors
///
/// Returns [`WebUi2ActivationError::Startup`] when the `[webui2]` section
/// is unreadable or the bind unparseable, and
/// [`WebUi2ActivationError::Bind`] when the address is taken.
pub async fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &ConfigLayer,
    bus: BusService,
    state: State,
    session_store: jinn_session_state::SessionStoreService,
    services: &Services,
) -> Result<(), error_stack::Report<WebUi2ActivationError>> {
    let webui2_config = config
        .get::<jinn_preferences_config::schemas::WebUi2Config>()
        .change_context(WebUi2ActivationError::Startup)
        .attach("failed to read the [webui2] config section")?;
    let bind = webui2_config
        .parse_bind()
        .map_err(|report| report.change_context(WebUi2ActivationError::Startup))?
        .to_string();

    // The provider cell, for the model picker's rows. Resolved from the
    // shared catalog the way the terminal's picker resolves it.
    #[expect(
        clippy::expect_used,
        reason = "bootstrap assertion: a missing provider slot is a broken boot order and must abort launch, not continue degraded"
    )]
    let provider_cell = host
        .slices()
        .reader::<jinn_provider_selection_msg::ProviderCell>(
            &jinn_provider_selection_msg::provider_state_slot(),
        )
        .expect("the cell catalog registers the provider slot before any slice activates");

    let outbound = outbound::spawn(
        host.system(),
        outbound::OutboundActorDeps {
            state,
            bus: bus.clone(),
            connections: connections::ConnectionRegistry::default(),
            revisions: revision::RevisionTracker::default(),
            services: services.clone(),
            provider_cell,
            session_store,
        },
    );

    let shared = std::sync::Arc::new(server::ServerShared {
        state: outbound.state().clone(),
        outbound: outbound.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });

    let (bound, _outbound) = listener::spawn(
        host.system(),
        &listener::ListenerDeps {
            bind,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            shared,
            bus: bus.clone(),
        },
    )
    .await
    .map_err(|report| report.change_context(WebUi2ActivationError::Bind))?;

    tracing::info!(url = %bound.to_url_base(), "second web UI listening");
    Ok(())
}
