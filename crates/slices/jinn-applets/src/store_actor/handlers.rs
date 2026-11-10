//! Message handlers for [`AppletsStoreActor`] — rescan, rename, delete.
//!
//! Every handler is one transaction over two stores: the in-memory
//! catalog cell (the authority during a run) and `applets.json` (the
//! persisted form). The read/write primitives both stores ride live in
//! [`crate::catalog_fs`] (scan and write-through); the write order is
//! cell-first, file-second, and a file write failure is logged loudly
//! but never rolled back into the cell — the next rescan reconciles the
//! file from disk's truth, which is what the rescan exists to do.

use marked_path::MarkedPathAccess as _;

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
