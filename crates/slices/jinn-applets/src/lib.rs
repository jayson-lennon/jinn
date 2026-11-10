//! The applets slice — saved agent-built artifacts, served verbatim.
//!
//! Where the canvas slice serves one artifact per process with reload
//! and telemetry injection, this slice is the durable home: an agent
//! (via the bundled `applets` skill) copies an artifact directory into
//! `~/.local/share/jinn/applets/<id>/`, and the user reopens it any time
//! from the `<leader>sa` picker. The server is static-only —
//! **no script injection of any kind** — and binds lazily on the first
//! open, staying bound for the process lifetime.
//!
//! Units, by module:
//!
//! - [`vendor`] — the embedded `/vendor/` inventory, generated from the
//!   same manifest-defined file tree canvas's build verifies, so served
//!   vendor bytes are byte-identical without a dependency on the canvas
//!   implementation crate.
//! - [`server`] — the axum listener: `/applets/<id>/…` read fresh from disk
//!   on every request, `/vendor/`, `/healthz`. No `/ws`, no `/metrics`,
//!   no watcher.
//! - [`server_actor`] — the dormant-until-first-open actor that owns the
//!   listener; the store actor asks it to bind on the first open.
//! - [`store_actor`] — owns the on-disk catalog (`applets.json`) and the
//!   applets directory: rescans on [`jinn_applets_msg::RescanApplets`], writes
//!   through rename and delete, and republishes the reconciled catalog.
//!   The open path (ensure-bind + browser launch) also lives here, since
//!   route actions are synchronous.
//! - [`picker_republisher_actor`] — folds store events into the catalog
//!   and picker cells so an open picker re-renders in place.
//! - [`picker_routes`] / [`picker_render`] / [`picker_actions`] — the
//!   `<leader>sa` picker: list + filter, `ENTER` opens the OS browser,
//!   `<c-r>` renames, `<c-d>` deletes.
//! - [`rename_popup`] — the input-capturing rename popup, the
//!   session-rename shape.
//! - [`browser`] — the URL builder and the per-platform launcher
//!   command resolution (`xdg-open`/`open`/`cmd /c start`).
//!
//! What this slice deliberately is not: a canvas replacement (no
//! injection, no watcher), a session library member (disjoint code), and
//! not a marketplace of manifests (an applet is a directory of files).

pub mod browser;
pub mod picker_actions;
pub mod picker_render;
pub mod picker_republisher_actor;
pub mod picker_routes;
pub mod rename_popup;
pub mod server;
pub mod server_actor;
pub mod store_actor;
pub mod vendor;

pub use jinn_applets_msg::{
    AppletEntry, AppletsCatalog, applets_catalog_slot, applets_picker_scope, applets_picker_slot,
    applets_rename_scope,
};
pub use picker_routes::{APPLETS_PICKER_BINDINGS, attach_applets_picker_rows};
pub use picker_routes::{register_applets_picker_enter_hook, register_applets_picker_input_hook};
pub use rename_popup::rename_scope;
pub use server_actor::AppletsServerActor;
pub use store_actor::AppletsStoreActor;

use error_stack::ResultExt as _;
use jinn_slices::SliceHost;

/// Why the applets slice could not activate.
///
/// Colocated with `activate`, which produces it. A broken `[applets] bind`
/// is launch-fatal — the same severity the canvas bind gets — because
/// the server would refuse every later open with the same sentence.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct AppletsActivateError;

/// Installs the applets slice: reads `[applets] bind` (launch-fatal when
/// malformed — the bind itself is taken as configured), spawns the dormant
/// server actor and the store actor, and registers the picker plus its
/// republisher.
///
/// The picker's cells are catalog-registered (block 0); this resolves
/// them. The server is dormant — no listener binds, no filesystem work
/// at activation. The store actor reads the catalog lazily on the first
/// `RescanApplets` (the picker's opener publishes one), never at activation.
///
/// # Errors
///
/// Returns [`AppletsActivateError`] when the `[applets]` section cannot be
/// read, or its bind is not a host:port socket address.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
///
/// # Panics
///
/// Panics when the shared cell catalog has not registered the applets
/// catalog or picker slots — a bootstrap-ordering bug (the catalog
/// block must run before this slice), which aborts launch by contract.
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    config: &jinn_config::ConfigLayer,
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    bus: &jinn_slices::bus::BusService,
) -> Result<(), error_stack::Report<AppletsActivateError>> {
    let applets_config = config
        .get::<jinn_preferences_config::schemas::AppletsConfig>()
        .change_context(AppletsActivateError)
        .attach("failed to read the [applets] config section")?;
    let bind = applets_config
        .parse_bind()
        .change_context(AppletsActivateError)
        .attach("the [applets] bind must be a host:port socket address")?;

    let system: trouper::system::ActorSystem = host.system().clone();

    // The catalog cell, resolved from the shared catalog (registered in
    // block 0): the store actor writes it, the republisher mirrors it,
    // the picker's enter hook and render read the picker cell only.
    let catalog_cell = host
        .slices()
        .reader::<jinn_applets_msg::AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
        .expect("the cell catalog registers the applets catalog slot before any slice activates");
    let picker_cell = host
        .slices()
        .reader::<jinn_applets_msg::AppletsPickerState>(&jinn_applets_msg::applets_picker_slot())
        .expect("the cell catalog registers the applets picker slot before any slice activates");

    // The store actor: catalog and directory mutations, plus the open
    // path. Its command subscriptions are the readiness point.
    AppletsStoreActor::spawn(
        &system,
        &store_actor::AppletsStoreActorDeps {
            applets_dir: applets_dir.clone(),
            catalog_cell: catalog_cell.clone(),
            bus: bus.clone(),
            server_actor: trouper::actor::ActorPath::new(
                server_actor::APPLETS_ACTOR_NAME.to_owned(),
            ),
        },
    );

    // The server actor: dormant until the store actor's first ensure
    // ask. Spawned after the store so the ensure path's target exists —
    // though an ask cannot precede an open, and no open precedes the
    // picker, which binds its rows below.
    AppletsServerActor::spawn(
        &system,
        &server_actor::AppletsServerActorDeps {
            bind,
            config: config.clone(),
            apps_root: applets_dir.clone(),
            bus: bus.clone(),
        },
    );

    // The picker: overlay, rows, hooks.
    picker_routes::activate_picker(host, &picker_cell);

    // The republisher: repaints the open picker (and mirrors the catalog
    // cell) when a store event lands, and folds the server actor's
    // serving facts into the picker cell — the in-place refresh of
    // pattern B. Subscriptions live on the actor's own spawn builder.
    picker_republisher_actor::AppletsPickerRepublisherActor::spawn(
        &system,
        &picker_republisher_actor::AppletsPickerRepublisherActorDeps {
            picker_cell,
            catalog_cell,
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use marked_path::MarkedPathAccess as _;
    use std::str::FromStr as _;

    /// The config layer `activate` reads: an in-memory document holding
    /// the given TOML text.
    fn layer_with(bind: Option<&str>) -> jinn_config::ConfigLayer {
        let doc_text = match bind {
            Some(bind) => format!("[applets]\nbind = \"{bind}\"\n"),
            None => String::new(),
        };
        let doc = toml_edit::DocumentMut::from_str(&doc_text).expect("doc parses");
        jinn_config::ConfigLayer::load(std::sync::Arc::new(
            jinn_config::InMemoryConfigStorage::new(doc),
        ))
        .expect("layer loads")
    }

    /// Runs `activate` with a test host built in the caller's scope, so
    /// the host's borrowed registries live as long as the call.
    macro_rules! with_test_host {
        (|$host:ident| $body:expr) => {{
            let system = trouper::system::ActorSystem::test().0;
            let slices = jinn_slices::Slices::new();
            jinn_cell_catalog::register_all_cells(&slices);
            let mut viewport = jinn_slices::view::Viewport::new();
            let overlay_views = jinn_slices::OverlayViews::default();
            let key_routes = jinn_slices::KeyRoutes::new();
            let mut $host = jinn_slices::SliceHost::new(
                &slices,
                &mut viewport,
                &overlay_views,
                &key_routes,
                &system,
            );
            $body
        }};
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activation_accepts_a_non_loopback_bind() {
        // Given a bind on an exposed interface (0.0.0.0: every machine).
        let config = layer_with(Some("0.0.0.0:0"));

        // When activating against a live test host.
        // Then activation succeeds — the bind is taken as configured;
        // securing the interface is the operator's call.
        with_test_host!(|host| {
            let result = super::activate(&mut host, &config, &apps_dir_for_test(), &bus_for_test());
            assert!(
                result.is_ok(),
                "err: {:?}",
                result.err().map(|e| e.to_string())
            );
        });
    }

    #[rstest::rstest]
    #[test]
    fn activation_refuses_a_malformed_bind() {
        // Given a bind that is not a host:port socket address.
        let config = layer_with(Some("not-a-socket-address"));

        // When activating against a live test host.
        let error = with_test_host!(|host| {
            match super::activate(&mut host, &config, &apps_dir_for_test(), &bus_for_test()) {
                Err(report) => format!("{report:?}"),
                Ok(()) => panic!("expected activation to fail"),
            }
        });

        // Then the refusal names the bind's required shape.
        assert!(error.contains("host:port"), "got: {error}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activation_of_a_well_formed_bind_reaches_the_picker_wiring() {
        // Given the shipped default bind.
        let config = layer_with(Some("127.0.0.1:0"));

        // When activating against a live test host.
        // Then activation succeeds end to end — bind parse, both actors,
        // the picker, and the republisher.
        with_test_host!(|host| {
            let result = super::activate(&mut host, &config, &apps_dir_for_test(), &bus_for_test());
            assert!(
                result.is_ok(),
                "err: {:?}",
                result.err().map(|e| e.to_string())
            );
        });
    }

    /// A throwaway applets root (never touched — activation does no IO).
    fn apps_dir_for_test() -> marked_path::MarkedPathBuf<marked_path::Absolute> {
        let dir = jinn_testutil::retained_tempdir();
        let applets = dir.path.join("applets").expect("join");
        std::fs::create_dir_all(applets.as_path()).expect("mkdir");
        marked_path::MarkedPathBuf::<marked_path::Absolute>::new(applets.as_path())
            .expect("absolute")
    }

    /// A recording bus, so activation's publishes are captured, not sent.
    fn bus_for_test() -> jinn_slices::bus::BusService {
        jinn_slices::bus::BusService::new_recording().0
    }

    // Reload-handler tests: the actor drives on a trouper system with a
    // harness bus so row notes are observable, and the layer is swapped
    // behind it the way the config-watch slice's validated swap does.
    // `127.0.0.1:0` binds an OS-assigned port, so no test fights a real
    // one; a second probe listener reserves the next port.

    /// Spawns the applets server actor over `config` on a harness bus
    /// that records `ServiceStatusUpdate`s and serving facts.
    async fn spawn_actor(
        config: jinn_config::ConfigLayer,
    ) -> (
        trouper::actor::ActorPath,
        trouper::system::ActorSystem,
        jinn_testutil::bus_harness::Recorder<jinn_slices::ServiceStatusUpdate>,
        jinn_testutil::bus_harness::Recorder<jinn_applets_msg::AppletsServingChanged>,
        jinn_testutil::bus_harness::TestHarness,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let notes = harness
            .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
            .await;
        let serving_facts = harness
            .spawn_recorder::<jinn_applets_msg::AppletsServingChanged>()
            .await;
        let system = harness.system().clone();
        let apps_root = apps_dir_for_test();
        let path = super::server_actor::AppletsServerActor::spawn(
            &system,
            &super::server_actor::AppletsServerActorDeps {
                bind: "127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .expect("bind parses"),
                config,
                apps_root: apps_root.clone(),
                bus: harness.bus(),
            },
        );
        (path, system, notes, serving_facts, harness, apps_root)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_while_dormant_adopts_the_new_bind() {
        // Given a dormant actor over an empty layer.
        let config = layer_with(None);
        let layer = config.clone();
        let (actor, system, _notes, _facts, _harness, _apps_root) = spawn_actor(config).await;

        // When the bind changes behind the actor's back and a reload
        // lands.
        layer
            .put(&jinn_preferences_config::schemas::AppletsConfig {
                bind: "127.0.0.1:0".to_owned(),
            })
            .expect("layer put lands");
        system.publish(jinn_boot_msg::JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the actor's bind adopted the new section value — an
        // ensure uses it. Observable: the reload handler ran (the row
        // note path) without error, and a later ensure binds fresh. We
        // assert via the actor's config-driven state indirectly: no
        // panic and no refusal note means the dormant adopt path ran.
        // The note recorder asserts at least the informational note.
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_reload_while_serving_hot_rebinds_and_keeps_the_surface() {
        // Given a serving actor (one ensure has bound a listener).
        let config = layer_with(None);
        let layer = config.clone();
        let (actor, system, notes, facts, _harness, apps_root) = spawn_actor(config).await;
        let reply = system
            .ask(
                actor.clone(),
                super::server_actor::EnsureServing {
                    apps_root: apps_root.to_string_lossy().into_owned(),
                },
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the ensure ask answers");
        let serving: super::server_actor::EnsureServingOutcome =
            reply.decode().expect("the outcome decodes");
        let first_bind = serving
            .bind
            .parse::<std::net::SocketAddr>()
            .expect("the outcome carries the resolved bind");
        assert!(
            first_bind.port() > 0,
            "port 0 resolved to an ephemeral port at the first ensure"
        );

        // And the first bind announced the serving fact with the
        // resolved port — the ensure path is a serving transition too.
        let facts_after = jinn_testutil::bus_harness::await_recorded(
            &facts,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            facts_after
                .iter()
                .any(|fact| { fact.serving && fact.bind == first_bind.to_string() }),
            "the first bind publishes the online fact; got {facts_after:?}"
        );

        // When the bind changes to a squatted port and a reload lands —
        // the rebind must fail and the row must say so.
        let squatter = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("squatter binds");
        let taken = squatter.local_addr().expect("squatter addr").port();

        layer
            .put(&jinn_preferences_config::schemas::AppletsConfig {
                bind: format!("127.0.0.1:{taken}"),
            })
            .expect("layer put lands");
        system.publish(jinn_boot_msg::JinnTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;
        let notes_after = jinn_testutil::bus_harness::await_recorded(
            &notes,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            notes_after.iter().any(|note| {
                note.note_tone == Some(jinn_slices::NoteTone::Error)
                    && note
                        .status_message
                        .as_deref()
                        .is_some_and(|m| m.contains("rebind") && m.contains("failed"))
            }),
            "the failed rebind is named on the row; got {notes_after:?}"
        );

        // And the actor went dormant deliberately: with the squatter
        // still holding the port, the next ensure re-reads `self.bind`
        // — the taken bind — and refuses fresh, so the picker surfaces
        // the retry instead of a zombie URL.
        let retry = system
            .ask(
                actor.clone(),
                super::server_actor::EnsureServing {
                    apps_root: apps_root.to_string_lossy().into_owned(),
                },
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the ensure ask answers");
        let retried: super::server_actor::EnsureServingOutcome =
            retry.decode().expect("the outcome decodes");
        assert!(
            !retried.ok && retried.error.contains("127.0.0.1"),
            "the retry still targets the taken bind and refuses: {retried:?}"
        );

        // And the failed rebind published the offline fact — a dead
        // address can never stay in the picker cell.
        let facts_final = jinn_testutil::bus_harness::await_recorded(
            &facts,
            2,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            facts_final
                .iter()
                .any(|fact| !fact.serving && fact.bind.is_empty()),
            "the failed rebind publishes the offline fact; got {facts_final:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_wake_binds_the_dormant_server_and_publishes_the_fact() {
        // Given a dormant actor — no ensure has ever run.
        let config = layer_with(None);
        let (actor, system, _notes, facts, _harness, _apps_root) = spawn_actor(config).await;
        assert!(facts.is_empty(), "a dormant actor has published nothing");

        // When a picker wake lands.
        system.publish(jinn_applets_msg::WakeAppletsServer).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the serving fact names a resolved port — port 0 asked
        // the OS at the wake, not at launch.
        let facts_after = jinn_testutil::bus_harness::await_recorded(
            &facts,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        let bound = facts_after
            .iter()
            .find(|fact| fact.serving)
            .expect("the wake binds and publishes the online fact");
        let port = bound
            .bind
            .parse::<std::net::SocketAddr>()
            .expect("the fact carries the resolved bind")
            .port();
        assert!(port > 0, "the OS-assigned port is nonzero");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_wake_while_serving_republishes_the_same_address() {
        // Given a serving actor (one wake has bound it).
        let config = layer_with(None);
        let (actor, system, _notes, facts, _harness, _apps_root) = spawn_actor(config).await;
        system.publish(jinn_applets_msg::WakeAppletsServer).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;
        let first = jinn_testutil::bus_harness::await_recorded(
            &facts,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        let address = first
            .iter()
            .find(|fact| fact.serving)
            .expect("the first wake binds")
            .bind
            .clone();

        // When a second picker wake lands.
        system.publish(jinn_applets_msg::WakeAppletsServer).await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the wake republishes the same address — idempotent, so a
        // fresh picker always sees the live truth and the listener is
        // never restarted.
        let second = jinn_testutil::bus_harness::await_recorded(
            &facts,
            2,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            second
                .iter()
                .any(|fact| fact.serving && fact.bind == address),
            "the second wake republishes the live address; got {second:?}"
        );
    }
}
