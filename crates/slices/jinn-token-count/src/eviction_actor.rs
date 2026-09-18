//! Session-lifecycle eviction for the shared token cache.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's `jinn.token-count`
//! topic (fed by the kernel bridge's forward routes). It folds
//! [`SessionClosed`] and removes the closed session's inner map from the
//! cache. Single instance, spawned once at slice activation. The prune
//! workers and the session actor receive clones of the cache; this actor
//! owns the eviction events.

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::session_id::SessionId;
use jinn_domain::feat::session::protocol::session_closed::SessionClosed;
use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;

/// The eviction actor's static trouper path.
pub const TOKEN_CACHE_EVICTION_PATH: &str = "token-cache-eviction";

/// Actor that owns session-lifecycle eviction of
/// [`HistoryWorkerChatEntryTokenCache`].
pub struct HistoryWorkerChatEntryTokenCacheEvictionActor {
    cache: HistoryWorkerChatEntryTokenCache,
}

impl ServiceActor for HistoryWorkerChatEntryTokenCacheEvictionActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the cache via
        // `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("HistoryWorkerChatEntryTokenCacheEvictionActor is spawned via start_with"),
        )
    }
}

impl HistoryWorkerChatEntryTokenCacheEvictionActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the token-count topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, cache: HistoryWorkerChatEntryTokenCache) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(TOKEN_CACHE_EVICTION_PATH))
            .start_with({
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            cache: cache.clone(),
                        })
                    })
                }
            })
            .handles::<SessionClosed>()
            .start()
    }

    fn handle_session_closed(&self, session_id: &SessionId) {
        tracing::debug!(
            session_id = %session_id,
            "HistoryWorkerChatEntryTokenCache: evicting session"
        );
        self.cache.remove_session(session_id);
    }
}

impl MsgHandler<SessionClosed> for HistoryWorkerChatEntryTokenCacheEvictionActor {
    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_closed(&msg.session_id);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::*;
    use jinn_core_types::chat_entry_id::ChatEntryId;

    /// Deterministic session id for tests (SessionId is a Uuid newtype).
    fn test_session_id(n: u8) -> SessionId {
        serde_json::from_str(&format!("\"10000000-0000-0000-0000-{n:012x}\""))
            .expect("valid SessionId JSON")
    }

    /// Deterministic entry id for tests (ChatEntryId is a Uuid newtype).
    fn test_entry_id(n: u8) -> ChatEntryId {
        serde_json::from_str(&format!("\"00000000-0000-0000-0000-{n:012x}\""))
            .expect("valid ChatEntryId JSON")
    }

    fn make_actor() -> HistoryWorkerChatEntryTokenCacheEvictionActor {
        HistoryWorkerChatEntryTokenCacheEvictionActor {
            cache: HistoryWorkerChatEntryTokenCache::new(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn handle_session_closed_removes_session_entries() {
        let actor = make_actor();
        let s_a = test_session_id(0);
        let s_b = test_session_id(1);
        let e = test_entry_id(0);

        actor.cache.insert(s_a.clone(), e.clone(), 10);
        actor.cache.insert(s_b.clone(), e.clone(), 20);

        actor.handle_session_closed(&s_a);
        assert_eq!(actor.cache.get(&s_a, &e), None);
        assert_eq!(actor.cache.get(&s_b, &e), Some(20));
    }

    #[rstest::rstest]
    #[test]
    fn handle_session_closed_is_noop_for_unknown_session() {
        let actor = make_actor();
        let s_known = test_session_id(0);
        let s_unknown = test_session_id(1);
        let e = test_entry_id(0);

        actor.cache.insert(s_known.clone(), e.clone(), 30);

        // Must not panic, must not disturb s_known.
        actor.handle_session_closed(&s_unknown);
        assert_eq!(actor.cache.get(&s_known, &e), Some(30));
    }
}
