//! Persona rescan actor tests (P14 from the spec).
//!
//! Every test drives the real bus through `TestHarness`: the actor is
//! spawned the way composition spawns it, `ResourcesChanged` is
//! published the way the config-watch slice publishes it, and the
//! assertion reads what the session actor's own `PersonasLoaded`
//! handler would consume — the republish is the actor's whole job.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use std::time::Duration;

use jinn_boot_msg::ResourcesChanged;
use jinn_kernel::Services;
use jinn_persona_msg::PersonasLoaded;
use jinn_slices::route::BusMessage;
use jinn_testutil::bus_harness::Recorder;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::temp::tempdir;
use marked_path::MarkedPathAccess as _;

use crate::persona_rescan_actor::PersonaRescanActor;

/// The bus round-trip budget.
const SETTLE_BUDGET: Duration = Duration::from_millis(2_000);

/// A harness with the actor spawned over a temp personas dir.
struct RescanFixture {
    harness: TestHarness,
    services: Services,
    /// The personas dir the actor's services resolve to.
    personas_dir: marked_path::MarkedPathBuf<marked_path::Absolute>,
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
        let personas_dir = services.paths.personas_dir();
        Self {
            harness,
            services,
            personas_dir,
            _temp: temp,
        }
    }

    /// Spawns the rescan actor the way composition does.
    fn spawn_actor(&self) {
        PersonaRescanActor::spawn(self.harness.system(), self.services.clone());
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

    /// Writes one parseable persona file into the watched dir (TOML
    /// frontmatter, the format the slice's parser reads).
    fn write_persona(&self, stem: &str, name: &str, description: &str) {
        std::fs::create_dir_all(self.personas_dir.as_path()).expect("personas dir creates");
        std::fs::write(
            self.personas_dir
                .join(format!("{stem}.md"))
                .expect("name join keeps the base absolute"),
            format!("+++\nname = \"{name}\"\ndescription = \"{description}\"\n+++\n\n{name} body."),
        )
        .expect("persona file writes");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_resources_changed_event_republishes_the_scanned_catalog() {
    // Given a personas dir holding one file and a spawned rescan actor.
    let fixture = RescanFixture::new().await;
    let loaded = fixture.recorder_of::<PersonasLoaded>().await;
    fixture.write_persona("reviewer", "reviewer", "Reviews changes");
    fixture.spawn_actor();

    // When ResourcesChanged lands (published by the config-watch slice).
    fixture.harness.publish(ResourcesChanged).await;

    // Then the actor republishes PersonasLoaded with the fresh scan.
    let events = jinn_testutil::wait::await_messages(&loaded, 1, SETTLE_BUDGET).await;
    assert_eq!(events.len(), 1, "the rescan republished the catalog");
    let event = events.first().expect("the republished catalog event");
    assert_eq!(event.error, None);
    assert_eq!(event.personas.len(), 1, "one persona file was scanned");
    assert_eq!(
        event.personas.first().map(|p| p.name.as_str()),
        Some("reviewer")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_removal_is_reflected_in_the_republished_catalog() {
    // Given a personas dir whose persona was already scanned once.
    let fixture = RescanFixture::new().await;
    let loaded = fixture.recorder_of::<PersonasLoaded>().await;
    fixture.write_persona("reviewer", "reviewer", "Reviews changes");
    fixture.spawn_actor();
    fixture.harness.publish(ResourcesChanged).await;
    let first = jinn_testutil::wait::await_messages(&loaded, 1, SETTLE_BUDGET).await;
    assert_eq!(first.len(), 1, "the first republish landed");

    // When the file is removed and ResourcesChanged lands again.
    std::fs::remove_file(
        fixture
            .personas_dir
            .join("reviewer.md")
            .expect("name join keeps the base absolute")
            .as_path(),
    )
    .expect("persona removes");
    fixture.harness.publish(ResourcesChanged).await;

    // Then the next republish carries the empty scan — the session
    // actor's handler rewrites the cell and falls every stored persona
    // back through its existing missing-persona path.
    let second = jinn_testutil::wait::await_messages(&loaded, 1, SETTLE_BUDGET).await;
    assert_eq!(second.len(), 1, "the post-removal republish landed");
    assert!(
        second
            .first()
            .is_some_and(|event| event.personas.is_empty()),
        "the removed persona is gone from the catalog"
    );
}
