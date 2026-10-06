//! Asking the phase actor to apply a [`PhaseCommand`].
//!
//! The one ask entry point for every publisher: kernel handlers, the
//! queue actor, the session actor's handlers. The actor path is static
//! and the reply decodes to a [`PhaseDecision`]; a refused decision
//! means the caller publishes nothing — that is the whole admission
//! policy, in one place.

use error_stack::{Report, ResultExt};
use trouper::actor::ActorPath;

use crate::common::services::Services;
use jinn_session_msg::phase_command::{PhaseCommand, PhaseDecision};

/// How long a phase ask may run. The actor's work is in-memory state
/// application; the timeout exists to bound a wedged fabric, not to
/// tolerate slow work.
const PHASE_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Why a phase command could not be applied.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct PhaseApplyError;

/// Applies one phase command through the phase actor and returns its
/// decision.
///
/// # Errors
///
/// Returns [`PhaseApplyError`] when the actor is absent (composition
/// bug — it spawns before any publisher), the ask times out, or the
/// reply fails to decode.
pub async fn apply_phase(
    services: &Services,
    command: PhaseCommand,
) -> Result<PhaseDecision, Report<PhaseApplyError>> {
    let reply = services
        .trouper_system
        .ask(
            ActorPath::new(jinn_session_msg::phase_command::SESSION_PHASE_PATH),
            command,
            PHASE_ASK_TIMEOUT,
        )
        .await
        .change_context(PhaseApplyError)
        .attach("phase actor ask failed; composition must spawn it before any publisher");
    let reply = match reply {
        Ok(reply) => reply,
        Err(report) => return Err(report),
    };
    reply.decode().map_err(|decode_error| {
        Report::new(PhaseApplyError)
            .attach("phase decision failed to decode")
            .attach(format!("{decode_error:?}"))
    })
}
