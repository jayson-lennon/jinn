//! Watcher behavior tests (W1–W12 from the spec).
//!
//! Every test drives the real path: real notify watches over a real temp
//! config directory, the real debounce, the real signature diff. A test
//! that faked the notify layer would prove nothing about the burst
//! coalescing and the atomic-rename handling the slice exists for.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use std::time::Duration;

use jinn_app_state::AppState;
use jinn_boot_msg::JinnTomlReloaded;
use jinn_boot_msg::ProvidersTomlReloaded;
use jinn_boot_msg::ResourcesChanged;
use jinn_kernel::Services;
use jinn_kernel::common::state::State;
use jinn_session_history_msg::PushChatEntry;
use jinn_slices::ServiceStatusUpdate;
use jinn_slices::route::BusMessage;
use jinn_stream_rules_msg::StreamRulesReloaded;
use jinn_testutil::bus_harness::Recorder;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::temp::tempdir;
use marked_path::MarkedPathAccess as _;
use marked_path::MarkedPathBuf;
use marked_path::Relative;

use crate::watch_actor::ConfigWatchActor;
use crate::watch_actor::DEBOUNCE;

/// The settle window plus slack for the notify round-trip.
const SETTLE_BUDGET: Duration = Duration::from_millis(DEBOUNCE.as_millis() as u64 * 4);

/// A harness over a temp config directory: every service the actor reads
/// points at the temp tree, so a test's writes are the watched reality.
struct WatchFixture {
    harness: TestHarness,
    services: Services,
    state: State,
    /// `<temp>/jinn` — the watched app-config dir.
    config_dir: marked_path::MarkedPathBuf<marked_path::Absolute>,
    /// The layer over the watched `jinn.toml`, for post-settle reads.
    layer: jinn_config::ConfigLayer,
    temp: jinn_testutil::temp::JinnTempDir,
}

impl WatchFixture {
    /// Builds the fixture: temp config dir, services rooted there, the
    /// config layer and provider storage over the watched files, watches
    /// live.
    async fn new(seed: &str) -> Self {
        let temp = tempdir();
        let harness = TestHarness::new().await;
        let state = State::new(AppState::default());

        let mut services = jinn_kernel::TestServices::builder()
            .paths(jinn_kernel::common::app_paths::AppPaths::new_in(
                &temp.path.as_marked_path(),
            ))
            .with_bus(harness.bus())
            .build();

        // The watched dir is whatever the services' paths name — derive it
        // from AppPaths rather than hand-building the layout, so the
        // fixture and the actor can never disagree about the tree.
        let config_dir = services.paths.app_config_dir();
        std::fs::create_dir_all(&config_dir).expect("config dir creates");
        std::fs::write(
            config_dir
                .join("jinn.toml")
                .expect("name join keeps the base absolute")
                .as_path(),
            seed,
        )
        .expect("seed jinn.toml");

        // Point the layer and the provider storage at the watched files:
        // a settle's `reload_validated` re-reads exactly what the test
        // writes, and the providers parse gate reads the same bytes.
        let layer_storage: std::sync::Arc<dyn jinn_config::ConfigDocumentStorage> =
            std::sync::Arc::new(jinn_config::FilesystemConfigStorage::new(abs_path(
                &config_dir.as_path().join("jinn.toml"),
            )));
        let layer = jinn_config::ConfigLayer::load(layer_storage).expect("layer loads");
        // A registered section gives the validation gate something to
        // enforce on candidate documents.
        layer.register::<jinn_preferences_config::schemas::StallWatchdogConfig>();
        services.config = layer.clone();
        services.config_storage = jinn_provider_config::ConfigStorageService::new(
            std::sync::Arc::new(jinn_provider_config::FilesystemConfigStorage::new(
                abs_path(&config_dir.as_path().join("providers.toml")),
            )),
        );
        let services: Services = services;

        Self {
            harness,
            services,
            state,
            config_dir,
            layer,
            temp,
        }
    }

    /// Spawns the watch actor and waits for the baseline row: it is the
    /// actor's last init act, so observing it proves the notify watches
    /// are bound before the test writes a byte. Call AFTER spawning
    /// recorders — a recorder is a declarant that must exist before the
    /// init-time baseline publish, or the baseline dead-letters.
    async fn spawn_actor(&self) {
        let notes = self.recorder_of::<ServiceStatusUpdate>().await;
        ConfigWatchActor::spawn(self.harness.system(), &self.services, &self.state)
            .expect("config-watch spawns");
        let baseline = jinn_testutil::wait::await_messages(&notes, 1, SETTLE_BUDGET).await;
        assert!(
            baseline.len() == 1,
            "config-watch baseline row never arrived; watches not established"
        );
    }

    /// A recorder for `M` on the fixture's bus.
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

    /// Waits until at least `min` events of `M` have accumulated,
    /// draining the recorder's buffer, within the settle budget. Fails
    /// the test (returns what arrived) on timeout — callers asserting
    /// absence use [`Self::assert_absent`] instead.
    async fn wait_for<M>(&self, recorder: &Recorder<M>, min: usize) -> Vec<M>
    where
        M: Clone + Send + 'static,
    {
        let collected = jinn_testutil::wait::await_messages(recorder, min, SETTLE_BUDGET).await;
        assert!(
            collected.len() >= min,
            "expected at least {min} events within {SETTLE_BUDGET:?}"
        );
        collected
    }

    /// Waits for `extra` *additional* events beyond what the recorder
    /// already holds, for tests that drive several settled edits in
    /// sequence.
    async fn wait_for_extra<M>(&self, recorder: &Recorder<M>, extra: usize) -> Vec<M>
    where
        M: Clone + Send + 'static,
    {
        let seen = recorder.len();
        let mut collected =
            jinn_testutil::wait::await_messages(recorder, seen + extra, SETTLE_BUDGET).await;
        collected.split_off(seen)
    }

    /// Asserts no further events arrive: the positive completion signal
    /// (`min` already seen) is observed first, so the absence claim is
    /// not made vacuously.
    async fn assert_absent<M>(&self, recorder: &Recorder<M>, seen: usize)
    where
        M: Clone + Send + 'static,
    {
        // Budget already elapsed since the positive signal; one more
        // settle window's worth of quiet is the proof.
        let stray = jinn_testutil::wait::await_messages(recorder, 1, SETTLE_BUDGET).await;
        assert!(
            stray.is_empty(),
            "expected no further events after {seen}; got {}",
            stray.len()
        );
    }

    /// Atomic save: temp file + rename, the editor pattern.
    fn atomic_write(&self, name: &str, contents: &str) {
        let tmp = self
            .config_dir
            .join(format!("{name}.tmp"))
            .expect("name join keeps the base absolute");
        std::fs::write(tmp.as_path(), contents).expect("write temp");
        std::fs::rename(
            tmp.as_path(),
            self.config_dir
                .join(name)
                .expect("name join keeps the base absolute")
                .as_path(),
        )
        .expect("rename into place");
    }

    /// A plain in-place write.
    fn write(&self, name: &str, contents: &str) {
        std::fs::write(
            self.config_dir
                .join(name)
                .expect("name join keeps the base absolute")
                .as_path(),
            contents,
        )
        .expect("write");
    }

    /// The watched jinn.toml's path.
    fn jinn_toml(&self) -> marked_path::MarkedPathBuf<marked_path::Absolute> {
        self.config_dir
            .join("jinn.toml")
            .expect("name join keeps the base absolute")
    }
}

fn rel(name: &str) -> MarkedPathBuf<Relative> {
    MarkedPathBuf::<Relative>::new(name).expect("valid relative name")
}

fn abs_path(path: &std::path::Path) -> MarkedPathBuf<marked_path::Absolute> {
    MarkedPathBuf::<marked_path::Absolute>::new(path).expect("absolute path")
}

// W1: an external edit to jinn.toml publishes JinnTomlReloaded.
#[rstest::rstest]
#[tokio::test]
async fn an_external_jinn_toml_edit_publishes_the_reload_event() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let recorder = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;

    // When the file is edited externally.
    fixture.atomic_write("jinn.toml", "[watchdog.stall]\ntimeout_secs = 2\n");

    // Then the reload event arrives within the settle budget.
    let events = fixture.wait_for(&recorder, 1).await;
    assert_eq!(events.len(), 1, "the edit published exactly one reload");
}

// W2: an atomic-rename save is observed (the editor pattern).
#[rstest::rstest]
#[tokio::test]
async fn an_atomic_rename_save_is_observed() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("").await;
    let recorder = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;

    // When the file lands via temp-file + rename.
    fixture.atomic_write("jinn.toml", "[other]\nkey = 1\n");

    // Then the reload event arrives — the rename burst was not missed.
    let events = fixture.wait_for(&recorder, 1).await;
    assert_eq!(events.len(), 1);
}

// W3: N rapid writes collapse into one settle.
#[rstest::rstest]
#[tokio::test]
async fn rapid_writes_collapse_into_one_settle() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("[a]\nx = 1\n").await;
    let recorder = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;

    // When five writes land back to back inside one window.
    for n in 1..=5 {
        fixture.atomic_write("jinn.toml", &format!("[a]\nx = {n}\n"));
    }

    // Then exactly one reload event arrives — the burst coalesced.
    let events = fixture.wait_for(&recorder, 1).await;
    assert_eq!(events.len(), 1, "one burst must settle once");
    // And no second settle follows the window.
    fixture.assert_absent(&recorder, 1).await;
}

// W4: a same-content rewrite publishes nothing.
#[rstest::rstest]
#[tokio::test]
async fn a_same_content_rewrite_publishes_nothing() {
    // Given a watched config directory and a first real edit.
    let fixture = WatchFixture::new("[a]\nx = 1\n").await;
    let recorder = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;
    fixture.atomic_write("jinn.toml", "[a]\nx = 2\n");
    let first = fixture.wait_for(&recorder, 1).await;
    assert_eq!(first.len(), 1, "the setup edit lands");

    // When the identical content is rewritten (a jinn no-op save).
    fixture.atomic_write("jinn.toml", "[a]\nx = 2\n");

    // Then no further reload arrives: the content hash did not change.
    fixture.assert_absent(&recorder, 1).await;
}

// W5: jinn's own real write (through the layer's storage) publishes.
#[rstest::rstest]
#[tokio::test]
async fn jinn_s_own_write_publishes_the_reload() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let recorder = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;

    // When jinn writes the watched file itself (the picker confirm's
    // storage boundary path).
    let storage =
        jinn_config::FilesystemConfigStorage::new(abs_path(fixture.jinn_toml().as_path()));
    let doc = "[watchdog.stall]\ntimeout_secs = 5\n"
        .parse()
        .expect("parses");
    jinn_config::ConfigDocumentStorage::write(&storage, &doc).expect("jinn's own write lands");

    // Then the watcher observes it like any external edit.
    let events = fixture.wait_for(&recorder, 1).await;
    assert_eq!(events.len(), 1, "jinn's own write propagates");
}

// W6: a malformed jinn.toml is refused — no event, refusal surfaced.
#[rstest::rstest]
#[tokio::test]
async fn a_malformed_jinn_toml_is_refused_and_surfaced() {
    // Given a watched config directory with a registered section.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    let notes = fixture.recorder_of::<ServiceStatusUpdate>().await;
    fixture.spawn_actor().await;

    // When the file is overwritten with bytes no TOML parser accepts.
    fixture.write("jinn.toml", "[watchdog.stall]\ntimeout_secs = \n");

    // Then no reload event is published, and the refusal is surfaced on
    // both the chat and the dashboard row.
    fixture.assert_absent(&reloads, 0).await;
    let pushed = fixture.wait_for(&entries, 1).await;
    assert_eq!(pushed.len(), 1, "the refusal reached the chat");
    let rows = fixture.wait_for(&notes, 1).await;
    assert!(
        rows.iter().any(|note| note
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("refused"))),
        "the refusal reached the dashboard row"
    );
}

// W7: valid TOML failing a registered section's validation is refused.
#[rstest::rstest]
#[tokio::test]
async fn a_validation_failure_is_refused_and_surfaced() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    fixture.spawn_actor().await;

    // When the file is edited to a shape the registered section rejects
    // (a string where a number belongs).
    fixture.write("jinn.toml", "[watchdog.stall]\ntimeout_secs = \"soon\"\n");

    // Then no reload event publishes, and the refusal reached the chat.
    fixture.assert_absent(&reloads, 0).await;
    let pushed = fixture.wait_for(&entries, 1).await;
    assert_eq!(pushed.len(), 1, "the refusal reached the chat");

    // And the layer still holds the last good value.
    let value = fixture
        .layer
        .get::<jinn_preferences_config::schemas::StallWatchdogConfig>()
        .expect("section reads");
    assert_eq!(
        value.effective_timeout_secs(),
        1,
        "the refused candidate never blanked the snapshot"
    );
}

// W8: an unparseable providers.toml publishes no provider event.
#[rstest::rstest]
#[tokio::test]
async fn an_unparseable_providers_toml_is_refused() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("").await;
    let reloads = fixture.recorder_of::<ProvidersTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    fixture.spawn_actor().await;

    // When providers.toml is written unparseable.
    fixture.write("providers.toml", "not = [valid\n");

    // Then no ProvidersTomlReloaded publishes — the parse gate held —
    // and the refusal reached the chat.
    fixture.assert_absent(&reloads, 0).await;
    let pushed = fixture.wait_for(&entries, 1).await;
    assert_eq!(pushed.len(), 1, "the providers refusal reached the chat");
}

// W9: deleting jinn.toml is a refusal — the running snapshot stays live.
#[rstest::rstest]
#[tokio::test]
async fn deleting_jinn_toml_is_a_refusal_and_surfaced() {
    // Given a watched config directory whose file holds content.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 7\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    fixture.spawn_actor().await;

    // When the file is deleted (missing hashes as empty — a settle
    // still fires, and the reload path now refuses).
    std::fs::remove_file(fixture.jinn_toml()).expect("delete");

    // Then no reload event publishes — a deletion must not reset the
    // running config to defaults — and the refusal is surfaced.
    fixture.assert_absent(&reloads, 0).await;
    let pushed = fixture.wait_for(&entries, 1).await;
    assert_eq!(pushed.len(), 1, "the deletion refusal reached the chat");

    // And the layer still holds the pre-deletion values.
    let value = fixture
        .layer
        .get::<jinn_preferences_config::schemas::StallWatchdogConfig>()
        .expect("section reads");
    assert_eq!(
        value.effective_timeout_secs(),
        7,
        "the deleted file's absence must not blank the snapshot"
    );

    // And when the file comes back (a sync client finishing), the
    // reload proceeds again.
    fixture.atomic_write("jinn.toml", "[watchdog.stall]\ntimeout_secs = 9\n");
    let events = fixture.wait_for(&reloads, 1).await;
    assert_eq!(events.len(), 1, "the restored file settles as a change");
}

// W9-companion: deleting providers.toml refuses rather than resetting
// to the default provider set (the storage load() would materialize a
// fresh default file on a miss).
#[rstest::rstest]
#[tokio::test]
async fn deleting_providers_toml_is_a_refusal() {
    // Given a watched config directory whose providers.toml exists and
    // has already settled (so the ledger holds its content signature).
    let fixture = WatchFixture::new("").await;
    let reloads = fixture.recorder_of::<ProvidersTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    fixture.spawn_actor().await;
    fixture.write(
        "providers.toml",
        "[providers.sample]\nbackend = \"sample\"\nmodels = [\"m\"]\n",
    );
    let settled = fixture.wait_for(&reloads, 1).await;
    assert_eq!(settled.len(), 1, "the write settled first");

    // When the file is deleted.
    std::fs::remove_file(
        fixture
            .config_dir
            .join("providers.toml")
            .expect("name join keeps the base absolute")
            .as_path(),
    )
    .expect("delete providers.toml");

    // Then no ProvidersTomlReloaded publishes and the refusal reaches
    // the chat.
    fixture.assert_absent(&reloads, 0).await;
    let pushed = fixture.wait_for(&entries, 1).await;
    assert!(
        pushed
            .iter()
            .any(|entry| entry.entry.text().contains("providers.toml")),
        "the providers deletion refusal reached the chat"
    );
}

// W9-companion: deleting stream_rules.toml is a legitimate reload —
// absence means "no rules file", which the matcher handles by keeping
// only the jinn.toml-sourced rules.
#[rstest::rstest]
#[tokio::test]
async fn deleting_stream_rules_toml_publishes_the_matcher_reload() {
    // Given a watched config directory whose rules file exists.
    let fixture = WatchFixture::new("").await;
    let reloads = fixture.recorder_of::<StreamRulesReloaded>().await;
    fixture.spawn_actor().await;
    fixture.write("stream_rules.toml", "[entry]\nname = \"a\"\n");
    fixture.wait_for(&reloads, 1).await;

    // When the file is deleted.
    std::fs::remove_file(
        fixture
            .config_dir
            .join("stream_rules.toml")
            .expect("name join keeps the base absolute")
            .as_path(),
    )
    .expect("delete stream_rules.toml");

    // Then the matcher-reload event still publishes: absent-as-empty is
    // a meaningful state for rules, and the matcher actor owns the
    // refuse-and-keep-old contract for anything bad in what remains.
    let events = fixture.wait_for_extra(&reloads, 1).await;
    assert_eq!(events.len(), 1, "the rules deletion settled as a change");
}

// W10: writes outside the watched set publish nothing.
#[rstest::rstest]
#[tokio::test]
async fn an_unrelated_dir_write_publishes_nothing() {
    // Given a watched config directory and an unrelated sibling dir.
    let fixture = WatchFixture::new("[a]\nx = 1\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    let providers = fixture.recorder_of::<ProvidersTomlReloaded>().await;
    let resources = fixture.recorder_of::<ResourcesChanged>().await;
    fixture.spawn_actor().await;
    let other_dir = fixture
        .temp
        .path
        .as_marked_path()
        .join_relative(&rel("other").as_marked_path());
    std::fs::create_dir_all(other_dir.as_path()).expect("sibling creates");

    // When noise lands out there and non-config files land in here.
    std::fs::write(other_dir.as_path().join("noise.txt"), "x").expect("noise write");
    fixture.write("state.toml", "[cache]\nkey = 1\n");
    fixture.write("jinn.log", "noise\n");

    // Then no event of any kind publishes: none are watched files.
    fixture.assert_absent(&reloads, 0).await;
    fixture.assert_absent(&providers, 0).await;
    fixture.assert_absent(&resources, 0).await;
}

// W11: a resource-dir change publishes ResourcesChanged.
#[rstest::rstest]
#[tokio::test]
async fn a_resource_dir_change_publishes_resources_changed() {
    // Given a watched config directory whose themes dir exists.
    let fixture = WatchFixture::new("").await;
    let resources = fixture.recorder_of::<ResourcesChanged>().await;
    fixture.spawn_actor().await;
    // The actor's establishment creates every resource dir under the
    // services' paths — resolve the themes dir the same way it does.
    let themes = fixture
        .services
        .paths
        .themes_dir()
        .as_marked_path()
        .to_path_buf();
    std::fs::create_dir_all(&themes).expect("themes dir creates");

    // When a theme file lands there.
    std::fs::write(themes.as_path().join("solarized.toml"), "x = 1\n").expect("theme write");

    // Then ResourcesChanged arrives.
    let events = fixture.wait_for(&resources, 1).await;
    assert_eq!(events.len(), 1, "the resource change was broadcast");
}

// W12: the snapshot swap lands before the publish — a handler of
// JinnTomlReloaded reads post-reload values through the layer.
#[rstest::rstest]
#[tokio::test]
async fn the_reload_lands_before_subscribers_read() {
    // Given a watched config directory whose layer the settle reloaded.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    fixture.spawn_actor().await;

    // When the edit settles and the event arrives.
    fixture.atomic_write("jinn.toml", "[watchdog.stall]\ntimeout_secs = 9\n");
    let events = fixture.wait_for(&reloads, 1).await;
    assert_eq!(events.len(), 1);

    // Then a handler reading the layer now sees the new value: the
    // settle swapped the snapshot before publishing (the event ordering
    // the production handler relies on).
    let value = fixture
        .layer
        .get::<jinn_preferences_config::schemas::StallWatchdogConfig>()
        .expect("section reads");
    assert_eq!(
        value.effective_timeout_secs(),
        9,
        "the post-reload snapshot was in place before the subscriber read"
    );
}

// P3: a [shell] edit re-resolves and swaps the shells service; a path
// that cannot work is refused and the previous resolution stays live.
#[rstest::rstest]
#[tokio::test]
async fn a_shell_edit_swaps_the_service_and_a_bad_path_is_refused() {
    // Given a watched config directory and the shells service resolved
    // from an empty [shell] section.
    let fixture = WatchFixture::new("[watchdog.stall]\ntimeout_secs = 1\n").await;
    let reloads = fixture.recorder_of::<JinnTomlReloaded>().await;
    let entries = fixture.recorder_of::<PushChatEntry>().await;
    fixture.spawn_actor().await;
    let before = fixture.services.shells.read();

    // When the edit names a bash path that does not exist.
    fixture.atomic_write(
        "jinn.toml",
        "[shell]\nbash_path = \"/nonexistent/jinn-test-bash\"\n",
    );
    fixture.wait_for(&reloads, 1).await;

    // Then the previous resolution stays live and the refusal is named
    // in the chat.
    let after = fixture.services.shells.read();
    assert_eq!(
        before.bash().expect("baseline bash resolves").program(),
        after
            .bash()
            .expect("a bad path keeps the working shell")
            .program(),
    );
    let pushed = fixture.wait_for(&entries, 1).await;
    assert!(
        pushed
            .iter()
            .any(|entry| { entry.entry.text().contains("/nonexistent/jinn-test-bash") }),
        "the refusal names the bad path"
    );

    // When a second edit names a path that does exist.
    let real_bash = "/bin/bash";
    fixture.atomic_write(
        "jinn.toml",
        &format!("[shell]\nbash_path = \"{real_bash}\"\n"),
    );
    let reloads_again = fixture.wait_for_extra(&reloads, 1).await;
    assert_eq!(reloads_again.len(), 1, "the second edit settled");

    // Then the service carries the new resolution.
    let swapped = fixture.services.shells.read();
    assert_eq!(
        swapped.bash().expect("an existing path resolves").program(),
        real_bash,
        "the swap landed"
    );
}

// stream_rules.toml: a change publishes the matcher-reload event.
#[rstest::rstest]
#[tokio::test]
async fn a_stream_rules_toml_change_publishes_the_matcher_reload() {
    // Given a watched config directory.
    let fixture = WatchFixture::new("").await;
    let reloads = fixture.recorder_of::<StreamRulesReloaded>().await;
    fixture.spawn_actor().await;

    // When stream_rules.toml changes.
    fixture.write("stream_rules.toml", "[entry]\nname = \"a\"\n");

    // Then StreamRulesReloaded arrives — the matcher rebuilds without a
    // restart, the same event the picker's confirm path publishes.
    let events = fixture.wait_for(&reloads, 1).await;
    assert_eq!(events.len(), 1);
}
