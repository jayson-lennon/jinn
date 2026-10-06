//! The session phase actor.
//!
//! The sole writer of session phase. Every phase transition, dispatch
//! admission, and turn termination arrives as a [`PhaseCommand`] and is
//! applied under this actor's one lock, against the session's own phase
//! machine and this actor's per-session turn record. No other component
//! writes the phase.
//!
//! The turn record answers "is this turn dead" for every component:
//!
//! - `generation` — strictly monotonic per session, minted at each
//!   `FreshTurn` admission. A stream carries the generation it started
//!   under, and a cancel kills exactly one generation.
//! - `stream_stamp` — the `dispatched_at` the admitted request carries.
//!   Completions resolve against it: an earlier stamp is a stale
//!   delivery from a superseded generation and is refused.
//! - `dead_generation` — the generation a [`PhaseCommand::TurnCanceled`]
//!   killed. A `ResumeTurn` or `ToolContinuation` whose live generation
//!   is dead is refused at mint: it publishes nothing, because there is
//!   nothing left to run.
//! - `ended` — the report-once latch for a turn's end. A cancel names a
//!   turn once; a second cancel for an already-ended turn reports
//!   nothing.
//!
//! Phase-change events are published from here and nowhere else, so a
//! subscriber cannot see a transition twice, or miss one.

use std::collections::HashMap;

use error_stack::Report;
use jiff::Timestamp;
use jinn_core_types::SessionId;
use jinn_kernel::BusService;
use jinn_kernel::common::state::State;

use jinn_session_msg::phase_command::{DispatchKind, PhaseCommand, PhaseDecision};
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

/// The static path the actor registers at.
pub use jinn_session_msg::phase_command::SESSION_PHASE_PATH;

/// The turn record for one session: the generation counter and the
/// liveness facts a dispatch, a completion, or a cancel resolves
/// against. Actor-internal; no accessor is exported.
#[derive(Debug, Default)]
struct TurnRecord {
    /// The live (or last) turn generation. Zero when no turn has ever
    /// been minted for this session.
    generation: u64,
    /// The stream stamp the live generation's request carries; `None`
    /// when no generation is live (never minted, ended, or not yet
    /// begun its stream).
    stream_stamp: Option<Timestamp>,
    /// The generation a cancel killed, if the live one is dead.
    dead_generation: Option<u64>,
    /// Whether the turn's end has already been reported. Set by a
    /// cancel that settles the session; cleared by a fresh-turn mint.
    ended: bool,
}

impl TurnRecord {
    /// Whether a live generation exists that a resume or continuation
    /// may still join.
    fn has_live_generation(&self) -> bool {
        self.generation > 0 && self.dead_generation.is_none() && !self.ended
    }
}

/// The phase actor's internal state: the shared application `State`
/// (whose sessions carry the phase machines this actor alone
/// transitions) and the per-session turn records.
pub struct SessionPhaseActor {
    state: State,
    bus: BusService,
    turns: HashMap<SessionId, TurnRecord>,
}

impl ServiceActor for SessionPhaseActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "service actor trait signature; the state is shared memory"
    )]
    async fn start(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn_service_builder` + `start_with`
        // (the state and bus handles cannot ride JSON args).
        Err(Report::new(RegistryError::InvalidSpec)
            .attach("SessionPhaseActor is spawned via start_with"))
    }
}

/// Whether a completion stamped `dispatched_at` resolves against the
/// record's live generation.
fn resolves(record: &TurnRecord, dispatched_at: Timestamp) -> bool {
    match record.stream_stamp {
        // No stamp: nothing is live, so nothing can resolve.
        None => false,
        Some(stamp) => dispatched_at >= stamp,
    }
}

impl SessionPhaseActor {
    /// Builds the actor over the state it transitions and the bus it
    /// publishes phase changes on.
    #[must_use]
    pub fn new(state: State, bus: BusService) -> Self {
        Self {
            state,
            bus,
            turns: HashMap::new(),
        }
    }

    /// Applies one command: admission policy first, then the machine
    /// edges under the one session write lock this actor holds.
    ///
    /// Returns the decision, and the pre/post phases to publish when
    /// they differ. A refusal applies nothing and publishes nothing.
    async fn apply(&mut self, command: &PhaseCommand) -> PhaseDecision {
        let session_id = command.session_id().clone();
        let record = self.turns.entry(session_id.clone()).or_default();

        match command {
            PhaseCommand::BeginStream {
                kind,
                dispatched_at,
                ..
            } => {
                // Admission first: a fresh turn always mints; a resume
                // or continuation may only join a live generation.
                match kind {
                    DispatchKind::FreshTurn => {
                        record.generation += 1;
                        record.dead_generation = None;
                        record.ended = false;
                        record.stream_stamp = Some(*dispatched_at);
                    }
                    DispatchKind::ResumeTurn | DispatchKind::ToolContinuation => {
                        if !record.has_live_generation() {
                            tracing::info!(
                                session_id = %session_id,
                                kind = ?kind,
                                generation = record.generation,
                                "dispatch refused: no live generation behind it"
                            );
                            return refused(record, &self.state, &session_id);
                        }
                        record.stream_stamp = Some(*dispatched_at);
                    }
                }

                // Admitted: the fused `Idle → Sending → Streaming` edge,
                // the shape every dispatch path took before. An already
                // `Streaming` session (a tool continuation) keeps its
                // phase; the stamp refresh above is its whole story. The
                // session-side stamp is armed here too — this actor is the
                // sole writer of the guard the stall watchdog reads.
                let stamp = record.stream_stamp;
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(&session_id);
                    let old = session.phase();
                    session.begin_streaming();
                    if let Some(stamp) = stamp {
                        session.arm_stream(stamp);
                    }
                    (old, session.phase())
                });
                PhaseDecision {
                    admitted: true,
                    old_phase: old,
                    new_phase: new,
                    generation: record.generation,
                    stream_stamp: record.stream_stamp,
                }
            }
            PhaseCommand::StreamEndedToolUse { dispatched_at, .. } => {
                if !resolves(record, *dispatched_at) {
                    tracing::info!(
                        session_id = %session_id,
                        generation = record.generation,
                        "tool-use completion refused: stale generation"
                    );
                    return refused(record, &self.state, &session_id);
                }
                // `Streaming → Idle → Sending`, the fused edge the
                // session actor's fold used to take. The generation
                // stays live: the tool loop continues.
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(&session_id);
                    let old = session.phase();
                    session.finish_streaming_via_machine();
                    session.begin_sending();
                    (old, session.phase())
                });
                PhaseDecision {
                    admitted: true,
                    old_phase: old,
                    new_phase: new,
                    generation: record.generation,
                    stream_stamp: record.stream_stamp,
                }
            }
            PhaseCommand::StreamEndedFinished { dispatched_at, .. }
            | PhaseCommand::StreamEndedError { dispatched_at, .. } => {
                if !resolves(record, *dispatched_at) {
                    tracing::info!(
                        session_id = %session_id,
                        generation = record.generation,
                        "completion refused: stale generation"
                    );
                    return refused(record, &self.state, &session_id);
                }
                // The turn is over from either busy phase: a normal
                // completion arrives from `Streaming`, and the tool loop's
                // chosen stop arrives from `Sending`. Both settle to `Idle`
                // and drop every registration; the stamp goes with the
                // generation. The actor is the sole writer of both.
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(&session_id);
                    let old = session.phase();
                    session.finish_turn_from_busy_via_machine();
                    session.clear_stream_generation();
                    (old, session.phase())
                });
                record.ended = true;
                record.stream_stamp = None;
                PhaseDecision {
                    admitted: true,
                    old_phase: old,
                    new_phase: new,
                    generation: record.generation,
                    stream_stamp: None,
                }
            }
            PhaseCommand::TurnCanceled { .. } => {
                if record.ended {
                    // The turn already ended; this cancel is late and
                    // reports nothing.
                    tracing::debug!(
                        session_id = %session_id,
                        "cancel for an already-ended turn; not reporting twice"
                    );
                    return refused(record, &self.state, &session_id);
                }
                let stamp = record.stream_stamp;
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(&session_id);
                    let old = session.phase();
                    session.cancel_streaming_via_machine();
                    // The cancelled generation owns the guard; killing it
                    // clears the stamp. The actor is the sole writer.
                    session.clear_stream_generation();
                    (old, session.phase())
                });
                record.ended = true;
                record.dead_generation = Some(record.generation);
                record.stream_stamp = None;
                PhaseDecision {
                    admitted: true,
                    old_phase: old,
                    new_phase: new,
                    generation: record.generation,
                    stream_stamp: stamp,
                }
            }
            PhaseCommand::InterceptRewind { dispatched_at, .. } => {
                if !resolves(record, *dispatched_at) {
                    tracing::info!(
                        session_id = %session_id,
                        generation = record.generation,
                        stream_stamp = ?record.stream_stamp,
                        event_stamp = %dispatched_at,
                        "intercept rewind refused: stale generation"
                    );
                    return refused(record, &self.state, &session_id);
                }
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(&session_id);
                    let old = session.phase();
                    session.rewind_for_retry();
                    (old, session.phase())
                });
                // The turn stays live: the re-dispatch joins the same
                // generation, whose stamp its own BeginStream refreshes.
                PhaseDecision {
                    admitted: true,
                    old_phase: old,
                    new_phase: new,
                    generation: record.generation,
                    stream_stamp: record.stream_stamp,
                }
            }
        }
    }

    /// Publishes the phase change when the transition was real.
    async fn publish_if_changed(&self, session_id: &SessionId, decision: &PhaseDecision) {
        if decision.old_phase != decision.new_phase {
            jinn_kernel::common::phase_events::publish_phase_change(
                &self.bus,
                session_id,
                decision.old_phase,
                decision.new_phase,
            )
            .await;
        }
    }
}

/// The refusal decision for a stale or dead resolution: no transition,
/// the phase as it stands, the record's generation facts.
fn refused(record: &TurnRecord, state: &State, session_id: &SessionId) -> PhaseDecision {
    let phase = state.read().session(session_id).phase();
    PhaseDecision {
        admitted: false,
        old_phase: phase,
        new_phase: phase,
        generation: record.generation,
        stream_stamp: record.stream_stamp,
    }
}

impl MsgHandler<PhaseCommand> for SessionPhaseActor {
    async fn handle(&mut self, command: &PhaseCommand, ctx: &mut MsgCtx<'_>) {
        let session_id = command.session_id().clone();
        let decision = self.apply(command).await;
        ctx.reply(decision.clone());
        self.publish_if_changed(&session_id, &decision).await;
    }
}

/// Spawns the actor at its static path.
#[must_use]
/// Admits or refuses a stream start for callers inside this crate —
/// the session actor's handlers. Same ask as the kernel's
/// `apply_phase`-based helpers; the refused fallback is the point.
pub(crate) async fn admit_stream(
    services: &jinn_kernel::common::services::Services,
    session_id: &SessionId,
    kind: DispatchKind,
    dispatched_at: jiff::Timestamp,
) -> PhaseDecision {
    match jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::BeginStream {
            session_id: session_id.clone(),
            kind,
            dispatched_at,
        },
    )
    .await
    {
        Ok(decision) => decision,
        Err(report) => {
            tracing::error!(
                session_id = %session_id,
                ?report,
                "phase admission ask failed; refusing the dispatch"
            );
            PhaseDecision::refused()
        }
    }
}

/// Asks the phase actor to complete the sending phase — the tool loop
/// stopping by choice. Resolves against the live generation; an ended
/// turn is refused.
pub(crate) async fn end_sending(
    services: &jinn_kernel::common::services::Services,
    session_id: &SessionId,
) -> PhaseDecision {
    match jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::StreamEndedFinished {
            session_id: session_id.clone(),
            dispatched_at: jiff::Timestamp::now(),
        },
    )
    .await
    {
        Ok(decision) => decision,
        Err(report) => {
            tracing::error!(
                session_id = %session_id,
                ?report,
                "phase ask failed; refusing the tool-loop stop"
            );
            PhaseDecision::refused()
        }
    }
}

/// Asks the phase actor to apply the terminal edge a stream completion
/// names. `ToolUse` keeps the turn (the fuse through `Sending`), and
/// `Finished`/`Error` end it; `Canceled` settles nothing here — the cancel
/// that preceded the completion already applied `TurnCanceled`.
pub(crate) async fn settle_stream(
    services: &jinn_kernel::common::services::Services,
    session_id: &SessionId,
    reason: jinn_inference_msg::StreamCompletedReason,
    dispatched_at: jiff::Timestamp,
) -> PhaseDecision {
    use jinn_inference_msg::StreamCompletedReason as R;
    let command = match reason {
        R::ToolUse => PhaseCommand::StreamEndedToolUse {
            session_id: session_id.clone(),
            dispatched_at,
        },
        R::Error => PhaseCommand::StreamEndedError {
            session_id: session_id.clone(),
            dispatched_at,
        },
        // Finished and any other reason ends the turn. `Canceled` arrives
        // here only when the completing generation was never cancelled
        // through the turn path (a stream the provider stopped): the same
        // settle applies.
        R::Finished | R::Canceled | R::RuleIntercept => PhaseCommand::StreamEndedFinished {
            session_id: session_id.clone(),
            dispatched_at,
        },
    };
    match jinn_kernel::common::phase_command::apply_phase(services, command).await {
        Ok(decision) => decision,
        Err(report) => {
            tracing::error!(
                session_id = %session_id,
                ?report,
                "settle ask failed; the completion's edge could not be applied"
            );
            PhaseDecision::refused()
        }
    }
}

/// Asks the phase actor to cancel a turn. The reply's `admitted` is
/// false exactly when the turn had already ended — the once-ness of a
/// turn's end lives here, in the turn record.
pub(crate) async fn cancel_turn(
    services: &jinn_kernel::common::services::Services,
    session_id: &SessionId,
) -> Result<PhaseDecision, error_stack::Report<jinn_kernel::common::phase_command::PhaseApplyError>>
{
    jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::TurnCanceled {
            session_id: session_id.clone(),
        },
    )
    .await
}

/// Asks the phase actor to rewind for a rule intercept's re-dispatch,
/// resolving against the interrupted generation's stamp. A stale or
/// cancelled generation is refused.
pub(crate) async fn admit_rewind(
    services: &jinn_kernel::common::services::Services,
    session_id: &SessionId,
    dispatched_at: jiff::Timestamp,
) -> PhaseDecision {
    match jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::InterceptRewind {
            session_id: session_id.clone(),
            dispatched_at,
        },
    )
    .await
    {
        Ok(decision) => decision,
        Err(report) => {
            tracing::error!(
                session_id = %session_id,
                ?report,
                "phase rewind ask failed; refusing the resume"
            );
            PhaseDecision::refused()
        }
    }
}

pub fn spawn(system: &trouper::system::ActorSystem, state: State, bus: BusService) -> ActorPath {
    trouper::builder::spawn_service_builder::<SessionPhaseActor>(system)
        .at(ActorPath::new(SESSION_PHASE_PATH))
        .start_with({
            let state = state.clone();
            let bus = bus.clone();
            move || {
                let state = state.clone();
                let bus = bus.clone();
                Box::pin(async move { Ok(SessionPhaseActor::new(state, bus)) })
            }
        })
        .handles::<PhaseCommand>()
        // Ask replies leave the handler through ctx.reply; phase-change
        // events leave through publish. The flush gate drops any
        // outbound type not declared here.
        .emits::<PhaseDecision>()
        .emits::<jinn_session_msg::SessionPhaseChanged>()
        .emits::<jinn_session_msg::WorkStateChanged>()
        .mailbox(64, trouper::inbox::OverloadPolicy::Block)
        .start()
}

/// Spawns the actor unless its path is already live. Test harnesses
/// compose the same system through several constructors; re-spawning an
/// identical manifest trips trouper's once-only path invariant, which
/// this tolerates by keeping the first registration.
#[must_use]
pub fn ensure_spawned(
    system: &trouper::system::ActorSystem,
    state: State,
    bus: BusService,
) -> Option<ActorPath> {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(system, state, bus)));
    result.ok()
}
