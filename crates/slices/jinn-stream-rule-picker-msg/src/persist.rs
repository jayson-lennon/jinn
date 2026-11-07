//! The confirm's persist command — the write that leaves the key-handler
//! thread.
//!
//! Pressing `<enter>` on the stream-rule picker publishes
//! [`PersistStreamRuleDisabled`]: the session active at confirm time, the
//! preferences path the picker read its list from, and every pending
//! change as a named struct. The picker slice's persist actor consumes it
//! — the writes run on a blocking worker, and the actor asks for the
//! matcher refresh only after they land. This crate is the pure contract:
//! data and one pure failure formatter, no actor.

use jinn_core_types::SessionId;
use marked_path::{Absolute, MarkedPathBuf};

/// One rule's pending disabled state, as the confirm captured it.
///
/// A wire contract deserves a name: `disabled: true` means "switch the
/// rule off", the same direction the picker's `<tab>` toggles and the
/// storage boundary persists.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StreamRuleDisabledChange {
    /// The rule's name, in the assembled set the picker listed.
    pub name: String,
    /// The pending state: `true` disables the rule.
    pub disabled: bool,
}

/// Persist the stream-rule picker's confirmed disabled set.
///
/// Published by the picker's confirm. Handled by the picker slice's
/// persist actor, which loops the changes through the config layer's
/// comment-preserving storage boundary on a blocking worker and, on
/// success, publishes `StreamRulesReloaded` (the stream-rules
/// contract's event) — the matcher rebuild follows the write, never
/// races it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Persist the stream-rule picker's disabled set, then refresh the matcher.")]
pub struct PersistStreamRuleDisabled {
    /// The session active when ENTER was pressed — where a failure report
    /// lands. Resolved by the chat, not by the actor: if the session is
    /// gone by write time, the push is a benign no-op downstream.
    pub session_id: SessionId,
    /// Where the run's `jinn.toml` lives — the same path the open read the
    /// list from, captured at activation and carried here so the actor
    /// never guesses.
    pub preferences_path: MarkedPathBuf<Absolute>,
    /// Every pending change, in the order the confirm captured them.
    pub changes: Vec<StreamRuleDisabledChange>,
}

impl jinn_slices::BusMessage for PersistStreamRuleDisabled {}

/// Re-exported so every constructor of a failure line imports the error
/// from the same crate that owns the wording — the picker's route module
/// never touches the storage boundary's types directly.
pub use jinn_preferences_config::StreamRulesStorageError;

/// The one-line failure report for a stream-rule persist error.
///
/// Shared by the confirm's status line (a refused confirm, still open) and
/// the persist actor's chat entry (a failure after the picker closed), so
/// the two surfaces cannot drift. The unknown-name wording carries the
/// reopen hint: the picker closed before the file changed underneath it.
#[must_use]
pub fn stream_rule_failure_line(error: &StreamRulesStorageError) -> String {
    match error {
        StreamRulesStorageError::UnknownRule { name } => {
            format!("no stream rule is named `{name}` — the file changed; reopen the picker")
        }
        StreamRulesStorageError::Read { detail } => {
            format!("stream rules could not be read: {detail}")
        }
        StreamRulesStorageError::Write { detail }
        | StreamRulesStorageError::WorkerPanicked { detail } => {
            format!("stream rules could not be written: {detail}")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use trouper::schema::Schema;

    #[rstest::rstest]
    fn persist_stream_rule_disabled_is_its_own_schema_named_after_the_type() {
        // Given the persist command.
        // When asking for its schema id.
        // Then it is named after the type — the fabric routes by this id,
        // so a rename would silently orphan a published command's
        // subscribers.
        assert_eq!(
            PersistStreamRuleDisabled::schema_id().name(),
            "PersistStreamRuleDisabled"
        );
    }

    #[rstest::rstest]
    fn persist_stream_rule_disabled_roundtrips_the_wire_with_its_changes() {
        // Given a persist command naming one session, one path, and two
        // changes in opposite directions. The preferences path is built from a
        // temp root, so it is an absolute path on every platform.
        let root = jinn_testutil::tempdir();
        let command = PersistStreamRuleDisabled {
            session_id: SessionId::new(),
            preferences_path: root
                .path
                .join("jinn.toml")
                .expect("temp root joins a bare name"),
            changes: vec![
                StreamRuleDisabledChange {
                    name: "no-todos".to_owned(),
                    disabled: true,
                },
                StreamRuleDisabledChange {
                    name: "no-yelling".to_owned(),
                    disabled: false,
                },
            ],
        };

        // When it crosses the wire as JSON and back.
        let payload = serde_json::to_value(&command).expect("serialize");
        let decoded: PersistStreamRuleDisabled =
            serde_json::from_value(payload).expect("deserialize");

        // Then every field survived — the actor writes what the confirm
        // captured, so a lossy trip would silently persist the wrong set.
        assert_eq!(decoded.session_id, command.session_id);
        assert_eq!(decoded.preferences_path, command.preferences_path);
        assert_eq!(decoded.changes, command.changes);
    }

    #[rstest::rstest]
    fn the_failure_line_names_an_unknown_rule_with_the_reopen_hint() {
        // Given an unknown-name error — the file changed under the picker.
        let error = StreamRulesStorageError::UnknownRule {
            name: "renamed-under-us".to_owned(),
        };

        // When formatting the failure line.
        let line = stream_rule_failure_line(&error);

        // Then it names the rule and tells the user how to recover.
        assert!(
            line.contains("`renamed-under-us`") && line.contains("reopen the picker"),
            "was: {line}"
        );
    }

    #[rstest::rstest]
    #[case(
        StreamRulesStorageError::Read {
            detail: "disk gone".to_owned(),
        },
        "could not be read"
    )]
    #[case(
        StreamRulesStorageError::Write {
            detail: "disk full".to_owned(),
        },
        "could not be written"
    )]
    fn the_failure_line_carries_a_read_or_write_failures_detail(
        #[case] error: StreamRulesStorageError,
        #[case] stem: &str,
    ) {
        // Given a read or write failure carrying its detail.
        // When formatting the failure line.
        let line = stream_rule_failure_line(&error);

        // Then the line keeps both the failure family and the detail —
        // the status line and the chat entry render this same string.
        assert!(line.contains(stem) && line.contains("disk"), "was: {line}");
    }
}
