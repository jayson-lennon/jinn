//! Positive unit tests for the TCaps invariants (TC5, TC7, TC8).
//!
//! These complement the compile-fail suite (`tests/tcaps_compile_fail.rs`),
//! asserting that the *intended* writes route through the cap-gated projections
//! and mutate the underlying state.

use crate::common::app_state::AppState;
use crate::common::state::State;
use crate::common::tcaps::mint;

// ── TC6 ───────────────────────────────────────────────────────────────────
// TC6 ("mint is the only construction site") is structurally subsumed by TC1
// (`tests/ui/tcap_forge.rs`). If a cap constructor leaked visibility outside
// the `tcaps/` subtree, the forge test would compile instead of failing. A
// separate TC6 test adds no coverage.

#[rstest::rstest]
#[test]
fn read_returns_full_snapshot() {
    // Given a State holding an AppState with default frontend and session.
    let state = State::new(AppState::default());

    // When reading the full snapshot.
    let guard = state.read();

    // Then both frontend and session are reachable from the single guard.
    // Reaching distinct sub-structs from one read confirms the snapshot is whole.
    let _: &crate::feat::ui::frontend_state::FrontendState = &guard.frontend;
    let _: &crate::common::session_map::SessionMap = &guard.session;
}

#[rstest::rstest]
#[test]
fn push_entry_routes_through_history_append() {
    // Given a State with one active session and a minted SessionCap.
    let state = State::new(AppState::default());
    let cap = mint::mint_session_cap();
    let before = state.read().active_session().history().len();

    // When appending an entry through the cap-gated projection.
    state.with_session(&cap, |view| {
        let entry = crate::protocol::ChatEntry::system("hello");
        view.session.map().active_session_mut().push_entry(entry);
    });

    // Then the active session's history grew by one.
    let after = state.read().active_session().history().len();
    assert_eq!(after, before + 1);
}

#[rstest::rstest]
#[test]
fn provider_write_routes_through_provider_ops() {
    // Given a State with no model cache and a minted ProviderCap.
    let state = State::new(AppState::default());
    assert!(state.read().provider.model_cache.is_none());
    let cap = mint::mint_provider_cap();

    // When writing a model cache through the cap-gated ProviderOps.
    state.with_provider(&cap, |view| {
        use crate::common::tcaps::provider::ModelCacheWrite;
        view.provider
            .set_model_cache(Some(crate::feat::provider_infra::ModelCache::new()));
    });

    // Then the provider's model cache is now set.
    assert!(state.read().provider.model_cache.is_some());
}
