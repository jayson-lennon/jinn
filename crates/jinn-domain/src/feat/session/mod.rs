//! Session management - session lifecycle, persistence, and loading.
//!
//! Provides persistence types ([`ChatSessionState`], [`SessionStore`], etc.)
//! used by the session actor, services container, and component crate.
//! Also contains the session actor, intent handlers, validators, entry loaders,
//! and picker rendering.

pub mod session_store;
pub mod session_summary;
pub mod sessions_list;

pub mod chat_session;
pub mod entries;
#[cfg(test)]
mod entries_tests;
#[cfg(test)]
#[path = "history_editor_tests.rs"]
mod history_editor_tests;
pub mod intent;
pub mod mutation_accumulator;
pub mod phase_machine;
pub mod picker_entry;
pub mod profile;
pub mod protocol;
pub mod prune_report;
pub mod session_actor;
pub mod steering_buffer;
pub mod token_stats;
pub mod tree_aggregate;
pub mod turn_counter;

#[cfg(test)]
mod token_stats_tests;
#[cfg(test)]
mod tree_aggregate_tests;

pub use tree_aggregate::{
    FrozenTreeNode, TreeAggregateStats, aggregate_tree_stats, find_tree_root, snapshot_frozen_node,
};
pub mod validator;

pub use chat_session::{ChatSessionState, SessionCore, SessionUi};
pub use profile::SessionProfile;
pub use session_store::{
    PoolConfig, SessionStore, SessionStoreError, SessionStoreService, SqliteSessionStore,
};
pub use session_summary::SessionSummary;
pub use token_stats::{AggregatedTokenStats, TokenRecord, TokenStats, aggregate_session_stats};
pub use turn_counter::compute_turn_count;

/// Returns a guidance message for when no API keys are found.
///
/// Instructs the user to create a `.env` file and shows the path to
/// `providers.toml` for reference. Uses [`crate::protocol::ChatEntry::info`]
/// so the message is excluded from LLM context.
pub fn no_api_keys_msg() -> crate::protocol::ChatEntry {
    let config_path = crate::feat::provider_infra::config_path()
        .to_string_lossy()
        .into_owned();

    let content = format!(
        "\
**No API keys found**

\
Create a `.env` file in your working directory with your API keys.
\
See `{config_path}` for available environment variables."
    );

    crate::protocol::ChatEntry::transient(content)
}

#[cfg(test)]
mod startup_msg_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::ChatEntryKind;

    #[rstest::rstest]
    fn no_api_keys_msg_is_transient_entry() {
        // When creating the no-api-keys message.
        let entry = no_api_keys_msg();

        // Then it is a Transient entry.
        assert!(matches!(entry.kind, ChatEntryKind::Transient(_)));
    }

    #[rstest::rstest]
    fn no_api_keys_msg_contains_guidance() {
        // When creating the no-api-keys message.
        let entry = no_api_keys_msg();

        // Then it mentions guidance keywords.
        let text = entry.text();
        assert!(text.contains("No API keys found"), "should mention header");
        assert!(text.contains(".env"), "should mention .env");
        assert!(
            text.contains("providers.toml"),
            "should mention providers.toml"
        );
    }
}
