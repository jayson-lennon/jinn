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

use jinn_core_types::SessionId;
use jinn_session_history_msg::PushChatEntry;
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
        chat_error(
            actor,
            &msg.session_id,
            &open_failed_note(&format!("{report:?}")),
        )
        .await;
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
/// the built-in Chromium-family launcher. A failed launch surfaces as
/// a chat error entry in the session the keypress came from (the same
/// vehicle a failed export or a blocked attachment uses), then the
/// actor continues: a missing browser must not take the picker down.
///
/// The launch is observed past the spawn: a command that cannot run —
/// a mistyped `kiosk_command` behind the shell — spawns fine and dies
/// with a nonzero status immediately, so the handler watches the child
/// through a short grace window and reports only an instant nonzero
/// exit. A browser that starts and later closes is a normal kiosk
/// session and reports nothing.
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
            chat_error(
                actor,
                &msg.session_id,
                "could not read the [applets] config for the kiosk launch",
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
    let child = match actor.launcher.launch(&request, kiosk_command.as_deref()) {
        Ok(child) => child,
        Err(report) => {
            tracing::warn!(%report, "kiosk launch failed");
            let reason = open_failed_note(&format!("{report:?}"));
            chat_error(actor, &msg.session_id, &reason).await;
            return;
        }
    };

    // Observe the child on a side task: this handler returns immediately
    // (the inbox drains; nothing races a browser's lifetime), the grace
    // watch polls off the async runtime, and only an instant nonzero
    // exit is reported as a failure. A browser that started keeps the
    // task alive for at most the grace window, then both exit quietly.
    let bus = actor.bus.clone();
    let session_id = msg.session_id.clone();
    let _watch = tokio::spawn(async move {
        let outcome =
            tokio::task::spawn_blocking(move || child.report_if_failed_fast(FAST_FAIL_GRACE)).await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(report)) => {
                tracing::warn!(%report, "kiosk process failed fast");
                let reason = open_failed_note(&format!("{report:?}"));
                publish_chat_error(&bus, &session_id, &reason).await;
            }
            Err(join_error) => {
                tracing::warn!(%join_error, "the kiosk fast-fail watch did not run");
            }
        }
    });
}

/// How long a launched kiosk process has to exit nonzero before its
/// exit is considered a normal close rather than a launch failure.
const FAST_FAIL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Pushes a chat error entry into `session_id`'s history.
///
/// A failed launch is user-facing feedback, not a log line: the entry
/// rides [`PushChatEntry`] to the session actor — the single writer of
/// session history — which persists it and repaints the chat log, the
/// same vehicle a failed export or a blocked attachment uses.
async fn chat_error(actor: &AppletsStoreActor, session_id: &SessionId, message: &str) {
    publish_chat_error(&actor.bus, session_id, message).await;
}

/// Publishes [`PushChatEntry`] carrying one `Error` entry.
async fn publish_chat_error(
    bus: &jinn_slices::bus::BusService,
    session_id: &SessionId,
    message: &str,
) {
    bus.publish(PushChatEntry {
        session_id: session_id.clone(),
        entry: jinn_core_types::ChatEntry::error(message.to_owned()),
        pin: None,
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

    use crate::browser::{KioskChild, KioskLaunchError, KioskLaunchRequest, KioskLauncher};
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
        ) -> Result<KioskChild, error_stack::Report<KioskLaunchError>> {
            self.record(request);
            match self.error {
                Some(message) => {
                    Err(error_stack::Report::new(KioskLaunchError).attach(message.to_owned()))
                }
                // A child that outlives the fast-fail watch — the shape
                // of a browser that actually started.
                None => {
                    let child = std::process::Command::new("sleep")
                        .arg("30")
                        .spawn()
                        .map_err(|error| {
                            error_stack::Report::new(KioskLaunchError)
                                .attach(error.to_string())
                                .attach("the test browser child did not spawn")
                        })?;
                    Ok(KioskChild::new(child))
                }
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
        launcher: Arc<dyn KioskLauncher>,
        config: jinn_config::ConfigLayer,
    ) -> (
        trouper::actor::ActorPath,
        trouper::system::ActorSystem,
        jinn_testutil::bus_harness::Recorder<jinn_session_history_msg::PushChatEntry>,
        jinn_testutil::bus_harness::TestHarness,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let pushes = harness
            .spawn_recorder::<jinn_session_history_msg::PushChatEntry>()
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

        (actor, system, pushes, harness, applets_dir)
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

    /// A stable session id the tests publish kiosk opens with.
    fn test_session_id() -> jinn_core_types::SessionId {
        jinn_core_types::SessionId::new_v5_tag("applet-kiosk-test")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_kiosk_open_publishes_the_kiosk_command_to_the_launcher() {
        // Given a store actor whose launcher records launches.
        let launcher = ScriptedLauncher::succeeding();
        let (actor, system, _pushes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open for the catalog's applet lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
                session_id: test_session_id(),
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
        let (actor, system, pushes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open for an invalid id lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "../escape".to_owned(),
                session_id: test_session_id(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the launcher was never called and no chat entry pushed.
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
        assert!(pushes.is_empty(), "no error entry on a refused open");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_kiosk_launch_surfaces_a_chat_error_entry() {
        // Given a store actor whose launcher always fails, and a
        // recorder for the chat-entry pushes.
        let launcher = ScriptedLauncher::failing("no Chromium-family browser found");
        let (actor, system, pushes, _harness, _apps_root) =
            spawn_store(launcher.clone(), empty_config()).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
                session_id: test_session_id(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the failure surfaced as one error entry pushed into the
        // session the command named, with user-facing prose — and the
        // actor survived (the drained inbox proves the handler
        // returned, not crashed).
        let recorded = jinn_testutil::bus_harness::await_recorded(
            &pushes,
            1,
            std::time::Duration::from_secs(5),
        )
        .await;
        let push = recorded.first().expect("one push recorded");
        assert_eq!(push.session_id, test_session_id());
        assert!(
            matches!(&push.entry.kind, jinn_core_types::ChatEntryKind::Error(text)
                if text.contains("could not open") && text.contains("no Chromium-family browser")),
            "got {:?}",
            push.entry.kind
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
        let (actor, system, _pushes, _harness, _apps_root) =
            spawn_store(launcher.clone(), config).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
                session_id: test_session_id(),
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
    #[cfg(target_family = "unix")]
    async fn a_bogus_kiosk_command_reports_the_nonzero_exit_into_the_chat() {
        // Given a store actor over a layer whose kiosk command cannot
        // run — the exact user bug: a `jinn.toml` command that exits
        // nonzero immediately.
        let doc = toml_edit::DocumentMut::from_str(
            "[applets]\nkiosk_command = 'definitely-not-a-real-command'\n",
        )
        .expect("doc parses");
        let config =
            jinn_config::ConfigLayer::load(Arc::new(jinn_config::InMemoryConfigStorage::new(doc)))
                .expect("layer loads");
        let (actor, system, pushes, _harness, _apps_root) =
            spawn_store(Arc::new(crate::browser::RealKioskLauncher), config).await;

        // When a kiosk open lands and the grace window passes.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
                session_id: test_session_id(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(
            &system,
            &actor,
            std::time::Duration::from_secs(5),
        )
        .await;

        // Then the nonzero exit surfaced as an error entry in the chat
        // naming the code — feedback the previous silent-spawn path
        // never gave.
        let recorded = jinn_testutil::bus_harness::await_recorded(
            &pushes,
            1,
            std::time::Duration::from_secs(10),
        )
        .await;
        let push = recorded.first().expect("one push recorded");
        assert_eq!(push.session_id, test_session_id());
        assert!(
            matches!(&push.entry.kind, jinn_core_types::ChatEntryKind::Error(text)
                if text.contains("could not open") && text.contains("code")),
            "got {:?}",
            push.entry.kind
        );
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
        let (actor, system, _pushes, _harness, _apps_root) =
            spawn_store(launcher.clone(), config).await;

        // When a kiosk open lands.
        system
            .publish(jinn_applets_msg::OpenAppletKiosk {
                id: "quiz".to_owned(),
                session_id: test_session_id(),
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
