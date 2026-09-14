//! The discovery settle contract — the slice's only owned crossing type.
//!
//! Emitted by the [`crate::worker::SessionDiscoveryWorker`] when a
//! session's three resource scans have settled (all reported, or the
//! 3000ms budget expired). The only consumer is the slice's notifier
//! (trouper topic, no reverse relay needed).

use jinn_core_types::SessionId;

/// The trouper topic the settled event crosses on. Equals the schema
/// name — the trouper topic-naming convention for slice-local events.
pub const SESSION_DISCOVERY_SETTLED_TOPIC: &str = "SessionDiscoverySettled";

/// A snapshot of what a session's discovery scan settled with.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiscoverySnapshot {
    /// Number of skills discovered.
    pub skill_count: usize,
    /// Error description if the skills scan failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_error: Option<String>,
    /// Number of prompt templates discovered.
    pub prompt_count: usize,
    /// Error description if the prompt scan failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_error: Option<String>,
    /// Number of context files (AGENTS.md / CLAUDE.md) discovered.
    pub context_file_count: usize,
    /// Error description if the context-files scan failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_error: Option<String>,
}

/// Emitted when a session's three resource scans have settled.
///
/// `delayed` is `None` on a normal settle (all three scans reported within
/// 3000ms). It is `Some("discovery delayed by <resource>")` when the
/// safety-net timer fired before all three arrived — surfaced so consumers
/// can show the reason (relevant on slow disks, e.g. ZFS raidz2 on spinners).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionDiscoverySettled {
    /// The session whose discovery settled.
    pub session_id: SessionId,
    /// What the scans settled with.
    pub snapshot: DiscoverySnapshot,
    /// `None` on normal settle; the delayed reason when the safety-net timer fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delayed: Option<String>,
}

impl jinn_slices::BusMessage for SessionDiscoverySettled {}

jinn_slices::crossing_schema!(SessionDiscoverySettled, "SessionDiscoverySettled",
trouper::schema::SchemaKind::Event,
description: "A session's three resource scans have settled (or the budget expired).",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "snapshot" => trouper::schema::FieldTy::Json,
    "delayed" => trouper::schema::FieldTy::Str,
]);

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn session_discovery_settled_roundtrips_all_fields() {
        // Given a settled event with a full snapshot and a delayed reason.
        let event = SessionDiscoverySettled {
            session_id: SessionId::new(),
            snapshot: DiscoverySnapshot {
                skill_count: 3,
                skill_error: None,
                prompt_count: 1,
                prompt_error: None,
                context_file_count: 2,
                context_error: None,
            },
            delayed: Some("discovery delayed by skills".to_owned()),
        };

        // When serializing then deserializing.
        let json = serde_json::to_string(&event).expect("serialize");
        let back: SessionDiscoverySettled = serde_json::from_str(&json).expect("deserialize");

        // Then all fields are preserved.
        assert_eq!(back.session_id, event.session_id);
        assert_eq!(back.snapshot, event.snapshot);
        assert_eq!(back.delayed, event.delayed);
    }

    #[rstest::rstest]
    fn session_discovery_settled_omits_delayed_when_none() {
        // Given a settled event with no delay.
        let event = SessionDiscoverySettled {
            session_id: SessionId::new(),
            snapshot: DiscoverySnapshot::default(),
            delayed: None,
        };

        // When serializing.
        let json = serde_json::to_string(&event).expect("serialize");

        // Then the `delayed` key is absent from the wire.
        assert!(
            !json.contains("delayed"),
            "delayed should be omitted when None, got: {json}"
        );
    }
}
