//! Session-lifecycle crossing contracts and shared leaf vocabulary.
//!
//! This crate is the low-level export surface of the session-lifecycle family:
//! setup/teardown messages cross the bus here, while the command-template,
//! builtin-registry, and picker-row types live here. The session-lifecycle
//! picker's identity and per-open state live here too, so the project picker
//! can reach this picker's scope without depending on the implementation
//! crate. The lifecycle slice itself depends on this crate;
//! the kernel depends on this crate. Neither direction forms a cycle.

pub mod arg_input;
pub mod builtin;
pub mod command;
pub mod command_template;
pub mod event;
pub mod lifecycle_script_state;
pub mod picker_entry;
pub mod picker_scope;
pub mod picker_state;

use jinn_core_types::SessionId;
use marked_path::{Absolute, MarkedPathBuf};
use serde::{Deserialize, Serialize};

pub use arg_input::{ArgInputState, arg_input_scope, arg_input_slot};
pub use builtin::{BuiltinHandler, BuiltinHandlerError, BuiltinRegistry};
pub use command::{
    CancelLifecycleCommand, CloseSession, FinishSessionSetup, FinishSessionTeardown,
    RunSessionSetup, RunSessionTeardown, SetSessionCwd, TeardownFollowUp, TeardownSessionTree,
};
pub use command_template::CommandTemplate;
pub use event::{SessionCreated, SessionCwdChanged};
pub use lifecycle_script_state::LifecycleScriptState;
pub use picker_entry::{SessionLifecycleEntry, lifecycle_row};
pub use picker_scope::session_lifecycle_picker_scope;
pub use picker_state::{
    RESULTS_VIEWPORT_FALLBACK, SessionLifecyclePickerState, session_lifecycle_picker_slot,
};

/// The system entry shown while a session setup command is running.
///
/// The kernel IntentHandler publishes this entry before the lifecycle actor
/// takes over, so the message builder belongs to the shared family crate.
#[must_use]
pub fn setup_running_msg() -> jinn_core_types::ChatEntry {
    jinn_core_types::ChatEntry::system("⚙️ Running setup script...")
}

/// Setup command completed (success or failure).
///
/// Emitted by the session-lifecycle actor after running a lifecycle
/// setup command. On success, `cwd` is the directory reported by the
/// command. On failure, `cwd` is the default CWD and `error` contains
/// the failure details.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's setup command completed (success or failure).")]
pub struct SessionSetupCompleted {
    /// The session that was being set up.
    pub session_id: SessionId,
    /// The resulting CWD on success, or default CWD on failure.
    pub cwd: MarkedPathBuf<Absolute>,
    /// Error message if setup failed.
    pub error: Option<String>,
}

/// Teardown command finished (success or failure).
///
/// Emitted by the session-lifecycle actor after running a lifecycle
/// teardown command. On success, the session has already been removed
/// from the sessions map. On failure, the session is still open and
/// `error` describes the problem.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's teardown command finished (success or failure).")]
pub struct SessionTeardownFinished {
    /// The session that was being torn down.
    pub session_id: SessionId,
    /// Error message if teardown failed.
    pub error: Option<String>,
}

impl jinn_slices::BusMessage for SessionSetupCompleted {}
impl jinn_slices::BusMessage for SessionTeardownFinished {}

/// Teardown command started.
///
/// Emitted by the session-lifecycle actor once a teardown command has been
/// spawned. Always followed by a `SessionTeardownFinished` when the command
/// concludes; a teardown refused before anything spawns publishes no started
/// event, only the finished one carrying the refusal.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's teardown command was spawned.")]
pub struct SessionTeardownStarted {
    /// The session being torn down.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for SessionTeardownStarted {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test code: a failed expect aborts the test loudly, which is the desired mode"
    )]

    use super::{SessionSetupCompleted, SessionTeardownFinished};
    use jinn_core_types::SessionId;

    #[rstest::rstest]
    #[test]
    fn lifecycle_events_roundtrip_through_json() {
        // Given one of each lifecycle event.
        let id = SessionId::new();
        let cwd_dir = jinn_testutil::tempdir();
        let cwd = cwd_dir.path.clone();
        let events = (
            SessionSetupCompleted {
                session_id: id.clone(),
                cwd: cwd.clone(),
                error: Some("boom".to_owned()),
            },
            SessionTeardownFinished {
                session_id: id.clone(),
                error: None,
            },
        );

        // When serializing and deserializing the tuple.
        let json = serde_json::to_string(&events).unwrap();
        let round: (SessionSetupCompleted, SessionTeardownFinished) =
            serde_json::from_str(&json).unwrap();

        // Then both survive with their fields intact.
        assert_eq!(round.0.session_id, id);
        assert_eq!(round.0.cwd, cwd);
        assert_eq!(round.0.error.as_deref(), Some("boom"));
        assert_eq!(round.1.session_id, id);
        assert_eq!(round.1.error, None);
    }
}
