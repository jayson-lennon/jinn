//! Session-lifecycle eviction for the shared token cache.
//!
//! Subscribes to [`SessionClosed`] and removes the closed session's inner
//! map from the cache. Single instance, spawned once in composition. The
//! prune workers and the session actor receive clones of the cache; this
//! actor owns the eviction events.

use jinn_core_types::session_id::SessionId;
use jinn_domain::common::actor_deps::ActorDeps;
use jinn_domain::feat::session::protocol::session_closed::SessionClosed;
use jinn_slices::HistoryWorkerChatEntryTokenCache;
use kameo::prelude::{Actor, ActorRef, Context, Message};

/// Actor that owns session-lifecycle eviction of
/// [`HistoryWorkerChatEntryTokenCache`].
pub struct HistoryWorkerChatEntryTokenCacheEvictionActor {
    cache: HistoryWorkerChatEntryTokenCache,
}

/// Dependencies for spawning a [`HistoryWorkerChatEntryTokenCacheEvictionActor`].
#[derive(Clone)]
pub struct HistoryWorkerChatEntryTokenCacheEvictionActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// Clone of the shared cache.
    pub cache: HistoryWorkerChatEntryTokenCache,
}

impl Actor for HistoryWorkerChatEntryTokenCacheEvictionActor {
    type Args = HistoryWorkerChatEntryTokenCacheEvictionActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<SessionClosed>())
            .await;
        Ok(Self { cache: args.cache })
    }
}

impl Message<SessionClosed> for HistoryWorkerChatEntryTokenCacheEvictionActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SessionClosed,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handle_session_closed(&msg.session_id);
    }
}

impl HistoryWorkerChatEntryTokenCacheEvictionActor {
    fn handle_session_closed(&self, session_id: &SessionId) {
        tracing::debug!(
            session_id = %session_id,
            "HistoryWorkerChatEntryTokenCache: evicting session"
        );
        self.cache.remove_session(session_id);
    }
}

#[cfg(test)]
impl HistoryWorkerChatEntryTokenCacheEvictionActor {
    /// Construct directly for unit testing.
    fn new(cache: HistoryWorkerChatEntryTokenCache) -> Self {
        Self { cache }
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
        HistoryWorkerChatEntryTokenCacheEvictionActor::new(HistoryWorkerChatEntryTokenCache::new())
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
