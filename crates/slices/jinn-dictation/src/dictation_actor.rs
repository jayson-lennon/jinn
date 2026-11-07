//! The dictation actor — the one owner of the backend lifecycle.
//!
//! Spawns at activation, runs init through `.kickoff`, and drives the
//! [`DictationMachine`] on every [`DictationToggle`] and every backend
//! completion tick. Blocking backend calls (`fetch`, `load`, `flush`)
//! run under `spawn_blocking` — the actor awaits the join handle, so
//! its async context never blocks. The machine's state lands in the
//! dictation state cell (this actor is the sole writer), and the
//! dashboard row's note rides [`ServiceStatusUpdate`] on every
//! observable transition.
//!
//! The capture arm (device open, audio thread) lives in
//! [`crate::capture`]; the actor calls its `open` and consumes its
//! reports. This file holds only the lifecycle.
//!
//! Subscriptions: [`DictationToggle`] (the route action's press, and
//! the init kickoff) and [`DictationLifecycleTick`] (the self-addressed
//! completion tick, the watchdog pattern). Emissions:
//! [`ServiceStatusUpdate`]. One declarant per schema: only this actor
//! declares all three. The flush gate is a runtime silence, not a
//! compile error — every outbound schema must appear in `.emits`
//! below.

use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_dictation_backend::DictationBackend;
use jinn_dictation_msg::DictationLevel;
use jinn_dictation_msg::DictationLifecycleTick;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::DictationToggle;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::TypedCell;

use crate::machine::DictationMachine;

/// The actor's trouper path — also the dashboard row's name.
pub const DICTATION_PATH: &str = "dictation";

/// The shared backend slot: the actor and its blocking closures pass
/// the backend through this mutex, because `DictationBackend` methods
/// take `&mut self` and the calls run off the actor's context.
pub type SharedBackend = std::sync::Arc<parking_lot::Mutex<Box<dyn DictationBackend>>>;

/// Dependencies for [`DictationActor`].
pub struct DictationActorDeps {
    /// The dictation state cell — minted by the shared catalog; this
    /// actor holds the one write handle.
    pub state_cell: TypedCell<DictationState>,
    /// The meter-level cell — minted by the shared catalog; sole writer.
    pub level_cell: TypedCell<DictationLevel>,
    /// The message fabric, for the dashboard row's status publications.
    pub bus: jinn_slices::bus::BusService,
    /// The blocking runtime handle the backend calls hop through.
    pub runtime: tokio::runtime::Handle,
    /// The live configuration layer. `[dictation]` is resolved from it
    /// at each arm (the heavy tier's deferred application: a config
    /// edit applies at the next dictation, never mid-recording), not
    /// snapshotted at activation.
    pub config: jinn_config::ConfigLayer,
    /// The settings the activation resolution produced — the baseline
    /// the reload handler diffs the fresh resolution against.
    pub applied: crate::config::DictationSettings,
    /// The platform's backend, built once at activation.
    pub backend: Box<dyn DictationBackend>,
    /// The shared application state handle, for the utterance commits.
    /// The commit appends to the focused session's input buffer — the
    /// same routing rule a bracketed paste follows.
    pub state: jinn_kernel::common::state::State,
}

/// The dictation actor: lifecycle owner and cell writer.
pub struct DictationActor {
    /// The lifecycle state machine — the phase truth the cells mirror.
    machine: DictationMachine,
    /// The platform backend, shared with the blocking closures.
    backend: SharedBackend,
    /// The dictation state cell — the one write handle.
    state_cell: TypedCell<DictationState>,
    /// The meter-level cell — the one write handle. The capture arm's
    /// audio callback writes the level through the shared slot; the
    /// actor's tick drains it here, and disarm zeroes it.
    level_cell: TypedCell<DictationLevel>,
    /// The message fabric, for the row publications.
    bus: jinn_slices::bus::BusService,
    /// The blocking runtime handle.
    runtime: tokio::runtime::Handle,
    /// The shared application state, for utterance commits.
    state: jinn_kernel::common::state::State,
    /// The live configuration layer — `[dictation]` resolves from it at
    /// each arm, so a reload lands at the next dictation without any
    /// rebuild here.
    config: jinn_config::ConfigLayer,
    /// The settings the currently running lifecycle was configured
    /// under: the diff baseline the reload handler compares the fresh
    /// resolution against, and the fallback when a reload's own
    /// resolution fails.
    applied: crate::config::DictationSettings,
    /// The lifecycle move currently running, if any. The actor never
    /// queues a second while one is in flight: a toggle mid-fetch is an
    /// idempotent acknowledgement (the hint re-pushed synchronously by
    /// the route action; this fold a no-op).
    in_flight: InFlight,
    /// The live capture arm while Recording, dropped on disarm. Its
    /// drop closes the device.
    capture: Option<crate::capture::CaptureArm>,
}

/// Which blocking operation, if any, the actor is waiting on.
#[derive(Default, PartialEq, Eq)]
pub(crate) enum InFlight {
    #[default]
    None,
    Fetch,
    Load,
}

pub(crate) mod handlers;

impl ServiceActor for DictationActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("DictationActor spawns via init_with"))
    }
}

impl jinn_kernel::common::actor_deps::BusPublish for DictationActor {
    fn bus(&self) -> &jinn_slices::bus::BusService {
        &self.bus
    }
}

impl DictationActor {
    /// Spawns the actor at `dictation` with the init kickoff.
    ///
    /// The kickoff is a [`DictationToggle`] one millisecond after the
    /// mailbox opens: the actor's own first press lands on Offline and
    /// folds through the same matrix as a user's — the machine's
    /// enabled gate refuses to run when disabled (staying Offline), and
    /// an enabled machine enters Fetching or Loading exactly as the
    /// press matrix's arm cycle would.
    ///
    /// Subscriptions: [`DictationToggle`],
    /// [`DictationLifecycleTick`]. Emissions:
    /// [`ServiceStatusUpdate`].
    pub fn spawn(system: &ActorSystem, deps: DictationActorDeps) -> ActorPath {
        let path = ActorPath::new(DICTATION_PATH);
        let supported: Vec<String> = crate::platform::SUPPORTED_MODELS
            .iter()
            .map(|model| (*model).to_owned())
            .collect();
        let backend: SharedBackend = std::sync::Arc::new(parking_lot::Mutex::new(deps.backend));
        let (machine, init_action) = {
            let cached = backend.lock().is_cached();
            let mut machine = DictationMachine::new(&deps.applied.model, deps.applied.enabled);
            let step = machine.init(&supported, cached);
            deps.state_cell.update(|state| *state = step.state.clone());
            // The row publish for the init state rides the kickoff fold
            // below: every toggle fold publishes the row first, on the
            // state as the machine already holds it — and the init
            // phase reached the cell with no fold at all, so the
            // kickoff's publish-first is what the dashboard sees.
            (machine, step.action)
        };
        let actor = Self {
            machine,
            backend,
            state_cell: deps.state_cell,
            level_cell: deps.level_cell,
            bus: deps.bus,
            runtime: deps.runtime,
            state: deps.state,
            config: deps.config.clone(),
            applied: deps.applied,
            in_flight: InFlight::None,
            capture: None,
        };
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with(move || {
                Box::pin(async move {
                    // The init step's action runs on the live actor: a
                    // Fetch or Load moves launch here (the machine's
                    // kickoff toggle below folds only when the phase
                    // still allows it).
                    Ok(actor)
                })
            })
            .handles::<DictationToggle>()
            .handles::<DictationLifecycleTick>()
            // The reload handler diffs `[dictation]` against the applied
            // settings and, when the machine is between sessions,
            // applies them; mid-recording it only names the pending
            // change on the row.
            .handles::<jinn_boot_msg::JinnTomlReloaded>()
            .emits::<ServiceStatusUpdate>()
            .emits::<DictationLifecycleTick>()
            .kickoff(DictationToggle, std::time::Duration::from_millis(1))
            .mailbox(16, trouper::inbox::OverloadPolicy::Block)
            .start();
        let _ = init_action;
        path
    }

    /// Appends a committed utterance to the focused chat input.
    ///
    /// Through the shared paste-append helper in the crossing crate —
    /// the same implementation the bracketed-paste intent path runs —
    /// resolved against the *active* session's buffer: whichever
    /// session holds focus at commit time, recording surviving scope
    /// and session switches included. A committed fragment is spaced
    /// from the buffer's existing tail so words do not fuse.
    pub(crate) fn commit_utterance(&self, utterance: &jinn_dictation_backend::Utterance) {
        let text = utterance.text.clone();
        let mut state = self.state.write();
        state.update_active_input(|input| {
            let separator = if input.is_empty() { "" } else { " " };
            jinn_chat_input_msg::append_paste_to_buffer(input, &format!("{separator}{text}"));
        });
    }
}

impl MsgHandler<DictationToggle> for DictationActor {
    async fn handle(&mut self, _msg: &DictationToggle, ctx: &mut MsgCtx<'_>) {
        self::handlers::handle_toggle(self, ctx).await;
    }
}

impl MsgHandler<DictationLifecycleTick> for DictationActor {
    async fn handle(&mut self, msg: &DictationLifecycleTick, ctx: &mut MsgCtx<'_>) {
        self::handlers::handle_tick(self, msg, ctx).await;
    }
}

impl MsgHandler<jinn_boot_msg::JinnTomlReloaded> for DictationActor {
    async fn handle(&mut self, _msg: &jinn_boot_msg::JinnTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        self::handlers::handle_reload(self).await;
    }
}
