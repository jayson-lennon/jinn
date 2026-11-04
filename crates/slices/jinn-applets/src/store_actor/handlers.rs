//! Message handlers for [`AppletsStoreActor`] — rescan, rename, delete.
//!
//! Every handler is one transaction over two stores: the in-memory
//! catalog cell (the authority during a run) and `applets.json` (the
//! persisted form). The write order is cell-first, file-second; a file
//! write failure is logged loudly but never rolled back into the cell —
//! the next rescan reconciles the file from disk's truth, which is what
//! the rescan exists to do.

use marked_path::MarkedPathAccess as _;

use crate::store_actor::AppletsStoreActor;

/// Handles a rescan: reconcile disk into cell and file, then publish.
///
/// The disk read is async-safe here (this runs on the store actor's own
/// task, never in a key-thread hook): the directory listing and the
/// catalog file are read, entries whose directory is gone are dropped,
/// and the reconciled set lands in both the cell and the file. A
/// missing/unreadable catalog file reads as empty — first run is a
/// state, not a failure.
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
                return;
            }
        },
        Err(error) => {
            tracing::error!(%error, "the applets server actor did not answer the ensure ask");
            return;
        }
    };
    if !outcome.ok {
        tracing::error!(reason = %outcome.error, "the applets server did not come up");
        return;
    }
    let Ok(bind) = outcome.bind.parse::<std::net::SocketAddr>() else {
        tracing::error!(bind = %outcome.bind, "the resolved bind did not parse");
        return;
    };

    let url = crate::browser::applet_url(bind, &msg.id);
    if let Err(report) = crate::browser::open_in_browser(&url) {
        tracing::warn!(%report, "browser launch failed");
    }
}

/// The outer bound on the ensure round trip inside the open path.
const OPEN_ENSURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reads the applets root into a reconciled catalog.
///
/// The directory is the truth for *existence* (an entry whose directory
/// is gone is dropped), `applets.json` is the truth for *metadata* (title
/// and created_at; a directory without a row is not listed — the skill
/// registers saves through the catalog, so an unlisted directory is a
/// half-finished copy, not an applet).
fn scan_catalog(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> jinn_applets_msg::AppletsCatalog {
    let mut catalog: jinn_applets_msg::AppletsCatalog =
        std::fs::read_to_string(catalog_path(applets_dir).as_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

    catalog
        .entries
        .retain(|entry| jinn_applets_msg::is_valid_app_id(&entry.id));
    catalog.entries.retain(|entry| {
        applets_dir
            .join_all([entry.id.as_str()])
            .is_ok_and(|dir| dir.as_path().is_dir())
    });
    catalog.sort_recency();
    catalog
}

/// Writes the catalog file, creating the applets root when absent.
///
/// A failure is logged, never panicked: a read-only or vanished applets
/// root degrades to an in-memory catalog, and the next rescan retries.
fn persist_catalog(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    catalog: &jinn_applets_msg::AppletsCatalog,
) {
    if let Err(error) = std::fs::create_dir_all(applets_dir.as_path()) {
        tracing::error!(%error, "failed to create the applets directory");
        return;
    }
    let json = match serde_json::to_string_pretty(catalog) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(%error, "failed to serialize the applets catalog");
            return;
        }
    };
    if let Err(error) = std::fs::write(catalog_path(applets_dir).as_path(), json) {
        tracing::error!(%error, "failed to write the applets catalog");
    }
}

/// The catalog file's path at the applets root.
///
/// # Panics
///
/// Panics only if the bare file name stops joining — unreachable while
/// `CATALOG_FILE_NAME` stays separator-free.
#[expect(
    clippy::panic,
    reason = "the bare file name joining is a compile-time invariant; a failure is a code bug, not a runtime state"
)]
fn catalog_path(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> marked_path::MarkedPathBuf<marked_path::Absolute> {
    let Ok(path) = applets_dir.join(jinn_applets_msg::CATALOG_FILE_NAME) else {
        panic!("catalog file name did not type-check")
    };
    path
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "test assertions need expect/unwrap"
    )]

    use super::{catalog_path, persist_catalog, scan_catalog};
    use jinn_applets_msg::{AppletEntry, AppletsCatalog};
    use marked_path::MarkedPathAccess as _;

    /// A temp applets root with one applet directory on disk.
    fn root_with_app(
        id: &str,
    ) -> (
        jinn_testutil::JinnTempDir,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let dir = jinn_testutil::retained_tempdir();
        let applets = dir.path.join("applets").expect("join");
        let app = applets.join_all([id]).expect("join");
        std::fs::create_dir_all(app.as_path()).expect("mkdir");
        std::fs::write(
            app.join("index.html").expect("join").as_path(),
            "<html></html>",
        )
        .expect("seed entry");
        let typed = marked_path::MarkedPathBuf::<marked_path::Absolute>::new(applets.as_path())
            .expect("absolute");
        (dir, typed)
    }

    fn entry(id: &str, created_at: &str) -> AppletEntry {
        AppletEntry {
            id: id.to_owned(),
            title: id.to_owned(),
            created_at: created_at.to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn missing_catalog_file_scans_as_empty() {
        // Given an applets root with an applet directory but no applets.json.
        let (_dir, applets) = root_with_app("solo");

        // When scanning.
        let catalog = scan_catalog(&applets);

        // Then the catalog is empty — the file is the metadata truth,
        // and a half-registered directory is not an applet.
        assert_eq!(catalog.entries.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn corrupt_catalog_file_scans_as_empty() {
        // Given an applets root whose applets.json is garbage.
        let (_dir, applets) = root_with_app("solo");
        std::fs::write(catalog_path(&applets).as_path(), "not json at all {{{")
            .expect("write garbage");

        // When scanning.
        let catalog = scan_catalog(&applets);

        // Then the catalog is empty — a corrupt file must not fail.
        assert_eq!(catalog.entries.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn rescan_drops_entries_whose_directory_is_gone() {
        // Given a catalog row whose directory was deleted out from
        // under it, and one whose directory is live.
        let (_dir, applets) = root_with_app("live");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("live", "2026-01-02T00:00:00Z"),
                entry("ghost", "2026-01-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then only the live entry survives.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["live"]);
    }

    #[rstest::rstest]
    #[test]
    fn rescan_orders_entries_newest_first() {
        // Given a catalog written oldest-first.
        let (_dir, applets) = root_with_app("old");
        let second = applets.join_all(["new"]).expect("join");
        std::fs::create_dir_all(second.as_path()).expect("mkdir");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("old", "2026-01-01T00:00:00Z"),
                entry("new", "2026-06-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then the newest leads.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[rstest::rstest]
    #[test]
    fn persist_then_scan_round_trips_the_catalog() {
        // Given a catalog persisted to disk.
        let (_dir, applets) = root_with_app("a");
        let second = applets.join_all(["b"]).expect("join");
        std::fs::create_dir_all(second.as_path()).expect("mkdir");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("a", "2026-01-01T00:00:00Z"),
                entry("b", "2026-02-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning it back.
        let scanned = scan_catalog(&applets);

        // Then the entries survive (order normalized to recency).
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[rstest::rstest]
    #[test]
    fn invalid_ids_are_dropped_by_the_scan() {
        // Given a catalog file carrying a traversal id.
        let (_dir, applets) = root_with_app("ok");
        let raw = r#"{"entries":[
            {"id":"ok","title":"Ok","created_at":"2026-01-01T00:00:00Z"},
            {"id":"..","title":"Escape","created_at":"2026-01-02T00:00:00Z"}
        ]}"#;
        std::fs::write(catalog_path(&applets).as_path(), raw).expect("write catalog");

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then the traversal row is gone.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["ok"]);
    }
}
