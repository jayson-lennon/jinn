//! The canvas slice — one HTML artifact, served inside the running jinn
//! over loopback HTTP with embedded vendored visualization libraries and
//! WebSocket live reload.
//!
//! The agent writes an artifact file with its ordinary file tools and
//! serves it through the `canvas` tool, which asks this slice's actor.
//! The server runs on the [`CanvasActor`]: dormant until the first
//! serve, live until jinn exits — no detached process, no state file,
//! no CLI surface.
//!
//! Units, by module:
//!
//! - [`vendor`] — the embedded `/vendor/` inventory and its manifest pin.
//! - [`server`] — the axum listener: artifact route with reload-snippet
//!   injection, vendor route, WebSocket route, health marker.
//! - [`watcher`] — the notify-based directory watcher driving reloads.
//! - [`errors`] — the serve path's refusal vocabulary and fix sentences.
//! - [`canvas_actor`] — the serving actor and the tool-facing handle.
//! - [`status`] — the dashboard row's status strings.
//!
//! The artifact skeleton ships inside the canvas skill
//! (`skills/canvas/references/artifact-template.md`), not here: the
//! template is a file the agent reads, not a binary constant.

pub mod canvas_actor;
pub mod errors;
pub mod server;
pub mod status;
#[cfg(test)]
mod status_tests;
pub mod vendor;
pub mod watcher;

/// The generated vendor-asset table, built from
/// `resources/vendor/vendor-manifest.toml` by `build.rs`.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/vendor_assets.rs"));
}

pub use generated::{VENDOR_ASSETS, VendorAsset};

use std::sync::Arc;

use error_stack::ResultExt as _;
use jinn_slices::SliceHost;
use trouper::system::ActorSystem;

use crate::canvas_actor::CanvasActor;
use crate::canvas_actor::CanvasActorDeps;

/// Installs the canvas actor and hands its serve handle to composition.
///
/// Activation itself does no serving: the actor spawns dormant (no
/// listener binds, no watcher establishes, no filesystem access) until
/// the first `canvas` tool call asks it to serve an artifact.
///
/// `config` is the same `jinn.toml` layer every slice receives — the
/// bind is read here, at activation; a changed bind takes effect on the
/// next jinn start. `set_handle` receives the tool-facing
/// [`jinn_canvas_msg::CanvasServerHandle`]: the kernel owns the slot it
/// lands in, the slice owns the actor behind it, and neither names the
/// other's internals.
///
/// # Errors
///
/// Returns the bind parse failure when `[canvas] bind` is not a
/// host:port socket address: a broken bind cannot serve anything, so
/// the failure aborts launch where it is visible instead of refusing
/// every later serve ask with the same sentence.
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &jinn_config::ConfigLayer,
    bus: jinn_slices::bus::BusService,
    set_handle: &dyn Fn(Arc<dyn jinn_canvas_msg::CanvasServerHandle>),
) -> Result<(), error_stack::Report<jinn_preferences_config::schemas::CanvasBindError>> {
    let canvas_config = config
        .get::<jinn_preferences_config::schemas::CanvasConfig>()
        .change_context(jinn_preferences_config::schemas::CanvasBindError)
        .attach("failed to read the [canvas] config section")?;
    let bind = canvas_config.parse_bind()?;
    let system: ActorSystem = host.system().clone();
    let path = CanvasActor::spawn(&system, &CanvasActorDeps { bind, bus });
    set_handle(canvas_actor::canvas_server_handle(system, path));
    Ok(())
}
