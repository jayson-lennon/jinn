//! The remote slice — the external control surface of a running jinn.
//!
//! A live instance serves an unauthenticated loopback HTTP API (`/health`,
//! `/sessions`, `/send`) bound per the `[remote]` config section, and
//! registers itself in an instance-registry file under the state dir so
//! the `jinn remote` CLI (or plain `curl`) can find it. Delivery reuses
//! the two tested chat-input paths: queue → `EnqueueUserMessage`, steer
//! → `SubmitSteeringMessage`, with a steer aimed at an idle session
//! falling back to queue.
//!
//! Strictly delivery: no external path creates a session, and bot
//! sessions are refused.
//!
//! Units, by module:
//!
//! - [`delivery`] — validate → route → publish; text is delivered
//!   verbatim, with no added attribution.
//! - [`server`] — the axum routes and the shared handler state.
//! - [`remote_actor`] — the listener actor: bind, registry write, serve
//!   task, best-effort deregistration.
//!
//! The `jinn remote` CLI client lives in the binary crate
//! (`src/remote_cli.rs`), not here: it needs no slice infrastructure,
//! only the `-msg` contract and the registry file.

pub mod delivery;
pub mod remote_actor;
pub mod server;

#[cfg(test)]
mod remote_tests;

use error_stack::ResultExt as _;
use jinn_kernel::State;
use jinn_slices::SliceHost;

/// Why remote activation failed.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum RemoteActivationError {
    /// The `[remote]` section failed to read, the bind failed to parse,
    /// the listener could not bind, or the registry write failed.
    #[error(debug)]
    Startup,
}

/// Installs the listener actor: bind → registry → serve.
///
/// Activation is fail-fast, mirroring canvas: a broken bind cannot
/// serve anything, so the failure aborts launch where it is visible
/// instead of starting an instance nothing can reach.
///
/// `config` is the same `jinn.toml` layer every slice receives — the
/// bind is read here, at activation; a changed bind takes effect on the
/// next jinn start. The function is async because the bind and the
/// registry write are; composition's `activate_all` already runs on
/// the actor runtime.
///
/// # Errors
///
/// Returns [`RemoteActivationError`] when the `[remote]` section is
/// unreadable, the bind is unparseable or not loopback, the port is
/// taken, or the registry file cannot be written.
pub async fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &jinn_config::ConfigLayer,
    bus: jinn_slices::bus::BusService,
    state: State,
    registry_path: marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> Result<(), error_stack::Report<RemoteActivationError>> {
    let remote_config = config
        .get::<jinn_preferences_config::schemas::RemoteConfig>()
        .change_context(RemoteActivationError::Startup)
        .attach("failed to read the [remote] config section")?;
    let bind = remote_config
        .parse_bind()
        .map_err(|report| report.change_context(RemoteActivationError::Startup))?;

    // Loopback only, no exceptions — the control surface has no
    // authentication, so it must not listen on an exposed interface.
    if !bind.ip().is_loopback() {
        return Err(
            error_stack::Report::new(RemoteActivationError::Startup).attach(format!(
                "the configured [remote] bind {bind} is not a loopback address; \
                 the control surface has no authentication"
            )),
        );
    }

    let system: trouper::system::ActorSystem = host.system().clone();
    let deps = remote_actor::RemoteListenerActorDeps {
        bind,
        state,
        bus,
        registry_path,
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    remote_actor::RemoteListenerActor::spawn(&system, &deps)
        .await
        .change_context(RemoteActivationError::Startup)
        .attach("failed to spawn the remote listener actor")?;
    Ok(())
}
