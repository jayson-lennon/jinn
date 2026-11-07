//! Stream-rules crossing contracts — the event that re-arms the live
//! matcher.
//!
//! The compiled stream-rule matcher lives in the `jinn-stream-rules` slice's
//! cell, installed once at activation. This crate carries the one event
//! that tells the matcher the rule sources changed, so a configuration
//! write can reach the running matcher without a restart: the stream-rules
//! slice handles the event, and any writer of the configuration publishes
//! it. The crate is the pure contract — data only, no matcher, no actor.

/// The stream-rule sources have been reloaded; rebuild the live matcher
/// from the current configuration.
///
/// A past-tense fact, not a request: the rule files already changed on
/// disk (the stream-rule picker's confirmed write, or the config-watch
/// slice's observed external edit) before this is published. Handled by
/// the `jinn-stream-rules` slice's matcher actor — its one declarant —
/// which re-runs the same read + compile + install path activation ran.
/// Several publishers is legal: an Event broadcasts to every declarant,
/// and there is exactly one.
///
/// Carries no payload: there is exactly one matcher cell and exactly one
/// source of rules, so the event names nothing that could be otherwise.
/// Rebuilding after every change is the whole cost — an idempotent
/// re-compile, not a diff.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "The stream-rule sources changed; rebuild the live matcher.")]
pub struct StreamRulesReloaded;

impl jinn_slices::BusMessage for StreamRulesReloaded {}

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
    fn the_event_is_its_own_schema_named_after_the_type() {
        // Given the reload event.
        // When asking for its schema id.
        // Then it is named after the type — the fabric routes by this id,
        // so a rename would silently orphan a published event's
        // subscribers.
        assert_eq!(
            StreamRulesReloaded::schema_id().name(),
            "StreamRulesReloaded"
        );
    }

    #[rstest::rstest]
    fn the_event_roundtrips_the_wire() {
        // Given the reload event.
        let event = StreamRulesReloaded;

        // When it crosses the wire as JSON and back.
        let payload = serde_json::to_value(&event).expect("serialize");
        let decoded: StreamRulesReloaded = serde_json::from_value(payload).expect("deserialize");

        // Then the decoded event is the same unit — nothing to lose in
        // transit, which is why it carries no payload.
        assert_eq!(
            serde_json::to_value(&decoded).expect("serialize"),
            serde_json::to_value(&event).expect("serialize")
        );
    }
}
