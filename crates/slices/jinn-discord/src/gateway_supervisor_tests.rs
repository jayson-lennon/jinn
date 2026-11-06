//! Gateway supervisor live-reload tests (P19 from the spec).
//!
//! Every test drives the supervisor the way the reload actor drives it
//! in production: `handle_reload` with a fresh section resolution, then
//! `handle_phase_changed` for the idle boundary. The observable state
//! is what the rest of jinn reads — the supervisor's state-machine
//! position and the `discord` slice flag — plus the row notes the
//! dashboard renders.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use std::time::Duration;

use crate::channels::DiscordGatewayChannels;
use jinn_cell_catalog::register_all_cells;
use jinn_kernel::Services;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_preferences_config::schemas::discord::DiscordConfig;
use jinn_slices::PhaseView;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::route::BusMessage;
use jinn_testutil::bus_harness::Recorder;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::temp::tempdir;

use crate::gateway_supervisor::GatewayState;
use crate::gateway_supervisor::GatewaySupervisor;

/// The bus round-trip budget.
const SETTLE_BUDGET: Duration = Duration::from_millis(2_000);

/// A harness: supervisor over fake services, with an attached no-op
/// spawner (the hook composition attaches; a test enable runs it).
struct Fixture {
    harness: TestHarness,
    supervisor: std::sync::Arc<tokio::sync::Mutex<GatewaySupervisor>>,
    /// The shared state the supervisor's busy check reads. `State`
    /// clones share the backing lock, so seeding here is what the
    /// supervisor observes.
    state: State,
    _temp: jinn_testutil::temp::JinnTempDir,
}

impl Fixture {
    /// A supervisor whose activation resolved `starting`.
    async fn new(starting: DiscordConfig) -> Self {
        let temp = tempdir();
        let harness = TestHarness::new().await;
        let services: Services = jinn_kernel::TestServices::builder()
            .paths(jinn_kernel::common::app_paths::AppPaths::new_in(
                &temp.path.as_marked_path(),
            ))
            .with_bus(harness.bus())
            .build();
        register_all_cells(&services.slices);
        let state = State::new(AppState::default());
        let supervisor = std::sync::Arc::new(tokio::sync::Mutex::new(GatewaySupervisor::new(
            DiscordGatewayChannels::detached(),
            starting,
            services.slices.clone(),
            state.clone(),
            services.bus.clone(),
            services.config.clone(),
        )));
        Self {
            harness,
            supervisor,
            state,
            _temp: temp,
        }
    }

    /// Attaches a no-op spawn hook: the enable path runs it without
    /// spawning a real gateway task — the state machine is the thing
    /// under test, not the gateway drain loop.
    async fn attach_noop_spawner(&self) {
        let mut supervisor = self.supervisor.lock().await;
        supervisor.attach_spawner(Box::new(|_handle, _shutdown| {}));
    }

    /// Puts one session mid-turn (`Streaming`) — the busy condition
    /// `any_session_busy` reads.
    fn make_busy(&self) {
        let session_id = {
            let guard = self.state.read();
            guard.session.active_session_id().clone()
        };
        self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .begin_streaming();
        });
    }

    /// Settles every session to `Idle` — the all-idle boundary.
    fn settle_all_idle(&self) {
        let session_id = {
            let guard = self.state.read();
            guard.session.active_session_id().clone()
        };
        self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .settle_to_idle();
        });
    }

    /// Folds a reload whose fresh section is `config` — the reload
    /// actor's exact call shape.
    async fn reload_with(&self, config: DiscordConfig) {
        self.supervisor.lock().await.handle_reload(&config).await;
    }

    /// Folds a phase transition — the reload actor's phase path.
    async fn phase(&self, phase: PhaseView) {
        self.supervisor
            .lock()
            .await
            .handle_phase_changed(phase)
            .await;
    }

    /// Reads the observable state: the state machine position plus the
    /// slice flag the rest of jinn reads.
    async fn observed(&self) -> (GatewayState, bool) {
        let supervisor = self.supervisor.lock().await;
        (*supervisor.gateway_state(), supervisor.flag())
    }

    async fn recorder_of<M>(&self) -> Recorder<M>
    where
        M: BusMessage
            + trouper::schema::Schema
            + serde::Serialize
            + serde::de::DeserializeOwned
            + Clone
            + Send
            + Sync
            + trouper::envelope::PayloadValue
            + 'static,
    {
        self.harness.spawn_recorder::<M>().await
    }
}

fn config(enabled: bool) -> DiscordConfig {
    DiscordConfig {
        enabled,
        ..DiscordConfig::default()
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_disable_while_a_session_is_busy_parks_with_a_pending_note() {
    // Given a running supervisor (enabled activation) with one mid-turn
    // session, and a recorder for row notes.
    let fixture = Fixture::new(config(true)).await;
    fixture.attach_noop_spawner().await;
    fixture.make_busy();
    let notes = fixture.recorder_of::<ServiceStatusUpdate>().await;

    // When a reload disables the bridge.
    fixture.reload_with(config(false)).await;

    // Then the disable parks — the flag stays set, nothing tore down —
    // and the row names the pending change.
    let (gateway, flag) = fixture.observed().await;
    assert_eq!(
        gateway,
        GatewayState::PendingDisable,
        "a busy disable parks instead of cutting the session"
    );
    assert!(flag, "the parked disable has not torn down yet");
    let rows = jinn_testutil::wait::await_messages(&notes, 1, SETTLE_BUDGET).await;
    assert!(
        rows.iter().any(|note| note
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("idle"))),
        "the pending disable is named on the row (got {rows:?})"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn re_enabling_cancels_a_parked_disable() {
    // Given a supervisor parked on a disable (busy session at the time).
    let fixture = Fixture::new(config(true)).await;
    fixture.attach_noop_spawner().await;
    fixture.make_busy();
    fixture.reload_with(config(false)).await;
    let (gateway, _) = fixture.observed().await;
    assert_eq!(gateway, GatewayState::PendingDisable);

    // When a second reload re-enables the bridge.
    fixture.reload_with(config(true)).await;

    // Then the parked disable is cancelled — back to Running, flag
    // still set, the gateway was never stopped.
    let (gateway, flag) = fixture.observed().await;
    assert_eq!(
        gateway,
        GatewayState::Running,
        "the re-enable cancels the pending disable"
    );
    assert!(flag, "the gateway was never stopped");
}

#[rstest::rstest]
#[tokio::test]
async fn the_parked_disable_fires_when_every_session_goes_idle() {
    // Given a supervisor parked on a disable with one mid-turn session.
    let fixture = Fixture::new(config(true)).await;
    fixture.attach_noop_spawner().await;
    fixture.make_busy();
    fixture.reload_with(config(false)).await;
    let (gateway, _) = fixture.observed().await;
    assert_eq!(gateway, GatewayState::PendingDisable);

    // When the session settles and the idle transition lands.
    fixture.settle_all_idle();
    fixture.phase(PhaseView::Idle).await;

    // Then the parked disable fired: the gateway stopped and the flag
    // cleared.
    let (gateway, flag) = fixture.observed().await;
    assert_eq!(
        gateway,
        GatewayState::Dormant,
        "the idle boundary fired the parked shutdown"
    );
    assert!(!flag, "the flag cleared with the shutdown");
}

#[rstest::rstest]
#[tokio::test]
async fn a_disable_while_every_session_is_idle_applies_immediately() {
    // Given a running supervisor over all-idle sessions.
    let fixture = Fixture::new(config(true)).await;
    fixture.attach_noop_spawner().await;

    // When a reload disables the bridge.
    fixture.reload_with(config(false)).await;

    // Then the disable applied outright — nothing to interrupt.
    let (gateway, flag) = fixture.observed().await;
    assert_eq!(gateway, GatewayState::Dormant);
    assert!(!flag, "the flag cleared with the shutdown");
}

#[rstest::rstest]
#[tokio::test]
async fn an_enable_while_dormant_spawns_through_the_hook() {
    // Given a dormant supervisor (disabled activation) with the hook
    // attached and a recorder for row notes.
    let fixture = Fixture::new(config(false)).await;
    fixture.attach_noop_spawner().await;
    let notes = fixture.recorder_of::<ServiceStatusUpdate>().await;

    // When a reload enables the bridge.
    fixture.reload_with(config(true)).await;

    // Then the gateway spawned through the hook: Running, flag set.
    let (gateway, flag) = fixture.observed().await;
    assert_eq!(
        gateway,
        GatewayState::Running,
        "the enable spawned without a restart"
    );
    assert!(flag);
    // And no row note claims something pending — the enable applied.
    let rows = jinn_testutil::wait::await_messages(&notes, 0, SETTLE_BUDGET).await;
    assert!(
        rows.is_empty(),
        "an unblocked enable publishes no pending note (got {rows:?})"
    );
}
