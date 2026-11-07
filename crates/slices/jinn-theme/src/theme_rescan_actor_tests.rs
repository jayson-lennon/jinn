//! Theme rescan actor tests.
//!
//! Every test drives the real bus through `TestHarness`: the actor is
//! spawned the way composition spawns it, `ResourcesChanged` is
//! published the way the config-watch slice publishes it, and the
//! assertion reads the `ThemeEntriesReloaded` event the app-state
//! actor's handler consumes. The ordered handoff — cell re-seed *then*
//! event — is the actor's whole job, so the tests verify the event and
//! the cell it guarantees, never the scan internals.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use std::time::Duration;

use jinn_boot_msg::ResourcesChanged;
use jinn_kernel::Services;
use jinn_slices::route::BusMessage;
use jinn_testutil::bus_harness::Recorder;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::temp::tempdir;
use jinn_theme_msg::ThemeEntries;
use jinn_theme_msg::ThemeEntriesReloaded;
use jinn_theme_msg::theme_entries_slot;
use marked_path::MarkedPathAccess as _;

use crate::theme_rescan_actor::ThemeRescanActor;

/// The bus round-trip budget.
const SETTLE_BUDGET: Duration = Duration::from_millis(2_000);

/// A harness with the actor spawned over a temp themes dir.
struct RescanFixture {
    harness: TestHarness,
    services: Services,
    /// The themes dir the actor's services resolve to.
    themes_dir: marked_path::MarkedPathBuf<marked_path::Absolute>,
    _temp: jinn_testutil::temp::JinnTempDir,
}

impl RescanFixture {
    async fn new() -> Self {
        let temp = tempdir();
        let harness = TestHarness::new().await;
        let services = jinn_kernel::TestServices::builder()
            .paths(jinn_kernel::common::app_paths::AppPaths::new_in(
                &temp.path.as_marked_path(),
            ))
            .with_bus(harness.bus())
            .build();
        // The builder constructs an empty registry; the rescan writes the
        // theme-entries cell, so seed the shared catalog the same way
        // production boot does.
        jinn_cell_catalog::register_all_cells(&services.slices);
        let themes_dir = services.paths.themes_dir();
        Self {
            harness,
            services,
            themes_dir,
            _temp: temp,
        }
    }

    /// Spawns the rescan actor the way composition does.
    fn spawn_actor(&self) {
        ThemeRescanActor::spawn(self.harness.system(), self.services.clone());
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

    /// Writes one parseable theme file into the watched dir (the shape
    /// the loader reads).
    fn write_theme(&self, stem: &str) {
        std::fs::create_dir_all(self.themes_dir.as_path()).expect("themes dir creates");
        std::fs::write(
            self.themes_dir
                .join(format!("{stem}.toml"))
                .expect("name join keeps the base absolute"),
            "focus_accent = \"red\"\n",
        )
        .expect("theme file writes");
    }

    /// Reads the entries cell the rescan re-seeds.
    fn entries_cell(&self) -> ThemeEntries {
        self.services
            .slices
            .reader::<ThemeEntries>(&theme_entries_slot())
            .expect("the cell catalog registers the theme-entries cell")
            .read()
            .clone()
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_resources_changed_event_reseeds_the_cell_and_publishes_the_fresh_scan() {
    // Given a themes dir holding one file and a spawned rescan actor.
    let fixture = RescanFixture::new().await;
    let reloaded = fixture.recorder_of::<ThemeEntriesReloaded>().await;
    fixture.write_theme("solarized");
    fixture.spawn_actor();

    // When ResourcesChanged lands (published by the config-watch slice).
    fixture.harness.publish(ResourcesChanged).await;

    // Then the actor publishes ThemeEntriesReloaded with the scan in
    // the cell, and the cell carries the scanned theme.
    let events = jinn_testutil::wait::await_messages(&reloaded, 1, SETTLE_BUDGET).await;
    assert_eq!(events.len(), 1, "the rescan published the fresh-scan event");
    let entries = fixture.entries_cell();
    assert!(
        entries.theme("solarized").is_some(),
        "the scanned theme is in the re-seeded cell"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_published_event_finds_the_scan_already_in_the_cell() {
    // Given a spawned rescan actor over an empty themes dir (the
    // baseline scan holds only the built-in default).
    let fixture = RescanFixture::new().await;
    let reloaded = fixture.recorder_of::<ThemeEntriesReloaded>().await;
    fixture.spawn_actor();

    // When a theme file lands and ResourcesChanged lands.
    fixture.write_theme("gruvbox-dark");
    fixture.harness.publish(ResourcesChanged).await;
    let events = jinn_testutil::wait::await_messages(&reloaded, 1, SETTLE_BUDGET).await;
    assert_eq!(events.len(), 1, "the rescan published");

    // Then by the time the event is observable the cell already holds
    // the new theme — the ordered handoff the app-state actor relies
    // on (it re-resolves on this event, never racing the rescan).
    let entries = fixture.entries_cell();
    assert!(
        entries.theme("gruvbox-dark").is_some(),
        "the event finds the scan already re-seeded"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_removal_is_reflected_in_the_reseeded_cell() {
    // Given a themes dir whose theme was already scanned once.
    let fixture = RescanFixture::new().await;
    let reloaded = fixture.recorder_of::<ThemeEntriesReloaded>().await;
    fixture.write_theme("solarized");
    fixture.spawn_actor();
    fixture.harness.publish(ResourcesChanged).await;
    let first = jinn_testutil::wait::await_messages(&reloaded, 1, SETTLE_BUDGET).await;
    assert_eq!(first.len(), 1, "the first rescan landed");
    assert!(fixture.entries_cell().theme("solarized").is_some());

    // When the file is removed and ResourcesChanged lands again.
    std::fs::remove_file(
        fixture
            .themes_dir
            .join("solarized.toml")
            .expect("name join keeps the base absolute")
            .as_path(),
    )
    .expect("remove theme file");
    fixture.harness.publish(ResourcesChanged).await;
    let again = jinn_testutil::wait::await_messages(&reloaded, 1, SETTLE_BUDGET).await;
    assert_eq!(again.len(), 1, "the second rescan published");

    // Then the re-seeded cell no longer lists the removed theme.
    assert!(
        fixture.entries_cell().theme("solarized").is_none(),
        "the removed theme is gone from the re-seeded cell"
    );
}
