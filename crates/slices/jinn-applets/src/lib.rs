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
/// malformed or non-loopback), spawns the dormant server actor and the
/// store actor, and registers the picker plus its republisher.
///
/// The picker's cells are catalog-registered (block 0); this resolves
/// them. The server is dormant — no listener binds, no filesystem work
/// at activation. The store actor reads the catalog lazily on the first
/// `RescanApplets` (the picker's opener publishes one), never at activation.
///
/// # Errors
///
/// Returns [`AppletsActivateError`] when the `[applets]` section cannot be
/// read, its bind is not a host:port socket address, or the bind is not
/// loopback (the server has no authentication, so a non-loopback bind is
/// refused at activation, not bound and warned).
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

    // Loopback only, no exceptions — the server has no authentication,
    // so it must not listen on an exposed interface. Refused here at
    // activation (launch-fatal), never bound and warned.
    if !bind.ip().is_loopback() {
        return Err(
            error_stack::Report::new(AppletsActivateError).attach(format!(
                "the configured [applets] bind {bind} is not a loopback address; \
             the saved-applets server has no authentication"
            )),
        );
    }

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
    AppletsServerActor::spawn(&system, &server_actor::AppletsServerActorDeps { bind });

    // The picker: overlay, rows, hooks.
    picker_routes::activate_picker(host, &picker_cell);

    // The republisher: repaints the open picker (and mirrors the catalog
    // cell) when a store event lands — the in-place refresh of pattern B.
    trouper::builder::spawn_service_builder::<
        picker_republisher_actor::AppletsPickerRepublisherActor,
    >(&system)
    .at(trouper::actor::ActorPath::new(
        picker_republisher_actor::AppletsPickerRepublisherActor::PATH.to_owned(),
    ))
    .init_with({
        move || {
            Box::pin(async move {
                Ok(
                    picker_republisher_actor::AppletsPickerRepublisherActor::new(
                        picker_cell,
                        catalog_cell,
                    ),
                )
            })
        }
    })
    .handles::<jinn_applets_msg::AppletsRescanned>()
    .handles::<jinn_applets_msg::AppletRenamed>()
    .handles::<jinn_applets_msg::AppletDeleted>()
    .start();

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
    #[test]
    fn activation_refuses_a_non_loopback_bind() {
        // Given an exposed-interface bind in the [applets] section.
        let config = layer_with(Some("192.168.1.10:0"));

        // When activating against a live test host.
        let error = with_test_host!(|host| {
            match super::activate(&mut host, &config, &apps_dir_for_test(), &bus_for_test()) {
                Err(report) => format!("{report:?}"),
                Ok(()) => panic!("expected activation to fail"),
            }
        });

        // Then the refusal names the loopback policy — the server has no
        // authentication, so it must not bind an exposed interface.
        assert!(error.contains("loopback"), "got: {error}");
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
        // Given a loopback bind (the shipped default).
        let config = layer_with(Some("127.0.0.1:0"));

        // When activating against a live test host.
        // Then activation succeeds end to end — bind parse, loopback
        // check, both actors, the picker, and the republisher.
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
}
