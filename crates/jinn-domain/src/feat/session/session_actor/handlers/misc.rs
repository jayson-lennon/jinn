//! Miscellaneous handlers - model refresh display and session picker loading.
//!
//! Handles pushing model refresh results as transient markdown entries to the chat log,
//! and loading session picker entries from the session store into app state.

use super::super::SessionPersistenceActor;
use crate::common::actor_deps::BusPublish;
use crate::feat::context::protocol::event::ContextOverrideChanged;
use crate::feat::provider::protocol::event::ModelsRefreshed;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
use crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations;

use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::{ChatEntry, PickerKind};

impl SessionPersistenceActor {
    /// Pushes a transient markdown entry after model refresh.
    ///
    /// Builds a markdown table from the refresh results and pushes it directly
    /// to session state. Does NOT emit `PushChatEntry` - transient entries
    /// are not persisted.
    pub(in crate::feat::session::session_actor) fn on_models_refreshed(
        &self,
        event: &ModelsRefreshed,
    ) {
        let content = if event.results.is_empty() && event.errors.is_empty() {
            "Models refreshed: no providers found".to_owned()
        } else {
            build_models_refresh_table(event)
        };

        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.push_entry(ChatEntry::transient(content));
        });
    }

    /// Pushes a transient entry listing discovered skills.
    pub(in crate::feat::session::session_actor) fn on_skills_loaded(
        &self,
        event: &crate::feat::skills::SkillsLoaded,
    ) {
        // Only show a message when the skill picker is active (manual refresh).
        // Startup scans arrive while no picker is open.
        let is_picker_active = {
            let state = self.state.read();
            state.frontend.picker_kind() == Some(PickerKind::Skill)
        };

        if !is_picker_active {
            return;
        }

        let content = if let Some(err) = &event.error {
            format!("Skills refresh failed: {err}")
        } else if event.skills.is_empty() {
            "Skills refreshed: no skills found".to_owned()
        } else {
            build_skills_refresh_message(&event.skills)
        };

        self.state.with_session(&self.cap, |view| {
            if let Some(session) = view.session.map().get_mut(&event.session_id) {
                session.push_entry(ChatEntry::transient(content));
            }
        });
    }

    /// Loads session picker entries from the session store into `AppState`.
    pub(in crate::feat::session::session_actor) async fn handle_load_session_picker_entries(
        &self,
        _payload: &LoadSessionPickerEntries,
    ) {
        {
            let store = &self.services.session_store;
            let theme = {
                let state = self.state.read();
                state.frontend.theme.clone()
            };
            let entries =
                crate::feat::session::entries::load_session_entries_from_store(store, &theme).await;
            let wrapped = crate::feat::session::entries::wrap_session_entries(entries);
            self.state.with_preferences(&self.frontend_cap, |ops| {
                let frontend = ops.frontend();
                frontend.session_picker_mut().set_items(wrapped);
            });
        }
    }

    /// Queues a batch of history mutations for deferred application.
    ///
    /// Workers submit `Vec<HistoryMutation>` batches via the
    /// `SubmitHistoryMutations` command. This handler pushes them to
    /// `pending_mutations` and applies them immediately if the session is
    /// idle (no active stream). If the session is streaming or sending,
    /// mutations are deferred until the next stream completion.
    pub(in crate::feat::session::session_actor) async fn handle_submit_history_mutations(
        &self,
        payload: &SubmitHistoryMutations,
    ) {
        if payload.mutations.is_empty() {
            return;
        }

        // Resolve token costs for all incoming context-override mutations in a
        // single read-guard pass, before taking the write lock. The cost map is
        // then consumed by the accumulator inside the write lock without any
        // self-borrow (which would deadlock against the held write guard).
        let threshold = {
            let state = self.state.read();
            state
                .frontend
                .preferences
                .auto_prune
                .accumulation_threshold_tokens
        };
        let token_costs: std::collections::HashMap<crate::protocol::ChatEntryId, u32> = {
            use crate::feat::context::strategy::token_estimator::TokenCounter;
            let state = self.state.read();
            let session = state.session.get(&payload.session_id);
            payload
                .mutations
                .iter()
                .filter_map(|m| match m {
                    crate::protocol::HistoryMutation::SetContextOverride { entry_id, source, .. } => {
                        // Only prune ForcedExclude mutations reach the
                        // accumulator, so only their cost is relevant.
                        // Worker ForcedInclude and compaction overrides
                        // apply immediately and need no cost.
                        if is_compaction_source(source) {
                            None
                        } else {
                            Some(entry_id)
                        }
                    }
                    _ => None,
                })
                .map(|entry_id| {
                    let cost = self
                        .token_cache
                        .get(&payload.session_id, entry_id)
                        .or_else(|| {
                            session
                                .and_then(|s| s.history().iter().find(|e| &e.id == entry_id))
                                .and_then(|e| e.prompt_text())
                                .map(|t| self.counter.count(t) as u32)
                        })
                        .unwrap_or(0);
                    (entry_id.clone(), cost)
                })
                .collect()
        };

        // Capture what changed (if anything) so events can be emitted after releasing the write lock.
        let (session_id, changed) = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(&payload.session_id);

                for mutation in payload.mutations.clone() {
                    if is_prune_override(&mutation) {
                        // Pruner ForcedExclude: route into the accumulation buffer
                        // so it counts toward the batch flush threshold.
                        if let crate::protocol::HistoryMutation::SetContextOverride { entry_id, value, source } = &mutation {
                            let cost = token_costs.get(entry_id).copied().unwrap_or(0);
                            session.core.ephemeral.accumulated_overrides
                                .push(entry_id.clone(), *value, source.clone(), cost);
                        }
                    } else {
                        // All other mutations apply immediately:
                        //   - compaction overrides (compaction is itself a context reduction),
                        //   - worker ForcedInclude (protection, never a prune),
                        //   - any non-context mutation.
                        // Only prune ForcedExclude is subject to the accumulation gate.
                        session.queue_mutations(vec![mutation]);
                    }
                }

                // Flush the accumulator if its deduplicated token total crossed the threshold.
                session.flush_accumulated_overrides_if_needed(threshold);

                tracing::debug!(
                    session_id = %payload.session_id,
                    queue_len = session.core.ephemeral.pending_mutations.len(),
                    accumulated = session.accumulated_overrides_total(),
                    threshold,
                    "routed history mutations from worker"
                );

                // If the session is idle (no active stream), drain immediately.
                // Otherwise mutations wait for the next stream completion.
                if matches!(session.phase(), PhaseKind::Idle) {
                    let (_count, changed) = session.drain_and_apply_pending_mutations();
                    (payload.session_id.clone(), changed)
                } else {
                    (payload.session_id.clone(), Vec::new())
                }
            })
        };

        // Emit ContextOverrideChanged events for any entry whose override actually changed.
        // Doing this outside the write lock keeps the bus dispatch decoupled from session state.
        for entry_id in changed {
            self.publish(ContextOverrideChanged {
                session_id: session_id.clone(),
                entry_id,
            })
            .await;
        }
    }
}

/// Whether a mutation is a prune `ForcedExclude` override — the only
/// direction subject to the accumulation gate.
///
/// The accumulator exists to batch pruner excludes into a single flush so
/// the server-side KV cache isn't invalidated per-entry. Worker
/// `ForcedInclude` (protection), compaction overrides, and any non-context
/// mutation apply immediately instead.
fn is_prune_override(mutation: &crate::protocol::HistoryMutation) -> bool {
    use crate::protocol::HistoryMutation;
    matches!(
        mutation,
        HistoryMutation::SetContextOverride {
            value: crate::protocol::ContextOverride::ForcedExclude,
            source,
            ..
        } if !is_compaction_source(source)
    )
}

/// Whether a `ChangeSource` is the compaction worker.
///
/// Compaction overrides are exempt from the accumulation gate: compaction is
/// itself a context reduction that must apply promptly, and holding back its
/// excludes would leave the gathered entries and the new summary both in
/// context simultaneously.
fn is_compaction_source(source: &crate::protocol::ChangeSource) -> bool {
    matches!(source, crate::protocol::ChangeSource::Worker { name } if name == "compaction")
}
/// Builds a markdown table string from the models refresh event.
///
/// Format:
/// ```markdown
/// | Provider | Models | Status |
/// |----------|--------|--------|
/// | ollama   | 5      | ✅     |
/// | openai   | 0      | ❌ API key not resolved |
/// ```
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
fn build_models_refresh_table(event: &ModelsRefreshed) -> String {
    // Collect all provider names and sort alphabetically.
    let mut all_providers: Vec<&str> = event
        .results
        .keys()
        .chain(event.errors.keys())
        .map(std::string::String::as_str)
        .collect();
    all_providers.sort_unstable();
    all_providers.dedup();

    let mut rows = Vec::new();
    for provider in all_providers {
        if let Some(models) = event.results.get(provider) {
            rows.push(format!("| {provider} | {} | ✅ |", models.len()));
        } else if let Some(err) = event.errors.get(provider) {
            rows.push(format!("| {provider} | 0 | ❌ {err} |"));
        }
    }

    let mut table = String::new();
    table.push_str("| Provider | Models | Status |\n");
    table.push_str("|----------|--------|--------|\n");
    for row in rows {
        table.push_str(&row);
        table.push('\n');
    }

    table
}

/// Builds a markdown message listing discovered skills.
fn build_skills_refresh_message(skills: &[crate::feat::skills::Skill]) -> String {
    let mut msg = format!("Skills refreshed: {} found\n\n", skills.len());
    for skill in skills {
        msg.push_str("- ");
        msg.push_str(&skill.name);
        msg.push('\n');
    }
    msg
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::unnecessary_mut_passed,
        reason = "test code"
    )]
    use crate::feat::provider::protocol::event::ModelsRefreshed;
    use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
    use crate::feat::session::session_actor::helpers::{
        test_actor_recording, test_actor_with_store_recording,
    };
    use crate::feat::ui::picker_states::PickerExt;
    use crate::protocol::{ChangeSource, ChatEntry, ChatEntryKind, SessionId};
    use jinn_provider::{InputModalities, ModelInfo};
    use std::collections::HashMap;

    #[rstest::rstest]
    #[tokio::test]
    async fn on_models_refreshed_pushes_transient_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = SessionId::new();

        let mut results = HashMap::new();
        results.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: InputModalities::text(),
            }],
        );
        actor.on_models_refreshed(&ModelsRefreshed {
            session_id: session_id.clone(),
            results,
            errors: HashMap::new(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        assert_eq!(session.history().len(), 1);
        let entry = &session.history()[0];
        assert!(matches!(&entry.kind, ChatEntryKind::Transient(t) if t.contains("ollama")));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_models_refreshed_empty_results_shows_no_providers_message() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = SessionId::new();

        actor.on_models_refreshed(&ModelsRefreshed {
            session_id: session_id.clone(),
            results: HashMap::new(),
            errors: HashMap::new(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        assert_eq!(session.history().len(), 1);
        let entry = &session.history()[0];
        assert!(
            matches!(&entry.kind, ChatEntryKind::Transient(t) if t.contains("no providers found")),
            "expected 'no providers found' message, got {:?}",
            entry.kind
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_models_refreshed_with_errors_shows_table() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = SessionId::new();

        let mut errors = HashMap::new();
        errors.insert("openai".to_owned(), "API key not resolved".to_owned());
        actor.on_models_refreshed(&ModelsRefreshed {
            session_id: session_id.clone(),
            results: HashMap::new(),
            errors,
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let entry = &session.history()[0];
        assert!(
            matches!(&entry.kind, ChatEntryKind::Transient(t) if t.contains("openai") && t.contains("API key not resolved")),
            "expected table with error, got {:?}",
            entry.kind
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn build_models_refresh_table_includes_provider_and_model_count() {
        let mut results = HashMap::new();
        results.insert(
            "ollama".to_owned(),
            vec![
                ModelInfo {
                    id: "llama3".to_owned(),
                    context_length: Some(8192),
                    input_modalities: InputModalities::text(),
                },
                ModelInfo {
                    id: "phi3".to_owned(),
                    context_length: None,
                    input_modalities: InputModalities::text(),
                },
            ],
        );
        let event = ModelsRefreshed {
            session_id: SessionId::new(),
            results,
            errors: HashMap::new(),
        };

        let table = super::build_models_refresh_table(&event);

        assert!(table.contains("ollama"), "expected provider name in table");
        assert!(table.contains('2'), "expected model count in table");
        assert!(table.contains("✅"), "expected success indicator");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_load_session_picker_entries_loads_from_store() {
        let session = crate::feat::session::chat_session::ChatSessionState::new();
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![session]).await;

        actor
            .handle_load_session_picker_entries(&LoadSessionPickerEntries)
            .await;

        let state = actor.state.read();
        assert!(
            !state.frontend.session_picker().items().is_empty(),
            "expected session picker to have entries after loading from store"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_buffers_subthreshold_override_when_idle() {
        // Given a default (10_000) accumulation threshold and one user entry.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        let entry_id = {
            let state = actor.state.read();
            state.session.get(&session_id).unwrap().history()[0]
                .id
                .clone()
        };

        // When submitting a single sub-threshold ForcedExclude override.
        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: entry_id.clone(),
                        value: crate::protocol::ContextOverride::ForcedExclude,
                        source: ChangeSource::Internal { label: "test".to_owned() },
                    },
                ],
                },
            )
            .await;

        // Then the override is buffered (not applied) and no pending batch exists.
        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert_eq!(
            session.history()[0].context_override(),
            crate::protocol::ContextOverride::Default
        );
        assert!(session.core.ephemeral.pending_mutations.is_empty());
        assert!(
            !session.core.ephemeral.accumulated_overrides.is_empty(),
            "sub-threshold override should be buffered in the accumulator"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_with_empty_batch_is_noop() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let state = actor.state.read();
            state.session.active_session_id().clone()
        };

        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![],
                },
            )
            .await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert!(session.core.ephemeral.pending_mutations.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_creates_session_if_missing() {
        let (actor, _audit) = test_actor_recording().await;
        let new_session_id = SessionId::new();

        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: new_session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: crate::protocol::ChatEntryId::new(),
                        value: crate::protocol::ContextOverride::ForcedExclude,
                        source: ChangeSource::Internal { label: "test".to_owned() },
                    },
                ],
                },
            )
            .await;

        let state = actor.state.read();
        let session = state.session.get(&new_session_id).unwrap();
        assert!(session.core.ephemeral.pending_mutations.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_buffers_prune_override_and_applies_include_immediately()
     {
        // Given a default (10_000) accumulation threshold and two user entries.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("first"));
            session.push_entry(ChatEntry::user("second"));
            state.session.active_session_id().clone()
        };
        let entry_id_1 = {
            let state = actor.state.read();
            state.session.get(&session_id).unwrap().history()[0]
                .id
                .clone()
        };
        let entry_id_2 = {
            let state = actor.state.read();
            state.session.get(&session_id).unwrap().history()[1]
                .id
                .clone()
        };

        // When submitting a sub-threshold ForcedExclude (prune) for entry 1
        // and a ForcedInclude (worker protection) for entry 2.
        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: entry_id_1,
                        value: crate::protocol::ContextOverride::ForcedExclude,
                        source: ChangeSource::Internal { label: "test".to_owned() },
                    },
                ],
                },
            )
            .await;
        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: entry_id_2,
                        value: crate::protocol::ContextOverride::ForcedInclude,
                        source: ChangeSource::Internal { label: "test".to_owned() },
                    },
                ],
                },
            )
            .await;

        // Then the ForcedExclude is buffered (entry 1 still Default) and the
        // ForcedInclude applies immediately (entry 2 is ForcedInclude), so only
        // one override sits in the accumulator.
        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert_eq!(session.core.ephemeral.pending_mutations.len(), 0);
        assert_eq!(
            session.history()[0].context_override(),
            crate::protocol::ContextOverride::Default
        );
        assert_eq!(
            session.history()[1].context_override(),
            crate::protocol::ContextOverride::ForcedInclude
        );
        assert_eq!(
            session.core.ephemeral.accumulated_overrides.len(),
            1,
            "only the ForcedExclude (prune) should be buffered; the include applies immediately"
        );
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_emits_context_override_changed_on_change() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        let entry_id = {
            let state = actor.state.read();
            state.session.get(&session_id).unwrap().history()[0]
                .id
                .clone()
        };

        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: entry_id.clone(),
                        value: crate::protocol::ContextOverride::ForcedExclude,
                        source: ChangeSource::Worker { name: "compaction".to_owned() },
                    },
                ],
                },
            )
            .await;

        assert!(
            audit.contains_name("ContextOverrideChanged"),
            "expected ContextOverrideChanged to be emitted for worker-applied change"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_submit_history_mutations_does_not_emit_on_noop_mutation() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let id = session.core.session_id.clone();
            session.core.history[0].apply_context_override(
                crate::protocol::ContextOverride::ForcedExclude,
                ChangeSource::Internal {
                    label: "setup".to_owned(),
                },
            );
            id
        };
        let entry_id = {
            let state = actor.state.read();
            state.session.get(&session_id).unwrap().history()[0]
                .id
                .clone()
        };

        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                    crate::protocol::HistoryMutation::SetContextOverride {
                        entry_id: entry_id.clone(),
                        value: crate::protocol::ContextOverride::ForcedExclude,
                        source: ChangeSource::Worker { name: "test_worker".to_owned() },
                    },
                ],
                },
            )
            .await;

        assert!(
            !audit.contains_name("ContextOverrideChanged"),
            "expected no ContextOverrideChanged for no-op mutation"
        );
        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert_eq!(
            session.history()[0].context_history.len(),
            1,
            "context_history should contain only the setup event"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn worker_forced_include_override_applies_immediately_not_buffered() {
        // Given a session with one assistant entry and the default 10_000 threshold.
        let (actor, _audit) = test_actor_recording().await;
        let (session_id, entry_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            let entry = ChatEntry::assistant("response");
            let id = entry.id.clone();
            session.push_entry(entry);
            (state.session.active_session_id().clone(), id)
        };

        // When submitting a worker ForcedInclude override (protection, never a prune).
        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                        crate::protocol::HistoryMutation::SetContextOverride {
                            entry_id: entry_id.clone(),
                            value: crate::protocol::ContextOverride::ForcedInclude,
                            source: ChangeSource::Worker { name: "auto-prune-todo".to_owned() },
                        },
                    ],
                },
            )
            .await;

        // Then the override applied immediately (no buffering) because worker
        // includes never count toward the prune threshold.
        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert_eq!(
            session.history()[0].context_override(),
            crate::protocol::ContextOverride::ForcedInclude,
            "worker ForcedInclude must apply immediately"
        );
        assert!(
            session.core.ephemeral.accumulated_overrides.is_empty(),
            "worker ForcedInclude must not enter the accumulation buffer"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn compaction_forced_exclude_override_applies_immediately_not_buffered() {
        // Given a session with one assistant entry.
        let (actor, _audit) = test_actor_recording().await;
        let (session_id, entry_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            let entry = ChatEntry::assistant("response");
            let id = entry.id.clone();
            session.push_entry(entry);
            (state.session.active_session_id().clone(), id)
        };

        // When submitting a compaction ForcedExclude override.
        actor
            .handle_submit_history_mutations(
                &crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations {
                    session_id: session_id.clone(),
                    mutations: vec![
                        crate::protocol::HistoryMutation::SetContextOverride {
                            entry_id: entry_id.clone(),
                            value: crate::protocol::ContextOverride::ForcedExclude,
                            source: ChangeSource::Worker { name: "compaction".to_owned() },
                        },
                    ],
                },
            )
            .await;

        // Then the override applied immediately and did not enter the buffer.
        let state = actor.state.read();
        let session = state.session.get(&session_id).unwrap();
        assert_eq!(
            session.history()[0].context_override(),
            crate::protocol::ContextOverride::ForcedExclude,
            "compaction ForcedExclude must apply immediately"
        );
        assert!(
            session.core.ephemeral.accumulated_overrides.is_empty(),
            "compaction ForcedExclude must not enter the accumulation buffer"
        );
    }
}
