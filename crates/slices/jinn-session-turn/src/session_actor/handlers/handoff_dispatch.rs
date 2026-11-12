//! The handoff dispatch: the actor-side half of end-of-conversation
//! compaction.
//!
//! When the gate fires at a model-request boundary, the pending request
//! is replaced by the handoff recap turn. The two boundary shapes:
//!
//! - **Idle-direct / resume** (`enqueue.rs`, [`HandoffBoundary::IdleDirect`]):
//!   the caller swaps the entry it was about to dispatch for the nudge
//!   entry; the held entry waits at the queue front and re-dispatches
//!   through the ordinary idle drain after the fold. The recap is its own
//!   turn: no loop is live at this boundary, so the turn definition ("a
//!   turn is the tool loop until `Idle`") places the boundary at the
//!   recap's own settle, and the held entry legitimately starts the next
//!   turn.
//! - **Mid-loop** (`tool_calls.rs`, [`HandoffBoundary::MidLoop`]): the
//!   recap continuation joins the live turn through the existing
//!   tool-loop machinery, with the nudge pushed ahead of it. The gate
//!   records `handoff_continuation_owed`; the recap's successful
//!   completion is then acked `Continue` instead of `End`, so the settle
//!   never touches `Idle` — the loop-continue path runs the fold and the
//!   post-fold continuation dispatches within the same live turn, under a
//!   fresh stream. Compaction is a mid-loop event, not a turn boundary.
//!
//! The nudge rides as a `RuleInterrupt` entry — the established
//! harness-steering kind: default-included (the model must see it — a
//! recap request whose instruction never reached the model would return
//! an ordinary answer), visible in the transcript, rendered as its own
//! kind so it never reads as typed user input, and named "handoff"
//! rather than a rule so its provenance is legible. The fold excludes it
//! after the cycle.
//!
//! The recap request keeps tools defined (prefix continuity) but the API
//! layer forbids calls: the dispatch transports map a pending handoff to
//! `tool_choice = none` (the session actor's continuation path and the
//! queue actor's prepared dispatch). By fold time the flag is cleared, so
//! the post-fold continuation and every later dispatch are unconstrained.
//!
//! A handoff fired with a regime sized below the protected floor re-fires
//! back-to-back *within* the same turn after each fold — the gate still
//! passes while pending because each fire's recap is a fresh boundary.
//! ESC is the escape; the regime's sizing is configuration
//! responsibility.

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

/// Which model-request boundary the gate is evaluating — the two shapes
/// a fire can interrupt, with different turn-liveness consequences.
///
/// Explicit, not derived from the session's phase: the phase projection
/// is a display fact that lags the ledger, and misreading the boundary
/// kind would either hold a loop continuation at the queue front (a
/// turn-shape nothing else produces) or owe a fold to a boundary with no
/// live turn to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session_actor) enum HandoffBoundary {
    /// The recap replaces a live tool-loop continuation (`continue_tool_loop`):
    /// its success must rejoin the loop within the same turn.
    MidLoop,
    /// The recap replaces a dispatch from `Idle` (a user message or a
    /// resume): its success ends the recap turn, and the held entry
    /// starts the next one through the idle drain.
    IdleDirect,
}

/// Point-of-use prompt name for the recap prompt.
const HANDOFF_PROMPT_FILE: &str = "_handoff.md";

impl super::super::SessionPersistenceActor {
    /// Evaluates the handoff gate for one model-request boundary.
    ///
    /// On a fire, marks the session handoff-pending — recording
    /// `owed` for a [`HandoffBoundary::MidLoop`] fire (the recap owes its
    /// post-fold continuation) — holds `held` at the queue front (an
    /// idle-direct fire's replaced request; it re-dispatches through the
    /// ordinary drain after the fold), and returns the nudge entry for
    /// the caller to dispatch instead. Returns `None` when the boundary
    /// dispatches normally.
    ///
    /// A handoff already in flight always passes: the recap turn's own
    /// dispatch must not re-trigger the gate.
    pub(in crate::session_actor) fn handoff_nudge_if_gated(
        &self,
        session_id: &jinn_core_types::SessionId,
        boundary: HandoffBoundary,
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
            (
                session.work_size(),
                session.take_handoff_excluded_cost(),
                session.persona_name().to_owned(),
            )
        };
        let (work_size, excluded_cost, persona_name) = gate_input;

        // Progress guard: after a fold, new user work is what makes
        // another fold meaningful. Refusing when none has arrived breaks
        // the pins-dominated fixed point (work estimate still above the
        // fire line, nothing new to summarize) instead of looping on it.
        let folded_since_last_user_turn = {
            self.state
                .read()
                .try_session(session_id)
                .is_some_and(jinn_kernel::ChatSessionState::folded_since_last_user_turn)
        };
        if folded_since_last_user_turn {
            tracing::warn!(
                session_id = %session_id,
                "handoff refused: no user turn since the previous fold"
            );
            return None;
        }

        if evaluate_handoff_gate(&config, work_size, excluded_cost) != GateDecision::Fire {
            return None;
        }

        // The persona's own handoff block replaces the shipped recap prompt
        // wholesale when it carries one; otherwise the shipped file applies.
        let recap_prompt = self
            .persona_handoff_prompt(&persona_name)
            .unwrap_or_else(|| self.current_handoff_prompt());
        let nudge = ChatEntry::rule_interrupt("handoff", build_handoff_nudge(&recap_prompt));

        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            session.begin_handoff();
            if boundary == HandoffBoundary::MidLoop {
                // The recap's success routes back into the live loop: the
                // completion routing reads this to ack `Continue`, and the
                // fold clears it with the pending flag.
                session.set_handoff_continuation_owed();
            }
            if let Some(held) = held {
                // The replaced request jumps nothing: it waits at the
                // front and re-dispatches first after the fold.
                session.enqueue_front(held);
            }
        });

        tracing::info!(
            session_id = %session_id,
            boundary = ?boundary,
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

    /// The session persona's `<compaction>` handoff block, when it carries
    /// one.
    ///
    /// Resolves the persona from the catalog cell by name. Returns `None`
    /// when the persona is unknown, the persona slice is not activated, or
    /// the persona carries no well-formed block — the caller then uses the
    /// shipped fallback via [`Self::current_handoff_prompt`].
    pub(in crate::session_actor) fn persona_handoff_prompt(
        &self,
        persona_name: &str,
    ) -> Option<String> {
        // The catalog cell is the persona slice's write handle; a missing
        // cell means the slice is not activated, which degrades to the
        // shipped fallback rather than failing the boundary.
        let cell = self
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())?;
        cell.read()
            .persona(persona_name)
            .map(|persona| persona.handoff_prompt.clone())
            // An empty block is a missing block: the caller falls back.
            .filter(|text| !text.is_empty())
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
    /// The handoff fields clear here — the single success-path owner.
    /// Callers run this before dispatching the follow-up request, so the
    /// folded context is what assembles: mid-loop, the same-turn
    /// continuation from `continue_tool_loop`; idle-direct, the held
    /// entry's drain.
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
                .and_then(jinn_kernel::ChatSessionState::context_size)
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

        // Apply synchronously and clear both handoff fields under the
        // same write guard — the fresh context is post-fold and nothing
        // is pending or owed anymore. The fold also marks that it ran
        // since the last user turn: the gate's progress guard refuses a
        // repeat fire until the next user turn re-arms it.
        let changed = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            session.set_last_fold_gathered();
            session.queue_mutations(mutations);
            let (_count, changed) = session.drain_and_apply_pending_mutations();
            session.end_handoff(true);
            changed
        });

        tracing::info!(
            session_id = %session_id,
            changed = changed.len(),
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
    }
}
