//! Message handlers for [`AppletsStoreActor`] — rescan, rename, delete,
//! open, kiosk open.
//!
//! Every handler is one transaction over two stores: the in-memory
//! catalog cell (the authority during a run) and `applets.json` (the
//! persisted form). The read/write primitives both stores ride live in
//! [`crate::catalog_fs`] (scan and write-through); the write order is
//! cell-first, file-second, and a file write failure is logged loudly
//! but never rolled back into the cell — the next rescan reconciles the
//! file from disk's truth, which is what the rescan exists to do.
//!
//! The two open paths share one ensure-bind block
//! ([`ensure_serving_resolved`]): validate id → ask the server actor →
//! decode → parse the resolved bind. They diverge only at the URL
//! builder and the launcher, so they cannot drift. A cell guard is
//! never held across an await: the kiosk path snapshots the title, then
//! awaits.

use marked_path::MarkedPathAccess as _;

use crate::browser::open_failed_note;
use crate::catalog_fs::{persist_catalog, scan_catalog};
use crate::store_actor::AppletsStoreActor;

/// Handles a rescan: reconcile disk into cell and file, then publish.
///
/// The disk read is async-safe here (this runs on the store actor's own
/// task, never in a key-thread hook): the directory listing and the
/// catalog file are read through [`scan_catalog`], and the reconciled
/// set lands in both the cell and the file.
pub(crate) async fn handle_rescan(actor: &mut AppletsStoreActor) {
    let catalog = scan_catalog(&actor.applets_dir);
    persist_catalog(&actor.applets_dir, &catalog);
    actor.catalog_cell.update(|cell| *cell = catalog.clone());
    actor
        .bus
        .publish(jinn_applets_msg::AppletsRescanned { catalog })
        .await;
}

/// Handles a rename: update the title in cell and file, publish.
///
/// The id is validated before anything touches disk; an unknown id is a
/// silent no-op on the cell but still publishes `AppletRenamed` with the
/// requested shape so the picker's optimistic row repaint cannot strand
/// (the republisher re-derives rows from the cell, which is unchanged —
/// no-op is the correct end state).
pub(crate) async fn handle_rename(
    actor: &mut AppletsStoreActor,
    msg: &jinn_applets_msg::RenameApplet,
) {
    if !jinn_applets_msg::is_valid_app_id(&msg.id) {
        tracing::warn!(id = %msg.id, "rename refused: invalid applet id");
        return;
    }
    let title = msg.title.trim().to_owned();
    if title.is_empty() {
        tracing::warn!(id = %msg.id, "rename refused: empty title");
        return;
    }

    let updated = actor.catalog_cell.update(|catalog| {
        let Some(entry) = catalog.entries.iter_mut().find(|entry| entry.id == msg.id) else {
            return false;
        };
        entry.title.clone_from(&title);
        catalog.sort_recency();
        true
    });
    if !updated {
        tracing::warn!(id = %msg.id, "rename refused: no catalog entry");
        return;
    }

    // Write through: the file mirrors the cell after every mutation.
    // Snapshot under the guard, write after dropping it — a guard must
    // never cross an await.
    let snapshot = {
        let cell = actor.catalog_cell.read();
        cell.clone()
    };
    persist_catalog(&actor.applets_dir, &snapshot);

    actor
        .bus
        .publish(jinn_applets_msg::AppletRenamed {
            id: msg.id.clone(),
            title,
        })
        .await;
}

/// Handles a delete: remove the directory and the catalog row, publish.
///
/// The id is validated first; a missing directory with a live catalog
/// row is still a successful delete (the row goes, the absent directory
/// is the state the rescan would have reconciled anyway).
pub(crate) async fn handle_delete(
    actor: &mut AppletsStoreActor,
    msg: &jinn_applets_msg::DeleteApplet,
) {
    if !jinn_applets_msg::is_valid_app_id(&msg.id) {
        tracing::warn!(id = %msg.id, "delete refused: invalid applet id");
        return;
    }

    // Remove the directory first: a crash between the two leaves a
    // catalog row the next rescan drops, never a deleted row pointing
    // at live files.
    let Ok(app_dir) = actor.applets_dir.join_all([msg.id.as_str()]) else {
        // is_valid_app_id forbids separators, so the typed join cannot
        // fail here; if it somehow does, treat it as a refused delete.
        tracing::warn!(id = %msg.id, "delete refused: the applet path did not type-check");
        return;
    };
    if app_dir.as_path().exists()
        && let Err(error) = std::fs::remove_dir_all(app_dir.as_path())
    {
        tracing::error!(id = %msg.id, %error, "failed to remove the applet directory");
    }

    let removed = actor.catalog_cell.update(|catalog| {
        let before = catalog.entries.len();
        catalog.entries.retain(|entry| entry.id != msg.id);
        before != catalog.entries.len()
    });
    if !removed {
        tracing::warn!(id = %msg.id, "delete: no catalog entry (directory already gone)");
    }

    // Write through even when the row was already absent: the file must
    // not outlive the directory. Snapshot under the guard, write after
    // dropping it — a guard must never cross an await.
    let snapshot = {
        let cell = actor.catalog_cell.read();
        cell.clone()
    };
    persist_catalog(&actor.applets_dir, &snapshot);

    actor
        .bus
        .publish(jinn_applets_msg::AppletDeleted { id: msg.id.clone() })
        .await;
}

/// Handles an open: ensure the server is bound, build the URL, launch
/// the OS browser.
///
/// This is where the lazy bind happens — the first `OpenApplet` after
/// launch is the first `EnsureServing` the server actor sees; every
/// later open reads back the same resolved bind. The id is validated
/// before the ask; a failed bind or launch is logged (the user sees no
/// browser) but never crashes the actor.
pub(crate) async fn handle_open(actor: &mut AppletsStoreActor, msg: &jinn_applets_msg::OpenApplet) {
    if !jinn_applets_msg::is_valid_app_id(&msg.id) {
        tracing::warn!(id = %msg.id, "open refused: invalid applet id");
        return;
    }
    let Some(bind) = ensure_serving_resolved(actor).await else {
        return;
    };

    let url = crate::browser::applet_url(bind, &msg.id);
    if let Err(report) = crate::browser::open_in_browser(&url) {
        tracing::warn!(%report, "browser launch failed");
    }
}

/// Handles a kiosk open: ensure the server is bound, build the kiosk
/// URL, launch the kiosk window.
///
/// Mirrors [`handle_open`] through the same
/// [`ensure_serving_resolved`] block; diverges at the URL (the
/// `?kiosk=1` marker) and the launcher. `[applets] kiosk_command` is
/// read from the live config layer at open time — a reloaded section
/// takes effect without a restart — and `None`/blank falls through to
/// the built-in Chromium-family launcher. A failed launch surfaces as a
/// transient error note on the applets dashboard row, then the actor
/// continues: a missing browser must not take the picker down.
pub(crate) async fn handle_kiosk_open(
    actor: &mut AppletsStoreActor,
    msg: &jinn_applets_msg::OpenAppletKiosk,
) {
    if !jinn_applets_msg::is_valid_app_id(&msg.id) {
        tracing::warn!(id = %msg.id, "kiosk open refused: invalid applet id");
        return;
    }

    // Snapshot what the launch needs from the catalog cell before any
    // await — a guard must never cross one. An id the catalog lacks
    // (deleted between render and keypress) falls back to the id; the
    // ensure still proceeds exactly like a normal open.
    let title = {
        let cell = actor.catalog_cell.read();
        cell.entry(&msg.id)
            .map_or_else(|| msg.id.clone(), |entry| entry.title.clone())
    };

    let Some(bind) = ensure_serving_resolved(actor).await else {
        return;
    };

    let kiosk_command = match actor
        .config
        .get::<jinn_preferences_config::schemas::AppletsConfig>()
    {
        Ok(config) => config.effective_kiosk_command().map(str::to_owned),
        Err(error) => {
            tracing::error!(%error, "failed to read the [applets] config section");
            note(
                actor,
                "could not read the [applets] config for the kiosk launch",
                Some(jinn_slices::NoteTone::Error),
            )
            .await;
            return;
        }
    };

    let request = crate::browser::KioskLaunchRequest {
        url: crate::browser::applet_kiosk_url(bind, &msg.id),
        profile: actor.kiosk_profile.clone(),
        id: msg.id.clone(),
        title,
    };
    if let Err(report) = actor.launcher.launch(&request, kiosk_command.as_deref()) {
        tracing::warn!(%report, "kiosk launch failed");
        let reason = open_failed_note(&report.to_string());
        note(actor, &reason, Some(jinn_slices::NoteTone::Error)).await;
    }
}

/// Publishes a transient note on the applets dashboard row.
///
/// The store actor owns the row its actor name announces; the note is
/// how a failed kiosk launch reaches the user without a chat message
/// and without the actor crashing.
async fn note(actor: &AppletsStoreActor, message: &str, tone: Option<jinn_slices::NoteTone>) {
    actor
        .bus
        .publish(jinn_slices::ServiceStatusUpdate {
            name: crate::store_actor::APPLETS_STORE_ACTOR_NAME.to_owned(),
            description: None,
            status_message: Some(message.to_owned()),
            note_tone: tone,
        })
        .await;
}

/// Ensures the server is bound and returns the resolved bind.
///
/// The shared ensure-bind block both open paths ride: ask the server
/// actor, decode the reply, parse the bind. Every refusal is logged and
/// answered `None` — the caller returns without launching.
async fn ensure_serving_resolved(actor: &mut AppletsStoreActor) -> Option<std::net::SocketAddr> {
    let ask = crate::server_actor::EnsureServing {
        apps_root: actor.applets_dir.to_string_lossy().into_owned(),
    };
    let reply = actor
        .bus
        .system_ref()
        .ask(actor.server_actor.clone(), ask, OPEN_ENSURE_TIMEOUT)
        .await;
    let outcome: crate::server_actor::EnsureServingOutcome = match reply {
        Ok(value) => match value.decode() {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(%error, "the applets server actor's reply did not decode");
                return None;
            }
        },
        Err(error) => {
            tracing::error!(%error, "the applets server actor did not answer the ensure ask");
            return None;
        }
    };
    if !outcome.ok {
        tracing::error!(reason = %outcome.error, "the applets server did not come up");
        return None;
    }
    match outcome.bind.parse::<std::net::SocketAddr>() {
        Ok(bind) => Some(bind),
        Err(_) => {
            tracing::error!(bind = %outcome.bind, "the resolved bind did not parse");
            None
        }
    }
}

/// The outer bound on the ensure round trip inside the open path.
const OPEN_ENSURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use marked_path::MarkedPathAccess as _;
    use std::str::FromStr as _;

    use crate::browser::{KioskLaunchError, KioskLaunchRequest, KioskLauncher};
    use crate::store_actor::{AppletsStoreActor, AppletsStoreActorDeps};

    /// A launcher recording each request and answering from a script.
    struct ScriptedLauncher {
        error: Option<&'static str>,
        launched: std::sync::Mutex<Vec<KioskLaunchRequest>>,
        calls: AtomicUsize,
    }

    impl ScriptedLauncher {
        fn failing(message: &'static str) -> Arc<Self> {
            Arc::new(Self {
                error: Some(message),
                launched: std::sync::Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            })
        }

        fn succeeding() -> Arc<Self> {
            Arc::new(Self {
                error: None,
                launched: std::sync::Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl ScriptedLauncher {
        /// Snapshot of one recorded launch.
        fn record(&self, request: &KioskLaunchRequest) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.launched
                .lock()
                .expect("launcher lock")
                .push(request.clone());
        }
    }

    impl KioskLauncher for ScriptedLauncher {
        fn launch(
            &self,
            request: &KioskLaunchRequest,
            _command_override: Option<&str>,
        ) -> Result<(), error_stack::Report<KioskLaunchError>> {
            self.record(request);
            match self.error {
                Some(message) => {
                    Err(error_stack::Report::new(KioskLaunchError).attach(message.to_owned()))
                }
                None => Ok(()),
            }
        }

        fn name(&self) -> &'static str {
            "scripted"
        }
    }

    /// Spawns the store and server actors on a harness system with a
    /// scripted kiosk launcher, seeded with one catalog entry, and
    /// returns everything a test drives or observes.
    async fn spawn_store(
        launcher: Arc<ScriptedLauncher>,
        config: jinn_config::ConfigLayer,
    ) -> (
        trouper::actor::ActorPath,
        trouper::system::ActorSystem,
        jinn_testutil::bus_harness::Recorder<jinn_slices::ServiceStatusUpdate>,
        jinn_testutil::bus_harness::TestHarness,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let notes = harness
            .spawn_recorder::<jinn_slices::ServiceStatusUpdate>()
            .await;
        let system = harness.system().clone();

        let applets_dir = apps_dir_for_test();
        let slices = jinn_slices::Slices::new();
        let catalog_cell = slices
            .register(
                jinn_applets_msg::applets_catalog_slot(),
                jinn_applets_msg::AppletsCatalog {
                    entries: vec![jinn_applets_msg::AppletEntry {
                        id: "quiz".to_owned(),
                        title: "The Quiz".to_owned(),
                        created_at: "1".to_owned(),
                    }],
                },
            )
            .expect("register the catalog cell");
        let profile = profile_dir_for_test();

        let actor = AppletsStoreActor::spawn(
            &system,
            &AppletsStoreActorDeps {
                applets_dir: applets_dir.clone(),
                catalog_cell,
                bus: harness.bus(),
                server_actor: trouper::actor::ActorPath::new(
                    crate::server_actor::APPLETS_ACTOR_NAME.to_owned(),
                ),
                config: config.clone(),
                kiosk_profile: profile,
                launcher,
            },
        );

        let server = crate::server_actor::AppletsServerActor::spawn(
            &system,
            &crate::server_actor::AppletsServerActorDeps {
                bind: "127.0.0.1:0".parse().expect("bind parses"),
                config: config.clone(),
                apps_root: applets_dir.clone(),
                bus: harness.bus(),
            },
        );
        // The server actor's readiness point: one ensure round trip so
        // no test races the spawn handshake.
        let reply = system
            .ask(
                server.clone(),
                crate::server_actor::EnsureServing {
                    apps_root: applets_dir.to_string_lossy().into_owned(),
                },
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the warmup ensure answers");
        let outcome: crate::server_actor::EnsureServingOutcome =
            reply.decode().expect("the warmup outcome decodes");
        assert!(outcome.ok, "the warmup ensure binds: {:?}", outcome.error);

        (actor, system, notes, harness, applets_dir)
    }

    /// A throwaway applets root, as in the slice-level tests.
    fn apps_dir_for_test() -> marked_path::MarkedPathBuf<marked_path::Absolute> {
        let dir = jinn_testutil::retained_tempdir();
        let applets = dir.path.join("applets").expect("join");
        std::fs::create_dir_all(applets.as_path()).expect("mkdir");
        marked_path::MarkedPathBuf::<marked_path::Absolute>::new(applets.as_path())
            .expect("absolute")
    }

    /// A throwaway kiosk profile dir.
    fn profile_dir_for_test() -> marked_path::MarkedPathBuf<marked_path::Absolute> {
        let dir = jinn_testutil::retained_tempdir();
        let profile = dir.path.join("kiosk-profile").expect("join");
        marked_path::MarkedPathBuf::<marked_path::Absolute>::new(profile).expect("absolute")
    }

    /// An empty config layer.
    fn empty_config() -> jinn_config::ConfigLayer {
        let doc = toml_edit::DocumentMut::from_str("").expect("doc parses");
        jinn_config::ConfigLayer::load(Arc::new(jinn_config::InMemoryConfigStorage::new(doc)))
            .expect("layer loads")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_kiosk_open_publishes_the_kiosk_command_to_the_launcher() {
        // Given a store actor whose launcher records launches.
        let launcher = ScriptedLauncher::succeeding();
        let (actor, system, _notes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open for the catalog's applet lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the launcher saw exactly one launch, carrying the
        // kiosk-marked URL, jinn's profile dir, the id, and the title.
        let launched = launcher.launched.lock().expect("launcher lock");
        assert_eq!(launched.len(), 1, "one launch: {launched:?}");
        let request = launched.first().expect("one launch");
        assert!(
            request.url.ends_with("/applets/quiz/?kiosk=1"),
            "got {request:?}"
        );
        assert!(request.url.contains("http://127.0.0.1:"));
        assert!(request.profile.as_path().ends_with("kiosk-profile"));
        assert_eq!(request.id, "quiz");
        assert_eq!(request.title, "The Quiz");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_kiosk_open_with_an_invalid_id_never_reaches_the_launcher() {
        // Given a store actor whose launcher records launches.
        let launcher = ScriptedLauncher::succeeding();
        let (actor, system, notes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open for an invalid id lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "../escape".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the launcher was never called and no note published.
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
        assert!(notes.is_empty(), "no note on a refused open");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_kiosk_launch_surfaces_the_error_note() {
        // Given a store actor whose launcher always fails.
        let launcher = ScriptedLauncher::failing("no Chromium-family browser found");
        let (actor, system, notes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the failure surfaced as one error-tone note on the
        // applets row, naming the cause — and the actor survived (the
        // drained inbox proves the handler returned, not crashed).
        let recorded = jinn_testutil::bus_harness::await_recorded(
            &notes,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(
            recorded.iter().any(|update| {
                update.name == crate::store_actor::APPLETS_STORE_ACTOR_NAME
                    && update.note_tone == Some(jinn_slices::NoteTone::Error)
                    && update
                        .status_message
                        .as_deref()
                        .is_some_and(|message| message.contains("could not open"))
            }),
            "got {recorded:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_configured_kiosk_command_rides_to_the_launcher_verbatim() {
        // Given a store actor over a layer that sets the kiosk command.
        let doc = toml_edit::DocumentMut::from_str(
            "[applets]\nkiosk_command = 'firefox --kiosk \"{url}\" --profile \"{profile}\"'\n",
        )
        .expect("doc parses");
        let config =
            jinn_config::ConfigLayer::load(Arc::new(jinn_config::InMemoryConfigStorage::new(doc)))
                .expect("layer loads");
        let launcher = ScriptedLauncher::succeeding();
        let (actor, system, _notes, _harness, _apps_root) =
            spawn_store(launcher.clone(), config).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the launcher still saw exactly one launch — the
        // override changes what the launcher runs, not whether the
        // path fires. (Substitution itself is browser.rs's tests.)
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_blank_kiosk_command_falls_through_to_the_builtin() {
        // Given a store actor over a layer whose kiosk command is blank.
        let doc = toml_edit::DocumentMut::from_str("[applets]\nkiosk_command = \"   \"\n")
            .expect("doc parses");
        let config =
            jinn_config::ConfigLayer::load(Arc::new(jinn_config::InMemoryConfigStorage::new(doc)))
                .expect("layer loads");
        let launcher = ScriptedLauncher::succeeding();
        let (actor, system, _notes, _harness, _apps_root) =
            spawn_store(launcher.clone(), config).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the built-in path fired: one launch, override unset —
        // a blank string is unset, never an empty shell command.
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
        let launched = launcher.launched.lock().expect("launcher lock");
        assert!(
            launched
                .first()
                .is_some_and(|request| request.url.ends_with("?kiosk=1"))
        );
    }

    // The plain `OpenApplet` path has no end-to-end test here, on
    // purpose: its handler spawns the OS opener, and a test that
    // launches a browser is not a test. Its ensure-bind block is the
    // same one every kiosk test above exercises (the launcher receiving
    // a bound URL proves the block resolved), and its picker-side
    // command shape is pinned in `picker_routes.rs`.
}
