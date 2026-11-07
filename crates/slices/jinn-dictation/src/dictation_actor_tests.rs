//! The actor-on-the-bus suite — kickoff row publications, platform-blind.
//!
//! Drives the real [`DictationActor::spawn`] against [`TestHarness`]
//! with the suite's stub backend and a scripted capture opener: no
//! network, no audio device, and no platform gate — the spawn takes any
//! `Box<dyn DictationBackend>`, arms open over a slot the test owns,
//! and the bus recorder sees what the dashboard sees. The kickoff
//! toggle is the publish-first seam: the spawn-time init state reached
//! the cell with no fold, so the kickoff is what carries it to the
//! dashboard row.

#![allow(clippy::expect_used, reason = "test code, panics are acceptable")]

use std::time::Duration;

use crate::capture::CaptureOpener;
use crate::capture::CaptureShared;
use crate::capture::SharedBackend;
use crate::config::DictationSettings;
use crate::dictation_actor::DictationActor;
use crate::dictation_actor::DictationActorDeps;
use crate::machine_tests::StubBackend;
use jinn_config::ConfigLayer;
use jinn_dictation_backend::Utterance;
use jinn_dictation_msg::DictationLevel;
use jinn_dictation_msg::DictationLifecycleTick;
use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::DictationToggle;
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
use std::str::FromStr as _;

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

/// A capture opener the tests script: `None` refuses every arm (the
/// deterministic no-device mode), `Some` hands back a live arm over a
/// slot the test owns — the actor's arm/loss/flush paths run against
/// it with no audio device in the loop.
struct ScriptedOpener {
    shared: Option<std::sync::Arc<parking_lot::Mutex<CaptureShared>>>,
}

impl CaptureOpener for ScriptedOpener {
    fn open(
        &self,
        configured: Option<&str>,
        _noise_reduction: bool,
        _backend: &SharedBackend,
    ) -> Result<crate::capture::CaptureArm, String> {
        match &self.shared {
            Some(shared) => Ok(crate::capture::CaptureArm::for_tests(
                std::sync::Arc::clone(shared),
            )),
            None => Err(crate::capture::arm_refusal_note(configured, &[])),
        }
    }

    fn name(&self) -> &'static str {
        "scripted"
    }
}

/// The shared capture slot a live-arm fixture hands its actor — a
/// cloneable handle the test scripts through the same free functions
/// the real audio callback uses.
#[derive(Clone, Default)]
struct SharedCapture(std::sync::Arc<parking_lot::Mutex<CaptureShared>>);

impl SharedCapture {
    fn queue_utterances(&self, texts: &[&str]) {
        crate::capture::push_utterances(
            &self.0,
            texts
                .iter()
                .map(|text| Utterance::new((*text).to_owned()))
                .collect(),
        );
    }

    fn report_loss(&self, reason: &str) {
        self.0.lock().lost = Some(reason.to_owned());
    }
}

/// The actor's kickoff publication, driven through the bus harness.
struct KickoffFixture {
    harness: TestHarness,
    /// The write handle the test minted; the actor holds its own
    /// handle from the deps. Held so the registry entry's writer stays
    /// alive for the whole test.
    state_cell: TypedCell<DictationState>,
    /// The actor's config layer, held for the reload tests to re-point
    /// at an edited document — the in-memory storage a jinn.toml edit
    /// would replace.
    config: ConfigLayer,
    /// The shared application state the actor commits into — retained
    /// so commit tests can read the input buffer afterwards.
    state: State,
}

impl KickoffFixture {
    /// Spawns the dictation actor on a fresh harness with the stub
    /// backend and an empty config layer, its cells minted locally (the
    /// catalog is a composition concern this suite does not need).
    /// The opener refuses — the deterministic no-device mode.
    async fn spawn(backend: StubBackend) -> Self {
        let (layer, applied) = default_layer_and_applied();
        Self::spawn_with_opener_and_layer(backend, ScriptedOpener { shared: None }, layer, applied)
            .await
    }

    /// Spawns the actor whose arms open over a slot the test owns: the
    /// live-arm mode the loss and flush tests drive. The returned
    /// [`SharedCapture`] is the same slot the actor's arm holds —
    /// level, loss, and queued utterances are scripted through it.
    async fn spawn_with_live_arm(backend: StubBackend) -> (Self, SharedCapture) {
        let (layer, applied) = default_layer_and_applied();
        let shared = SharedCapture::default();
        let fixture = Self::spawn_with_opener_and_layer(
            backend,
            ScriptedOpener {
                shared: Some(std::sync::Arc::clone(&shared.0)),
            },
            layer,
            applied,
        )
        .await;
        (fixture, shared)
    }

    /// Spawns the actor with an explicit layer and applied baseline and
    /// the refusing opener — the reload tests drive the layer variant.
    async fn spawn_with_layer(
        backend: StubBackend,
        config: ConfigLayer,
        applied: DictationSettings,
    ) -> Self {
        Self::spawn_with_opener_and_layer(backend, ScriptedOpener { shared: None }, config, applied)
            .await
    }

    /// Full construction with an explicit opener, layer, and baseline.
    async fn spawn_with_opener_and_layer(
        backend: StubBackend,
        opener: ScriptedOpener,
        config: ConfigLayer,
        applied: DictationSettings,
    ) -> Self {
        let harness = TestHarness::new().await;
        let slices = Slices::new();
        let state_cell = slices
            .register(dictation_state_slot(), DictationState::default())
            .expect("fresh registry takes the state cell");
        let level_cell = slices
            .register(dictation_level_slot(), DictationLevel::default())
            .expect("fresh registry takes the level cell");
        // The commit path's cell: production sessions read and write the
        // input draft through the slice registry, so the fixture's state
        // must carry the chat-inputs cell too — `AppState::default()`
        // ships no registry, and `update_input` is a no-op without one.
        slices
            .register(
                jinn_chat_input_msg::chat_inputs_slot(),
                jinn_chat_input_msg::ChatInputs::new(),
            )
            .expect("fresh registry takes the chat-inputs cell");
        let state = State::new(AppState::default());
        state.read().session.attach_slices(slices);
        let services = harness.services().await;
        DictationActor::spawn(
            harness.system(),
            DictationActorDeps {
                state_cell: state_cell.clone(),
                level_cell,
                bus: services.bus.clone(),
                runtime: tokio::runtime::Handle::current(),
                config: config.clone(),
                applied,
                backend: Box::new(backend),
                opener: std::sync::Arc::new(opener),
                state: state.clone(),
            },
        );
        Self {
            harness,
            state_cell: state_cell.clone(),
            config,
            state,
        }
    }

    /// Points the actor's layer at a fresh document, the way a watched
    /// file edit swaps the backing storage under the same layer.
    fn replace_layer_storage(&self, storage: std::sync::Arc<jinn_config::InMemoryConfigStorage>) {
        self.config
            .use_storage(storage)
            .expect("the actor's layer re-reads the edited document");
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

#[rstest::rstest]
#[tokio::test]
async fn arm_resets_the_backend_before_capture_opens() {
    // Given a spawned actor on a warm cache whose opener refuses every
    // arm — the deterministic no-device mode — and the shared reset
    // counter its stub backend carries (cloned before the backend was
    // boxed, so it survives behind the trait object).
    let resets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = KickoffFixture::spawn(StubBackend {
        cached: true,
        resets: std::sync::Arc::clone(&resets),
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the machine runs to Ready and two toggles are pressed. Each
    // press arms: the stub's reset runs on the blocking context before
    // the scripted opener refuses, so the first reset is observable
    // once the first press's arm fold completes; the refusal returns
    // the machine to Ready, and the second press repeats the cycle.
    await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;
    fixture.harness.publish(DictationToggle).await;
    let deadline = std::time::Instant::now() + WAIT;
    while resets.load(std::sync::atomic::Ordering::SeqCst) < 1 {
        assert!(
            deadline.saturating_duration_since(std::time::Instant::now()) > Duration::ZERO,
            "the first arm never reset the backend"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    fixture.harness.publish(DictationToggle).await;
    while resets.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        assert!(
            deadline.saturating_duration_since(std::time::Instant::now()) > Duration::ZERO,
            "the second arm never reset the backend"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Then every arm reset the backend: two sessions, two resets — no
    // session can start on stale VAD state, whatever the device did.
    assert_eq!(
        resets.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "every arm must reset the backend before capture opens"
    );
}

/// Polls the fixture's state cell until the phase matches, within the
/// suite budget — the deadline pattern the reload-busy test uses, so
/// the 100 ms level-sync cadence never hard-codes a sleep.
async fn await_phase(cell: &TypedCell<DictationState>, expected: DictationPhase) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if cell.read().phase == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the machine never reached {expected:?}; cell reads {:?}",
            cell.read().phase
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[rstest::rstest]
#[tokio::test]
async fn capture_lost_tick_commits_queued_utterances_and_returns_to_ready() {
    // Given a spawned actor with a live arm whose shared slot carries
    // one queued utterance the backend closed before the loss.
    let (fixture, shared) = KickoffFixture::spawn_with_live_arm(StubBackend {
        cached: true,
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;
    await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;
    fixture.harness.publish(DictationToggle).await;
    await_phase(&fixture.state_cell, DictationPhase::Recording).await;
    shared.queue_utterances(&["pre-loss words"]);

    // When the capture-lost tick lands (published directly — the same
    // schema the real arm's error task uses, without the timing).
    fixture
        .harness
        .publish(DictationLifecycleTick {
            kind: "capture-lost".to_owned(),
            detail: Some("capture stream error: device gone".to_owned()),
        })
        .await;

    // Then the queued utterance was committed to the focused input and
    // the machine returned to Ready, carrying the reason.
    await_phase(&fixture.state_cell, DictationPhase::Ready).await;
    let buffer = fixture
        .state
        .read()
        .with_active_input(|input| input.text().to_owned(), String::new);
    assert!(
        buffer.contains("pre-loss words"),
        "a capture loss commits what decoded before it (got {buffer:?})"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn level_sync_tick_folds_a_loss_reported_in_the_shared_slot() {
    // Given a spawned actor with a live arm already Recording — the
    // 100 ms level-sync tick is scheduled while the arm lives.
    let (fixture, shared) = KickoffFixture::spawn_with_live_arm(StubBackend {
        cached: true,
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;
    await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;
    fixture.harness.publish(DictationToggle).await;
    await_phase(&fixture.state_cell, DictationPhase::Recording).await;

    // When the arm's slot reports the stream dead — what a vanished
    // device writes — and the actor's own tick drains it.
    shared.report_loss("capture stream error: vanished");

    // Then the tick's fold returned the machine to Ready with the
    // reason, without any press.
    await_phase(&fixture.state_cell, DictationPhase::Ready).await;
    let note = fixture.state_cell.read().error.clone();
    assert_eq!(
        note.as_deref(),
        Some("capture stream error: vanished"),
        "the loss reason landed on the state, not silently"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn disarm_flush_commits_the_trailing_utterance() {
    // Given a spawned actor with a live arm whose stub backend scripts
    // one trailing utterance out of the disarm flush.
    let (fixture, _shared) = KickoffFixture::spawn_with_live_arm(StubBackend {
        cached: true,
        flushed: Some(Utterance::new("trailing".to_owned())),
        ..StubBackend::default()
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;
    await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;

    // When a press arms and a second press disarms.
    fixture.harness.publish(DictationToggle).await;
    await_phase(&fixture.state_cell, DictationPhase::Recording).await;
    fixture.harness.publish(DictationToggle).await;
    await_phase(&fixture.state_cell, DictationPhase::Ready).await;

    // Then the flush's utterance was committed to the focused input.
    let buffer = fixture
        .state
        .read()
        .with_active_input(|input| input.text().to_owned(), String::new);
    assert!(
        buffer.contains("trailing"),
        "a disarm commits the flushed trailing utterance (got {buffer:?})"
    );
}

/// The fixture's default layer and baseline: an enabled `[dictation]`
/// with the suite's model, no device, no denoise. The plain and
/// live-arm spawns both start here; the reload tests replace either.
fn default_layer_and_applied() -> (ConfigLayer, DictationSettings) {
    let layer = ConfigLayer::load(std::sync::Arc::new(
        jinn_config::InMemoryConfigStorage::default(),
    ))
    .expect("empty layer loads");
    let applied = DictationSettings {
        enabled: true,
        device: None,
        model: MODEL.to_owned(),
        noise_reduction: false,
        options: jinn_dictation_backend::BackendOptions::default(),
    };
    (layer, applied)
}

/// Seeds a `[dictation]` section into a layer the reload tests can
/// mutate between spawns and publishes.
fn layer_with(dictation_toml: &str) -> ConfigLayer {
    let doc = toml_edit::DocumentMut::from_str(dictation_toml).expect("seed document parses");
    ConfigLayer::load(std::sync::Arc::new(
        jinn_config::InMemoryConfigStorage::new(doc),
    ))
    .expect("seeded layer loads")
}

#[rstest::rstest]
#[tokio::test]
async fn reload_while_offline_applies_at_the_next_lifecycle_move() {
    // Given an offline actor (disabled) whose layer now carries an
    // enabled section with the supported model.
    let backend = StubBackend {
        cached: true,
        ..StubBackend::default()
    };
    let layer = layer_with("[dictation]\nenabled = true\n");
    let fixture = KickoffFixture::spawn_with_layer(backend, layer.clone(), {
        let mut settings = DictationSettings::disabled();
        settings.noise_reduction = false;
        settings
    })
    .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the reload event lands.
    fixture
        .harness
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;

    // Then the machine re-ran its init gate under the new settings: the
    // row shows the cached model loading, not the disabled state.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;
    let notes: Vec<String> = events
        .iter()
        .filter_map(|update| update.status_message.clone())
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("loading") || note.contains("ready")),
        "the reload applied the new settings (got {notes:?})"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn reload_with_an_unsupported_model_lands_in_error() {
    // Given an offline enabled actor whose layer now names a model the
    // platform does not carry.
    let backend = StubBackend {
        cached: true,
        ..StubBackend::default()
    };
    let layer = layer_with("[dictation]\nenabled = true\nmodel = \"no-such-model\"\n");
    let fixture =
        KickoffFixture::spawn_with_layer(backend, layer.clone(), DictationSettings::disabled())
            .await;
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;

    // When the reload event lands.
    fixture
        .harness
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;

    // Then the machine's init gate refuses into Error, the same
    // refusal an activation-time typo gets — not a silent fallback.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;
    assert!(
        events.iter().any(|update| {
            update
                .status_message
                .as_deref()
                .is_some_and(|note| note.contains("failed") && note.contains("no-such-model"))
        }),
        "an unsupported model in a reload lands in Error naming the value"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn reload_with_an_unchanged_section_publishes_nothing() {
    // Given an actor whose applied settings match the layer.
    let applied = DictationSettings {
        enabled: false,
        device: None,
        model: MODEL.to_owned(),
        noise_reduction: true,
        options: jinn_dictation_backend::BackendOptions::default(),
    };
    let fixture =
        KickoffFixture::spawn_with_layer(StubBackend::default(), layer_with(""), applied).await;
    // Drain the kickoff publication before the event.
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;
    let kickoff = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;
    assert_eq!(kickoff.len(), 1, "kickoff drained");

    // When the reload event lands.
    fixture
        .harness
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Then nothing publishes: the diff found no change.
    let stray = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;
    assert!(
        stray.is_empty(),
        "an unchanged section must not republish the row (got {:?})",
        stray.iter().map(|u| &u.status_message).collect::<Vec<_>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn reload_while_busy_names_the_pending_change_and_the_next_arm_picks_it_up() {
    // Given an enabled actor that ran to Ready — a busy (non-Offline)
    // phase, deterministically reached from a warm cache — whose layer
    // now names a different device.
    let applied = DictationSettings {
        enabled: true,
        device: None,
        model: MODEL.to_owned(),
        noise_reduction: false,
        options: jinn_dictation_backend::BackendOptions::default(),
    };
    let fixture = KickoffFixture::spawn_with_layer(
        StubBackend {
            cached: true,
            ..StubBackend::default()
        },
        layer_with("[dictation]\nenabled = true\n"),
        applied,
    )
    .await;
    // The edit lands in the actor's OWN layer (an external write would
    // not be seen by this in-memory layer): the same layer, a device
    // now named. The arm at kickoff resolved no device; the reload
    // diff therefore sees one.
    let doc = toml_edit::DocumentMut::from_str(
        "[dictation]\nenabled = true\ndevice = \"usb-mic-reload-probe\"\n",
    )
    .expect("edit document parses");
    fixture.replace_layer_storage(std::sync::Arc::new(
        jinn_config::InMemoryConfigStorage::new(doc),
    ));
    let seen = fixture
        .harness
        .spawn_recorder::<ServiceStatusUpdate>()
        .await;
    // Run to Ready: kickoff note, then the loaded fold's Ready note.
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 2, WAIT).await;
    assert!(
        events.iter().any(|update| update
            .status_message
            .as_deref()
            .is_some_and(|note| note.contains("ready"))),
        "the machine reached Ready before the reload"
    );

    // When the reload lands while the machine is busy.
    fixture
        .harness
        .publish(jinn_boot_msg::JinnTomlReloaded)
        .await;
    let events = await_recorded::<ServiceStatusUpdate>(&seen, 1, WAIT).await;

    // Then the pending change is named, not applied.
    assert!(
        events.iter().any(|update| update
            .status_message
            .as_deref()
            .is_some_and(|note| note.contains("applies at the next dictation"))),
        "a busy machine names the pending change on its row (got {:?})",
        events.iter().map(|u| &u.status_message).collect::<Vec<_>>()
    );

    // And the next lifecycle move resolves the fresh settings: the arm
    // opens under the new device name, so the refusal (this host has no
    // such input) names it — the applied settings moved. The fold
    // publishes the pre-toggle row and the arming row synchronously,
    // and the device-naming refusal lands only after the blocking
    // device open, so a count-based wait can return before the note
    // exists (the race that failed the Windows host); the wait polls
    // until a note actually names the reloaded device, within the same
    // budget.
    fixture.harness.publish(DictationToggle).await;
    let deadline = std::time::Instant::now() + WAIT;
    let mut events: Vec<ServiceStatusUpdate> = Vec::new();
    while !events.iter().any(|update| {
        update
            .status_message
            .as_deref()
            .is_some_and(|note| note.contains("usb-mic-reload-probe"))
    }) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        events.extend(await_recorded::<ServiceStatusUpdate>(&seen, 1, remaining).await);
    }
    assert!(
        events.iter().any(|update| update
            .status_message
            .as_deref()
            .is_some_and(|note| note.contains("usb-mic-reload-probe"))),
        "the next arm resolved the reloaded device name (got {:?})",
        events.iter().map(|u| &u.status_message).collect::<Vec<_>>()
    );
}
