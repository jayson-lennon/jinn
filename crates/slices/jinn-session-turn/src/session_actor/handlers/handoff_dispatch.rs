//! The handoff dispatch: the actor-side half of end-of-conversation
//! compaction.
//!
//! When the gate fires at a model-request boundary, the pending request
//! is replaced by the handoff turn. The three boundary shapes:
//!
//! - **Idle-direct / resume** (`enqueue.rs`): the caller swaps the entry
//!   it was about to dispatch for the nudge entry; the held entry waits
//!   at the queue front and re-dispatches through the ordinary idle
//!   drain after the fold. The turn is an ordinary fresh turn.
//! - **Mid-loop** (`tool_calls.rs`): the recap continuation joins the
//!   live turn through the existing tool-loop machinery, with the nudge
//!   pushed ahead of it. The owed loop continuation waits at the queue
//!   front; the fold re-mints a fresh turn for it.
//!
//! The nudge rides as a `RuleInterrupt` entry — the established
//! harness-steering kind: default-included (the model must see it — a
//! recap request whose instruction never reached the model would return
//! an ordinary answer), visible in the transcript, rendered as its own
//! kind so it never reads as typed user input, and named "handoff"
//! rather than a rule so its provenance is legible. The fold excludes it
//! after the cycle.
//!
//! The queue actor maps a pending handoff to `tool_choice = none` on the
//! dispatch (tools stay defined — prefix continuity; the API layer
//! forbids calls). By fold time the flag is cleared, so the held
//! request's re-dispatch and the re-minted continuation dispatch
//! unconstrained.

use crate::session_actor::handlers::handoff::GateDecision;
use crate::session_actor::handlers::handoff::build_fold_mutations;
use crate::session_actor::handlers::handoff::build_handoff_nudge;
use crate::session_actor::handlers::handoff::build_recap_entry;
use crate::session_actor::handlers::handoff::evaluate_handoff_gate;
use crate::session_actor::handlers::handoff::is_fold_protected;
use jinn_core_types::ChatEntry;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_llm_support::token_estimator::TokenCounter;
use jinn_preferences_config::schemas::HandoffCompactionConfig;
use jinn_turn_dispatch_msg::QueueItem;

#[cfg(test)]
mod gate_dispatch_tests;

/// Point-of-use prompt name for the recap prompt.
const HANDOFF_PROMPT_FILE: &str = "_handoff.md";

impl super::super::SessionPersistenceActor {
    /// Evaluates the handoff gate for one model-request boundary.
    ///
    /// On a fire, marks the session handoff-pending, holds `held` at the
    /// queue front (the request this boundary was about to run — it
    /// re-dispatches through the ordinary drain after the fold), and
    /// returns the nudge entry for the caller to dispatch instead.
    /// Returns `None` when the boundary dispatches normally.
    ///
    /// A handoff already in flight always passes: the recap turn's own
    /// dispatch must not re-trigger the gate.
    pub(in crate::session_actor) async fn handoff_nudge_if_gated(
        &self,
        session_id: &jinn_core_types::SessionId,
        held: Option<QueueItem>,
    ) -> Option<ChatEntry> {
        // Read everything the gate needs under one write guard (the
        // excluded-cost take mutates), config outside the lock (Snapshot
        // Read semantics: a jinn.toml edit applies at the next boundary,
        // never mid-evaluation).
        let config = self
            .services
            .config
            .get::<HandoffCompactionConfig>()
            .unwrap_or_default();
        let gate_input = {
            let mut state = self.state.write();
            let session = state.session_mut_or_create(session_id);
            if session.is_handoff_pending() {
                return None;
            }
            let model_name = session.profile().model.display_str().to_owned();
            (
                session.context_size(),
                session.take_handoff_excluded_cost(),
                model_name,
            )
        };
        let (assembled_size, excluded_cost, model_name) = gate_input;
        let model_reported_limit = resolve_model_context_length(&self.state, &model_name);

        if evaluate_handoff_gate(&config, model_reported_limit, assembled_size, excluded_cost)
            != GateDecision::Fire
        {
            return None;
        }

        let recap_prompt = self.current_handoff_prompt();
        let nudge = ChatEntry::rule_interrupt("handoff", build_handoff_nudge(&recap_prompt));

        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            session.begin_handoff();
            if let Some(held) = held {
                // The replaced request jumps nothing: it waits at the
                // front and re-dispatches first after the fold.
                session.enqueue_front(held);
            }
        });

        tracing::info!(
            session_id = %session_id,
            "handoff gate fired: dispatching recap turn, holding the pending request"
        );
        Some(nudge)
    }

    /// The recap prompt: re-read at point of use, falling back to the
    /// launch copy — the same discipline the compaction worker uses, so
    /// an edited `_handoff.md` applies to the next handoff without a
    /// restart and a deleted file costs nothing but freshness.
    pub(in crate::session_actor) fn current_handoff_prompt(&self) -> String {
        match jinn_common::system_resource::load_system_resource(
            HANDOFF_PROMPT_FILE,
            &self.services.paths.prompts_dir().as_marked_path(),
        ) {
            Ok(prompt) => prompt,
            Err(_) => self.handoff_prompt.clone(),
        }
    }

    /// Runs the fold after a successful handoff turn: excludes every
    /// entry except user turns, Intersession dispatches, and pins, and
    /// inserts the recap as a context-riding `Compaction` entry.
    ///
    /// The raw handoff response is an `Assistant` entry —
    /// default-included — so the fold's `ForcedExclude` is load-bearing
    /// for it; the nudge (`RuleInterrupt`) is likewise excluded. A prior
    /// cycle's `Compaction` recap is excluded too: superseded by this
    /// cycle's.
    ///
    /// Mutations apply synchronously under one write guard — not via
    /// `SubmitHistoryMutations` — so the fresh context the follow-up
    /// dispatch assembles is guaranteed post-fold (a bus round-trip
    /// would race this actor's own mutation application), and so the
    /// prune accumulation gate cannot defer the very reduction the
    /// turn exists to perform. The same write-time loop expansion and
    /// validation apply: it is the session's own mutation application
    /// path.
    ///
    /// If the replaced request was a mid-loop continuation (the marker
    /// this actor left at the queue front), the fold re-mints a fresh
    /// turn and hands it to the dispatch queue: the fresh context
    /// continues executing — the remaining plan's next step is the
    /// continuation's answer, now authored against task + recap.
    pub(in crate::session_actor) async fn run_handoff_fold(
        &self,
        session_id: &jinn_core_types::SessionId,
    ) {
        // Snapshot the pieces under the read guard; no writes here.
        let (history, last_entry, model_used) = {
            let state = self.state.read();
            let Some(session) = state.try_session(session_id) else {
                return;
            };
            (
                session.history().to_vec(),
                session.history().last().cloned(),
                session.profile().model.display_str().to_owned(),
            )
        };

        // Token accounting for the recap entry: before is the cached
        // assembled estimate; after is the recap's own cost (the fresh
        // context is the recap plus the protected set).
        let tokens_before = {
            let state = self.state.read();
            state
                .try_session(session_id)
                .and_then(|session| session.context_size())
                .unwrap_or_default()
        };
        let recap_prose = last_entry
            .as_ref()
            .and_then(|entry| entry.prompt_text())
            .unwrap_or_default()
            .to_owned();
        let tokens_after = self.counter.count(&recap_prose) as u32;
        let excluded_count = history
            .iter()
            .filter(|entry| !is_fold_protected(entry))
            .count();

        let recap_entry = build_recap_entry(
            jinn_core_types::ChatEntryId::new(),
            recap_prose,
            tokens_before as usize,
            tokens_after as usize,
            excluded_count,
            model_used,
        );
        let last_id = history.last().map(|entry| entry.id.clone());
        let mutations = build_fold_mutations(&history, recap_entry, last_id);
        if mutations.is_empty() {
            return;
        }

        // Apply synchronously; collect the changed entries for the
        // override broadcasts outside the lock, and whether the fold
        // found an owed continuation.
        let (changed, continuation_owed) = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            session.queue_mutations(mutations);
            let (_count, changed) = session.drain_and_apply_pending_mutations();
            // The owed mid-loop continuation: pop the marker this actor
            // left at the queue front. `true` means the replaced request
            // was a loop continuation and the fresh context owes it a
            // dispatch.
            let owed = session.pop_if_front_continuation();
            (changed, owed)
        });

        tracing::info!(
            session_id = %session_id,
            changed = changed.len(),
            continuation_owed,
            "handoff fold applied: fresh context is task + pins + recap"
        );
        for entry_id in changed {
            self.publish(jinn_context_assembly_msg::ContextOverrideChanged {
                session_id: session_id.clone(),
                entry_id,
            })
            .await;
        }
        super::super::helpers::emit_history_appended(self.bus(), session_id).await;

        // The mid-loop continuation re-dispatch: a fresh turn on the
        // fresh context. `DispatchTurn` → the queue actor's prepared
        // dispatch assembles the folded history (guaranteed — the
        // mutations above applied synchronously, before this publish)
        // and joins the turn minted here.
        // The fresh context takes the next action: mint a fresh turn
        // for the continuation.
        self.redispatch_after_fold_if_owed(session_id, continuation_owed)
            .await;
    }

    /// Mints a fresh turn and asks the dispatch queue to run it, when
    /// the fold found an owed continuation.
    async fn redispatch_after_fold_if_owed(
        &self,
        session_id: &jinn_core_types::SessionId,
        owed: bool,
    ) {
        if !owed {
            return;
        }
        let minted = jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &self.services,
            &self.state,
            session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await;
        if minted.is_none() {
            tracing::warn!(
                session_id = %session_id,
                "post-fold continuation dropped: the ledger refused a fresh mint"
            );
            return;
        }
        self.publish(jinn_turn_dispatch_msg::DispatchTurn {
            session_id: session_id.clone(),
        })
        .await;
    }
}

/// Resolves the model's reported context length from the model cache —
/// the same lookup the status bar and background compaction use, so gate
/// and display agree. The gate resolves the full firing regime
/// (explicit `context_limit` overrides this) in
/// `HandoffCompactionConfig::resolve_regime`.
fn resolve_model_context_length(
    state: &jinn_kernel::common::state::State,
    model_name: &str,
) -> Option<u32> {
    let model_cache = state
        .read()
        .provider_state()
        .and_then(|cell| cell.read().model_cache.clone());
    let cache = model_cache?;
    let provider_name = model_name.split('/').next()?;
    let models = cache.entries.get(provider_name)?;
    let model_suffix = model_name.get(provider_name.len().wrapping_add(1)..)?;
    models
        .iter()
        .find(|model| model.id == model_suffix)
        .and_then(|model| model.context_length)
}
