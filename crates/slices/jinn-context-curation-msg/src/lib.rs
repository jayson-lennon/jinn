//! Context-curation crossing contracts.
//!
//! The EXPORT surface of the context-curation slice: the command that
//! asks the slice to compact a session's history. Kernel publishers (the
//! chat-input `/compact` and `/compact-all` intent paths) depend on this
//! crate; the slice's compaction actor consumes the command over the
//! `jinn.context-curation` trouper topic.
//!
//! The prune side has no crossing command — prune strategies trigger on
//! `HistoryAppended` (forwarded from `jinn-session-history-msg`) and
//! publish `SubmitHistoryMutations` (also `jinn-session-history-msg`).

pub use jinn_core_types::SessionId;

use serde::{Deserialize, Serialize};

/// Ask the context-curation slice to compact a session's history.
///
/// Published by the `/compact` and `/compact-all` slash commands. The
/// compaction actor receives this command, runs the compaction worker,
/// and submits the resulting mutations via `SubmitHistoryMutations`
/// (plus feedback system entries for queued/skipped/failed outcomes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerCompaction {
    /// The session to compact.
    pub session_id: SessionId,
    /// Whether to force-compact all entries (ignore reserve).
    pub compact_all: bool,
}

/// The trouper topic the context-curation slice's contracts cross on.
#[must_use]
pub fn curation_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.context-curation")
}

impl jinn_slices::BusMessage for TriggerCompaction {}

jinn_slices::crossing_schema!(TriggerCompaction, "TriggerCompaction",
trouper::schema::SchemaKind::Command,
description: "Ask the context-curation slice to compact a session's history.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,
         "compact_all" => trouper::schema::FieldTy::Bool]);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn roundtrips_through_json() {
        // Given a command with a session id and the compact-all flag.
        let cmd = TriggerCompaction {
            session_id: SessionId::new(),
            compact_all: true,
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: TriggerCompaction = serde_json::from_str(&json).expect("deserialize");

        // Then the roundtrip preserves both fields.
        assert_eq!(back.session_id, cmd.session_id);
        assert!(back.compact_all);
    }

    #[rstest::rstest]
    #[test]
    fn deserializes_from_json_payload() {
        // Given a JSON payload shaped like a bridge relay's deserialized
        // body.
        let payload = serde_json::json!({
            "session_id": "01933dc5-2b14-7e21-8f52-3d1d8f4e7f9a",
            "compact_all": false
        });

        // When deserializing into the command.
        let msg: TriggerCompaction = serde_json::from_value(payload).unwrap();

        // Then the fields carry through.
        assert_eq!(
            msg.session_id.to_string(),
            "01933dc5-2b14-7e21-8f52-3d1d8f4e7f9a"
        );
        assert!(!msg.compact_all);
    }

    #[rstest::rstest]
    #[test]
    fn schema_def_carries_name_kind_and_fields() {
        // Given the command's schema definition.
        let schema = <TriggerCompaction as trouper::schema::Schema>::schema_def();

        // Then it is a version-1 command named TriggerCompaction with the
        // two fields.
        assert_eq!(schema.name, "TriggerCompaction");
        assert_eq!(schema.version, 1);
        assert!(matches!(schema.kind, trouper::schema::SchemaKind::Command));
        assert_eq!(schema.fields.len(), 2);
        let first = schema.fields.first().expect("at least one field");
        assert_eq!(first.name, "session_id");
    }
}
