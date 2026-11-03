//! The actor-on-the-bus suite — kickoff row publications, platform-blind.
//!
//! Drives the real [`DictationActor::spawn`] against [`TestHarness`]
//! with the suite's stub backend: no network, no audio device, and no
//! linux gate — the spawn takes any `Box<dyn DictationBackend>`, and
//! the bus recorder sees what the dashboard sees. The kickoff toggle
//! is the publish-first seam: the spawn-time init state reached the
//! cell with no fold, so the kickoff is what carries it to the
//! dashboard row.

#![allow(clippy::expect_used, reason = "test code, panics are acceptable")]

use std::time::Duration;

use crate::config::DictationSettings;
use crate::dictation_actor::DictationActor;
use crate::dictation_actor::DictationActorDeps;
use crate::machine_tests::StubBackend;
use jinn_dictation_msg::DictationLevel;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::dictation_level_slot;
use jinn_dictation_msg::dictation_state_slot;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::state::State;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::Slices;
use jinn_slices::TypedCell;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::bus_harness::await_recorded;

/// The wait budget for a kickoff publication.
const WAIT: Duration = Duration::from_secs(10);

/// The model the actor's spawn validates against: the platform's
/// supported set carries this name on every platform, and the stub
/// backend answers `is_cached`/`fetch`/`load` without caring which
/// model it was built for.
const MODEL: &str = match *crate::platform::SUPPORTED_MODELS {
    [model, ..] => model,
    [] => panic!("the platform supported-model set is never empty"),
};

/// The actor's kickoff publication, driven through the bus harness.
struct KickoffFixture {
    harness: TestHarness,
    /// The write handle the test minted; the actor holds its own
    /// handle from the deps. Held so the registry entry's writer stays
    /// alive for the whole test.
    _state_cell: TypedCell<DictationState>,
}

impl KickoffFixture {
    /// Spawns the dictation actor on a fresh harness with the stub
    /// backend, its cells minted locally (the catalog is a composition
    /// concern this suite does not need).
    async fn spawn(backend: StubBackend) -> Self {
        let harness = TestHarness::new().await;
        let slices = Slices::new();
        let state_cell = slices
            .register(dictation_state_slot(), DictationState::default())
            .expect("fresh registry takes the state cell");
        let level_cell = slices
            .register(dictation_level_slot(), DictationLevel::default())
            .expect("fresh registry takes the level cell");
        let services = harness.services().await;
        DictationActor::spawn(
            harness.system(),
            DictationActorDeps {
                state_cell: state_cell.clone(),
                level_cell,
                bus: services.bus.clone(),
                runtime: tokio::runtime::Handle::current(),
                config: DictationSettings {
                    enabled: true,
                    device: None,
                    model: MODEL.to_owned(),
                    noise_reduction: false,
                },
                backend: Box::new(backend),
                state: State::new(AppState::default()),
            },
        );
        Self {
            harness,
            _state_cell: state_cell.clone(),
        }
    }
}

#[rstest::rstest]
#[tokio::test]
async fn kickoff_publishes_the_cold_cache_init_row() {
    // Given a spawned actor whose stub backend reports a cold cache —
    // init entered Fetching before the mailbox opened.
    let fixture = KickoffFixture::spawn(StubBackend::default()).await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the kickoff toggle arrives and its fold publishes first.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;

    // Then the dashboard row names the spawn-time state: the machine
    // is downloading the configured model.
    let expected = format!("downloading {MODEL}");
    assert!(
        events
            .iter()
            .any(|update| update.status_message.as_deref() == Some(expected.as_str())),
        "expected the kickoff to publish the Fetching note, saw: {:?}",
        events
            .iter()
            .map(|update| update.status_message.clone())
            .collect::<Vec<_>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn kickoff_publishes_the_warm_cache_init_row() {
    // Given a spawned actor whose stub backend reports a warm cache —
    // init entered Loading before the mailbox opened.
    let fixture = KickoffFixture::spawn(StubBackend {
        cached: true,
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the kickoff toggle arrives and its fold publishes first.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;

    // Then the dashboard row names the spawn-time state: loading.
    let expected = format!("loading {MODEL}");
    assert!(
        events
            .iter()
            .any(|update| update.status_message.as_deref() == Some(expected.as_str())),
        "expected the kickoff to publish the Loading note, saw: {:?}",
        events
            .iter()
            .map(|update| update.status_message.clone())
            .collect::<Vec<_>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_full_lifecycle_publishes_every_transition() {
    // Given a spawned actor on a warm cache whose stub load succeeds —
    // the kickoff's publish-first and the load fold both publish.
    let fixture = KickoffFixture::spawn(StubBackend {
        cached: true,
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the lifecycle runs to resident-and-idle: the kickoff
    // publishes the init note, the loaded tick's fold publishes Ready.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;
    let notes: Vec<_> = events
        .iter()
        .filter_map(|update| update.status_message.clone())
        .collect();

    // Then both transitions reached the dashboard. The exact ordering
    // is the fabric's; the set is the behavior.
    for expected in [format!("loading {MODEL}"), format!("ready ({MODEL})")] {
        assert!(
            notes.contains(&expected),
            "expected a {expected:?} note in the lifecycle publications, saw: {notes:?}"
        );
    }
}
