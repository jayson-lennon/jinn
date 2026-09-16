//! The token-count slice — per-entry token counting and cache eviction.
//!
//! Owns the shared [`HistoryWorkerChatEntryTokenCache`] cell
//! ([`token_cache_slot`]) and hosts the two actors that consume it: the
//! count actor (fills `ChatEntry::token_count` in memory on history
//! events) and the eviction actor (clears a closed session's entries).
//! The session actor's accumulation gate and the prune workers hold
//! clones of the same cache for their reads.
//!
//! Kernel dependency (see Cargo.toml): both actors are async kameo
//! actors writing through tcaps — the sync capability pattern does not
//! fit them.

use jinn_slices::SliceHost;

pub mod count_actor;
pub mod eviction_actor;

pub use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;
pub use jinn_token_count_msg::token_cache_slot;

/// Activates the slice: registers the shared token-cache cell and returns
/// the cache for composition to hand to the actor spawn sites (count,
/// eviction) and the kernel consumers (session actor, prune workers).
///
/// The actors themselves spawn from composition — they need the
/// supervised kameo runtime.
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
) -> HistoryWorkerChatEntryTokenCache {
    let cache = HistoryWorkerChatEntryTokenCache::new();
    let _cell = host
        .register_cell(token_cache_slot(), cache.clone())
        .expect("token-count slot is registered exactly once at wiring");
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
        let cache = activate(&mut host);
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
