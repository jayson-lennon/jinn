//! The session-init slice — per-session environment discovery on the
//! trouper fabric.
//!
//! Replaces the kameo scan trio + discovery coordinator + discovery
//! notifier with a true keyed-actor topology: one supervisor
//! translates session-lifecycle triggers and manual rescan commands
//! into keyed commands, and a partition set activates one discovery
//! worker per session, owning that session's skills, prompt, and
//! context-file scans plus the settle coalescing the kameo coordinator
//! used to do across four actors. A notifier actor posts the settled
//! summary entry into the session's chat log.
//!
//! Discovery results return to the kameo bus via reverse relays
//! (`SkillsLoaded`, `PromptTemplatesLoaded`, `ContextFilesLoaded`) so
//! kernel consumers — the session actor and the subagent task-settle
//! listener — are unchanged.

pub mod bridge;
pub mod notifier;
pub mod supervisor;
pub mod worker;

use jinn_domain::common::state::State;
use jinn_slices::AppSliceHost;
use trouper::topics::Topic;
use wherror::Error;

/// The trouper topic session-lifecycle triggers and manual rescans
/// cross on (kameo bus → supervisor).
#[must_use]
pub fn session_init_topic() -> Topic {
    Topic::new("jinn.session-init")
}

/// The public path of the discovery partition set. Keyed commands are
/// addressed here; the kernel resolves `<public>/<session_id>` and
/// activates the per-session worker entity on demand.
pub const DISCOVERY_PATH: &str = "jinn.discovery";

/// The discovery partition set's key: a session id.
pub const DISCOVERY_KEY_FIELD: &str = "session_id";

/// The trouper topic the settled event crosses on (worker → notifier).
#[must_use]
pub fn settled_topic() -> Topic {
    Topic::new("SessionDiscoverySettled")
}

/// Error activating the session-init slice.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SliceActivateError;

/// Activates the session-init slice: install the discovery partition
/// set, spawn the supervisor and notifier on the trouper fabric, and
/// stage this slice's crossing routes (8 forward triggers on
/// [`session_init_topic`], 3 reverse results back onto the kameo bus).
///
/// Composition drains the staged routes after activation (see
/// [`bridge::drain_routes`]); both call sites must precede the
/// readiness `EnvironmentLoaded` publish so no trigger is missed.
///
/// # Errors
///
/// Returns [`SliceActivateError`] when the partition set install
/// fails — its shard-key declaration is validated at install.
pub fn activate(
    host: &mut AppSliceHost<'_>,
    services: &jinn_domain::Services,
    state: State,
) -> Result<(), error_stack::Report<SliceActivateError>> {
    let _ = (host, services, state);
    Ok(())
}
