//! The token-count slice — per-entry token counting and cache eviction.
//!
//! Owns the shared [`HistoryWorkerChatEntryTokenCache`] cell
//! ([`token_cache_slot`]) and hosts the two trouper [`ServiceActor`]s that
//! consume it: the count actor (fills `ChatEntry::token_count` in memory
//! on history events) and the eviction actor (clears a closed session's
//! entries), both fed by the `jinn.token-count` forward route. The session
//! actor's accumulation gate and the prune workers hold clones of the same
//! cache for their reads.
//!
//! Kernel dependency (see Cargo.toml): both actors write through tcaps
//! (State + SessionCap), granted at activation.

pub mod bridge;
pub mod count_actor;
pub mod eviction_actor;

use trouper::schema::Schema;

use jinn_slices::SliceHost;

pub use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;
pub use jinn_token_count_msg::token_cache_slot;

/// The token-count slice's crossing topic (`jinn.token-count`): kernel
/// session events forward onto it for the slice's actors.
#[must_use]
pub fn token_count_topic() -> trouper::topics::Topic {
    trouper::topics::Topic::new("jinn.token-count")
}

/// Activates the slice: registers the shared token-cache cell, spawns the
/// count + eviction actors on trouper and subscribes them to the
/// [`token_count_topic`] (the readiness point), stages the slice's three
/// forward routes, and returns the cache for composition to hand to the
/// kernel consumers (session actor, prune workers).
///
/// Composition drains the staged routes after activation (see
/// [`bridge::drain_routes`]).
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a
/// wiring bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    state: jinn_domain::common::state::State,
) -> HistoryWorkerChatEntryTokenCache {
    let cache = HistoryWorkerChatEntryTokenCache::new();
    let _cell = host
        .register_cell(token_cache_slot(), cache.clone())
        .expect("token-count slot is registered exactly once at wiring");

    let count_path = count_actor::TokenCountActor::spawn(host.system(), state);
    host.subscribe_service(&count_path, &token_count_topic())
        .expect("token count actor subscribes to the token-count topic");
    let eviction_path = eviction_actor::HistoryWorkerChatEntryTokenCacheEvictionActor::spawn(
        host.system(),
        cache.clone(),
    );
    host.subscribe_service(&eviction_path, &token_count_topic())
        .expect("token cache eviction actor subscribes to the token-count topic");

    host.forward::<jinn_session_history_msg::HistoryAppended, _>(token_count_topic(), || {
        jinn_session_history_msg::HistoryAppended::schema_def()
    });
    host.forward::<
        jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted,
        _,
    >(token_count_topic(), || {
        jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted::schema_def()
    });
    host.forward::<jinn_domain::feat::session::protocol::session_closed::SessionClosed, _>(
        token_count_topic(),
        || jinn_domain::feat::session::protocol::session_closed::SessionClosed::schema_def(),
    );

    cache
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use jinn_core_types::chat_entry_id::ChatEntryId;
    use jinn_core_types::session_id::SessionId;

    /// The cell the activation registers and the cache it returns must be
    /// one instance: a count inserted through the cell handle is visible
    /// through the returned clone (the session actor's accumulation gate
    /// and the prune workers read through their own clones).
    #[rstest::rstest]
    #[tokio::test]
    async fn activate_registers_cell_backed_by_the_returned_cache() {
        // Given an activated slice host.
        let mut services = jinn_domain::Services::new_fake().await;
        let mut host = jinn_slices::SliceHost::new(
            &services.slices,
            &mut services.viewport,
            &services.overlay_views,
            &services.key_routes,
            &services.trouper_system,
        );

        // When activating and inserting through the registered cell.
        let cache = activate(
            &mut host,
            jinn_domain::common::state::State::new(
                jinn_domain::common::app_state::AppState::default(),
            ),
        );
        let cell = services
            .slices
            .reader::<HistoryWorkerChatEntryTokenCache>(&token_cache_slot())
            .expect("cell registered");
        let s = SessionId::new();
        let e = ChatEntryId::new();
        cell.read().insert(s.clone(), e.clone(), 33);

        // Then the returned cache observes the same state.
        assert_eq!(cache.get(&s, &e), Some(33));
    }
}
