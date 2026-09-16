//! Tool call state tracking handlers - manage tool call lifecycle during streaming.
//!
//! Handles the full tool call lifecycle: creation via streaming, argument assembly,
//! execution tracking, result collection, and batch completion routing.

use crate::common::actor_deps::BusPublish;
use crate::feat::context::protocol::event::ContextOverrideChanged;
use crate::feat::context::snapshot::{assemble_via_service, build_assembly_inputs};
use crate::feat::provider::protocol::command::SendToLlmProvider;
use crate::feat::session::chat_entry::PinPosition;
use crate::feat::session::model_selection::ModelSelection;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::token_stats::TokenRecord;
use crate::feat::tools_actor::protocol::event::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted,
};

use super::super::SessionPersistenceActor;

impl SessionPersistenceActor {
    /// Begins tracking a streaming tool call.
    pub(in crate::feat::session::session_actor) fn on_tool_use_started(
        &self,
        event: &ToolUseStarted,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.begin_tool_call(event.index, &event.id, &event.name, event.dispatched_at);
        });
    }
    /// Finalizes the tool call entry with complete arguments.
    ///
    /// The placeholder entry was created by `on_tool_use_started`. This updates
    /// it in place with the full arguments string, avoiding a duplicate entry.
    pub(in crate::feat::session::session_actor) fn on_tool_call_received(
        &self,
        event: &ToolCallReceived,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.finalize_tool_call(
                &event.tool_call.id,
                &event.tool_call.name,
                &event.tool_call.arguments,
            );
        });
    }

    /// Appends a partial JSON delta to a streaming tool call.
    pub(in crate::feat::session::session_actor) fn on_tool_call_streaming(
        &self,
        event: &ToolCallStreaming,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            if let Err(e) = session.append_tool_call_delta(event.index, &event.partial_json) {
                tracing::error!(err = ?e, "failed to append tool call delta");
            }
        });
    }
    /// Pushes a tool result entry into the session history.
    pub(in crate::feat::session::session_actor) async fn on_tool_execution_completed(
        &self,
        event: &ToolExecutionCompleted,
    ) {
        {
            let should_continue = self.state.with_session(&self.cap, |view| -> bool {
                let session = view.session.map().get_or_create(&event.session_id);
                // Drop stale results that arrive after a cancel. Legitimate tool
                // execution only ever runs in `Sending`; a result landing in any
                // other phase (e.g. `Idle` after cancel) is a straggler whose
                // background task has not yet been aborted.
                if !matches!(session.phase(), PhaseKind::Sending) {
                    tracing::debug!(
                        session_id = %event.session_id,
                        phase = ?session.phase(),
                        "dropping stale ToolExecutionCompleted: session not in Sending"
                    );
                    return false;
                }
                session.finalize_tool_result(
                    &event.result.tool_call_id,
                    &event.result.name,
                    &event.result.content,
                    event.result.success,
                    event.result.full_content.clone(),
                    event.result.truncation.clone(),
                    event.result.pin_position.map(PinPosition::from),
                );
                true
            });
            if !should_continue {
                return;
            }
        };

        super::super::helpers::emit_history_appended(self.bus(), &event.session_id).await;
        self.save_active_session(&event.session_id).await;
    }

    /// Creates a pending ToolResult entry when a streaming tool starts executing.
    pub(in crate::feat::session::session_actor) fn on_tool_execution_started(
        &self,
        event: &ToolExecutionStarted,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.begin_tool_result(&event.tool_call_id, &event.name, event.dispatched_at);
        });
    }
    /// Appends incremental output to a pending ToolResult entry.
    pub(in crate::feat::session::session_actor) fn on_tool_execution_output(
        &self,
        event: &ToolExecutionOutput,
    ) {
        self.state.with_session(&self.cap, |view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.append_tool_result_output(&event.tool_call_id, &event.output, event.kind);
        });
    }
    /// Drains pending history mutations and steering buffer entries, emitting
    /// ContextOverrideChanged events for any modified entries.
    async fn apply_pending_mutations_and_steering(&self, session_id: &crate::protocol::SessionId) {
        // Drain and apply pending history mutations, then normalize loop
        // layout so committed loops never contain interstitials before
        // assembly (the read-side converter stays simple).
        let changed = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                let (count, changed) = session.drain_and_apply_pending_mutations();
                if count > 0 {
                    tracing::debug!(
                        session_id = %session_id,
                        applied = count,
                        "applied pending history mutations"
                    );
                }
                session.edit_history().normalize_loop_layout();
                changed
            })
        };
        // Emit ContextOverrideChanged events outside the write lock.
        for entry_id in changed {
            self.publish(ContextOverrideChanged {
                session_id: session_id.clone(),
                entry_id,
            })
            .await;
        }

        // Drain any pending steering fragments into history.
        {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                if let Some(entry) = session.steering_buffer_mut().drain_into_entry() {
                    let entry_id = entry.id.clone();
                    let index = session.push_entry(entry);
                    tracing::debug!(
                        session_id = %session_id,
                        entry_id = %entry_id,
                        history_index = index,
                        "drained steering entry into history at tool-batch boundary"
                    );
                }
            });
        }
    }

    /// Assembles the continuation prompt, transitions to streaming phase,
    /// and emits the SendToLlmProvider command.
    async fn assemble_and_send_continuation(&self, session_id: &crate::protocol::SessionId) {
        let assembled = {
            let inputs = {
                let guard = self.state.read();
                // FIXME: make spawn_blocking probably
                build_assembly_inputs(&guard, session_id)
            };
            match assemble_via_service(&self.services, inputs).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        session_id = %session_id,
                        "context assembly failed; continuation aborted"
                    );
                    return;
                }
            }
        };

        // Resolve model under write lock (round-robin mutates index), push token
        // record, and transition phase — all in one lock acquisition.
        let (provider_id, model_used, reasoning_effort, endpoint_tag, old_phase, new_phase) = {
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().get_or_create(session_id);
                let old_phase = session.phase();
                session.begin_streaming();

                let reasoning_effort = {
                    let profile = session.profile();
                    crate::resolve_effort(profile.reasoning_effort)
                };
                let (provider_id, model_used, endpoint_tag) = {
                    // Snapshot the endpoint tag immutably before mutating the model
                    // (alloy round-robin mutates index during resolve_model).
                    let endpoint_tag = match (&session.profile().model, &session.profile().endpoint)
                    {
                        (ModelSelection::Single(_), Some(ep)) => Some(ep.tag.clone()),
                        _ => None,
                    };
                    let model = &mut session.profile_mut().model;
                    if model.is_no_provider() {
                        (None, None, endpoint_tag)
                    } else {
                        let resolved = model.resolve_model();
                        (Some(resolved.clone()), Some(resolved), endpoint_tag)
                    }
                };

                session.push_token_record(TokenRecord {
                    model_used: model_used.clone(),
                    timestamp: jiff::Timestamp::now(),
                    tokens_sent: assembled.estimated_tokens(),
                    tokens_received: 0,
                    cost: None,
                    prompt_tokens: None,
                    cached_tokens: None,
                });

                (
                    provider_id,
                    model_used,
                    reasoning_effort,
                    endpoint_tag,
                    old_phase,
                    session.phase(),
                )
            })
        };
        super::super::helpers::emit_phase_changed(self.bus(), session_id, old_phase, new_phase)
            .await;

        let estimated_tokens = assembled.estimated_tokens();

        tracing::info!(
            session_id = %session_id,
            model = ?model_used,
            "emitting SendToLlmProvider"
        );
        self.publish(SendToLlmProvider {
            origin: crate::feat::provider::protocol::command::StreamOrigin::ToolContinuation,
            model_used,
            reasoning_effort,
            endpoint_tag,
            session_id: session_id.clone(),
            messages: assembled.messages,
            system_prompt: assembled.system_prompt,
            provider_id,
            estimated_tokens,
            tool_definitions: assembled.tool_definitions,
            dispatched_at: jiff::Timestamp::now(),
        })
        .await;
    }

    /// All tools in a batch have finished — route the continuation through
    /// context assembly so token counting and prompt strategy apply.
    ///
    /// By this point, the session history already contains `ToolCall`,
    /// `ToolResult`, and `Assistant` entries from earlier event handlers,
    /// and the session is already in sending state (set by `on_stream_completed`
    /// for the `ToolUse` reason). We just need to assemble the prompt via
    /// the full session history.
    pub(in crate::feat::session::session_actor) async fn on_tool_batch_completed(
        &self,
        event: &ToolBatchCompleted,
    ) {
        tracing::info!(
            session_id = ?event.session_id,
            result_count = event.results.len(),
            "on_tool_batch_completed"
        );

        // Buffer-or-process: a legitimate `ToolBatchCompleted` arrives when the
        // session is `Sending` (tools run between stream turns). If it arrives
        // while still `Streaming`, the matching `StreamCompleted(ToolUse)` is
        // in flight on the bus and hasn't transitioned the phase yet — buffer
        // the results and let `on_stream_completed` drain them once the phase
        // advances. Any other phase (e.g. `Idle` after cancel) is a stale
        // straggler that must not restart the loop. Must precede the
        // `tool_loop_disabled` branch.
        {
            let should_continue = self.state.with_session(&self.cap, |view| -> bool {
                let session = view.session.map().get_or_create(&event.session_id);
                match session.phase() {
                    PhaseKind::Sending => { /* normal path — proceed below */ }
                    PhaseKind::Streaming => {
                        tracing::info!(
                            session_id = ?event.session_id,
                            result_count = event.results.len(),
                            "buffering early ToolBatchCompleted: StreamCompleted(ToolUse) still in flight"
                        );
                        session.core.ephemeral.pending_tool_batch = Some(event.results.clone());
                        return false;
                    }
                    other => {
                        tracing::warn!(
                            session_id = ?event.session_id,
                            phase = ?other,
                            "dropping stale ToolBatchCompleted: session not in Sending"
                        );
                        return false;
                    }
                }
                true
            });
            if !should_continue {
                return;
            }
        }

        self.continue_tool_loop(&event.session_id).await;
    }

    /// Continues the tool loop after a batch completes: applies pending
    /// mutations, checks `tool_loop_disabled`, and dispatches the next
    /// `SendToLlmProvider` (or finishes sending if the loop is disabled).
    ///
    /// Called from both `on_tool_batch_completed` (normal path, phase already
    /// `Sending`) and `on_stream_completed` (draining a buffered batch that
    /// raced ahead of `StreamCompleted(ToolUse)`).
    pub(in crate::feat::session::session_actor) async fn continue_tool_loop(
        &self,
        session_id: &crate::protocol::SessionId,
    ) {
        // If tool loop is disabled, end the turn instead of continuing.
        // This is used by judge verdict tools to prevent infinite tool-call loops.
        // finish_sending_via_machine delegates to on_tool_batch_completed() which
        // reads and clears the tool_loop_disabled flag from the machine.
        let tool_loop_disabled = {
            let state = self.state.read();
            let session = state.session(session_id);
            session.is_tool_loop_disabled()
        };

        if tool_loop_disabled {
            let (old_phase, new_phase) = {
                self.state.with_session(&self.cap, |view| {
                    let session = view.session.map().get_or_create(session_id);
                    let old_phase = session.phase();
                    session.finish_sending_via_machine();
                    (old_phase, session.phase())
                })
            };
            super::super::helpers::emit_phase_changed(self.bus(), session_id, old_phase, new_phase)
                .await;
            return;
        }

        self.apply_pending_mutations_and_steering(session_id).await;

        self.assemble_and_send_continuation(session_id).await;
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
    use super::super::super::helpers::{test_actor, test_actor_recording};
    use crate::feat::provider::protocol::event::{StreamCompleted, StreamCompletedReason};
    use crate::feat::session::phase_machine::PhaseKind;
    use crate::feat::session::token_stats::TokenRecord;
    use crate::feat::session::tool_result_status::ToolResultStatus;
    use crate::feat::tools_actor::protocol::event::{
        ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionOutput,
        ToolExecutionStarted, ToolOutputKind, ToolUseStarted,
    };
    use crate::feat::tools_actor::tool_types::{ToolCall, ToolResult};
    use crate::protocol::{ChangeSource, ChatEntry, ChatEntryKind};

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_emits_send_to_llm_provider() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider command to be emitted, got: {:?}",
            audit.names()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_batch_completed_via_bus_emits_continuation() {
        // Given a spawned session actor with a tool-call entry in its history.
        use crate::common::app_state::AppState;
        use crate::common::bus::test_harness::{TestHarness, await_recorded};
        use crate::common::state::State;
        use crate::feat::context::strategy::token_estimator::TiktokenCounter;
        use crate::feat::provider::protocol::command::SendToLlmProvider;
        use crate::feat::session::session_actor::{
            SessionPersistenceActor, SessionPersistenceActorDeps,
        };
        use crate::feat::session_lifecycle::builtin::BuiltinRegistry;
        use std::time::Duration;

        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        {
            let mut s = state.write_test_no_cap();
            let session = s.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            session.begin_sending();
        }
        let session_id = state.read().session.active_session_id().clone();

        let actor_ref = harness
            .spawn_actor::<SessionPersistenceActor>(SessionPersistenceActorDeps {
                deps: {
                    let mut deps = harness.actor_deps().await;
                    let _ = jinn_context_assembly::service::ensure_spawned(
                        &deps.services.trouper_system,
                    );
                    deps
                },
                state,
                cap: crate::common::tcaps::mint::mint_session_cap(),
                frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
                counter: TiktokenCounter::o200k_base(),
                token_cache:
                    crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache::default(),
                builtin_registry: BuiltinRegistry::new(),
                shell: "/bin/sh".to_owned(),
                image_converter: crate::feat::image_convert::ImageConverterService::unavailable(),
            })
            .await;
        actor_ref.wait_for_startup().await;

        // When ToolBatchCompleted is published to the bus.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        harness.publish(event).await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;

        // Then the actor published SendToLlmProvider via the Message handler.
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "expected SendToLlmProvider to reach the bus via the Message handler"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn canceled_after_tool_use_does_not_redispatch() {
        // Given a spawned session actor with a streaming session whose tool
        // batch has already landed (the buffered-batch pre-cancel state: history
        // holds the tool call, phase is Streaming).
        use crate::common::app_state::AppState;
        use crate::common::bus::test_harness::{TestHarness, await_recorded};
        use crate::common::state::State;
        use crate::feat::context::strategy::token_estimator::TiktokenCounter;
        use crate::feat::provider::protocol::command::SendToLlmProvider;
        use crate::feat::provider::protocol::event::{StreamCompleted, StreamCompletedReason};
        use crate::feat::session::phase_machine::PhaseKind;
        use crate::feat::session::session_actor::{
            SessionPersistenceActor, SessionPersistenceActorDeps,
        };
        use crate::feat::session_lifecycle::builtin::BuiltinRegistry;
        use crate::feat::tools_actor::protocol::event::ToolBatchCompleted;
        use crate::feat::tools_actor::tool_types::ToolResult;
        use crate::protocol::ChatEntry;
        use std::time::Duration;

        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        {
            let mut s = state.write_test_no_cap();
            let session = s.active_session_mut();
            session.push_entry(ChatEntry::user("fetch a thing"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "sample_tool",
                r#"{"arg":"x"}"#,
            ));
            session.begin_streaming();
        }
        let session_id = state.read().session.active_session_id().clone();

        let actor_ref = harness
            .spawn_actor::<SessionPersistenceActor>(SessionPersistenceActorDeps {
                deps: {
                    let mut deps = harness.actor_deps().await;
                    let _ = jinn_context_assembly::service::ensure_spawned(
                        &deps.services.trouper_system,
                    );
                    deps
                },
                state: state.clone(),
                cap: crate::common::tcaps::mint::mint_session_cap(),
                frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
                counter: TiktokenCounter::o200k_base(),
                token_cache:
                    crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache::default(),
                builtin_registry: BuiltinRegistry::new(),
                shell: "/bin/sh".to_owned(),
                image_converter: crate::feat::image_convert::ImageConverterService::unavailable(),
            })
            .await;
        actor_ref.wait_for_startup().await;

        // When the tool batch lands while the session is still Streaming — the
        // tool-call-watchdog race shape: the batch is buffered, then the aborted
        // stream task's StreamCompleted(ToolUse) arrives just BEFORE the cancel.
        let batch = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "sample_tool".to_owned(),
                content: "boom".to_owned(),
                success: false,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        harness.publish(batch).await;
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("calling the tool".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        harness.publish(event).await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "precondition: the tool loop dispatched its continuation"
        );

        // And StreamCompleted(Canceled) arrives afterwards.
        let cancel = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        harness.publish(cancel).await;

        // Then no FURTHER SendToLlmProvider is dispatched — the Canceled
        // completion must not re-enter the tool loop. (The recorder drains on
        // read, so this observes only post-cancel traffic.)
        let extra = await_recorded::<SendToLlmProvider>(&recorder, 0, Duration::from_secs(1)).await;
        assert!(
            extra.iter().all(|m| m.session_id != session_id),
            "cancel after tool-use must not trigger a second dispatch, got {extra:?}"
        );

        // And the session settles in Idle with a single cancel entry appended.
        let s = state.read();
        let session = s.session.get_unchecked(&session_id);
        assert_eq!(
            session.phase(),
            PhaseKind::Idle,
            "terminal phase after the cancel must be Idle"
        );
        let cancelled_entries = session
            .history()
            .iter()
            .filter(|e| matches!(e.kind, crate::protocol::ChatEntryKind::Error { .. }))
            .count();
        assert_eq!(
            cancelled_entries, 1,
            "exactly one 'Cancelled' error entry should be appended"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_survives_token_burst_on_unbounded_mailbox() {
        // Regression guard for the unbounded-mailbox fix. Production log
        // evidence showed that a >64-token burst at a `[DONE]` peak could fill the
        // session actor's bounded(64) mailbox, and the bus's BestEffort delivery
        // (try_send) silently dropped the terminal `StreamCompleted(ToolUse)` on
        // MailboxFull — wedging the session in Streaming forever (FIFO violation:
        // a message published first was dropped while one published 1.7ms later was
        // delivered). The session actor now spawns with an unbounded mailbox.
        //
        // This test locks in the production-shaped path under load: a BestEffort
        // bus, an unbounded session mailbox, a >64 token burst immediately
        // followed by the terminal event while a batch is buffered. It asserts the
        // terminal is delivered (buffer drains → SendToLlmProvider) and that the
        // Guaranteed→unbounded combination does NOT deadlock (the core argument
        // against switching the bus itself to Guaranteed). The drop race itself is
        // timing-dependent and not deterministically reproducible here; this guard
        // ensures the wiring stays correct and the burst path stays livelock-free.
        use crate::common::app_state::AppState;
        use crate::common::bus::test_harness::{TestHarness, await_recorded};
        use crate::common::state::State;
        use crate::feat::context::strategy::token_estimator::TiktokenCounter;
        use crate::feat::provider::protocol::command::SendToLlmProvider;
        use crate::feat::provider::protocol::event::StreamToken;
        use crate::feat::session::session_actor::{
            SessionPersistenceActor, SessionPersistenceActorDeps,
        };
        use crate::feat::session_lifecycle::builtin::BuiltinRegistry;
        use std::time::Duration;

        let harness = TestHarness::new_best_effort().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let dispatched_at = jiff::Timestamp::now();
        {
            let mut s = state.write_test_no_cap();
            let session = s.active_session_mut();
            session.begin_streaming();
            // Simulate an already-finished tool batch racing ahead of
            // StreamCompleted(ToolUse) — exactly the wedge precondition.
            session.core.ephemeral.pending_tool_batch = Some(vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "ok".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }]);
        }

        let actor_ref = harness
            .spawn_actor_with_mailbox::<SessionPersistenceActor>(
                SessionPersistenceActorDeps {
                    deps: {
                        let mut deps = harness.actor_deps().await;
                        let _ = jinn_context_assembly::service::ensure_spawned(
                            &deps.services.trouper_system,
                        );
                        deps
                    },
                    state,
                    cap: crate::common::tcaps::mint::mint_session_cap(),
                    frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
                    counter: TiktokenCounter::o200k_base(),
                    token_cache:
                        crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache::default(),
                    builtin_registry: BuiltinRegistry::new(),
                    shell: "/bin/sh".to_owned(),
                    image_converter: crate::feat::image_convert::ImageConverterService::unavailable(
                    ),
                },
                kameo::mailbox::unbounded(),
            )
            .await;

        // When a >64-token burst is published, immediately followed by the
        // terminal StreamCompleted(ToolUse).
        for i in 0..200 {
            harness
                .publish(StreamToken {
                    session_id: session_id.clone(),
                    index: i,
                    token: "x".to_owned(),
                    is_thinking: false,
                    dispatched_at,
                })
                .await;
        }
        harness
            .publish(StreamCompleted {
                session_id: session_id.clone(),
                reason: StreamCompletedReason::ToolUse,
                assistant_content: None,
                tool_calls: Some(vec![]),
                cost: None,
                provider_completion_tokens: None,
                provider_prompt_tokens: None,
                cached_tokens: None,
                thinking_content: None,
                model_used: None,
                dispatched_at,
            })
            .await;

        // Then the terminal was delivered: the buffered batch drained and a
        // continuation (SendToLlmProvider) was dispatched. A dropped terminal
        // would leave the session wedged in Streaming with no dispatch.
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "StreamCompleted(ToolUse) must survive a >64 token burst on an unbounded mailbox"
        );
        drop(actor_ref);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_transitions_session_to_sending() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![],
        };
        actor.on_tool_batch_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(matches!(session.phase(), PhaseKind::Streaming));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_batch_completed_buffers_when_session_is_streaming() {
        // Given a session in Streaming phase (StreamCompleted(ToolUse) not yet processed).
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "read".to_owned(),
                content: "file".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then no continuation is dispatched yet (buffered, not dropped).
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "ToolBatchCompleted during Streaming must not dispatch a continuation"
        );
        // And the results are buffered pending the matching StreamCompleted(ToolUse).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.core.ephemeral.pending_tool_batch.is_some(),
            "results should be buffered while still Streaming"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_tool_use_drains_buffered_tool_batch() {
        // Given a Streaming session with a buffered ToolBatchCompleted.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.core.ephemeral.pending_tool_batch = Some(vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "read".to_owned(),
                content: "file".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }]);
            state.session.active_session_id().clone()
        };

        // When StreamCompleted(ToolUse) arrives, transitioning Streaming → Sending.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            tool_calls: Some(vec![]),
            assistant_content: None,
            thinking_content: None,
            model_used: None,
            dispatched_at: jiff::Timestamp::now(),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
        };
        actor.on_stream_completed(&event).await;

        // Then the buffered batch is drained and the continuation dispatched.
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "drained buffered ToolBatchCompleted should dispatch a continuation"
        );
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.core.ephemeral.pending_tool_batch.is_none(),
            "buffer should be drained after StreamCompleted(ToolUse)"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_counts_tool_call_arguments() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_token_record(TokenRecord {
                model_used: None,
                timestamp: jiff::Timestamp::now(),
                tokens_sent: 100,
                tokens_received: 0,
                cost: None,
                prompt_tokens: None,
                cached_tokens: None,
            });
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("checking".to_owned()),
            tool_calls: Some(vec![ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls -la /very/long/path"}"#.to_owned(),
            }]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.on_stream_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert!(
            ledger[0].tokens_received > 2,
            "expected tokens_received > 2 (text only), got {}",
            ledger[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_execution_completed_emits_history_appended() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "bash",
                r#"{\"command\":\"ls\"}"#,
            ));
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = crate::feat::tools_actor::protocol::event::ToolExecutionCompleted {
            session_id: session_id.clone(),
            result: crate::feat::tools_actor::tool_types::ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
        };
        actor.on_tool_execution_completed(&event).await;

        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after tool execution completed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_execution_completed_dropped_when_not_sending() {
        // Given a session driven to Idle via the cancel path.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "bash",
                r#"{\"command\":\"ls\"}"#,
            ));
            session.begin_sending();
            session.cancel_stream_and_drain();
            state.session.active_session_id().clone()
        };

        // When a stale ToolExecutionCompleted arrives post-cancel.
        let event = crate::feat::tools_actor::protocol::event::ToolExecutionCompleted {
            session_id: session_id.clone(),
            result: crate::feat::tools_actor::tool_types::ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            },
        };
        actor.on_tool_execution_completed(&event).await;

        // Then no finalized ToolResult entry is added to history.
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session");
            let tr = session
                .history()
                .iter()
                .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id == "tc-1"));
            assert!(
                tr.is_none(),
                "expected no finalized ToolResult entry for tc-1 after drop"
            );
        }
        // And no HistoryAppended is emitted.
        assert!(
            !audit.contains_name("HistoryAppended"),
            "expected no HistoryAppended for dropped stale tool result"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_dropped_when_not_sending() {
        // Given a session driven to Idle via the cancel path.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "bash",
                r#"{\"command\":\"ls\"}"#,
            ));
            session.begin_sending();
            session.cancel_stream_and_drain();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };

        // When a stale ToolBatchCompleted arrives post-cancel.
        actor.on_tool_batch_completed(&event).await;

        // Then the loop is not restarted: phase stays Idle.
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            assert!(
                matches!(session.phase(), PhaseKind::Idle),
                "expected Idle after cancel, got {:?}",
                session.phase()
            );
        }

        // And no continuation send is emitted.
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "expected no SendToLlmProvider for dropped stale batch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_skips_send_when_tool_loop_disabled() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            session.set_tool_loop_disabled();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseKind::Idle),
            "expected Idle after tool_loop_disabled, got {:?}",
            session.phase()
        );

        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "expected no SendToLlmProvider when tool_loop_disabled"
        );

        drop(state);
        let mut state = actor.state.write_test_no_cap();
        let session = state.session_mut_or_create(&session_id);
        assert!(
            !session.take_tool_loop_disabled(),
            "tool_loop_disabled should be cleared after on_tool_batch_completed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_unaffected_without_tool_loop_disabled() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            session.begin_streaming();
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider for normal session without tool_loop_disabled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_use_started_creates_tool_call_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        actor.on_tool_use_started(&ToolUseStarted {
            session_id: session_id.clone(),
            index: 0,
            id: "tc-1".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-1"));
        assert!(tc.is_some(), "expected ToolCall entry with id tc-1");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_call_entry_gets_dispatched_at_from_tool_use_started() {
        // Given a session in streaming state.
        let actor = test_actor().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        let dispatched = jiff::Timestamp::now();

        // When a tool use starts with a specific dispatched_at.
        actor.on_tool_use_started(&ToolUseStarted {
            session_id: session_id.clone(),
            index: 0,
            id: "tc-dispatch".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: dispatched,
        });

        // Then the ToolCall entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-dispatch"))
            .expect("tool call entry");
        match &tc.timing {
            crate::protocol::EntryTiming::Streamed { dispatched_at, .. } => {
                assert_eq!(dispatched_at, &dispatched);
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_call_received_finalizes_arguments() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.begin_tool_call(0, "tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        actor.on_tool_call_received(&ToolCallReceived {
            session_id: session_id.clone(),
            tool_call: ToolCall {
                id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls"}
"#
                .to_owned(),
            },
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-1"))
            .expect("tool call entry");
        if let ChatEntryKind::ToolCall { arguments, .. } = &tc.kind {
            assert!(
                arguments.contains("ls"),
                "expected arguments to contain 'ls', got: {arguments}"
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_call_streaming_appends_delta() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.begin_tool_call(0, "tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        actor.on_tool_call_streaming(&ToolCallStreaming {
            session_id: session_id.clone(),
            index: 0,
            partial_json: "{\"co".to_owned(),
        });
        actor.on_tool_call_streaming(&ToolCallStreaming {
            session_id: session_id.clone(),
            index: 0,
            partial_json: "mmand\":\"ls\"}".to_owned(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc-1"))
            .expect("tool call entry");
        if let ChatEntryKind::ToolCall { arguments, .. } = &tc.kind {
            assert_eq!(arguments, "{\"command\":\"ls\"}");
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_execution_started_creates_pending_result() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_sending();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        actor.on_tool_execution_started(&ToolExecutionStarted {
            session_id: session_id.clone(),
            tool_call_id: "tc-1".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tr = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id == "tc-1"));
        assert!(tr.is_some(), "expected ToolResult entry with id tc-1");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_execution_output_appends_to_pending_result() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.begin_sending();
            session.begin_streaming();
            session.begin_tool_result("tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        actor.on_tool_execution_output(&ToolExecutionOutput {
            session_id: session_id.clone(),
            tool_call_id: "tc-1".to_owned(),
            output: "file1.txt\n".to_owned(),
            kind: ToolOutputKind::default(),
        });
        actor.on_tool_execution_output(&ToolExecutionOutput {
            session_id: session_id.clone(),
            tool_call_id: "tc-1".to_owned(),
            output: "file2.txt\n".to_owned(),
            kind: ToolOutputKind::default(),
        });

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tr = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id == "tc-1"))
            .expect("tool result");
        if let ChatEntryKind::ToolResult { content, .. } = &tr.kind {
            assert_eq!(content, "file1.txt\nfile2.txt\n");
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_applies_pending_mutations() {
        let (actor, audit) = test_actor_recording().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            let entry = ChatEntry::assistant("checking");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            session.begin_streaming();
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            session.queue_mutations(vec![
                crate::feat::session::history_mutation::HistoryMutation::SetContextOverride {
                    entry_id: entry_id.clone(),
                    value: crate::protocol::ContextOverride::ForcedExclude,
                    source: ChangeSource::Internal {
                        label: "test".into(),
                    },
                },
            ]);
            (entry_id, state.session.active_session_id().clone())
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("assistant entry exists");
        assert_eq!(
            assistant.context_override(),
            crate::protocol::ContextOverride::ForcedExclude,
            "expected mutation to be applied at tool batch completion"
        );

        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider after mutation application"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_empty_mutation_queue_is_noop() {
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            session.begin_streaming();
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider with empty mutation queue"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_drained_steering_entry_lands_after_tool_results() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write_test_no_cap();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_result(
                "tc-1",
                "bash",
                "file1.txt",
                ToolResultStatus::Success,
            ));
            session.push_entry(ChatEntry::tool_result(
                "tc-1",
                "bash",
                "file2.txt",
                ToolResultStatus::Success,
            ));
            session
                .steering_buffer_mut()
                .push_fragment("stay at the foo part");
            session.finish_streaming(true, jiff::Timestamp::now());
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }],
        };
        actor.on_tool_batch_completed(&event).await;

        let state = actor.state.read();
        let session = state.session.active_session();
        let history = session.history();
        let tool_result_indices: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.kind, ChatEntryKind::ToolResult { .. }))
            .map(|(i, _)| i)
            .collect();
        let steer_index = history
            .iter()
            .enumerate()
            .find(|(_, e)| matches!(&e.kind, ChatEntryKind::User { expanded, .. } if expanded == "stay at the foo part"))
            .map(|(i, _)| i);
        assert!(
            !tool_result_indices.is_empty(),
            "expected tool_result entries in history"
        );
        let steer_idx = steer_index.expect("drained steering entry must appear in history");
        for &tr_idx in &tool_result_indices {
            assert!(
                steer_idx > tr_idx,
                "steering entry at {steer_idx} must come after tool_result at {tr_idx}"
            );
        }
    }
}
