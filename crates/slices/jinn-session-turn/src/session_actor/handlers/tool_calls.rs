//! Tool call state tracking handlers - manage tool call lifecycle during streaming.
//!
//! Handles the full tool call lifecycle: creation via streaming, argument assembly,
//! execution tracking, result collection, and batch completion routing.

use jinn_context_assembly::inputs::build_assembly_inputs;
use jinn_context_assembly::inputs_snapshot::assemble_via_service;
use jinn_context_assembly_msg::ContextOverrideChanged;
use jinn_core_types::PinPosition;
use jinn_core_types::model_selection::ModelSelection;
use jinn_inference_msg::SendToLlmProvider;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_msg::PhaseKind;
use jinn_token_count_msg::TokenRecord;
use jinn_tools_msg::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted,
};

use super::super::SessionPersistenceActor;

impl SessionPersistenceActor {
    /// Begins tracking a streaming tool call.
    pub(in crate::session_actor) fn on_tool_use_started(&self, event: &ToolUseStarted) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.begin_tool_call(event.index, &event.id, &event.name, event.dispatched_at);
        });
    }
    /// Finalizes the tool call entry with complete arguments.
    ///
    /// The placeholder entry was created by `on_tool_use_started`. This updates
    /// it in place with the full arguments string, avoiding a duplicate entry.
    pub(in crate::session_actor) fn on_tool_call_received(&self, event: &ToolCallReceived) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.finalize_tool_call(
                &event.tool_call.id,
                &event.tool_call.name,
                &event.tool_call.arguments,
            );
        });
    }

    /// Appends a partial JSON delta to a streaming tool call.
    pub(in crate::session_actor) fn on_tool_call_streaming(&self, event: &ToolCallStreaming) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            if let Err(e) = session.append_tool_call_delta(event.index, &event.partial_json) {
                // A refused delta is never silent: the index and the phase the
                // session was in at the time are what a reader needs to work out
                // whether the call was never registered or its registration was
                // lost to a phase transition.
                tracing::warn!(
                    err = ?e,
                    index = event.index,
                    current_phase = ?session.phase(),
                    "refused tool call delta - no live registration for this index"
                );
            }
        });
    }
    /// Pushes a tool result entry into the session history.
    pub(in crate::session_actor) async fn on_tool_execution_completed(
        &self,
        event: &ToolExecutionCompleted,
    ) {
        {
            let should_continue = self.state.with_session(|view| -> bool {
                let session = view.session.map().get_or_create(&event.session_id);
                // Drop stale results that arrive after a cancel. Legitimate tool
                // execution only ever runs in `Sending`; a result landing in any
                // other phase (e.g. `Idle` after cancel) is a straggler whose
                // background task has not yet been aborted.
                if !matches!(session.phase(), PhaseKind::Sending) {
                    tracing::debug!(
                        current_phase = ?session.phase(),
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
    pub(in crate::session_actor) fn on_tool_execution_started(&self, event: &ToolExecutionStarted) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.begin_tool_result(&event.tool_call_id, &event.name, event.dispatched_at);
        });
    }
    /// Appends incremental output to a pending ToolResult entry.
    pub(in crate::session_actor) fn on_tool_execution_output(&self, event: &ToolExecutionOutput) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.append_tool_result_output(&event.tool_call_id, &event.output, event.kind);
        });
    }
    /// Drains pending history mutations and steering buffer entries, emitting
    /// ContextOverrideChanged events for any modified entries.
    async fn apply_pending_mutations_and_steering(&self, session_id: &jinn_core_types::SessionId) {
        // Drain and apply pending history mutations, then normalize loop
        // layout so committed loops never contain interstitials before
        // assembly (the read-side converter stays simple).
        let changed = {
            self.state.with_session(|view| {
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
            self.state.with_session(|view| {
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

    /// Assembles the continuation prompt, asks the phase actor to admit
    /// the continuation dispatch, and emits the `SendToLlmProvider`.
    ///
    /// The admission ask replaces the old synchronous `begin_streaming`:
    /// a continuation whose generation was cancelled is refused here and
    /// publishes nothing, instead of being published and torn down
    /// downstream by the inference actor's tombstone.
    async fn assemble_and_send_continuation(&self, session_id: &jinn_core_types::SessionId) {
        let stamp = jiff::Timestamp::now();
        let decision = crate::phase_actor::admit_stream(
            &self.services,
            session_id,
            jinn_session_msg::phase_command::DispatchKind::ToolContinuation,
            stamp,
        )
        .await;
        if !decision.admitted {
            tracing::info!(
                session_id = %session_id,
                "tool continuation refused: its generation was cancelled"
            );
            return;
        }

        let assembled = {
            let inputs = {
                let guard = self.state.read();
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

        // Resolve model under write lock (round-robin mutates index) and
        // push the token record against the admitted stamp. The phase
        // transition already happened — the admission above IS the write.
        let registry = self.services.provider_registry.clone();
        let (provider_id, model_used, reasoning_effort, endpoint_tag) = {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(session_id);

                let reasoning_effort = {
                    let profile = session.profile();
                    jinn_kernel::resolve_effort(profile.reasoning_effort)
                };
                let (provider_id, model_used, endpoint_tag) = {
                    let is_single = matches!(session.profile().model, ModelSelection::Single(_));
                    let model = &mut session.profile_mut().model;
                    let (provider_id, model_used) = if model.is_no_provider() {
                        (None, None)
                    } else {
                        let resolved = model.resolve_model();
                        (Some(resolved.clone()), Some(resolved))
                    };
                    // The routing endpoint is keyed by model, so it is
                    // resolved from the member `resolve_model` just produced
                    // — but only for a `Single` selection. An alloy rotates,
                    // and a row keyed to one of its members would pin
                    // whichever member this turn landed on.
                    let endpoint_tag = if is_single {
                        model_used
                            .as_deref()
                            .and_then(|resolved| registry.pinned_endpoint_tag(resolved))
                    } else {
                        None
                    };
                    (provider_id, model_used, endpoint_tag)
                };

                session.push_token_record(TokenRecord {
                    model_used: model_used.clone(),
                    timestamp: stamp,
                    tokens_sent: assembled.estimated_tokens(),
                    tokens_received: 0,
                    cost: None,
                    prompt_tokens: None,
                    cached_tokens: None,
                });

                (provider_id, model_used, reasoning_effort, endpoint_tag)
            })
        };

        let estimated_tokens = assembled.estimated_tokens();

        tracing::info!(
            session_id = %session_id,
            model = ?model_used,
            "emitting SendToLlmProvider"
        );
        self.publish(SendToLlmProvider {
            origin: jinn_inference_msg::StreamOrigin::ToolContinuation,
            model_used,
            reasoning_effort,
            endpoint_tag,
            session_id: session_id.clone(),
            messages: assembled.messages,
            system_prompt: assembled.system_prompt,
            provider_id,
            estimated_tokens,
            tool_definitions: assembled.tool_definitions,
            dispatched_at: stamp,
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
    pub(in crate::session_actor) async fn on_tool_batch_completed(
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
            let should_continue = self.state.with_session(|view| -> bool {
                let session = view.session.map().get_or_create(&event.session_id);
                match session.phase() {
                    PhaseKind::Sending => { /* normal path — proceed below */ }
                    PhaseKind::Streaming => {
                        tracing::info!(
                            session_id = ?event.session_id,
                            result_count = event.results.len(),
                            "buffering early ToolBatchCompleted: StreamCompleted(ToolUse) still in flight"
                        );
                        session.buffer_tool_results(event.results.clone());
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
    pub(in crate::session_actor) async fn continue_tool_loop(
        &self,
        session_id: &jinn_core_types::SessionId,
    ) {
        // If tool loop is disabled, end the turn instead of continuing.
        // This is used by judge verdict tools to prevent infinite tool-call loops.
        // Consume-on-read: the flag is per-turn and self-clearing, the same
        // take semantics the machine edge used to apply. A flag that survived
        // its turn would end every later turn's continuation too.
        let tool_loop_disabled = self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(session_id)
                .take_tool_loop_disabled()
        });

        if tool_loop_disabled {
            // The tool loop stops here: the turn ends through the phase
            // actor (`Sending → Idle`), which publishes the transition.
            let decision = crate::phase_actor::end_sending(&self.services, session_id).await;
            if !decision.admitted {
                tracing::warn!(
                    session_id = %session_id,
                    "tool-loop stop refused: the turn had already ended"
                );
            }
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
    use super::super::super::helpers::{ensure_context_assembly, test_actor, test_actor_recording};
    use jinn_core_types::ToolResultStatus;
    use jinn_core_types::tool_types::{ToolCall, ToolResult};
    use jinn_inference_msg::{StreamCompleted, StreamCompletedReason};
    use jinn_kernel::protocol::{ChangeSource, ChatEntry, ChatEntryKind};
    use jinn_session_msg::PhaseKind;
    use jinn_token_count_msg::TokenRecord;
    use jinn_tools_msg::{
        ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionOutput,
        ToolExecutionStarted, ToolOutputKind, ToolUseStarted,
    };

    use super::SessionPersistenceActor;

    /// The actor half of a bus-harness test, driven through the bus.
    struct BusActor {
        harness: jinn_testutil::bus_harness::TestHarness,
    }

    impl BusActor {
        /// Spawns the session actor onto the bus, wired to the harness.
        async fn spawn(
            harness: jinn_testutil::bus_harness::TestHarness,
            state: jinn_kernel::common::state::State,
        ) -> Self {
            use crate::session_actor::SessionPersistenceActorDeps;
            use jinn_kernel::common::bus::HarnessServices;
            use jinn_llm_support::token_estimator::TiktokenCounter;

            let deps = {
                let deps = harness.actor_deps().await;
                ensure_context_assembly(&deps.services.trouper_system);
                // The phase actor owns the phase; admission asks fail over
                // to refused without it, and every mid-turn fixture needs a
                // real mint.
                let _ = crate::phase_actor::ensure_spawned(
                    harness.system(),
                    state.clone(),
                    harness.bus(),
                );
                deps
            };
            SessionPersistenceActor::spawn(
                harness.system(),
                SessionPersistenceActorDeps {
                    deps,
                    state: state.clone(),
                    counter: TiktokenCounter::o200k_base(),
                    token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
                    image_converter:
                        jinn_llm_support::image_convert::ImageConverterService::unavailable(),
                },
            );
            Self { harness }
        }

        /// Publishes a message onto the bus.
        async fn publish<M>(&self, message: M)
        where
            M: jinn_kernel::common::bus::BusMessage
                + trouper::schema::Schema
                + serde::Serialize
                + Clone
                + Send
                + Sync
                + trouper::envelope::PayloadValue,
        {
            self.harness.publish(message).await;
        }
    }

    /// Spawns a session actor onto a Guaranteed bus in the buffered-batch
    /// pre-cancel state: the tool batch lands while the session is still
    /// Streaming (the tool-call-watchdog race shape), so the aborted stream
    /// task's StreamCompleted(ToolUse) dispatches one continuation first.
    async fn spawn_buffered_batch_actor() -> (
        BusActor,
        Vec<jinn_inference_msg::SendToLlmProvider>,
        jinn_kernel::common::state::State,
        jinn_core_types::SessionId,
    ) {
        use jinn_inference_msg::SendToLlmProvider;
        use jinn_kernel::common::app_state::AppState;
        use jinn_kernel::common::state::State;
        use jinn_testutil::bus_harness::{TestHarness, await_recorded};
        use std::time::Duration;

        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        {
            let mut s = state.write();
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
        let actor = BusActor::spawn(harness, state.clone()).await;
        // Mint the turn the way the dispatch path would: the phase actor
        // stamps the session, so the ToolUse completion below resolves.
        {
            use jinn_kernel::common::bus::HarnessServices;
            let services = actor.harness.services().await;
            let minted = jinn_kernel::common::phase_command::apply_phase(
                &services,
                jinn_session_msg::PhaseCommand::BeginStream {
                    session_id: session_id.clone(),
                    kind: jinn_session_msg::phase_command::DispatchKind::FreshTurn,
                    dispatched_at: jiff::Timestamp::now(),
                },
            )
            .await
            .expect("phase actor reachable");
            assert!(minted.admitted, "fixture mint must be admitted");
        }
        actor
            .publish(ToolBatchCompleted {
                session_id: session_id.clone(),
                results: vec![tool_result("tc-1", "sample_tool", "boom", false)],
            })
            .await;
        actor
            .publish(stream_completed(
                &session_id,
                StreamCompletedReason::ToolUse,
            ))
            .await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        (actor, sent, state, session_id)
    }

    /// Polls the session's phase until it reaches `Idle` or the deadline passes.
    async fn await_phase_idle(
        state: &jinn_kernel::common::state::State,
        session_id: &jinn_core_types::SessionId,
    ) {
        use std::time::Duration;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            {
                let s = state.read();
                if s.session.get_unchecked(session_id).phase() == PhaseKind::Idle {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "session did not settle to Idle"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Builds the `StreamCompleted` event a provider emits when a turn ends.
    fn stream_completed(
        session_id: &jinn_core_types::SessionId,
        reason: StreamCompletedReason,
    ) -> StreamCompleted {
        StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        }
    }

    /// Builds the terminal `StreamCompleted(ToolUse)` a provider emits after
    /// requesting tool calls, stamped with the dispatch time of its turn.
    fn terminal_tool_use_event(
        session_id: &jinn_core_types::SessionId,
        dispatched_at: jiff::Timestamp,
    ) -> StreamCompleted {
        StreamCompleted {
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
        }
    }

    /// Builds the tool result a tool run reports back for one call.
    fn tool_result(tool_call_id: &str, name: &str, content: &str, success: bool) -> ToolResult {
        ToolResult {
            tool_call_id: tool_call_id.to_owned(),
            name: name.to_owned(),
            content: content.to_owned(),
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
        }
    }

    /// Mints a live turn for the named session through the phase actor.
    /// The admission ask is the only writer of the phase now, so every
    /// "session mid-turn" fixture starts here.
    async fn mint_turn(actor: &SessionPersistenceActor, session_id: &jinn_core_types::SessionId) {
        use jinn_kernel::common::phase_command::apply_phase;
        use jinn_session_msg::phase_command::{DispatchKind, PhaseCommand};
        let minted = apply_phase(
            &actor.services,
            PhaseCommand::BeginStream {
                session_id: session_id.clone(),
                kind: DispatchKind::FreshTurn,
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await
        .expect("phase actor reachable");
        assert!(
            minted.admitted,
            "a fresh turn's mint must be admitted: {minted:?}"
        );
    }

    /// Applies the tool-use edge for a live generation, leaving the session
    /// in the mid-tool-loop `Sending` phase — the shape a
    /// `StreamCompleted(ToolUse)` leaves.
    async fn apply_tool_use_edge(
        actor: &SessionPersistenceActor,
        session_id: &jinn_core_types::SessionId,
    ) {
        use jinn_kernel::common::phase_command::apply_phase;
        use jinn_session_msg::phase_command::PhaseCommand;
        let ended = apply_phase(
            &actor.services,
            PhaseCommand::StreamEndedToolUse {
                session_id: session_id.clone(),
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await
        .expect("phase actor reachable");
        assert!(
            ended.admitted,
            "a live generation's tool-use edge must be admitted: {ended:?}"
        );
    }

    /// Puts the active session into the mid-tool-loop sending phase,
    /// returning its id.
    async fn begin_sending_session(actor: &SessionPersistenceActor) -> jinn_core_types::SessionId {
        let session_id = actor.state.read().session.active_session_id().clone();
        mint_turn(actor, &session_id).await;
        apply_tool_use_edge(actor, &session_id).await;
        session_id
    }

    /// Puts the active session into sending phase with a tool call and one
    /// streamed assistant entry around it, returning its id.
    async fn begin_sending_session_with_tool_call(
        actor: &SessionPersistenceActor,
    ) -> jinn_core_types::SessionId {
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
        }
        let session_id = actor.state.read().session.active_session_id().clone();
        mint_turn(actor, &session_id).await;
        apply_tool_use_edge(actor, &session_id).await;
        session_id
    }

    /// Puts the active session into sending phase holding a complete tool loop
    /// and a buffered steering fragment, returning its id.
    async fn seed_sending_session_with_steering_fragment(
        actor: &SessionPersistenceActor,
    ) -> jinn_core_types::SessionId {
        {
            let mut state = actor.state.write();
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
        }
        let session_id = actor.state.read().session.active_session_id().clone();
        mint_turn(actor, &session_id).await;
        apply_tool_use_edge(actor, &session_id).await;
        session_id
    }

    /// Puts the active session into sending phase with a streamed assistant
    /// entry targeted by a queued context-override mutation, returning the
    /// targeted entry id and the session id.
    async fn seed_sending_session_with_pending_mutation(
        actor: &SessionPersistenceActor,
    ) -> (jinn_core_types::ChatEntryId, jinn_core_types::SessionId) {
        let entry_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            let entry = ChatEntry::assistant("checking");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
            entry_id
        };
        let session_id = actor.state.read().session.active_session_id().clone();
        mint_turn(actor, &session_id).await;
        apply_tool_use_edge(actor, &session_id).await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.queue_mutations(vec![jinn_core_types::HistoryMutation::SetContextOverride {
                entry_id: entry_id.clone(),
                value: jinn_core_types::ContextOverride::ForcedExclude,
                source: ChangeSource::Internal {
                    label: "test".into(),
                },
            }]);
        }
        (entry_id, session_id)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_emits_send_to_llm_provider() {
        // Given a session in sending phase holding a completed tool loop.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor).await;

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id,
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then a SendToLlmProvider command is emitted.
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
        use jinn_inference_msg::SendToLlmProvider;
        use jinn_kernel::common::app_state::AppState;
        use jinn_kernel::common::state::State;
        use jinn_testutil::bus_harness::{TestHarness, await_recorded};
        use std::time::Duration;

        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        {
            let mut s = state.write();
            let session = s.active_session_mut();
            session.push_entry(ChatEntry::user("list files"));
            session.push_entry(ChatEntry::assistant("checking"));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::assistant("here are the files"));
        }
        let session_id = state.read().session.active_session_id().clone();
        let actor = BusActor::spawn(harness, state).await;
        // The continuation's admission needs a live generation: mint the turn
        // and apply the tool-use edge, the shape a StreamCompleted(ToolUse)
        // leaves before the next batch.
        {
            use jinn_kernel::common::bus::HarnessServices;
            let services = actor.harness.services().await;
            let mint = |kind| jinn_session_msg::PhaseCommand::BeginStream {
                session_id: session_id.clone(),
                kind,
                dispatched_at: jiff::Timestamp::now(),
            };
            let minted = jinn_kernel::common::phase_command::apply_phase(
                &services,
                mint(jinn_session_msg::phase_command::DispatchKind::FreshTurn),
            )
            .await
            .expect("phase actor reachable");
            assert!(minted.admitted, "fixture mint must be admitted");
            let ended = jinn_kernel::common::phase_command::apply_phase(
                &services,
                jinn_session_msg::PhaseCommand::StreamEndedToolUse {
                    session_id: session_id.clone(),
                    dispatched_at: jiff::Timestamp::now(),
                },
            )
            .await
            .expect("phase actor reachable");
            assert!(ended.admitted, "fixture tool-use edge must be admitted");
        }

        // When ToolBatchCompleted is published to the bus.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.publish(event).await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;

        // Then the actor published SendToLlmProvider via the MsgHandler.
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "expected SendToLlmProvider to reach the bus via the MsgHandler"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn canceled_after_tool_use_does_not_redispatch() {
        // Given a spawned session actor in the buffered-batch pre-cancel state.
        use jinn_inference_msg::SendToLlmProvider;
        use jinn_testutil::bus_harness::await_recorded;
        use std::time::Duration;

        let (actor, sent, _state, session_id) = spawn_buffered_batch_actor().await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "precondition: the tool loop dispatched its continuation"
        );
        let recorder = actor.harness.spawn_recorder::<SendToLlmProvider>().await;

        // When StreamCompleted(Canceled) arrives afterwards.
        actor
            .publish(stream_completed(
                &session_id,
                StreamCompletedReason::Canceled,
            ))
            .await;

        // Then no FURTHER SendToLlmProvider is dispatched — the Canceled
        // completion must not re-enter the tool loop. (The recorder drains on
        // read, so this observes only post-cancel traffic.)
        let extra = await_recorded::<SendToLlmProvider>(&recorder, 0, Duration::from_secs(1)).await;
        assert!(
            extra.iter().all(|m| m.session_id != session_id),
            "cancel after tool-use must not trigger a second dispatch, got {extra:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn canceled_after_tool_use_appends_single_cancelled_entry() {
        // Given a spawned session actor in the buffered-batch pre-cancel state.
        let (actor, _recorder, state, session_id) = spawn_buffered_batch_actor().await;

        // When StreamCompleted(Canceled) arrives afterwards. The cancel is
        // handled asynchronously on the actor mailbox, so the terminal phase
        // is polled rather than read straight away.
        actor
            .publish(stream_completed(
                &session_id,
                StreamCompletedReason::Canceled,
            ))
            .await;
        await_phase_idle(&state, &session_id).await;

        // Then the session settles in Idle with a single cancel entry appended.
        let s = state.read();
        let session = s.session.get_unchecked(&session_id);
        let cancelled_entries = session
            .history()
            .iter()
            .filter(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Error { .. }))
            .count();
        assert_eq!(
            cancelled_entries, 1,
            "exactly one 'Cancelled' error entry should be appended"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_survives_token_burst_on_unbounded_mailbox() {
        // Given a BestEffort bus and a session actor whose deep mailbox already
        // holds a buffered tool batch that raced ahead of the terminal
        // StreamCompleted(ToolUse). (Regression guard for the deep-mailbox fix:
        // production logs showed a >64-token burst at a `[DONE]` peak filling the
        // old bounded(64) mailbox, where BestEffort try_send silently dropped the
        // terminal event and wedged the session in Streaming forever.)
        use jinn_inference_msg::SendToLlmProvider;
        use jinn_inference_msg::StreamToken;
        use jinn_kernel::common::app_state::AppState;
        use jinn_kernel::common::state::State;
        use jinn_testutil::bus_harness::{TestHarness, await_recorded};
        use std::time::Duration;

        let harness = TestHarness::new_best_effort().await;
        let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
        let state = State::new(AppState::default());
        let session_id = state.read().session.active_session_id().clone();
        let dispatched_at = jiff::Timestamp::now();
        {
            let mut s = state.write();
            let session = s.active_session_mut();
            session.buffer_tool_results(vec![tool_result("tc-1", "bash", "ok", true)]);
        }
        let actor = BusActor::spawn(harness, state).await;
        // A live generation behind the buffered batch: the terminal
        // ToolUse resolves against the mint's stamp.
        {
            use jinn_kernel::common::bus::HarnessServices;
            let services = actor.harness.services().await;
            let minted = jinn_kernel::common::phase_command::apply_phase(
                &services,
                jinn_session_msg::PhaseCommand::BeginStream {
                    session_id: session_id.clone(),
                    kind: jinn_session_msg::phase_command::DispatchKind::FreshTurn,
                    dispatched_at,
                },
            )
            .await
            .expect("phase actor reachable");
            assert!(minted.admitted, "fixture mint must be admitted");
        }
        let terminal = terminal_tool_use_event(&session_id, dispatched_at);

        // When a >64-token burst is published, immediately followed by the
        // terminal StreamCompleted(ToolUse).
        for i in 0..200 {
            actor
                .publish(StreamToken {
                    session_id: session_id.clone(),
                    index: i,
                    token: "x".to_owned(),
                    is_thinking: false,
                    dispatched_at,
                })
                .await;
        }
        actor.publish(terminal).await;

        // Then the terminal was delivered: the buffered batch drained and a
        // continuation (SendToLlmProvider) was dispatched. A dropped terminal
        // would leave the session wedged in Streaming with no dispatch.
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "StreamCompleted(ToolUse) must survive a >64 token burst on the deep mailbox"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_transitions_session_to_sending() {
        // Given a session in sending phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When an empty tool batch completes.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the session is Streaming again, waiting for the response.
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
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![tool_result("tc-1", "read", "file", true)],
        };

        // When the tool batch completes.
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
            session.has_buffered_tool_results(),
            "results should be buffered while still Streaming"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_tool_use_drains_buffered_tool_batch() {
        // Given a streaming session with a live generation and a buffered
        // ToolBatchCompleted — the batch arrived while the stream was still
        // running, and the buffer survives outside the machine.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let session_id = actor.state.read().session.active_session_id().clone();
            mint_turn(&actor, &session_id).await;
            session_id
        };
        {
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session.buffer_tool_results(vec![ToolResult {
                tool_call_id: "tc-1".to_owned(),
                name: "read".to_owned(),
                content: "file".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
            }]);
        }

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
            !session.has_buffered_tool_results(),
            "buffer should be drained after StreamCompleted(ToolUse)"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_counts_tool_call_arguments() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
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

        // When the stream completes requesting a tool call with long arguments.
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

        // Then the tool call arguments are counted alongside the text.
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
        // Given a sending session holding an unanswered tool call.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
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

        // When the tool execution completes.
        let event = jinn_tools_msg::ToolExecutionCompleted {
            session_id,
            result: tool_result("tc-1", "bash", "file1.txt", true),
        };
        actor.on_tool_execution_completed(&event).await;

        // Then HistoryAppended is emitted.
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
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "bash",
                r#"{\"command\":\"ls\"}"#,
            ));
            session.begin_sending();
            session.finalize_entries_for_cancel(jiff::Timestamp::now());
            session.cancel_streaming_via_machine();
            session.drain_cancelled_work_to_input();
            state.session.active_session_id().clone()
        };

        // When a stale ToolExecutionCompleted arrives post-cancel.
        let event = jinn_tools_msg::ToolExecutionCompleted {
            session_id: session_id.clone(),
            result: jinn_core_types::tool_types::ToolResult {
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
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::tool_call(
                "tc-1",
                "bash",
                r#"{\"command\":\"ls\"}"#,
            ));
            session.begin_sending();
            session.finalize_entries_for_cancel(jiff::Timestamp::now());
            session.cancel_streaming_via_machine();
            session.drain_cancelled_work_to_input();
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
        // Given a mid-tool-loop session whose tool loop is disabled — the
        // shape a `StreamCompleted(ToolUse)` leaves before the next batch.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.set_tool_loop_disabled();
        }

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the turn ends in Idle without dispatching a continuation.
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
        let mut state = actor.state.write();
        let session = state.session_mut_or_create(&session_id);
        assert!(
            !session.take_tool_loop_disabled(),
            "tool_loop_disabled should be cleared after on_tool_batch_completed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_unaffected_without_tool_loop_disabled() {
        // Given a normal sending session holding a completed tool loop.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor).await;

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id,
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the continuation is dispatched as usual.
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider for normal session without tool_loop_disabled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_use_started_creates_tool_call_entry() {
        // Given a session in streaming phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When a tool use starts.
        actor.on_tool_use_started(&ToolUseStarted {
            session_id: session_id.clone(),
            index: 0,
            id: "tc-1".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // Then a ToolCall entry with that id is in the history.
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
            let mut state = actor.state.write();
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
            jinn_core_types::EntryTiming::Streamed { dispatched_at, .. } => {
                assert_eq!(dispatched_at, &dispatched);
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_call_received_finalizes_arguments() {
        // Given a streaming session with a tool call whose arguments are pending.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.begin_tool_call(0, "tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        // When the fully-formed tool call arrives.
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

        // Then the entry's arguments hold the finalized payload.
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
        // Given a streaming session with a tool call whose arguments are pending.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.begin_tool_call(0, "tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        // When two argument deltas stream in for that tool call.
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

        // Then both deltas concatenate into the entry's arguments.
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
    async fn on_tool_call_streaming_logs_index_and_phase_when_the_delta_is_refused() {
        use std::sync::{Arc, Mutex};

        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;

        /// A `MakeWriter` capturing formatted log output for assertions.
        #[derive(Clone, Default)]
        struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

        impl CapturingWriter {
            fn contents(&self) -> String {
                String::from_utf8(self.0.lock().expect("poisoned").clone())
                    .expect("captured output is utf-8")
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
            type Writer = CapturingSink;

            fn make_writer(&'a self) -> Self::Writer {
                CapturingSink(self.0.clone())
            }
        }

        struct CapturingSink(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for CapturingSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|err| {
                        std::io::Error::other(format!("capture buffer mutex poisoned: {err}"))
                    })?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        // Given a session with a turn in flight but no registration for index 7.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        let capture = CapturingWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(capture.clone())
                .with_ansi(false)
                .with_filter(tracing_subscriber::EnvFilter::new("warn")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        // When a delta arrives for an index that was never registered.
        actor.on_tool_call_streaming(&ToolCallStreaming {
            session_id,
            index: 7,
            partial_json: "{\"command\":\"ls\"}".to_owned(),
        });

        // Then the refusal is logged, naming both the index and the phase, so a
        // reader can tell an unregistered index from one lost to a transition.
        let contents = capture.contents();
        assert!(
            contents.contains("index=7"),
            "the refused delta should log its tool-call index; got: {contents}"
        );
        assert!(
            contents.contains("current_phase=Sending"),
            "the refused delta should log the session phase; got: {contents}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_execution_started_creates_pending_result() {
        // Given a session in streaming phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_sending();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When a tool execution starts.
        actor.on_tool_execution_started(&ToolExecutionStarted {
            session_id: session_id.clone(),
            tool_call_id: "tc-1".to_owned(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // Then a pending ToolResult entry with that id is in the history.
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
        // Given a streaming session with a pending tool result.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_sending();
            session.begin_streaming();
            session.begin_tool_result("tc-1", "bash", jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };

        // When two output chunks arrive for that tool result.
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

        // Then both chunks concatenate into the entry's content.
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
        // Given a sending session with one queued context-override mutation.
        let (actor, audit) = test_actor_recording().await;
        let (entry_id, session_id) = seed_sending_session_with_pending_mutation(&actor).await;

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id: session_id.clone(),
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the queued mutation is applied to the assistant entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("assistant entry exists");
        assert_eq!(
            assistant.context_override(),
            jinn_core_types::ContextOverride::ForcedExclude,
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
        // Given a sending session with nothing queued for mutation application.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor).await;

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id,
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the continuation is still dispatched.
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider with empty mutation queue"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_tool_batch_completed_drained_steering_entry_lands_after_tool_results() {
        // Given a sending session holding a complete tool loop and a buffered
        // steering fragment the user typed while the tools ran.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_sending_session_with_steering_fragment(&actor).await;

        // When the tool batch completes.
        let event = ToolBatchCompleted {
            session_id,
            results: vec![tool_result("tc-1", "bash", "file1.txt", true)],
        };
        actor.on_tool_batch_completed(&event).await;

        // Then the drained steering entry lands after every tool result.
        let state = actor.state.read();
        let history = state.session.active_session().history();
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
