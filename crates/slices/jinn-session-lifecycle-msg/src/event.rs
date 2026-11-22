//! Lifecycle event structs - completion callbacks for setup/teardown.
//!
//! [`SessionSetupCompleted`] and [`SessionTeardownFinished`] live in
//! `jinn-session-msg` (the crossing-contract crate) and are re-exported
//! here so the kernel path stays stable.

use marked_path::{Absolute, MarkedPathBuf};

/// The launch directory a legacy blob without a cwd resolves to.
#[expect(
    clippy::expect_used,
    reason = "cwd and temp-dir fallback are absolute on supported platforms"
)]
fn default_cwd() -> MarkedPathBuf<Absolute> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| tempfile::env::temp_dir());
    MarkedPathBuf::<Absolute>::new(cwd).expect("cwd and temp dir are absolute")
}

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_slices::BusMessage;

/// A new chat session was created.
///
/// Emitted when a session is created from a lifecycle — the shared state
/// layer's `handle_session_lifecycle_setup()` inserts the new session into the
/// sessions map and publishes this alongside it. Other actors subscribe to
/// this event to run side effects (e.g., lifecycle scripts).
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A new chat session was created.")]
pub struct SessionCreated {
    /// The newly created session's ID.
    pub session_id: SessionId,
    /// The working directory the session starts in.
    #[serde(default = "default_cwd")]
    pub cwd: MarkedPathBuf<Absolute>,
}

/// A session's working directory changed.
///
/// Emitted by the session-persistence actor when it applies a `SetSessionCwd`
/// command. The discovery scan actors subscribe to this to re-scan skills,
/// prompts, and context files for the session's new cwd.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's working directory changed.")]
pub struct SessionCwdChanged {
    /// The session whose cwd changed.
    pub session_id: SessionId,
    /// The new working directory.
    pub cwd: MarkedPathBuf<Absolute>,
}

impl BusMessage for SessionCwdChanged {}

impl BusMessage for SessionCreated {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn old_session_created_json_without_cwd_deserializes() {
        // Given JSON from before the event carried a cwd.
        let json = r#"{"session_id":"00000000-0000-0000-0000-000000000001"}"#;

        // When deserializing.
        let event: SessionCreated = serde_json::from_str(json).expect("deserialize");

        // Then the cwd falls back to the module's default (backwards
        // compatible) and the session id round-trips.
        assert_eq!(event.cwd, default_cwd());
        assert_eq!(
            event.session_id,
            SessionId::try_from_string("00000000-0000-0000-0000-000000000001")
                .expect("fixed session id")
        );
    }
}
