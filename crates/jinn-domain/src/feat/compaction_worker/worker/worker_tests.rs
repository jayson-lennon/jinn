//! Integration tests for [`CompactionWorker`] using `FakeLlmServiceFactory`.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::string_slice,
    reason = "test code"
)]

//!
//! Tests cover the full `evaluate_with_config` pipeline: boundary finding,
//! token accumulation, LLM summarization, and mutation production.
//! Bug regression tests verify the three reported compaction bugs are fixed.

use std::sync::Arc;

use jinn_provider::RetryConfig;

use crate::common::app_state::AppState;
use crate::common::services::test_services::TestServices;
use crate::common::state::State;
use crate::feat::compaction_worker::worker::{CompactionTrigger, CompactionWorker};
use crate::feat::provider_infra::{FakeLlmServiceFactory, LlmServiceFactoryService};
use crate::feat::session::chat_session::ChatSessionState;
use crate::protocol::HistoryMutation;
use crate::protocol::SessionId;
use crate::protocol::{ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride};
use jinn_core_types::model_selection::ModelSelection;
use jinn_preferences_config::schemas::CompactionConfig;

// ── Helpers ─────────────────────────────────────────────────────────────

const FAKE_SUMMARY: &str = "This is a compaction summary of the conversation.";

fn small_reserve_config() -> CompactionConfig {
    CompactionConfig {
        model: None,
        threshold: 0.8,
        reserve_tokens: 100,
        fallback_context_window: 150_000,
    }
}

fn huge_reserve_config() -> CompactionConfig {
    CompactionConfig {
        model: None,
        threshold: 0.8,
        reserve_tokens: 1_000_000,
        fallback_context_window: 150_000,
    }
}

fn moderate_reserve_config() -> CompactionConfig {
    CompactionConfig {
        model: None,
        threshold: 0.8,
        reserve_tokens: 10_000,
        fallback_context_window: 150_000,
    }
}

/// Build a history with N alternating user/assistant turns.
fn alternating_history(turns: usize) -> Vec<ChatEntry> {
    (0..turns)
        .flat_map(|i| {
            vec![
                ChatEntry::user(format!(
                    "User message {i} with enough text to accumulate tokens"
                )),
                ChatEntry::assistant(format!(
                    "Assistant response {i} with enough text to accumulate tokens"
                )),
            ]
        })
        .collect()
}

/// Build a compaction entry for use in history.
fn compaction_entry(summary: &str) -> ChatEntry {
    ChatEntry {
        id: ChatEntryId::new(),
        timing: crate::protocol::EntryTiming::instant_now(),
        kind: ChatEntryKind::Compaction {
            summary: summary.to_owned(),
            tokens_before: 100,
            tokens_after: 50,
            entries_compacted: 5,
            model_used: "test-model".to_owned(),
        },
        pin_position: None,
        context_override: ContextOverride::Default,
        context_history: Vec::new(),
        token_count: None,
    }
}

/// Build a history with a prior compaction summary followed by N turns.
fn history_with_prior_compaction(turns: usize) -> Vec<ChatEntry> {
    let mut entries = vec![compaction_entry("previous summary")];
    entries.extend(alternating_history(turns));
    entries
}

/// Create a `CompactionWorker` backed by a fake LLM.
fn test_worker(summary_text: &str) -> CompactionWorker {
    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(Arc::new(
            FakeLlmServiceFactory::new(vec![summary_text.to_owned()]),
        )))
        .build();
    let handle = services.handle.clone();
    CompactionWorker::new(
        services,
        handle,
        State::new(AppState::default()),
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    )
}

/// Create a `CompactionWorker` backed by a fake LLM, with state containing
/// a session with the given entries.
fn test_worker_with_session(
    summary_text: &str,
    entries: Vec<ChatEntry>,
) -> (CompactionWorker, SessionId) {
    let mut session = ChatSessionState::new();
    let session_id = session.session_id().clone();
    for entry in entries {
        session.push_entry(entry);
    }

    let state = State::new(AppState::default());
    {
        let mut app = state.write_test_no_cap();
        app.session.insert(session);
    }

    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(Arc::new(
            FakeLlmServiceFactory::new(vec![summary_text.to_owned()]),
        )))
        .build();
    let handle = services.handle.clone();

    let worker = CompactionWorker::new(
        services,
        handle,
        state,
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    );

    (worker, session_id)
}

/// Run `evaluate_with_config` in a tokio runtime.
fn run_evaluate(
    worker: &CompactionWorker,
    history: &[ChatEntry],
    config: &CompactionConfig,
) -> Vec<HistoryMutation> {
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    rt.block_on(async {
        worker
            .evaluate_with_config(
                history,
                config,
                "test-model",
                "Summarize this conversation.",
                &RetryConfig::default(),
                false,
                ChatEntryId::new(),
            )
            .await
            .expect("evaluate_with_config should succeed")
    })
}

/// Extract the `SetContextOverride` mutations from a mutation list.
fn forced_exclude_ids(mutations: &[HistoryMutation]) -> Vec<ChatEntryId> {
    mutations
        .iter()
        .filter_map(|m| match m {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => matches!(value, ContextOverride::ForcedExclude).then(|| entry_id.clone()),
            _ => None,
        })
        .collect()
}

/// Extract the `InsertEntry` mutations from a mutation list.
fn insert_entries(mutations: &[HistoryMutation]) -> Vec<&HistoryMutation> {
    mutations
        .iter()
        .filter(|m| matches!(m, HistoryMutation::InsertEntry { .. }))
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 1: Compaction worker integration tests
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn worker_produces_mutations_when_threshold_exceeded() {
    // Given a worker and a long history that exceeds the small reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(20);
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then mutations are produced.
    assert!(
        !mutations.is_empty(),
        "should produce mutations for long history"
    );
}

#[rstest::rstest]
#[test]
fn compaction_passes_prompt_explicitly_not_in_message_array() {
    // Given a worker whose fake LLM records calls, and a long history.
    let factory = FakeLlmServiceFactory::new(vec![FAKE_SUMMARY.to_owned()]);
    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(Arc::new(factory.clone())))
        .build();
    let handle = services.handle.clone();
    let worker = CompactionWorker::new(
        services,
        handle,
        State::new(AppState::default()),
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    );
    let history = alternating_history(20);
    let config = small_reserve_config();
    let compaction_prompt = "COMPACT-MARKER-PROMPT";

    // When evaluating with the compaction prompt.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    rt.block_on(async {
        worker
            .evaluate_with_config(
                &history,
                &config,
                "test-model",
                compaction_prompt,
                &RetryConfig::default(),
                false,
                ChatEntryId::new(),
            )
            .await
            .expect("evaluate_with_config should succeed")
    });

    // Then the compaction prompt traveled as the explicit system prompt.
    let prompts = factory.received_system_prompts();
    assert_eq!(prompts.len(), 1, "one LLM call expected");
    assert_eq!(
        prompts[0].as_deref(),
        Some(compaction_prompt),
        "compaction prompt must be the system prompt parameter"
    );
    // And the message array holds only the serialized-history User message.
    let calls = factory.received_calls();
    assert_eq!(calls.len(), 1);
    let messages = &calls[0];
    assert_eq!(messages.len(), 1, "exactly one message expected");
    assert!(
        matches!(messages[0], jinn_provider::LlmMessage::User { .. }),
        "message array holds a User message, got {:?}",
        messages[0]
    );
}

#[rstest::rstest]
#[test]
fn worker_returns_empty_when_within_reserve() {
    // Given a worker and a short history that fits in the huge reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(2);
    let config = huge_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then no mutations are produced.
    assert!(
        mutations.is_empty(),
        "should produce no mutations when everything fits in reserve"
    );
}

#[rstest::rstest]
#[test]
fn mutations_exclude_gathered_entries() {
    // Given a worker and a long history.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(20);
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then there are SetContextOverride(ForcedExclude) mutations.
    let excluded_ids = forced_exclude_ids(&mutations);
    assert!(!excluded_ids.is_empty(), "should have excluded entries");

    // And all excluded IDs correspond to history entries.
    let history_ids: Vec<ChatEntryId> = history.iter().map(|e| e.id.clone()).collect();
    for id in &excluded_ids {
        assert!(
            history_ids.contains(id),
            "excluded entry ID {id:?} should be in history"
        );
    }
}

#[rstest::rstest]
#[test]
fn mutations_insert_compaction_summary_at_boundary() {
    // Given a worker and a long history.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(20);
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then there is exactly one InsertEntry mutation.
    let inserts = insert_entries(&mutations);
    assert_eq!(
        inserts.len(),
        1,
        "should have exactly one InsertEntry mutation"
    );

    // And it has an InsertEntry with a Compaction kind.
    match inserts[0] {
        HistoryMutation::InsertEntry {
            after_entry_id,
            entry,
        } => {
            // The after_entry_id should be the last gathered entry.
            assert!(
                after_entry_id.is_some(),
                "InsertEntry should reference a prior entry"
            );

            // The entry should be a Compaction kind with the fake summary.
            match &entry.kind {
                ChatEntryKind::Compaction { summary, .. } => {
                    assert_eq!(
                        summary, FAKE_SUMMARY,
                        "summary should contain the fake LLM response"
                    );
                }
                other => panic!("expected Compaction kind, got {other:?}"),
            }
        }
        other => panic!("expected InsertEntry, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn mutations_exclude_entries_around_prior_compaction() {
    // Given a history with a prior compaction followed by entries.
    let worker = test_worker(FAKE_SUMMARY);
    let history = history_with_prior_compaction(5);
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then there are excluded entries (the user/assistant entries after the compaction).
    let excluded_ids = forced_exclude_ids(&mutations);
    assert!(
        !excluded_ids.is_empty(),
        "should have excluded entries after prior compaction"
    );

    // And the compaction entry itself is NOT excluded (compaction entries are skipped by gather).
    let compaction_entry_id = &history[0].id;
    assert!(
        !excluded_ids.contains(compaction_entry_id),
        "prior compaction entry should not be excluded"
    );
}

#[rstest::rstest]
#[test]
fn worker_preserves_recent_entries_in_reserve() {
    // Given a history with enough entries to trigger compaction, but with
    // a moderate reserve that keeps the most recent entries.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(20);
    let config = moderate_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then the last few entries should NOT be excluded.
    let excluded_ids = forced_exclude_ids(&mutations);

    // The very last entry should not be excluded (it's in the reserve).
    let last_entry_id = history
        .last()
        .expect("history should have entries")
        .id
        .clone();
    assert!(
        !excluded_ids.contains(&last_entry_id),
        "the most recent entry should be preserved in the reserve"
    );
}

/// Build a long assistant entry whose token estimate (~1 token / 4 chars)
/// comfortably exceeds a small reserve, forcing the cut to land just after it.
fn big_assistant(marker: &str) -> ChatEntry {
    ChatEntry::assistant(format!("{marker} {padding}", padding = "w".repeat(600)))
}

#[rstest::rstest]
#[test]
fn kept_region_after_compaction_opens_with_a_valid_turn() {
    // Given a history whose reserve boundary lands on an Assistant opener:
    //   [User(small), Assistant(BIG), Assistant(small-opener), User(small)]
    // compute_cut_index lands the cut on index 2 (the Assistant) because the
    // big Assistant at index 1 alone exceeds the reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = vec![
        ChatEntry::user("start"),
        big_assistant("big"),
        ChatEntry::assistant("recent opener"),
        ChatEntry::user("recent turn"),
    ];
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then the boundary Assistant opener (index 2) is force-excluded,
    // NOT kept. Without Pass 3 it would be the first in-context entry.
    let excluded_ids = forced_exclude_ids(&mutations);
    let assistant_opener_id = &history[2].id;
    assert!(
        excluded_ids.contains(assistant_opener_id),
        "Assistant opener should be absorbed into the compaction, not kept"
    );

    // And the kept region opens with the User entry at index 3, which is
    // never excluded.
    let user_opener_id = &history[3].id;
    assert!(
        !excluded_ids.contains(user_opener_id),
        "the User opener of the kept region must be preserved"
    );

    // And exactly one compaction summary is inserted.
    assert_eq!(
        insert_entries(&mutations).len(),
        1,
        "should insert exactly one compaction summary"
    );
}

#[rstest::rstest]
#[test]
fn evaluate_for_session_returns_empty_for_empty_history() {
    // Given a worker with a session that has no history.
    let (worker, session_id) = test_worker_with_session(FAKE_SUMMARY, vec![]);

    // When evaluating.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let result = rt.block_on(async {
        worker
            .evaluate_for_session(&CompactionTrigger {
                session_id,
                compact_all: false,
            })
            .await
    });

    // Then no mutations are produced.
    let mutations = result.expect("empty history should not error");
    assert!(
        mutations.is_empty(),
        "empty history should produce no mutations"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 2: Bug regression tests
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn no_compaction_below_threshold() {
    // Given a short history and a huge reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(2);
    let config = huge_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then no compaction triggers.
    assert!(
        mutations.is_empty(),
        "should not compact when tokens are below threshold"
    );
}

#[rstest::rstest]
#[test]
fn compaction_triggers_above_threshold() {
    // Given a long history and a small reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = alternating_history(20);
    let config = small_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then compaction triggers.
    assert!(
        !mutations.is_empty(),
        "should compact when tokens exceed threshold"
    );
}

#[rstest::rstest]
#[test]
fn no_double_compaction_after_first() {
    // Bug 2 regression: after compaction mutations are applied and prompt
    // re-assembled with small context_size, no second compaction should trigger.

    // Given a history with a prior compaction followed by a few entries that
    // fit within the huge reserve.
    let worker = test_worker(FAKE_SUMMARY);
    let history = history_with_prior_compaction(3);
    let config = huge_reserve_config();

    // When evaluating.
    let mutations = run_evaluate(&worker, &history, &config);

    // Then no mutations are produced - the 3 turns after the compaction
    // boundary fit within the reserve.
    assert!(
        mutations.is_empty(),
        "should not double-compact when entries after boundary fit in reserve"
    );
}

#[rstest::rstest]
#[test]
fn session_continues_after_background_compaction() {
    // Bug 3 regression: compaction mutations applied during an active session
    // should not change the session phase.

    // Given a session in Sending phase with enough history to trigger compaction.
    let mut session = ChatSessionState::new();
    let history = alternating_history(20);
    for entry in &history {
        session.push_entry(entry.clone());
    }
    session.begin_sending();
    let session_id = session.session_id().clone();

    let state = State::new(AppState::default());
    {
        let mut app = state.write_test_no_cap();
        app.session.insert(session);
        // Use a tiny reserve so compaction triggers with just 20 turns.
        app.frontend.preferences.compaction = CompactionConfig {
            model: None,
            threshold: 0.8,
            reserve_tokens: 100,
            fallback_context_window: 150_000,
        };
    }

    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(Arc::new(
            FakeLlmServiceFactory::new(vec![FAKE_SUMMARY.to_owned()]),
        )))
        .build();
    // Sync test preferences to the in-memory storage.
    let prefs = state.read().frontend.preferences.clone();
    services
        .user_preferences_storage
        .save(&prefs)
        .expect("save test prefs");
    let handle = services.handle.clone();

    let worker = CompactionWorker::new(
        services,
        handle,
        state,
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    );

    // When evaluating compaction for the session.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let result = rt.block_on(async {
        worker
            .evaluate_for_session(&CompactionTrigger {
                session_id: session_id.clone(),
                compact_all: false,
            })
            .await
    });

    // Then compaction produced mutations.
    let mutations = result.expect("should not error");
    assert!(
        !mutations.is_empty(),
        "should have mutations for long history"
    );

    // And the session phase is still Sending (compaction doesn't change phase).
    let guard = worker.state.read();
    let session = guard.session(&session_id);
    assert_eq!(
        session.phase(),
        crate::feat::session::phase_machine::PhaseKind::Sending,
        "session should remain in Sending phase after background compaction"
    );
}

#[rstest::rstest]
#[test]
fn threshold_uses_fresh_history_not_stale_context_size() {
    // Bug 1 regression: the worker uses the passed-in history, not a stale
    // cached_context_size from a previous prompt assembly.

    // Given the same worker and config.
    let worker = test_worker(FAKE_SUMMARY);
    let config = huge_reserve_config();

    // When evaluating with a short history.
    let short_history = alternating_history(2);
    let mutations_short = run_evaluate(&worker, &short_history, &config);

    // And evaluating with a long history using the same config.
    let long_history = alternating_history(20);
    let config_small = small_reserve_config();
    let mutations_long = run_evaluate(&worker, &long_history, &config_small);

    // Then the results differ - proving the worker uses the passed-in history,
    // not some stale cached value.
    assert!(
        mutations_short.is_empty(),
        "short history should not compact"
    );
    assert!(!mutations_long.is_empty(), "long history should compact");
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 3: Threshold gate integration tests
// ═══════════════════════════════════════════════════════════════════════════
//
// These tests exercise the auto-compaction path (evaluate_history) which
// reads context_size() and context_length from shared state to decide
// whether to compact. They go through the HistoryWorker trait's evaluate()
// method which delegates to evaluate_history.

use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::feat::provider_infra::ModelCache;
use jinn_provider::{InputModalities, ModelInfo};

/// Builder for constructing a test environment with full control over
/// context_size, model cache, compaction config, and history.
struct ThresholdTestEnv {
    state: State,
    session_id: SessionId,
}

impl ThresholdTestEnv {
    /// Create a new env with a session that has 20 turns of history
    /// (enough to produce mutations if the threshold gate passes and reserve is small).
    fn new() -> Self {
        let mut session = ChatSessionState::new();
        session.set_model(ModelSelection::Single("provider/model-200k".to_owned()));
        let history = alternating_history(20);
        for entry in &history {
            session.push_entry(entry.clone());
        }
        let session_id = session.session_id().clone();
        let state = State::new(AppState::default());
        {
            let mut app = state.write_test_no_cap();
            app.session.insert(session);
        }
        Self { state, session_id }
    }

    /// Set the session's cached context_size (tiktoken count from last assembly).
    fn set_context_size(&self, size: Option<u32>) {
        let mut app = self.state.write_test_no_cap();
        let session = app
            .session
            .get_mut(&self.session_id)
            .expect("session exists");
        if let Some(s) = size {
            session.set_context_size(s);
        }
        // If None, leave it unset (default is None)
    }

    /// Set the model cache with context_length entries.
    fn set_model_cache(&self, cache: ModelCache) {
        let mut app = self.state.write_test_no_cap();
        app.provider.model_cache = Some(cache);
    }

    /// Set the compaction config.
    fn set_compaction_config(&self, config: CompactionConfig) {
        let mut app = self.state.write_test_no_cap();
        app.frontend.preferences.compaction = config;
    }

    /// Build a CompactionWorker backed by a fake LLM that returns the given summary.
    fn build_worker(&self, summary_text: &str) -> CompactionWorker {
        let services = TestServices::builder()
            .llm_service(LlmServiceFactoryService::new(Arc::new(
                FakeLlmServiceFactory::new(vec![summary_text.to_owned()]),
            )))
            .build();
        // Sync test preferences to the in-memory storage so
        // the worker can load them via services.user_preferences_storage.
        let prefs = self.state.read().frontend.preferences.clone();
        services
            .user_preferences_storage
            .save(&prefs)
            .expect("save test prefs");
        let handle = services.handle.clone();
        CompactionWorker::new(
            services,
            handle,
            self.state.clone(),
            crate::common::tcaps::mint::mint_session_cap(),
            String::new(),
        )
    }

    /// Run evaluate (auto-compaction path) and return mutations.
    fn run_evaluate(&self, worker: &CompactionWorker) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        rt.block_on(async { worker.evaluate(&self.session_id, Arc::from([])).await })
    }
}

/// Helper: build a ModelCache with a single provider and model.
fn model_cache_with(provider: &str, model_id: &str, context_length: u32) -> ModelCache {
    let mut cache = ModelCache::new();
    cache.entries.insert(
        provider.to_owned(),
        vec![ModelInfo {
            id: model_id.to_owned(),
            context_length: Some(context_length),
            input_modalities: InputModalities::text(),
        }],
    );
    cache
}

/// Helper: build a ModelCache with a single provider and model with no context_length.
fn model_cache_no_context_length(provider: &str, model_id: &str) -> ModelCache {
    let mut cache = ModelCache::new();
    cache.entries.insert(
        provider.to_owned(),
        vec![ModelInfo {
            id: model_id.to_owned(),
            context_length: None,
            input_modalities: InputModalities::text(),
        }],
    );
    cache
}

/// Config with small reserve (so evaluate_with_config produces mutations
/// once the threshold gate passes) and a specific threshold.
fn threshold_config(threshold: f64, fallback: usize) -> CompactionConfig {
    CompactionConfig {
        model: None,
        threshold,
        reserve_tokens: 100, // small so 20 turns of history exceeds it
        fallback_context_window: fallback,
    }
}

// ── Test 1: context_size is None ──

#[rstest::rstest]
#[test]
fn gate_skips_when_context_size_is_none() {
    let env = ThresholdTestEnv::new();
    // context_size defaults to None - don't set it.
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact when context_size is None"
    );
}

// ── Test 2: context_size is 0 ──

#[rstest::rstest]
#[test]
fn gate_skips_when_context_size_is_zero() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(0));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact when context_size is 0"
    );
}

// ── Test 3: below threshold ──

#[rstest::rstest]
#[test]
fn gate_skips_when_below_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(100_000)); // 100k/200k = 50% < 70%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact at 50% with 70% threshold"
    );
}

// ── Test 4: above threshold ──

#[rstest::rstest]
#[test]
fn gate_triggers_when_above_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(150_000)); // 150k/200k = 75% > 70%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact at 75% with 70% threshold"
    );
}

// ── Test 5: exactly at threshold ──

#[rstest::rstest]
#[test]
fn gate_triggers_when_exactly_at_threshold() {
    let env = ThresholdTestEnv::new();
    // 140_000 / 200_000 = 0.7 exactly
    env.set_context_size(Some(140_000));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact at exactly 70% (>= threshold)"
    );
}

// ── Test 6: just below threshold ──

#[rstest::rstest]
#[test]
fn gate_skips_just_below_threshold() {
    let env = ThresholdTestEnv::new();
    // 139_999 / 200_000 = 0.69999... < 0.7
    env.set_context_size(Some(139_999));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact at 69.999% with 70% threshold"
    );
}

// ── Test 7: uses fallback when model cache is None ──

#[rstest::rstest]
#[test]
fn gate_uses_fallback_when_no_model_cache() {
    let env = ThresholdTestEnv::new();
    // No model cache at all - should use fallback.
    env.set_context_size(Some(120_000)); // 120k/150k = 80% > 70%
    // Don't set model cache.
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact using fallback context window"
    );
}

// ── Test 8: uses fallback when model not in cache ──

#[rstest::rstest]
#[test]
fn gate_uses_fallback_when_model_not_in_cache() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(100_000)); // 100k/200k = 50% < 70%
    // Cache has a different provider - "provider/model-200k" won't match.
    env.set_model_cache(model_cache_with("other-provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 200_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should skip - fallback 200k, context at 50%"
    );
}

// ── Test 9: uses fallback when model context_length is None ──

#[rstest::rstest]
#[test]
fn gate_uses_fallback_when_model_context_length_is_none() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(120_000)); // 120k/150k = 80% > 70%
    env.set_model_cache(model_cache_no_context_length("provider", "model-200k"));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact - model has no context_length, fallback used"
    );
}

// ── Test 10: session not found ──

#[rstest::rstest]
#[test]
fn gate_skips_when_session_not_found() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(150_000));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    // Use a session ID that doesn't exist.
    let fake_id = SessionId::new();
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations = rt.block_on(async { worker.evaluate(&fake_id, Arc::from([])).await });

    assert!(
        mutations.is_empty(),
        "should not compact for nonexistent session"
    );
}

// ── Test 11: high threshold triggers ──

#[rstest::rstest]
#[test]
fn gate_triggers_at_high_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(180_000)); // 180k/200k = 90% >= 90%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.9, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact at 90% with 90% threshold"
    );
}

// ── Test 12: high threshold skips ──

#[rstest::rstest]
#[test]
fn gate_skips_at_high_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(170_000)); // 170k/200k = 85% < 90%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.9, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact at 85% with 90% threshold"
    );
}

// ── Test 13: low threshold triggers ──

#[rstest::rstest]
#[test]
fn gate_triggers_at_low_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(50_000)); // 50k/200k = 25% > 20%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.2, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact at 25% with 20% threshold"
    );
}

// ── Test 14: low threshold skips ──

#[rstest::rstest]
#[test]
fn gate_skips_at_low_threshold() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(30_000)); // 30k/200k = 15% < 20%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.2, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact at 15% with 20% threshold"
    );
}

// ── Test 15: context_size equals context_limit ──

#[rstest::rstest]
#[test]
fn gate_triggers_when_context_size_equals_limit() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(200_000)); // 200k/200k = 100%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact when context is 100% full"
    );
}

// ── Test 16: context_size exceeds context_limit ──

#[rstest::rstest]
#[test]
fn gate_triggers_when_context_size_exceeds_limit() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(250_000)); // 250k/200k = 125% - over budget
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact when context exceeds limit"
    );
}

// ── Test 17: manual compact_all bypasses gate ──

#[rstest::rstest]
#[test]
fn manual_compact_all_bypasses_threshold_gate() {
    let env = ThresholdTestEnv::new();
    // context_size is 0 - threshold gate would block, but compact_all ignores it.
    env.set_context_size(Some(0));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let result = rt.block_on(async {
        worker
            .evaluate_for_session(&CompactionTrigger {
                session_id: env.session_id.clone(),
                compact_all: true,
            })
            .await
    });

    let mutations = result.expect("should succeed");
    assert!(
        !mutations.is_empty(),
        "compact_all should bypass threshold gate"
    );
}

// ── Test 18: manual compact (non-all) uses evaluate_for_session path ──
// evaluate_for_session reads its own history/config from state and goes
// directly to evaluate_with_config - it does NOT go through evaluate_history.
// So it should produce mutations regardless of context_size.

#[rstest::rstest]
#[test]
fn manual_compact_bypasses_threshold_gate() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(0)); // would block auto-compaction
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    // Use small reserve so evaluate_with_config produces mutations.
    env.set_compaction_config(CompactionConfig {
        model: None,
        threshold: 0.7,
        reserve_tokens: 100,
        fallback_context_window: 150_000,
    });

    let worker = env.build_worker(FAKE_SUMMARY);
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let result = rt.block_on(async {
        worker
            .evaluate_for_session(&CompactionTrigger {
                session_id: env.session_id.clone(),
                compact_all: false,
            })
            .await
    });

    let mutations = result.expect("should succeed");
    assert!(
        !mutations.is_empty(),
        "manual /compact should bypass threshold gate"
    );
}

// ── Test 19: provider/model format splits correctly ──

#[rstest::rstest]
#[test]
fn gate_splits_provider_model_format() {
    let env = ThresholdTestEnv::new();
    // Session model is "ollama/llama3" - provider="ollama", model="llama3"
    {
        let mut app = env.state.write_test_no_cap();
        let session = app.session.get_mut(&env.session_id).expect("session");
        session.set_model(ModelSelection::Single("ollama/llama3".to_owned()));
    }
    env.set_context_size(Some(150_000)); // 150k/200k = 75% > 70%
    env.set_model_cache(model_cache_with("ollama", "llama3", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact with ollama/llama3 model lookup"
    );
}

// ── Test 20: nested provider path ──

#[rstest::rstest]
#[test]
fn gate_handles_nested_provider_path() {
    let env = ThresholdTestEnv::new();
    // Session model is "openrouter/anthropic/claude-sonnet"
    // provider = "openrouter", model = "anthropic/claude-sonnet"
    {
        let mut app = env.state.write_test_no_cap();
        let session = app.session.get_mut(&env.session_id).expect("session");
        session.set_model(ModelSelection::Single(
            "openrouter/anthropic/claude-sonnet".to_owned(),
        ));
    }
    env.set_context_size(Some(150_000)); // 150k/200k = 75% > 70%
    env.set_model_cache(model_cache_with(
        "openrouter",
        "anthropic/claude-sonnet",
        200_000,
    ));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact with nested provider/model path"
    );
}

// ── Test 21: empty history above threshold ──
// Threshold gate passes but evaluate_with_config finds nothing to compact.

#[rstest::rstest]
#[test]
fn gate_passes_but_nothing_to_compact_with_empty_history() {
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("provider/model-200k".to_owned()));
    // No entries - empty history.
    let session_id = session.session_id().clone();
    session.set_context_size(150_000);

    let state = State::new(AppState::default());
    {
        let mut app = state.write_test_no_cap();
        app.session.insert(session);
        app.provider.model_cache = Some(model_cache_with("provider", "model-200k", 200_000));
        app.frontend.preferences.compaction = threshold_config(0.7, 150_000);
    }

    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(Arc::new(
            FakeLlmServiceFactory::new(vec![FAKE_SUMMARY.to_owned()]),
        )))
        .build();
    // Sync test preferences to the in-memory storage.
    let prefs = state.read().frontend.preferences.clone();
    services
        .user_preferences_storage
        .save(&prefs)
        .expect("save test prefs");
    let handle = services.handle.clone();
    let worker = CompactionWorker::new(
        services,
        handle,
        state,
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    );

    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations = rt.block_on(async { worker.evaluate(&session_id, Arc::from([])).await });

    assert!(
        mutations.is_empty(),
        "threshold passes but empty history = no mutations"
    );
}

// ── Test 22: ratio matches status bar exactly ──

#[rstest::rstest]
#[test]
fn gate_ratio_matches_status_bar_math() {
    let env = ThresholdTestEnv::new();
    // 105_000 / 150_000 = 0.7 exactly - same as status bar "70.0%" display
    env.set_context_size(Some(105_000));
    env.set_model_cache(model_cache_with("provider", "model-150k", 150_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should compact at exactly 70.0% like status bar"
    );
}

// ── Test 23: threshold 1.0 requires full context ──

#[rstest::rstest]
#[test]
fn gate_threshold_one_requires_full_context() {
    let env = ThresholdTestEnv::new();
    // 199_999 / 200_000 = 0.99999... < 1.0
    env.set_context_size(Some(199_999));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(1.0, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        mutations.is_empty(),
        "should not compact at 99.999% with threshold 1.0"
    );
}

// ── Test 24: threshold 0.0 always triggers ──

#[rstest::rstest]
#[test]
fn gate_threshold_zero_always_triggers() {
    let env = ThresholdTestEnv::new();
    // 1 / 200_000 = 0.0005% - but threshold is 0.0 so anything >= 0 triggers
    env.set_context_size(Some(1));
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.0, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "threshold 0.0 should trigger for any non-zero context"
    );
}

// ── Test 25: uses session model not compaction model for lookup ──
// The threshold gate uses session.profile().model, not config.model.

#[rstest::rstest]
#[test]
fn gate_uses_session_model_for_context_lookup() {
    let env = ThresholdTestEnv::new();
    // Session model is "provider/model-200k" (matches cache entry)
    // Compaction config model is "other/model-tiny" (doesn't match and shouldn't be used)
    env.set_context_size(Some(150_000)); // 150k/200k = 75% > 70%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(CompactionConfig {
        model: Some("other/model-tiny".to_owned()), // compaction model - not used for threshold
        threshold: 0.7,
        reserve_tokens: 100,
        fallback_context_window: 150_000,
    });

    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations = env.run_evaluate(&worker);

    assert!(
        !mutations.is_empty(),
        "should use session model for threshold, not compaction model"
    );
}

// ── Test 26: concurrent HistoryAppended - in_flight guard ─
// This test verifies the HistoryWorkerActor's in_flight guard prevents
// duplicate compaction. This is tested in history_worker/tests.rs; here
// we verify that a second evaluate call with the same session still works
// (the in_flight guard is at the actor level, not the worker level).

#[rstest::rstest]
#[test]
fn gate_prevents_double_compaction_after_first() {
    let env = ThresholdTestEnv::new();
    env.set_context_size(Some(150_000)); // 75% > 70%
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 150_000));

    let worker = env.build_worker(FAKE_SUMMARY);

    // First call should produce mutations.
    let mutations_1 = env.run_evaluate(&worker);
    assert!(!mutations_1.is_empty(), "first call should compact");

    // Simulate what happens after compaction: context_size drops below threshold.
    env.set_context_size(Some(50_000)); // 50k/200k = 25% < 70%

    // Second call should not compact.
    let mutations_2 = env.run_evaluate(&worker);
    assert!(
        mutations_2.is_empty(),
        "second call should not compact after context_size drops"
    );
}

// ── Test 27: threshold re-checked on next HistoryAppended after skip ──

#[rstest::rstest]
#[test]
fn gate_re_evaluated_on_subsequent_event() {
    let env = ThresholdTestEnv::new();
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 200_000));

    // First event: below threshold.
    env.set_context_size(Some(139_999)); // 69.999% < 70%
    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations_1 = env.run_evaluate(&worker);
    assert!(
        mutations_1.is_empty(),
        "first event below threshold - no compact"
    );

    // Second event: crosses threshold (new entry pushed, prompt reassembled).
    env.set_context_size(Some(140_000)); // 70% >= 70%
    let mutations_2 = env.run_evaluate(&worker);
    assert!(
        !mutations_2.is_empty(),
        "second event at threshold - should compact"
    );
}

// ── Test 28: context_size updates after prompt reassembly ──

#[rstest::rstest]
#[test]
fn gate_skips_after_compaction_reduces_context_size() {
    let env = ThresholdTestEnv::new();
    env.set_model_cache(model_cache_with("provider", "model-200k", 200_000));
    env.set_compaction_config(threshold_config(0.7, 200_000));

    // Before compaction: above threshold.
    env.set_context_size(Some(150_000));
    let worker = env.build_worker(FAKE_SUMMARY);
    let mutations_1 = env.run_evaluate(&worker);
    assert!(
        !mutations_1.is_empty(),
        "before compaction - should compact"
    );

    // After compaction + reassembly: context_size drops to 50k (25% < 70%).
    env.set_context_size(Some(50_000));
    let mutations_2 = env.run_evaluate(&worker);
    assert!(
        mutations_2.is_empty(),
        "after reassembly below threshold - should not compact"
    );
}

// ── Compaction deduplication tests ──────────────────────────────────

fn set_compaction_in_flight(
    worker: &CompactionWorker,
    session_id: &SessionId,
    entry_id: ChatEntryId,
) {
    worker
        .compaction_in_progress
        .lock()
        .insert(session_id.clone());
    worker
        .pending_compaction_id
        .lock()
        .insert(session_id.clone(), entry_id);
}

fn worker_in_flight(worker: &CompactionWorker, session_id: &SessionId) -> bool {
    worker.compaction_in_progress.lock().contains(session_id)
}

fn worker_pending_id(worker: &CompactionWorker, session_id: &SessionId) -> Option<ChatEntryId> {
    worker.pending_compaction_id.lock().get(session_id).cloned()
}

#[rstest::rstest]
#[test]
fn snapshot_skipped_when_compaction_in_flight() {
    // Given a worker with compaction in flight.
    let worker = test_worker("summary");
    let session_id = SessionId::new();
    let pending_id = ChatEntryId::new();
    set_compaction_in_flight(&worker, &session_id, pending_id.clone());

    // And a snapshot that does NOT contain the pending compaction entry.
    let snapshot: Arc<[ChatEntry]> = Arc::from(alternating_history(5));

    // When evaluating.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations = rt.block_on(async { worker.evaluate(&session_id, snapshot).await });

    // Then no mutations are produced.
    assert!(
        mutations.is_empty(),
        "should skip when compaction is in flight"
    );

    // And the flag is still set.
    assert!(
        worker_in_flight(&worker, &session_id),
        "flag should still be set"
    );
    assert_eq!(worker_pending_id(&worker, &session_id), Some(pending_id));
}

#[rstest::rstest]
#[test]
fn snapshot_clears_flag_when_compaction_entry_found() {
    // Given a worker with compaction in flight.
    let worker = test_worker("summary");
    let session_id = SessionId::new();
    let pending_id = ChatEntryId::new();
    set_compaction_in_flight(&worker, &session_id, pending_id.clone());

    // And a snapshot that CONTAINS the pending compaction entry.
    let mut snapshot = vec![compaction_entry("old summary")];
    snapshot[0].id = pending_id;
    let snapshot: Arc<[ChatEntry]> = Arc::from(snapshot);

    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations = rt.block_on(async { worker.evaluate(&session_id, snapshot).await });

    // Then the flag is cleared.
    assert!(
        !worker_in_flight(&worker, &session_id),
        "flag should be cleared"
    );
    assert!(
        worker_pending_id(&worker, &session_id).is_none(),
        "pending ID should be cleared"
    );

    // And mutations are empty (no context_size set, threshold not crossed).
    assert!(
        mutations.is_empty(),
        "no compaction needed after clearing flag"
    );
}

#[rstest::rstest]
#[test]
fn error_clears_flag_and_allows_retry() {
    // Given a worker with a failing LLM factory.
    let failing_factory: Arc<dyn jinn_provider::LlmServiceFactory> = Arc::new(FailingLlmFactory);
    let services = TestServices::builder()
        .llm_service(LlmServiceFactoryService::new(failing_factory))
        .build();
    let handle = services.handle.clone();
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single("provider/model-200k".to_owned()));
    let history = alternating_history(20);
    for entry in &history {
        session.push_entry(entry.clone());
    }
    let session_id = session.session_id().clone();
    let state = State::new(AppState::default());
    {
        let mut app = state.write_test_no_cap();
        app.session.insert(session);
    }

    // Set up threshold so evaluation proceeds past the gate.
    {
        let mut app = state.write_test_no_cap();
        app.frontend.preferences.compaction = CompactionConfig {
            threshold: 0.5,
            ..CompactionConfig::default()
        };
        let prefs = app.frontend.preferences.clone();
        services
            .user_preferences_storage
            .save(&prefs)
            .expect("save");
    }
    {
        let mut app = state.write_test_no_cap();
        if let Some(session) = app.session.get_mut(&session_id) {
            session.set_context_size(150_000);
        }
        app.provider.model_cache = Some(model_cache_with("provider", "model-200k", 200_000));
    }

    let worker = CompactionWorker::new(
        services,
        handle,
        state,
        crate::common::tcaps::mint::mint_session_cap(),
        String::new(),
    );

    // When evaluating (LLM will fail).
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations = rt.block_on(async { worker.evaluate(&session_id, Arc::from([])).await });

    // Then mutations are empty (error swallowed).
    assert!(mutations.is_empty(), "error path returns empty mutations");

    // And the flag is cleared.
    assert!(
        !worker_in_flight(&worker, &session_id),
        "flag should be cleared after error"
    );
    assert!(
        worker_pending_id(&worker, &session_id).is_none(),
        "pending ID should be cleared after error"
    );
}

/// A factory whose LLM service always returns an error on `chat_stream`.
#[derive(Debug)]
struct FailingLlmFactory;

#[async_trait::async_trait]
impl jinn_provider::LlmServiceFactory for FailingLlmFactory {
    fn create(
        &self,
    ) -> Result<
        Box<dyn jinn_provider::LlmService>,
        error_stack::Report<jinn_provider::LlmServiceError>,
    > {
        Ok(Box::new(FailingLlmService))
    }

    fn name(&self) -> &'static str {
        "failing-test"
    }
}

#[derive(Debug)]
struct FailingLlmService;

#[async_trait::async_trait]
impl jinn_provider::LlmService for FailingLlmService {
    fn name(&self) -> &'static str {
        "failing-test"
    }

    async fn chat_stream(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<jinn_provider::LlmMessage>,
    ) -> Result<jinn_provider::ChatStream, error_stack::Report<jinn_provider::LlmServiceError>>
    {
        Err(
            error_stack::Report::new(jinn_provider::LlmServiceError::ApiKey)
                .attach("intentional test failure"),
        )
    }

    async fn chat_stream_with_tools(
        &self,
        _system_prompt: Option<&str>,
        _messages: Vec<jinn_provider::LlmMessage>,
        _tools: Vec<jinn_core_types::ToolDefinition>,
    ) -> Result<jinn_provider::ToolStream, error_stack::Report<jinn_provider::LlmServiceError>>
    {
        Err(
            error_stack::Report::new(jinn_provider::LlmServiceError::ApiKey)
                .attach("intentional test failure"),
        )
    }
}

#[rstest::rstest]
#[test]
fn manual_compaction_does_not_set_flag() {
    // Given a worker.
    let (worker, session_id) = test_worker_with_session("summary", alternating_history(20));
    let trigger = CompactionTrigger {
        session_id: session_id.clone(),
        compact_all: true,
    };

    // When running manual compaction.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let result = rt.block_on(async { worker.evaluate_for_session(&trigger).await });

    // Then it produces mutations.
    assert!(result.is_ok(), "manual compaction should succeed");
    assert!(
        !result.expect("ok").is_empty(),
        "manual compaction should produce mutations"
    );

    // And the flag is NOT set.
    assert!(
        !worker_in_flight(&worker, &session_id),
        "manual compaction should not set the flag"
    );
    assert!(
        worker_pending_id(&worker, &session_id).is_none(),
        "manual compaction should not set pending ID"
    );
}

// ── Multi-session isolation tests ──────────────────────────────────

#[rstest::rstest]
#[test]
fn one_session_in_flight_does_not_suppress_another() {
    // Given a worker where session A has compaction in flight (pending
    // summary not yet landed) and session B is independent.
    let worker = test_worker("summary");
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let pending_a = ChatEntryId::new();
    set_compaction_in_flight(&worker, &session_a, pending_a.clone());

    // And a snapshot for session B that does NOT contain session A's pending
    // compaction entry (and is otherwise at the gate's "nothing to do" path).
    let snapshot_b: Arc<[ChatEntry]> = Arc::from(alternating_history(5));

    // When evaluating session B.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let mutations_b = rt.block_on(async { worker.evaluate(&session_b, snapshot_b).await });

    // Then session B is NOT suppressed by A's guard: it evaluates normally
    // (empty here because no threshold/context_size is set, but crucially it
    // did not take the "in flight, skip" early return).
    assert!(
        mutations_b.is_empty(),
        "session B evaluates normally, returns empty (no threshold)"
    );
    // And session A's in-flight state is untouched by session B's evaluation.
    assert!(
        worker_in_flight(&worker, &session_a),
        "session A guard untouched by session B"
    );
    assert_eq!(
        worker_pending_id(&worker, &session_a),
        Some(pending_a),
        "session A pending ID untouched"
    );
}

#[rstest::rstest]
#[test]
fn clearing_session_b_does_not_clear_session_a() {
    // Given a worker where BOTH sessions have compaction in flight.
    let worker = test_worker("summary");
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let pending_a = ChatEntryId::new();
    let pending_b = ChatEntryId::new();
    set_compaction_in_flight(&worker, &session_a, pending_a.clone());
    set_compaction_in_flight(&worker, &session_b, pending_b.clone());

    // And a snapshot for session B that CONTAINS its pending entry (so B
    // should clear).
    let mut snapshot = vec![compaction_entry("b summary")];
    snapshot[0].id = pending_b.clone();
    let snapshot_b: Arc<[ChatEntry]> = Arc::from(snapshot);

    // When evaluating session B.
    let rt = tokio::runtime::Runtime::new().expect("test runtime");
    let _mutations_b = rt.block_on(async { worker.evaluate(&session_b, snapshot_b).await });

    // Then session B's guard is cleared.
    assert!(
        !worker_in_flight(&worker, &session_b),
        "session B cleared after its summary landed"
    );
    assert!(
        worker_pending_id(&worker, &session_b).is_none(),
        "session B pending ID cleared"
    );
    // And session A's guard is untouched (not cleared by B).
    assert!(
        worker_in_flight(&worker, &session_a),
        "session A guard NOT cleared by session B"
    );
    assert_eq!(
        worker_pending_id(&worker, &session_a),
        Some(pending_a),
        "session A pending ID NOT cleared by session B"
    );
}
