//! The webui slice — the browser-facing mirror of the terminal UI.
//!
//! A live instance serves an unauthenticated HTTP web UI bound per the
//! `[webui]` config section. The page lists loaded and archived
//! sessions, renders the active session's full-fidelity transcript,
//! sends messages, switches models, and cancels a running turn. Live
//! updates stream over SSE from a fan-out actor that batches stream
//! tokens into HTML fragments.
//!
//! The design invariant: **the web UI is a pull renderer, not a state
//! holder.** Every GET renders server-side from `AppState`/cells —
//! exactly what a TUI frame does, on demand, for one viewer — and the
//! cursor-addressed transcript endpoint is the correctness dependency.
//! The fan-out actor holds transport state only (a broadcast channel
//! plus per-session token buffers): it owns nothing authoritative,
//! never replays, and a crash loses nothing. A reconnecting page
//! re-pulls everything after its cursor at connect time and
//! re-subscribes.
//!
//! Zero new bus schemas: actions publish the existing commands
//! (`EnqueueUserMessage`, `CancelTurn`, `ProviderSwitch`,
//! `SessionLoadRequested`); the fan-out subscribes existing events.
//!
//! The web UI and the TUI share one active session per instance —
//! switching from the web publishes the same `SessionLoadRequested`
//! the TUI's picker does, so the terminal's view moves too.
//!
//! Units, by module:
//!
//! - [`contract`] — the client/server DOM-and-URL contract: every id,
//!   attribute, and path the browser client targets, with the
//!   referential-integrity and parity gates beside it.
//! - [`listener`] — the listener actor: bind, dashboard note, serve
//!   task. The bind is frozen: read once here, never re-read on
//!   config reload, because a rebind would strand every connected
//!   page's SSE stream.
//! - [`server`] — the axum routes and the shared handler state: the
//!   cross-region chrome (page shell, SSE endpoint, action posts,
//!   vendor, health).
//! - [`region`] — the region registry: the trait each page region
//!   declares, and the fan-out/router's resolution surface.
//! - [`regions`] — one module per region (sessions, transcript,
//!   composer, model picker): the region's renders, pull handler, and
//!   settle declaration.
//! - [`delivery`] — the action paths: validate → publish, with the
//!   remote slice's one-guard discipline.
//! - [`transcript`] — the transcript projection and render functions.
//! - [`fanout`] — the SSE fan-out actor: events in, HTML fragments
//!   out.
//! - [`vendor`] — the embedded, pre-gzipped frontend asset set (the
//!   compiled client bundle plus any third-party dists).
//! - [`templates`] — the askama page and fragment templates.
//!
//! Like the remote surface there is no authentication; unlike remote,
//! non-loopback binds are taken as configured — reaching the UI over a
//! VPN is the intended use, and endpoint security is the operator's.

pub mod contract;
pub mod delivery;
pub mod fanout;
pub mod listener;
pub mod region;
pub mod regions;
pub mod server;
pub mod templates;
pub mod transcript;
pub mod vendor;

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
mod region_tests;

#[cfg(test)]
mod serve_tests;

#[cfg(test)]
mod smoke_tests;

use error_stack::ResultExt as _;
use jinn_kernel::State;
use jinn_slices::SliceHost;
use jinn_slices::bus::BusService;

/// Why webui activation failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum WebUiActivationError {
    /// The `[webui]` section failed to read, the bind failed to parse,
    /// or the listener could not bind.
    #[error(debug)]
    Startup,
}

/// Installs the web UI: read the frozen bind → spawn the listener.
///
/// Activation is fail-fast, mirroring remote: a broken bind cannot
/// serve anything, so the failure aborts launch where it is visible
/// instead of starting an instance nothing can reach. There is no
/// loopback refusal — the bind is taken as configured, because the
/// intended reach is a VPN the operator already secures.
///
/// The bind is read **once**: no config-reload handling exists in the
/// slice. A live `[webui]` edit applies at the next restart. The
/// function is async because the bind is; composition's `activate_all`
/// already runs on the actor runtime.
///
/// # Errors
///
/// Returns [`WebUiActivationError`] when the `[webui]` section is
/// unreadable, the bind is unparseable, or the port cannot be bound.
///
/// # Panics
///
/// Panics when the shared cell catalog has not registered the provider
/// slot — a bootstrap-ordering bug (the catalog block must run before
/// this slice), which aborts launch by contract.
pub async fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &jinn_config::ConfigLayer,
    bus: BusService,
    state: State,
    session_store: jinn_session_state::SessionStoreService,
) -> Result<(), error_stack::Report<WebUiActivationError>> {
    // Frozen at activation, deliberately: the web page's SSE streams are
    // long-lived pipes tied to the address they opened on. A hot rebind
    // (the remote surface's trick) would strand every connected page
    // with no way to discover the new port. A fixed default port makes
    // the URL bookmarkable; a changed bind takes effect next restart.
    let webui_config = config
        .get::<jinn_preferences_config::schemas::WebUiConfig>()
        .change_context(WebUiActivationError::Startup)
        .attach("failed to read the [webui] config section")?;
    let bind = webui_config
        .parse_bind()
        .map_err(|report| report.change_context(WebUiActivationError::Startup))?;

    // The provider cell, for the model picker. Resolved from the shared
    // catalog (registered in composition's block 0, before any slice
    // activates) — the same reader the provider pickers use.
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

    let system: trouper::system::ActorSystem = host.system().clone();
    let fanout = fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone());

    listener::WebUiListenerActor::spawn(
        &system,
        &listener::WebUiListenerActorDeps {
            bind,
            state,
            bus,
            session_store,
            provider_cell,
            fanout,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
    .await
    .change_context(WebUiActivationError::Startup)
    .attach("failed to spawn the webui listener actor")?;
    Ok(())
}

#[cfg(test)]
mod fanout_tests;

#[cfg(test)]
mod listener_tests;

#[cfg(test)]
mod server_tests;
