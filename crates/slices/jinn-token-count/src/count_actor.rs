//! The token-count actor — computes tiktoken-based counts for chat entries.
//!
//! A trouper [`ServiceActor`] subscribed to the slice's `jinn.token-count`
//! topic (fed by the kernel bridge's forward routes). It folds
//! [`HistoryAppended`] and [`SessionLoadCompleted`] to compute per-entry
//! token counts and fill them into the entries themselves, in memory. A
//! count is a content-derived fact about the (immutable) entry text, so
//! only entries whose count is not yet computed are tokenized; the filled
//! counts persist as a side effect of the regular session-snapshot persist
//! path (`entries.token_count` column). No separate cache exists — the
//! field on the entry is the single source of truth.

use std::collections::HashMap;

use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_domain::common::state::State;
use jinn_domain::common::tcaps::session::SessionCap;
use jinn_domain::feat::context::strategy::token_estimator::{
    TiktokenCounter, TokenCounter, TokenEstimator, estimate_entry_content_tokens,
};
use jinn_session_history_msg::HistoryAppended;
use jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted;

/// The token count actor's static trouper path.
pub const TOKEN_COUNT_PATH: &str = "token-count";

/// The token count actor.
///
/// Computes tiktoken-based token counts for chat entries and fills them into
/// `ChatEntry::token_count` in memory. Runs asynchronously so the render
/// pipeline is never blocked by tiktoken computation.
pub struct TokenCountActor {
    state: State,
    counter: TiktokenCounter,
    session_cap: SessionCap,
}

/// Thin adapter that implements [`TokenEstimator`] by delegating to
/// [`TiktokenCounter::count()`]. This bridges the gap between the
/// `TokenCounter` trait (which `TiktokenCounter` implements) and the
/// `TokenEstimator` trait (which `estimate_entry_content_tokens` requires).
struct TiktokenEstimator(TiktokenCounter);

impl TokenEstimator for TiktokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        self.0.count(text)
    }

    fn name(&self) -> &'static str {
        "tiktoken_adapter"
    }
}

impl ServiceActor for TokenCountActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle and
        // capability via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("TokenCountActor is spawned via start_with"),
        )
    }
}

impl TokenCountActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the token-count topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, state: State) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(TOKEN_COUNT_PATH))
            .start_with({
                move || {
                    let state = state.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            counter: TiktokenCounter::o200k_base(),
                            session_cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
                        })
                    })
                }
            })
            .handles::<HistoryAppended>()
            .handles::<SessionLoadCompleted>()
            .start()
    }

    /// Handles a [`HistoryAppended`] event by computing counts for the active
    /// session's entries that don't have one yet, filling them in memory.
    pub fn handle_history_appended(&self, session_id: &jinn_domain::protocol::SessionId) {
        let counts = {
            let state = self.state.read();
            let Some(session) = state.try_session(session_id) else {
                return;
            };
            self.compute_missing_counts(session.history())
        };

        if counts.is_empty() {
            return;
        }
        self.fill_counts(session_id, &counts);
    }

    /// Handles a [`SessionLoadCompleted`] command by computing counts for the
    /// loaded session's entries that don't have one yet, filling them in
    /// memory. The session was inserted into state before this event was
    /// emitted, so the fill lands on the live session.
    pub fn handle_session_load_completed(
        &self,
        session: &jinn_domain::feat::session::ChatSessionState,
    ) {
        let counts = self.compute_missing_counts(session.history());
        if counts.is_empty() {
            return;
        }
        let session_id = session.session_id().clone();
        self.fill_counts(&session_id, &counts);
    }

    /// Computes counts for history entries whose count is not yet computed.
    ///
    /// Content-derived: entries already carrying a count are skipped — their
    /// text is immutable, so recomputing could only produce the same value.
    fn compute_missing_counts(
        &self,
        history: &[jinn_domain::protocol::ChatEntry],
    ) -> HashMap<jinn_domain::protocol::ChatEntryId, u32> {
        let estimator = TiktokenEstimator(self.counter);
        let mut counts = HashMap::new();
        for entry in history {
            if entry.token_count.is_some() {
                continue;
            }
            let tokens = estimate_entry_content_tokens(&estimator, entry);
            counts.insert(entry.id.clone(), tokens as u32);
        }
        counts
    }

    /// Fills computed counts into the named session's entries.
    fn fill_counts(
        &self,
        session_id: &jinn_domain::protocol::SessionId,
        counts: &HashMap<jinn_domain::protocol::ChatEntryId, u32>,
    ) {
        self.state.with_session(&self.session_cap, |view| {
            if let Some(session) = view.session.map().get_mut(session_id) {
                session.fill_missing_token_counts(counts);
            }
        });
    }
}

impl MsgHandler<HistoryAppended> for TokenCountActor {
    async fn handle(&mut self, msg: HistoryAppended, _ctx: &mut MsgCtx<'_>) {
        self.handle_history_appended(&msg.session_id);
    }
}

impl MsgHandler<SessionLoadCompleted> for TokenCountActor {
    async fn handle(&mut self, msg: SessionLoadCompleted, _ctx: &mut MsgCtx<'_>) {
        self.handle_session_load_completed(&msg.session);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::tcaps::mint::mint_session_cap;
    use jinn_domain::feat::context::strategy::token_estimator::estimate_entry_tokens;
    use jinn_domain::feat::session::ChatSessionState;
    use jinn_domain::protocol::ChatEntry;
    use jinn_domain::protocol::ChangeSource;
    use jinn_domain::protocol::ContextOverride;

    fn actor_for(state: &State) -> TokenCountActor {
        TokenCountActor {
            state: state.clone(),
            counter: TiktokenCounter::o200k_base(),
            session_cap: mint_session_cap(),
        }
    }

    #[rstest::rstest]
    fn history_appended_fills_count_for_entry_without_one() {
        // Given a state with one user entry whose count is not yet computed.
        let mut app_state = AppState::default();
        app_state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello world"));
        let state = State::new(app_state);
        let actor = actor_for(&state);

        // When handling HistoryAppended.
        let session_id = state.read().session.active_session_id().clone();
        actor.handle_history_appended(&session_id);

        // Then the entry's count was filled in memory.
        let guard = state.read();
        let entry = guard.try_session(&session_id).expect("session");
        let count = entry.history()[0].token_count;
        drop(guard);
        assert!(count.is_some(), "entry should have a computed token count");
        // And it matches direct tiktoken counting.
        assert_eq!(
            usize::try_from(count.expect("checked is_some")).expect("token counts fit usize"),
            TiktokenCounter::o200k_base().count("hello world"),
        );
    }

    #[rstest::rstest]
    fn history_appended_skips_entry_with_existing_count() {
        // Given a state with one entry already carrying a count.
        let mut app_state = AppState::default();
        app_state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello world"));
        app_state
            .active_session_mut()
            .edit_history()
            .with_entry_at_mut(0, |e| e.token_count = Some(42));
        let state = State::new(app_state);
        let actor = actor_for(&state);

        // When handling HistoryAppended.
        let session_id = state.read().session.active_session_id().clone();
        actor.handle_history_appended(&session_id);

        // Then the existing count is unchanged (not re-computed).
        let guard = state.read();
        let entry = guard.try_session(&session_id).expect("session");
        assert_eq!(entry.history()[0].token_count, Some(42));
    }

    #[rstest::rstest]
    fn session_load_batch_fills_all_entries_missing_counts() {
        // Given a loaded session with 5 entries and no computed counts.
        let mut loaded_session = ChatSessionState::new();
        for i in 0..5 {
            loaded_session.push_entry(ChatEntry::user(format!("message {i}")));
        }

        // And the session is already inserted into state (the load path
        // inserts before emitting SessionLoadCompleted).
        let mut app_state = AppState::default();
        app_state.session.insert(loaded_session.clone());
        let state = State::new(app_state);
        let actor = actor_for(&state);

        // When handling SessionLoadCompleted.
        actor.handle_session_load_completed(&loaded_session);

        // Then all 5 entries in the live session have counts.
        let session_id = loaded_session.session_id().clone();
        let guard = state.read();
        let live = guard.try_session(&session_id).expect("session");
        for entry in live.history() {
            assert!(
                entry.token_count.is_some(),
                "entry {:?} should have a computed count",
                entry.id
            );
        }
    }

    #[rstest::rstest]
    fn count_matches_tiktoken_for_known_text() {
        // Given a counter and a known text.
        let counter = TiktokenCounter::o200k_base();
        let text = "hello world";
        let expected = counter.count(text);

        // When computing via the content estimator adapter.
        let estimator = TiktokenEstimator(counter);
        let entry = ChatEntry::user(text);
        let actual = estimate_entry_content_tokens(&estimator, &entry);

        // Then the count matches direct tiktoken counting.
        assert_eq!(actual, expected);
    }

    #[rstest::rstest]
    fn content_estimator_counts_excluded_entry_nonzero() {
        // Given a ForcedExclude user entry.
        let mut entry = ChatEntry::user("hello world");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);

        // When estimating with the content estimator.
        let estimator = TiktokenEstimator(TiktokenCounter::o200k_base());
        let content = estimate_entry_content_tokens(&estimator, &entry);

        // Then the count reflects the text despite exclusion.
        assert!(content > 0, "content count must ignore context membership");
        // And the budget-facing estimator still returns 0 for it.
        assert_eq!(estimate_entry_tokens(&estimator, &entry), 0);
    }

    #[rstest::rstest]
    fn large_tool_call_arguments_counted_correctly() {
        // Given a tool call entry with large arguments (simulating write tool).
        let large_content = "fn main() { println!(\"hello\"); }\n".repeat(200);
        let arguments = format!(r#"{{\"path\":\"test.rs\",\"content\":\"{large_content}\"}}"#);
        let counter = TiktokenCounter::o200k_base();
        let direct_count = counter.count(&arguments);

        let mut app_state = AppState::default();
        app_state
            .active_session_mut()
            .push_entry(ChatEntry::tool_call("call_1", "write", &arguments));

        let state = State::new(app_state);
        let actor = actor_for(&state);

        // When handling HistoryAppended.
        let session_id = state.read().session.active_session_id().clone();
        actor.handle_history_appended(&session_id);

        // Then the filled count should be close to the direct count.
        let guard = state.read();
        let entry = guard.try_session(&session_id).expect("session");
        let count =
            usize::try_from(entry.history()[0].token_count.expect("count filled")).unwrap_or(0);
        let tool_name_tokens = counter.count("write");
        let expected_min = direct_count + tool_name_tokens;
        assert!(
            count > expected_min / 2,
            "expected count > {} (half of {}), got {}",
            expected_min / 2,
            expected_min,
            count
        );
        assert!(
            count > 500,
            "expected count > 500 for large tool call, got {count}"
        );
    }
}
