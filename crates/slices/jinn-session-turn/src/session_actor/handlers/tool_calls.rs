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
use jinn_token_count_msg::TokenRecord;
use jinn_tools_msg::{
    ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted, ToolExecutionOutput,
    ToolExecutionStarted, ToolUseStarted,
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
    ///
    /// Admission is the ledger's: a completion is admitted when its turn
    /// names the live turn and the turn is not draining a cancel. No phase
    /// snapshot is consulted — `Sending`, `Streaming`, whichever phase the
    /// history happens to sit in, the check is identity, not timing. This
    /// is the bug-21 fix: a result landing while the provider stream is
    /// still open finalizes in place instead of being dropped.
    pub(in crate::session_actor) async fn on_tool_execution_completed(
        &self,
        event: &ToolExecutionCompleted,
    ) {
        // Admission is the ledger's token-equality check, via `call`: a
        // completion is admitted when its turn names the live turn and the
        // turn is not draining a cancel. No phase snapshot is consulted —
        // `Sending`, `Streaming`, whichever phase the history happens to
        // sit in, the check is identity, not timing. This is the bug-21
        // fix: a result landing while the provider stream is still open
        // finalizes in place instead of being dropped.
        let admitted = self
            .services
            .turn_ledger
            .call(
                &event.session_id,
                &event.turn,
                jinn_kernel::common::turn_ledger::Req::TurnScoped,
                || (),
            )
            .is_ok();
        if !admitted {
            tracing::debug!(
                session_id = ?event.session_id,
                turn = ?event.turn,
                tool_call_id = %event.result.tool_call_id,
                "dropping ToolExecutionCompleted: the turn ledger refused it"
            );
            return;
        }

        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            // A synthesized completion is the cancel drain's bookkeeping: it
            // resolves a call that never finished but must never overwrite a
            // real result that landed first, and it never pushes a detached
            // entry. `None` keeps the ordinary completion's behavior.
            let synthesized = event.synthesized.then_some(true);
            session.finalize_tool_result_with_attachments(
                &event.result.tool_call_id,
                &event.result.name,
                &event.result.content,
                event.result.success,
                event.result.full_content.clone(),
                event.result.truncation.clone(),
                event.result.pin_position.map(PinPosition::from),
                event.result.attachments.clone(),
                synthesized,
            );
        });

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
        // The continuation joins the live turn through the ledger, minting
        // its stream id. A turn the cancel drained (or that already
        // settled) refuses the join — nothing is sent for a dead turn.
        let Some(grant) = self.services.turn_ledger.mint(
            session_id,
            jinn_kernel::common::turn_ledger::MintKind::ToolContinuation,
        ) else {
            tracing::info!(
                session_id = %session_id,
                "tool continuation refused: no live turn behind it"
            );
            return;
        };

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
            turn: grant.turn,
            stream: grant.stream,
        })
        .await;
    }

    /// Every open call in the turn has closed — the turn's work set is
    /// empty. What happens next is the settlement's recorded intent: a
    /// tool-use stream retirement recorded `Continue` (dispatch the
    /// continuation), a turn with no more streams coming recorded `End`,
    /// a cancelled turn recorded `Cancel`.
    ///
    /// There is no batch message and no buffering: order independence is
    /// the work set's, not the bus's. Called by the stream-completion fold
    /// and the tool-completion path when `close_call` reports a settlement.
    ///
    /// `End` and `Cancel` settle here, not downstream: the stream-completion
    /// path cannot own their settle on this route — a cancel during the tool
    /// loop finds a turn whose stream has already retired, so its stamped
    /// completion is refused by the ledger (`Refused::StaleStream`) and the
    /// close this settlement rode in on is the turn's last fact. The ledger
    /// emits exactly one settlement per round, so whichever site observes
    /// it — this close path or the completion's ack — settles exactly once.
    pub(in crate::session_actor) async fn on_turn_work_settled(
        &self,
        session_id: &jinn_core_types::SessionId,
        next: jinn_kernel::common::turn_ledger::Next,
    ) {
        tracing::info!(
            session_id = ?session_id,
            next = ?next,
            "turn work settled"
        );
        match next {
            jinn_kernel::common::turn_ledger::Next::Continue => {
                self.continue_tool_loop(session_id).await;
            }
            jinn_kernel::common::turn_ledger::Next::End
            | jinn_kernel::common::turn_ledger::Next::Cancel => {
                self.settle_turn(session_id, next).await;
            }
        }
    }

    /// Continues the tool loop after a batch completes: applies pending
    /// mutations, checks `tool_loop_disabled`, and dispatches the next
    /// `SendToLlmProvider` (or finishes sending if the loop is disabled).
    ///
    /// Called from the work-settlement sites: the session actor's completion
    /// handler when its close emptied the work set, and the stream-completion
    /// fold when the retirement did.
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
            // The tool loop stops here: retire the live stream and end the
            // turn. The ledger settles the turn (work set is empty by
            // construction on this path); the machine edge below applies
            // the session's `Idle` view.
            let _ = self.services.turn_ledger.finish_streaming(session_id);
            self.state.with_session(|view| {
                view.session
                    .map()
                    .get_or_create(session_id)
                    .settle_to_idle();
            });
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
    use jinn_inference_msg::{CancelTurn, StreamCompleted, StreamCompletedReason};
    use jinn_kernel::protocol::{ChangeSource, ChatEntry, ChatEntryKind};
    use jinn_slices::PhaseView;
    use jinn_token_count_msg::TokenRecord;
    use jinn_tools_msg::{
        ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted, ToolExecutionOutput,
        ToolExecutionStarted, ToolOutputKind, ToolUseStarted,
    };

    use super::SessionPersistenceActor;

    /// The actor half of a bus-harness test, driven through the bus.
    /// Holds the one `Services` the actor was spawned with: `Services`
    /// mints a fresh ledger per construction, so a second grab would mint
    /// into a ledger nobody reads.
    struct BusActor {
        harness: jinn_testutil::bus_harness::TestHarness,
        services: jinn_kernel::common::services::Services,
    }

    impl BusActor {
        /// Spawns the session actor onto the bus, wired to the harness.
        fn spawn(
            harness: jinn_testutil::bus_harness::TestHarness,
            state: &jinn_kernel::common::state::State,
            services: jinn_kernel::common::services::Services,
        ) -> Self {
            use crate::session_actor::SessionPersistenceActorDeps;
            use jinn_llm_support::token_estimator::TiktokenCounter;

            ensure_context_assembly(&services.trouper_system);
            SessionPersistenceActor::spawn(
                harness.system(),
                SessionPersistenceActorDeps {
                    deps: jinn_kernel::common::actor_deps::ActorDeps {
                        services: services.clone(),
                    },
                    state: state.clone(),
                    counter: TiktokenCounter::o200k_base(),
                    token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
                    image_converter:
                        jinn_llm_support::image_convert::ImageConverterService::unavailable(),
                },
            );
            Self { harness, services }
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
        jinn_core_types::turn_identity::TurnToken,
        jinn_core_types::turn_identity::StreamId,
    ) {
        use jinn_inference_msg::SendToLlmProvider;
        use jinn_kernel::common::app_state::AppState;
        use jinn_kernel::common::bus::HarnessServices;
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
        let services = harness.services().await;
        let actor = BusActor::spawn(harness, &state, services.clone());
        // Mint the turn the way the dispatch path would: the ledger mints
        // the turn and stream, so the completion and ToolUse below resolve.
        let (turn, stream) = {
            let services = services.clone();
            let grant = services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("fixture mint must be admitted");
            // The batch's call is registered, as the orchestrator would.
            services
                .turn_ledger
                .register_and_dispatch(
                    &session_id,
                    &grant.turn,
                    vec![jinn_core_types::ToolCallId::from("tc-1")],
                    || (),
                )
                .expect("registration on a live turn");
            (grant.turn, grant.stream)
        };
        // The completion lands BEFORE the stream retirement — the bug-21
        // order the work set makes safe.
        actor
            .publish(ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "sample_tool", "boom", false),
                turn,
                synthesized: false,
            })
            .await;
        actor
            .publish({
                let mut event =
                    stream_completed(&services, &session_id, StreamCompletedReason::ToolUse);
                event.turn = turn;
                event.stream = stream;
                event
            })
            .await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        (actor, sent, state, session_id, turn, stream)
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
                if s.session.get_unchecked(session_id).phase() == PhaseView::Idle {
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

    /// Builds the `StreamCompleted` event a provider emits when a turn ends,
    /// stamped with the live turn's identity — the mint the completion
    /// resolves against.
    fn stream_completed(
        services: &jinn_kernel::common::services::Services,
        session_id: &jinn_core_types::SessionId,
        reason: StreamCompletedReason,
    ) -> StreamCompleted {
        let live = services
            .turn_ledger
            .live(session_id)
            .expect("fixture completion needs a live turn to resolve against");
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
            turn: live.turn,
            stream: live.stream.unwrap_or_default(),
            retryable: false,
        }
    }

    /// Builds the terminal `StreamCompleted(ToolUse)` a provider emits after
    /// requesting tool calls, stamped with the dispatch time of its turn and
    /// the identity the dispatch rode in on.
    fn terminal_tool_use_event(
        session_id: &jinn_core_types::SessionId,
        dispatched_at: jiff::Timestamp,
        turn: jinn_core_types::turn_identity::TurnToken,
        stream: jinn_core_types::turn_identity::StreamId,
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
            turn,
            stream,
            retryable: false,
        }
    }

    /// Builds the tool result a tool run reports back for one call.
    fn tool_result(tool_call_id: &str, name: &str, content: &str, success: bool) -> ToolResult {
        ToolResult {
            tool_call_id: tool_call_id.to_owned().into(),
            name: name.to_owned(),
            content: content.to_owned(),
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    }

    /// Mints a live turn for the named session through the turn ledger.
    /// The ledger is the only admission, so every "session mid-turn"
    /// fixture starts here.
    fn mint_turn(actor: &SessionPersistenceActor, session_id: &jinn_core_types::SessionId) {
        let minted = actor.services.turn_ledger.mint(
            session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        );
        assert!(minted.is_some(), "a fresh turn's mint must be admitted");
    }

    /// Applies the tool-use edge for a live turn, leaving the session in the
    /// mid-tool-loop shape a `StreamCompleted(ToolUse)` leaves: the stream
    /// retired with `Continue`, the session machine advanced to `Sending`.
    fn apply_tool_use_edge(
        actor: &SessionPersistenceActor,
        session_id: &jinn_core_types::SessionId,
    ) {
        let token = actor
            .services
            .turn_ledger
            .live(session_id)
            .expect("live turn")
            .turn;
        let ack = actor.services.turn_ledger.ack_stream_ended(
            session_id,
            &token,
            &actor
                .services
                .turn_ledger
                .live(session_id)
                .expect("live turn")
                .stream
                .expect("live stream"),
            jinn_kernel::common::turn_ledger::Next::Continue,
        );
        assert!(
            ack.is_ok(),
            "a live turn's tool-use edge must be admitted: {ack:?}"
        );
        actor.state.with_session(|view| {
            view.session.map().get_or_create(session_id).begin_sending();
        });
    }

    /// Puts the active session into the mid-tool-loop sending phase,
    /// returning its id.
    fn begin_sending_session(actor: &SessionPersistenceActor) -> jinn_core_types::SessionId {
        let session_id = actor.state.read().session.active_session_id().clone();
        mint_turn(actor, &session_id);
        apply_tool_use_edge(actor, &session_id);
        session_id
    }

    /// Puts the active session into sending phase with a tool call and one
    /// streamed assistant entry around it, returning its id.
    fn begin_sending_session_with_tool_call(
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
        mint_turn(actor, &session_id);
        // The orchestrator registers every call in the dispatch's ledger
        // call; the fixture registers tc-1 the same way so its completion
        // closes a call that is actually open.
        let token = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn")
            .turn;
        actor
            .services
            .turn_ledger
            .register_and_dispatch(
                &session_id,
                &token,
                vec![jinn_core_types::ToolCallId::from("tc-1")],
                || (),
            )
            .expect("registration on a live turn");
        apply_tool_use_edge(actor, &session_id);
        session_id
    }

    /// Puts the active session into sending phase holding a complete tool loop
    /// and a buffered steering fragment, returning its id.
    fn seed_sending_session_with_steering_fragment(
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
        mint_turn(actor, &session_id);
        apply_tool_use_edge(actor, &session_id);
        session_id
    }

    /// Puts the active session into sending phase with a streamed assistant
    /// entry targeted by a queued context-override mutation, returning the
    /// targeted entry id and the session id.
    fn seed_sending_session_with_pending_mutation(
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
        mint_turn(actor, &session_id);
        apply_tool_use_edge(actor, &session_id);
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
    async fn on_turn_work_settled_emits_send_to_llm_provider() {
        // Given a session mid-tool-loop holding a completed tool call entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor);

        // When the call's completion closes it and the work set empties.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live")
            .turn;
        actor
            .on_tool_execution_completed(&ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "file1.txt", true),
                turn,
                synthesized: false,
            })
            .await;
        let settlement = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
            .expect("the call was open");
        actor
            .on_turn_work_settled(&session_id, settlement.next)
            .await;

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
        use jinn_kernel::common::bus::HarnessServices;
        use jinn_kernel::common::state::State;
        use jinn_testutil::bus_harness::{TestHarness, await_recorded};
        use std::time::Duration;

        let (actor, session_id, recorder) = {
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
            let services = harness.services().await;
            let actor = BusActor::spawn(harness, &state, services.clone());
            (actor, session_id, recorder)
        };
        // The continuation's admission needs a live turn: mint it and apply
        // the tool-use edge, the shape a StreamCompleted(ToolUse) leaves
        // before the next batch.
        let turn = {
            let services = actor.services.clone();
            let grant = services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("fixture mint must be admitted");
            services
                .turn_ledger
                .register_and_dispatch(
                    &session_id,
                    &grant.turn,
                    vec![jinn_core_types::ToolCallId::from("tc-1")],
                    || (),
                )
                .expect("registration on a live turn");
            services
                .turn_ledger
                .ack_stream_ended(
                    &session_id,
                    &grant.turn,
                    &grant.stream,
                    jinn_kernel::common::turn_ledger::Next::Continue,
                )
                .expect("the tool-use edge retires the live stream");
            grant.turn
        };

        // When the completion is published to the bus, closing the call.
        actor
            .publish(ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "file1.txt", true),
                turn,
                synthesized: false,
            })
            .await;
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;

        // Then the actor published SendToLlmProvider via the MsgHandler —
        // the close emptied the work set and the settlement dispatched it.
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

        let (actor, sent, _state, session_id, _turn, _stream) = spawn_buffered_batch_actor().await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "precondition: the tool loop dispatched its continuation"
        );
        let recorder = actor.harness.spawn_recorder::<SendToLlmProvider>().await;

        // When StreamCompleted(Canceled) arrives afterwards, stamped with
        // the identity live at cancel time — the continuation swapped the
        // stream, so the fixture reads the ledger now, as production's
        // terminate_turn does.
        let live = actor.services.turn_ledger.live(&session_id).expect("live");
        actor
            .publish({
                let mut event = stream_completed(
                    &actor.services,
                    &session_id,
                    StreamCompletedReason::Canceled,
                );
                event.turn = live.turn;
                event.stream = live.stream.expect("in-flight stream");
                event
            })
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
        let (actor, _recorder, state, session_id, _turn, _stream) =
            spawn_buffered_batch_actor().await;

        // When StreamCompleted(Canceled) arrives afterwards — stamped with
        // the identity live at cancel time (the continuation swapped the
        // stream). The cancel is handled asynchronously on the actor mailbox,
        // so the terminal phase is polled rather than read straight away.
        let live = actor.services.turn_ledger.live(&session_id).expect("live");
        actor
            .publish({
                let mut event = stream_completed(
                    &actor.services,
                    &session_id,
                    StreamCompletedReason::Canceled,
                );
                event.turn = live.turn;
                event.stream = live.stream.expect("in-flight stream");
                event
            })
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

        let (actor, session_id, recorder, dispatched_at) = {
            let harness = TestHarness::new_best_effort().await;
            let recorder = harness.spawn_recorder::<SendToLlmProvider>().await;
            let state = State::new(AppState::default());
            let session_id = state.read().session.active_session_id().clone();
            let dispatched_at = jiff::Timestamp::now();
            let actor = {
                use jinn_kernel::common::bus::HarnessServices;
                let services = harness.services().await;
                BusActor::spawn(harness, &state, services.clone())
            };
            (actor, session_id, recorder, dispatched_at)
        };
        // A live turn with the call registered (as the orchestrator would at
        // dispatch): the terminal ToolUse retires the stream and the
        // settlement dispatches the continuation.
        let (turn, stream) = {
            let services = actor.services.clone();
            let grant = services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("fixture mint must be admitted");
            services
                .turn_ledger
                .register_and_dispatch(
                    &session_id,
                    &grant.turn,
                    vec![jinn_core_types::ToolCallId::from("tc-1")],
                    || (),
                )
                .expect("registration on a live turn");
            (grant.turn, grant.stream)
        };
        let terminal = terminal_tool_use_event(&session_id, dispatched_at, turn, stream);

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
        // The call's result closes the work set; the settlement dispatches
        // the continuation. (The old buffered batch drained on the terminal
        // alone; order independence is now the work set's.)
        actor
            .publish(ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "ok", true),
                turn,
                synthesized: false,
            })
            .await;

        // Then the terminal was delivered: the call closed and a
        // continuation (SendToLlmProvider) was dispatched. A dropped terminal
        // would leave the session wedged in Streaming with no dispatch.
        let sent = await_recorded::<SendToLlmProvider>(&recorder, 1, Duration::from_secs(2)).await;
        assert!(
            sent.iter().any(|m| m.session_id == session_id),
            "StreamCompleted(ToolUse) must survive a >64 token burst on the deep mailbox"
        );
    }

    /// The mid-tool-loop shape ESC-ESC actually meets: the tool-use stream
    /// retired toward a continuation (`Continue`), and one long-running call
    /// is registered as open work — the exact state of a bash command still
    /// executing between stream turns. The fixture mirrors production's
    /// edge: the ledger ack retires the stream, and the projection writes
    /// the `Working` phase.
    fn mid_tool_loop_session_with_open_call(
        actor: &SessionPersistenceActor,
    ) -> jinn_core_types::SessionId {
        let session_id = begin_sending_session(actor);
        actor
            .services
            .turn_ledger
            .register_and_dispatch(
                &session_id,
                &actor
                    .services
                    .turn_ledger
                    .live(&session_id)
                    .expect("live turn")
                    .turn,
                vec![jinn_core_types::ToolCallId::from("tc-1")],
                || (),
            )
            .expect("registration on a live turn");
        session_id
    }

    /// Reads the status off the tool-result entry for `call_id`, if one is
    /// in history.
    fn tool_result_status(
        actor: &SessionPersistenceActor,
        session_id: &jinn_core_types::SessionId,
        call_id: &str,
    ) -> Option<jinn_core_types::ToolResultStatus> {
        let state = actor.state.read();
        let session = state.session.get(session_id)?;
        session.history().iter().find_map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, status, .. } if id.as_str() == call_id => Some(*status),
            _ => None,
        })
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn close_driven_cancel_settles_to_idle() {
        // Given a mid-tool-loop session with one open bash call — the shape
        // ESC-ESC meets when the user cancels a long-running command.
        let (actor, audit) = test_actor_recording().await;
        let session_id = mid_tool_loop_session_with_open_call(&actor);
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                drain_buffers_to_input: false,
            })
            .await;

        // When the synthesized completion closes the last open call — the
        // cancel drain's own bookkeeping, delivered through the ordinary
        // completion path exactly as the orchestrator publishes it.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the drain leaves the turn live until the settle")
            .turn;
        let settlement = {
            actor
                .on_tool_execution_completed(&ToolExecutionCompleted {
                    session_id: session_id.clone(),
                    result: tool_result("tc-1", "bash", "canceled", false),
                    turn,
                    synthesized: true,
                })
                .await;
            actor
                .services
                .turn_ledger
                .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
        };

        // Then the settlement carried the cancel, and the close-driven settle
        // ended the turn: phase Idle, in-flight guard cleared.
        assert_eq!(
            settlement.map(|s| s.next),
            Some(jinn_kernel::common::turn_ledger::Next::Cancel),
            "the last close must report the cancel settlement"
        );
        if let Some(settlement) = settlement {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            assert_eq!(session.phase(), PhaseView::Idle);
            assert!(!session.has_in_flight_stream());
        }

        // And the cancellation was reported exactly once.
        let reports = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(
            reports.len(),
            1,
            "the close-driven settle reports the cancellation once"
        );
        assert_eq!(reports[0].outcome, jinn_session_msg::TurnOutcome::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn close_driven_cancel_sweeps_pending_results_to_failure() {
        // Given a mid-tool-loop session whose bash call pushed a pending
        // result entry — output streamed, result never finalized.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = mid_tool_loop_session_with_open_call(&actor);
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                drain_buffers_to_input: false,
            })
            .await;
        actor.on_tool_execution_started(&ToolExecutionStarted {
            turn: actor
                .services
                .turn_ledger
                .live(&session_id)
                .expect("live turn")
                .turn,
            session_id: session_id.clone(),
            tool_call_id: "tc-1".into(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // When the synthesized completion closes the last open call and the
        // settle runs.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the drain leaves the turn live until the settle")
            .turn;
        actor
            .on_tool_execution_completed(&ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "canceled", false),
                turn,
                synthesized: true,
            })
            .await;
        if let Some(settlement) = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
        {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }

        // Then the pending result was swept to Failure — an unresolved batch
        // would make the context converter strip the whole tool-call block.
        assert_eq!(
            tool_result_status(&actor, &session_id, "tc-1"),
            Some(jinn_core_types::ToolResultStatus::Failure),
            "the cancel settle sweeps the still-pending result to Failure"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn close_driven_esc_esc_drain_applies_at_settle() {
        // Given a mid-tool-loop session holding a steering fragment and a
        // queued message, cancelled with the ESC-ESC drain flag.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = mid_tool_loop_session_with_open_call(&actor);
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                drain_buffers_to_input: true,
            })
            .await;
        {
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session.steering_buffer_mut().push_fragment("steer this");
            session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user("queued work"),
            )));
        }

        // When the synthesized completion closes the last open call and the
        // close-driven settle runs.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the drain leaves the turn live until the settle")
            .turn;
        actor
            .on_tool_execution_completed(&ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "canceled", false),
                turn,
                synthesized: true,
            })
            .await;
        if let Some(settlement) = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
        {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }

        // Then the buffered work landed in the input box at the settle —
        // the dump the flagged cancel asked for — and the buffers are empty.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "steer this\n\n---\n\nqueued work"
        );
        assert_eq!(session.queue_len(), 0);
        assert!(session.steering_buffer().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn late_cancel_completion_after_close_settle_settles_nothing() {
        // Given a mid-tool-loop session whose cancel settle already ran —
        // the close was the last fact, exactly once.
        let (actor, audit) = test_actor_recording().await;
        let session_id = mid_tool_loop_session_with_open_call(&actor);
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                drain_buffers_to_input: false,
            })
            .await;
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the drain leaves the turn live until the settle")
            .turn;
        actor
            .on_tool_execution_completed(&ToolExecutionCompleted {
                session_id: session_id.clone(),
                result: tool_result("tc-1", "bash", "canceled", false),
                turn,
                synthesized: true,
            })
            .await;
        if let Some(settlement) = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
        {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }
        audit.clear();

        // When the cancel's own completion arrives after the settle — the
        // stamp production's terminate_turn publishes: the turn captured
        // while live, over the turn's unset stream, which the ledger
        // refuses (`StaleStream`).
        actor
            .on_stream_completed(&StreamCompleted {
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
                turn,
                stream: jinn_core_types::turn_identity::StreamId::default(),
                retryable: false,
            })
            .await;

        // Then it settles nothing: the turn was reported exactly once, by
        // the close, and nothing re-reports it.
        let reports = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert!(
            reports.is_empty(),
            "a post-settle completion must not re-report the turn"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_turn_work_settled_transitions_session_to_streaming() {
        // Given a session mid-tool-loop with no open calls.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor);

        // When the work settlement dispatches the continuation.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

        // Then the session is Streaming again, waiting for the response —
        // derived from the ledger: the continuation joined the live turn
        // with a fresh in-flight stream.
        assert!(matches!(
            actor.services.turn_ledger.view(&session_id),
            jinn_kernel::common::turn_ledger::PhaseView::Streaming
        ));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn completion_before_tool_use_waits_for_the_stream_retirement() {
        // Given a streaming session with a live turn whose call is open —
        // the batch landed while the stream still ran (bug #21's order).
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let session_id = actor.state.read().session.active_session_id().clone();
            mint_turn(&actor, &session_id);
            session_id
        };
        actor
            .services
            .turn_ledger
            .register_and_dispatch(
                &session_id,
                &actor
                    .services
                    .turn_ledger
                    .live(&session_id)
                    .expect("live")
                    .turn,
                vec![jinn_core_types::ToolCallId::from("tc-1")],
                || (),
            )
            .expect("registration on a live turn");

        // When the completion closes the call while the stream is live.
        let settled = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
            .is_some();

        // Then nothing settles yet — the stream is still in flight.
        assert!(!settled, "no settlement while the stream has not retired");
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "an early completion must not dispatch a continuation"
        );

        // And when the stream then retires with Continue, the settlement
        // dispatches the continuation (TM-5's other half).
        let live = actor.services.turn_ledger.live(&session_id).expect("live");
        let ack = actor.services.turn_ledger.ack_stream_ended(
            &session_id,
            &live.turn,
            &live.stream.expect("live stream"),
            jinn_kernel::common::turn_ledger::Next::Continue,
        );
        assert!(
            matches!(ack, Ok(Some(_))),
            "the retirement that empties the work set settles: {ack:?}"
        );
        if let Ok(Some(settlement)) = ack {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "the stream retirement that empties the work set dispatches the continuation"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_before_completion_waits_for_the_call_close() {
        // Given a streaming session with a live turn whose call is open —
        // the stream retired before the completion landed (the reverse of
        // the buffered-batch order).
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let session_id = actor.state.read().session.active_session_id().clone();
            mint_turn(&actor, &session_id);
            session_id
        };
        let live = actor.services.turn_ledger.live(&session_id).expect("live");
        actor
            .services
            .turn_ledger
            .register_and_dispatch(
                &session_id,
                &live.turn,
                vec![jinn_core_types::ToolCallId::from("tc-1")],
                || (),
            )
            .expect("registration on a live turn");

        // When the stream retires with Continue while the call is still open.
        let ack = actor.services.turn_ledger.ack_stream_ended(
            &session_id,
            &live.turn,
            &live.stream.expect("live stream"),
            jinn_kernel::common::turn_ledger::Next::Continue,
        );

        // Then nothing settles yet — the call is still open.
        assert!(
            matches!(ack, Ok(None)),
            "no settlement while a call is open: {ack:?}"
        );
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "a stream retirement with open calls must not dispatch a continuation"
        );

        // And when the call then closes, the settlement dispatches.
        if let Some(settlement) = actor
            .services
            .turn_ledger
            .close_call(&session_id, &jinn_core_types::ToolCallId::from("tc-1"))
        {
            actor
                .on_turn_work_settled(&session_id, settlement.next)
                .await;
        }
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "the close that empties the work set dispatches the continuation"
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

        // When the stream completes requesting a tool call with long arguments,
        // stamped with the harness's live mint.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("harness minted a live turn");
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("checking".to_owned()),
            tool_calls: Some(vec![ToolCall {
                id: "tc-1".into(),
                name: "bash".to_owned(),
                arguments: r#"{"command":"ls -la /very/long/path"}"#.to_owned(),
            }]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
            turn: live.turn,
            stream: live.stream.unwrap_or_default(),
            retryable: false,
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

        // When the tool execution completes, carrying the live turn's
        // identity — the mint the ledger's admission checks.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("harness minted a live turn")
            .turn;
        let event = jinn_tools_msg::ToolExecutionCompleted {
            turn,
            synthesized: false,
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
        // Given a session whose turn is draining a cancel: the ledger's
        // TurnScoped admission refuses completions while `next` is Cancel
        // (the synthesized cancellations go through the close path, not
        // admission). The fixture uses the minted token — production no
        // longer sends legacy ones on this path.
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
            state.session.active_session_id().clone()
        };
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("harness minted a live turn")
            .turn;
        let _ = actor.services.turn_ledger.begin_cancel(&session_id);

        // When a completion for the cancelled turn arrives.
        let event = jinn_tools_msg::ToolExecutionCompleted {
            turn,
            session_id: session_id.clone(),
            synthesized: false,
            result: jinn_core_types::tool_types::ToolResult {
                tool_call_id: "tc-1".into(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
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
                .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id.as_str() == "tc-1"));
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
    async fn completion_dropped_when_no_live_turn() {
        // Given a session whose turn was minted and then removed — the
        // shape a session close or delete leaves behind a straggler tool
        // result.
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
            session.settle_to_idle();
            session.drain_cancelled_work_to_input();
            state.session.active_session_id().clone()
        };
        // The completion carries the identity the removed turn was minted
        // under — the real stamp a straggler tool result rides, not an
        // unset one. The ledger has no live turn to admit it against.
        let minted = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the harness minted a live turn")
            .turn;
        actor.services.turn_ledger.remove(&session_id);

        let event = ToolExecutionCompleted {
            session_id: session_id.clone(),
            synthesized: false,
            result: ToolResult {
                tool_call_id: "tc-1".into(),
                name: "bash".to_owned(),
                content: "file1.txt".to_owned(),
                success: true,
                full_content: None,
                truncation: None,
                pin_position: None,
                attachments: Vec::new(),
            },
            turn: minted,
        };

        // When a stale completion arrives post-cancel — no live turn to
        // admit it, so the ledger refuses.
        actor.on_tool_execution_completed(&event).await;

        // Then the loop is not restarted: phase stays Idle.
        {
            let state = actor.state.read();
            let session = state.session.get(&session_id).expect("session exists");
            assert!(
                matches!(session.phase(), PhaseView::Idle),
                "expected Idle after cancel, got {:?}",
                session.phase()
            );
        }

        // And no continuation send is emitted.
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "expected no SendToLlmProvider for dropped stale tool result"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn work_settled_skips_send_when_tool_loop_disabled() {
        // Given a mid-tool-loop session whose tool loop is disabled — the
        // shape a `StreamCompleted(ToolUse)` leaves before the next batch.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor);
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.set_tool_loop_disabled();
        }

        // When the tool loop stops at the work settlement.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

        // Then the turn ends in Idle without dispatching a continuation.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseView::Idle),
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
            "tool_loop_disabled should be cleared after the work settlement"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_turn_work_settled_unaffected_without_tool_loop_disabled() {
        // Given a normal mid-tool-loop session holding a completed tool call.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor);

        // When the work settlement dispatches the continuation.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

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
            id: "tc-1".into(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // Then a ToolCall entry with that id is in the history.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session.history().iter().find(
            |e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc-1"),
        );
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
            id: "tc-dispatch".into(),
            name: "bash".to_owned(),
            dispatched_at: dispatched,
        });

        // Then the ToolCall entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tc = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc-dispatch"))
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
            session.begin_tool_call(
                0,
                &jinn_core_types::ToolCallId::from("tc-1"),
                "bash",
                jiff::Timestamp::now(),
            );
            state.session.active_session_id().clone()
        };

        // When the fully-formed tool call arrives.
        actor.on_tool_call_received(&ToolCallReceived {
            session_id: session_id.clone(),
            tool_call: ToolCall {
                id: "tc-1".into(),
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
            .find(
                |e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc-1"),
            )
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
            session.begin_tool_call(
                0,
                &jinn_core_types::ToolCallId::from("tc-1"),
                "bash",
                jiff::Timestamp::now(),
            );
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
            .find(
                |e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id.as_str() == "tc-1"),
            )
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
        let (actor, session_id, capture, _guard) = {
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
            // The guard must outlive the block: it installs the subscriber
            // for the rest of the test.
            let guard = tracing::subscriber::set_default(subscriber);
            (actor, session_id, capture, guard)
        };

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
            contents.contains("current_phase=Working"),
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

        // When a tool execution starts, carrying the live turn's identity.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("harness minted a live turn")
            .turn;
        actor.on_tool_execution_started(&ToolExecutionStarted {
            turn,
            session_id: session_id.clone(),
            tool_call_id: "tc-1".into(),
            name: "bash".to_owned(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // Then a pending ToolResult entry with that id is in the history.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tr = session.history().iter().find(
            |e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id.as_str() == "tc-1"),
        );
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
            session.begin_tool_result(
                &jinn_core_types::ToolCallId::from("tc-1"),
                "bash",
                jiff::Timestamp::now(),
            );
            state.session.active_session_id().clone()
        };

        // When two output chunks arrive for that tool result, carrying the
        // live turn's identity.
        let turn = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("harness minted a live turn")
            .turn;
        actor.on_tool_execution_output(&ToolExecutionOutput {
            turn,
            session_id: session_id.clone(),
            tool_call_id: "tc-1".into(),
            output: "file1.txt\n".to_owned(),
            kind: ToolOutputKind::default(),
        });
        actor.on_tool_execution_output(&ToolExecutionOutput {
            turn,
            session_id: session_id.clone(),
            tool_call_id: "tc-1".into(),
            output: "file2.txt\n".to_owned(),
            kind: ToolOutputKind::default(),
        });

        // Then both chunks concatenate into the entry's content.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session");
        let tr = session
            .history()
            .iter()
            .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id.as_str() == "tc-1"))
            .expect("tool result");
        if let ChatEntryKind::ToolResult { content, .. } = &tr.kind {
            assert_eq!(content, "file1.txt\nfile2.txt\n");
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_turn_work_settled_applies_pending_mutations() {
        // Given a mid-tool-loop session with one queued context-override mutation.
        let (actor, audit) = test_actor_recording().await;
        let (entry_id, session_id) = seed_sending_session_with_pending_mutation(&actor);

        // When the work settlement dispatches the continuation.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

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
    async fn on_turn_work_settled_empty_mutation_queue_is_noop() {
        // Given a mid-tool-loop session with nothing queued for mutation application.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session_with_tool_call(&actor);

        // When the work settlement dispatches the continuation.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

        // Then the continuation is still dispatched.
        assert!(
            audit.contains_name("SendToLlmProvider"),
            "expected SendToLlmProvider with empty mutation queue"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn work_settled_drained_steering_entry_lands_after_tool_results() {
        // Given a sending session holding a complete tool loop and a buffered
        // steering fragment the user typed while the tools ran.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_sending_session_with_steering_fragment(&actor);

        // When the work settlement dispatches the continuation.
        actor
            .on_turn_work_settled(
                &session_id,
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .await;

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
