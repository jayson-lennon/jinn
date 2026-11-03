//! The dictation actor's handlers — one function per message, the
//! `MsgHandler` impl delegates here.
//!
//! The toggle matrix per current machine phase:
//!
//! - **Ready** — arm: the machine moves to Recording and the capture
//!   opens. A refused device open pulls the state back to Ready with
//!   the refusal note ([`DictationMachine::arm_failed`]) — never the
//!   terminal Error phase, because a missing or misnamed device is
//!   fixable without a restart.
//! - **Recording** — disarm: the backend flush runs on the blocking
//!   context (the actor awaits the join handle — no blocking call on
//!   its async context), the flushed utterance appends to the focused
//!   input through the shared paste-append helper, and the machine
//!   returns to Ready.
//! - **Offline** — disabled: nothing. The route action already pushed
//!   the disabled hint; this fold is the idempotence seam (init's
//!   kickoff arrives through the same schema).
//! - **Fetching / Loading** — busy: nothing. A press mid-flight is an
//!   acknowledgement; the hint re-pushed synchronously by the route
//!   action.
//! - **Error** — terminal: nothing, ever.
//!
//! Backend completions arrive as [`DictationLifecycleTick`] — the
//! self-addressed tick pattern the watchdog and search-index actors
//! use: the blocking task parks its outcome in the actor's completion
//! slot and schedules the tick, and the actor folds the outcome on the
//! tick's delivery.

use tokio::runtime::Handle;
use trouper::context::MsgCtx;

use super::DictationActor;
use super::InFlight;
use crate::machine::StepAction;
use jinn_dictation_msg::DictationLifecycleTick;

/// One backend completion or capture report, published as a tick.
enum Completion {
    /// The fetch finished; `Some` carries the rendered fault.
    Fetched(Option<String>),
    /// The load finished; `Some` carries the rendered fault.
    Loaded(Option<String>),
}

/// Publishes a completion as the lifecycle tick the actor folds.
///
/// The completion is an Event schema on the fabric, declared only by
/// this actor — the self-addressed tick pattern the watchdog uses, with
/// the outcome riding in the message instead of a parked slot.
async fn publish_tick(bus: &jinn_slices::bus::BusService, completion: Completion) {
    let (kind, detail) = match completion {
        Completion::Fetched(fault) => ("fetched", fault),
        Completion::Loaded(fault) => ("loaded", fault),
    };
    bus.publish(DictationLifecycleTick {
        kind: kind.to_owned(),
        detail,
    })
    .await;
}

/// Folds one toggle press onto the machine and runs the step.
///
/// The `Fetching` and `Loading` arms are also how init's work starts:
/// the machine entered those phases at spawn (the init step ran before
/// the mailbox opened), and the kickoff toggle — the same schema a user
/// press rides — launches the pending run. The in-flight guard makes a
/// second press a no-op, so a user pressing alt-i mid-download only
/// re-pushes the hint (the route action's synchronous half) and folds
/// nothing here.
pub(super) async fn handle_toggle(actor: &mut DictationActor, ctx: &mut MsgCtx<'_>) {
    match actor.machine.state().phase {
        jinn_dictation_msg::DictationPhase::Ready => {
            if actor.in_flight != InFlight::None {
                return;
            }
            let step = actor.machine.arm();
            apply(actor, &step).await;
            open_capture(actor).await;
            // While the arm lives, the tick rides every 100 ms: level
            // into the meter cell, loss drained into the machine.
            if actor.capture.is_some() {
                ctx.schedule_self(
                    DictationLifecycleTick {
                        kind: "level-sync".to_owned(),
                        detail: None,
                    },
                    std::time::Duration::from_millis(100),
                );
            }
        }
        jinn_dictation_msg::DictationPhase::Recording => {
            disarm_with_flush(actor, ctx).await;
        }
        jinn_dictation_msg::DictationPhase::Fetching => {
            if actor.in_flight == InFlight::None {
                actor.in_flight = InFlight::Fetch;
                run_fetch(actor);
            }
        }
        jinn_dictation_msg::DictationPhase::Loading => {
            if actor.in_flight == InFlight::None {
                actor.in_flight = InFlight::Load;
                run_load(actor);
            }
        }
        jinn_dictation_msg::DictationPhase::Offline | jinn_dictation_msg::DictationPhase::Error => {
        }
    }
}

/// Folds one lifecycle tick: the completion a blocking task published.
pub(super) async fn handle_tick(
    actor: &mut DictationActor,
    msg: &DictationLifecycleTick,
    ctx: &mut MsgCtx<'_>,
) {
    let fault = msg.detail.clone();
    match msg.kind.as_str() {
        "fetched" => {
            actor.in_flight = InFlight::None;
            let step = actor.machine.fetch_finished(fault);
            apply(actor, &step).await;
            // The machine's next step is Load; run it immediately
            // rather than waiting for a press.
            if step.action == StepAction::Load {
                actor.in_flight = InFlight::Load;
                run_load(actor);
            }
        }
        "loaded" => {
            actor.in_flight = InFlight::None;
            let step = actor.machine.load_finished(fault);
            apply(actor, &step).await;
        }
        "capture-lost" => {
            let reason = fault.unwrap_or_else(|| "capture device closed".to_owned());
            let step = actor.machine.capture_lost(&reason);
            close_capture(actor);
            apply(actor, &step).await;
        }
        "level-sync" => {
            // The meter's live level, and the arm's loss report: a
            // vanished device lands here as a capture-lost fold.
            if let Some(arm) = actor.capture.as_ref() {
                crate::capture::sync_level(arm.shared(), &actor.level_cell);
                if let Some(reason) = crate::capture::take_lost(arm.shared()) {
                    let step = actor.machine.capture_lost(&reason);
                    close_capture(actor);
                    apply(actor, &step).await;
                } else {
                    // Still recording: keep the tick alive.
                    ctx.schedule_self(
                        DictationLifecycleTick {
                            kind: "level-sync".to_owned(),
                            detail: None,
                        },
                        std::time::Duration::from_millis(100),
                    );
                }
            }
        }
        other => {
            tracing::warn!(kind = other, "unknown dictation lifecycle tick; ignored");
        }
    }
}

/// Disarms: flush on the blocking context, commit the trailing words,
/// back to Ready.
async fn disarm_with_flush(actor: &mut DictationActor, _ctx: &mut MsgCtx<'_>) {
    let backend = std::sync::Arc::clone(&actor.backend);
    let runtime = actor.runtime.clone();
    let outcome = runtime
        .spawn_blocking(move || {
            let mut backend = backend.lock();
            backend.flush().map_or_else(
                |report| (None, Some(format!("{report:?}"))),
                |utterance| (utterance, None),
            )
        })
        .await;
    let (flushed, fault) = outcome
        .unwrap_or_else(|join_error| (None, Some(format!("capture thread failed: {join_error}"))));
    let step = actor.machine.disarm(flushed.clone(), fault);
    if let Some(utterance) = flushed {
        actor.commit_utterance(&utterance);
    }
    close_capture(actor);
    apply(actor, &step).await;
}

/// Applies a step: write the state cell, publish the row, and run the
/// step's action. Async only because the row publication rides the bus.
async fn apply(actor: &mut DictationActor, step: &crate::machine::Step) {
    actor.state_cell.update(|state| *state = step.state.clone());
    crate::row::publish_row(&actor.bus, &actor.state_cell).await;
    match step.action {
        StepAction::Fetch => {
            actor.in_flight = InFlight::Fetch;
            run_fetch(actor);
        }
        StepAction::Load => {
            actor.in_flight = InFlight::Load;
        }
        StepAction::Idle | StepAction::Arm | StepAction::Disarm => {
            // Idle: nothing further. Arm: the capture open runs in the
            // toggle arm. Disarm: the close ran before this fold.
        }
    }
}

/// Runs the backend fetch on the blocking context, feeding progress
/// into the state cell as it arrives, then parking the completion.
///
/// The task owns the backend through the shared mutex for the fetch's
/// duration; the in-flight guard keeps every other lifecycle move off
/// it until the tick folds the completion.
pub(super) fn run_fetch(actor: &mut DictationActor) {
    let backend = std::sync::Arc::clone(&actor.backend);
    let runtime = actor.runtime.clone();
    let state_cell = actor.state_cell.clone();
    let bus = actor.bus.clone();
    runtime.spawn(async move {
        let outcome = Handle::current()
            .spawn_blocking(move || {
                let mut backend = backend.lock();
                backend
                    .fetch(&mut |progress| {
                        let percent = progress.percent();
                        state_cell.update(|cell| {
                            cell.fetch_progress_percent = percent;
                        });
                    })
                    .map_or_else(|report| Some(format!("{report:?}")), |()| None)
            })
            .await;
        let fault =
            outcome.unwrap_or_else(|join_error| Some(format!("fetch task failed: {join_error}")));
        publish_tick(&bus, Completion::Fetched(fault)).await;
    });
}

/// Runs the backend load on the blocking context, parking the
/// completion.
pub(super) fn run_load(actor: &mut DictationActor) {
    let backend = std::sync::Arc::clone(&actor.backend);
    let runtime = actor.runtime.clone();
    let bus = actor.bus.clone();
    runtime.spawn(async move {
        let outcome = Handle::current()
            .spawn_blocking(move || {
                let mut backend = backend.lock();
                backend
                    .load()
                    .map_or_else(|report| Some(format!("{report:?}")), |()| None)
            })
            .await;
        let fault =
            outcome.unwrap_or_else(|join_error| Some(format!("load task failed: {join_error}")));
        publish_tick(&bus, Completion::Loaded(fault)).await;
    });
}

/// Opens the capture arm: the device open, on the blocking context, so
/// a hung driver cannot stall the actor.
///
/// A refusal pulls the machine back to Ready with the refusal note
/// naming the configured device and the available inputs. On success
/// the arm handle parks on the actor; its drop (on disarm or capture
/// loss) closes the device.
pub(super) async fn open_capture(actor: &mut DictationActor) {
    let backend = std::sync::Arc::clone(&actor.backend);
    let configured = actor.config.device.clone();
    let noise_reduction = actor.config.noise_reduction;
    let outcome = {
        let runtime = actor.runtime.clone();
        runtime
            .spawn_blocking(move || {
                crate::capture::open(configured.as_deref(), noise_reduction, &backend)
            })
            .await
    };
    match outcome {
        Ok(Ok(arm)) => {
            actor.capture = Some(arm);
        }
        Ok(Err(reason)) => {
            let step = actor.machine.arm_failed(&reason);
            apply(actor, &step).await;
        }
        Err(join_error) => {
            let reason = format!("capture open task failed: {join_error}");
            let step = actor.machine.arm_failed(&reason);
            apply(actor, &step).await;
        }
    }
}

/// Drops the live capture arm and reports the meter's idle level.
pub(super) fn close_capture(actor: &mut DictationActor) {
    if actor.capture.take().is_some() {
        actor.level_cell.update(|cell| cell.level = 0.0);
    }
}
