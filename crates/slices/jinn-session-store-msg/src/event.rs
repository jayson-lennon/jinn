//! Session-store events published after live-state transitions complete.

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// Emitted after a session is fully initialized and inserted into live state.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session was loaded, initialized, and inserted into state.")]
pub struct SessionLoadCompleted {
    /// The fully initialized session's ID.
    pub session_id: SessionId,
}

impl SessionLoadCompleted {
    /// Returns the completed session ID.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

impl jinn_slices::BusMessage for SessionLoadCompleted {}

/// Emitted once when startup hydration of the unarchived sessions ends.
///
/// Published strictly after the session map's hydration flag is cleared, on
/// every path that clears it: every saved session landed, there were none to
/// load, or the summary read failed. A push-only surface (the the web UI rail)
/// cannot read that flag the way the terminal's every-tick render does, so
/// this event is how it learns the loading indicator must come down.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Startup hydration of the unarchived sessions ended.")]
pub struct StartupHydrationFinished;

impl jinn_slices::BusMessage for StartupHydrationFinished {}

/// Emitted after the session picker's cell is written with the store's rows.
///
/// Published strictly after the cell write, on every path that writes it.
/// The terminal re-reads that cell on every frame and needs no announcement;
/// a push-only surface — the web UI's Archive picker — cannot, so this event
/// is how it learns fresh rows are there to be read.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The session picker's rows were written from the store.")]
pub struct SessionPickerEntriesLoaded;

impl jinn_slices::BusMessage for SessionPickerEntriesLoaded {}

/// Emitted after a session's library membership changes.
///
/// Library membership is a durable flag on the session, so this event tells
/// consumers (the sidebar's Sessions and Library sections) that their view of
/// which sessions are members — and therefore which appear in which list — is
/// stale.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's library membership changed.")]
pub struct SessionLibraryMembershipChanged {
    /// The session whose membership changed.
    pub session_id: SessionId,
    /// The session's new membership state.
    pub in_library: bool,
}

impl jinn_slices::BusMessage for SessionLibraryMembershipChanged {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::{
        SessionLibraryMembershipChanged, SessionLoadCompleted, SessionPickerEntriesLoaded,
        StartupHydrationFinished,
    };
    use jinn_core_types::SessionId;

    #[rstest::rstest]
    #[test]
    fn startup_hydration_finished_round_trips() {
        // Given the field-less event.
        let event = StartupHydrationFinished;

        // When it is serialized and deserialized.
        let json = serde_json::to_string(&event).unwrap();
        let restored: StartupHydrationFinished = serde_json::from_str(&json).unwrap();

        // Then the wire form is the bare unit, and it reads back as the event.
        assert_eq!(json, "null");
        let _: StartupHydrationFinished = restored;
    }

    #[rstest::rstest]
    #[test]
    fn session_picker_entries_loaded_round_trips() {
        // Given the field-less event.
        let event = SessionPickerEntriesLoaded;

        // When it is serialized and deserialized.
        let json = serde_json::to_string(&event).unwrap();
        let restored: SessionPickerEntriesLoaded = serde_json::from_str(&json).unwrap();

        // Then the wire form is the bare unit, and it reads back as the event.
        assert_eq!(json, "null");
        let _: SessionPickerEntriesLoaded = restored;
    }

    #[rstest::rstest]
    #[test]
    fn load_completed_roundtrips_session_id() {
        // Given a fully initialized session id.
        let event = SessionLoadCompleted {
            session_id: SessionId::new(),
        };

        // When serializing and deserializing the event.
        let json = serde_json::to_string(&event).unwrap();
        let restored: SessionLoadCompleted = serde_json::from_str(&json).unwrap();

        // Then the id survives unchanged.
        assert_eq!(restored.session_id, event.session_id);
    }

    #[rstest::rstest]
    #[test]
    fn library_membership_changed_roundtrips() {
        // Given a membership-change event.
        let event = SessionLibraryMembershipChanged {
            session_id: SessionId::new(),
            in_library: true,
        };

        // When serializing and deserializing the event.
        let json = serde_json::to_string(&event).unwrap();
        let restored: SessionLibraryMembershipChanged = serde_json::from_str(&json).unwrap();

        // Then both fields survive unchanged.
        assert_eq!(restored.session_id, event.session_id);
        assert!(restored.in_library);
    }
}
