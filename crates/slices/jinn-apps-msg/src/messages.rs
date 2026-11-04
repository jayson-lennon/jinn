//! The apps slice's crossing messages.
//!
//! All discovery and mutation ride the bus as data: the picker's opener
//! publishes [`RescanApps`], the rename and delete actions publish
//! [`RenameApp`] and [`DeleteApp`], and the store actor answers with the
//! [`AppsRescanned`], [`AppRenamed`], and [`AppDeleted`] events — which
//! the picker's republisher folds into the catalog cell so an open
//! picker re-renders in place. No request/reply handle exists: the
//! picker is the only consumer of the events, and one-way publication
//! keeps the sync route actions off any await.

use jinn_slices::BusMessage;
use serde::{Deserialize, Serialize};

/// Requests a rescan of the apps directory.
///
/// The picker's `<leader>sa` opener publishes this and pushes the picker
/// scope in the same action; the store actor reads disk async and
/// republishes the catalog, so the open picker's first render shows the
/// previous cell contents (the accepted one-frame-stale open) and the
/// repaint lands a few ms later without a close/reopen.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Rescan the saved-apps directory into the catalog.")]
pub struct RescanApps;

impl BusMessage for RescanApps {}

/// Announces a finished rescan: the catalog as it now stands.
///
/// Published by the store actor after it has read the directory and
/// `apps.json`, dropped entries whose directory is gone, and written the
/// reconciled file back. The picker's republisher folds this into the
/// catalog cell and rebuilds the rows.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "The reconciled saved-app catalog after a rescan.")]
pub struct AppsRescanned {
    /// The reconciled catalog, newest first.
    pub catalog: crate::catalog::AppCatalog,
}

impl BusMessage for AppsRescanned {}

/// Renames one saved app's title.
///
/// The picker's `<c-r>` confirm path publishes this; the store actor
/// updates the catalog entry, writes `apps.json` through, and confirms
/// with [`AppRenamed`]. The directory name (`id`) is never touched —
/// renaming changes what the picker shows, not any URL the user has.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Rename a saved app's title.")]
pub struct RenameApp {
    /// The app's directory id.
    pub id: String,
    /// The new display title.
    pub title: String,
}

impl BusMessage for RenameApp {}

/// Announces a completed rename.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "A saved app's title was renamed.")]
pub struct AppRenamed {
    /// The app's directory id.
    pub id: String,
    /// The title now on record.
    pub title: String,
}

impl BusMessage for AppRenamed {}

/// Deletes one saved app.
///
/// The picker's `<c-d>` path publishes this; the store actor removes the
/// directory and the catalog row, writes `apps.json` through, and
/// confirms with [`AppDeleted`]. The picker stays open.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Delete a saved app (directory and catalog row).")]
pub struct DeleteApp {
    /// The app's directory id.
    pub id: String,
}

impl BusMessage for DeleteApp {}

/// Announces a completed delete.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "A saved app was deleted.")]
pub struct AppDeleted {
    /// The id that was removed.
    pub id: String,
}

impl BusMessage for AppDeleted {}

/// Opens one saved app in the user's browser.
///
/// The picker's `ENTER` publishes this; the store actor validates the
/// id, ensures the server is bound (the lazy bind happens on the first
/// open), builds `http://<resolved>/apps/<id>/`, and launches the OS
/// opener. Route actions are synchronous, so the ensure-and-launch
/// lives here rather than in the keypress.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Open a saved app in the user's browser.")]
pub struct OpenApp {
    /// The app's directory id.
    pub id: String,
}

impl BusMessage for OpenApp {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{AppDeleted, AppRenamed, AppsRescanned, DeleteApp, OpenApp, RenameApp, RescanApps};
    use crate::catalog::AppCatalog;
    use trouper::schema::{Schema, SchemaKind};

    #[rstest::rstest]
    #[test]
    fn crossing_schema_kinds_are_stable() {
        // Given the crossing contracts.
        // When reading their schema definitions.
        // Then commands and events carry the kinds their names promise —
        // an imperative message stays a Command and a past-tense fact
        // stays an Event.
        assert!(matches!(
            <RescanApps as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <RenameApp as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <DeleteApp as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <OpenApp as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <AppsRescanned as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
        assert!(matches!(
            <AppRenamed as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
        assert!(matches!(
            <AppDeleted as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
    }

    #[rstest::rstest]
    #[test]
    fn rescan_round_trips_through_json() {
        // Given the rescan command (a unit struct).
        let command = RescanApps;

        // When serializing then re-parsing.
        let json = serde_json::to_string(&command).expect("serialize");
        let back: RescanApps = serde_json::from_str(&json).expect("reparse");

        // Then the parse succeeds.
        let _ = back;
    }

    #[rstest::rstest]
    #[test]
    fn apps_rescanned_carries_the_catalog() {
        // Given a rescanned event with one entry.
        let event = AppsRescanned {
            catalog: AppCatalog {
                entries: vec![crate::catalog::AppEntry {
                    id: "a".to_owned(),
                    title: "A".to_owned(),
                    created_at: "1".to_owned(),
                }],
            },
        };

        // When serializing then re-parsing.
        let json = serde_json::to_string(&event).expect("serialize");
        let back: AppsRescanned = serde_json::from_str(&json).expect("reparse");

        // Then the catalog survives.
        assert_eq!(back, event);
    }

    #[rstest::rstest]
    #[test]
    fn rename_and_delete_round_trip_through_json() {
        // Given a rename and a delete.
        let rename = RenameApp {
            id: "sales-dashboard".to_owned(),
            title: "Q4 Sales".to_owned(),
        };
        let delete = DeleteApp {
            id: "sales-dashboard".to_owned(),
        };

        // When serializing then re-parsing each.
        let rename_json = serde_json::to_string(&rename).expect("serialize rename");
        let rename_back: RenameApp = serde_json::from_str(&rename_json).expect("reparse rename");
        let delete_json = serde_json::to_string(&delete).expect("serialize delete");
        let delete_back: DeleteApp = serde_json::from_str(&delete_json).expect("reparse delete");

        // Then the fields survive.
        assert_eq!(rename_back, rename);
        assert_eq!(delete_back, delete);
    }

    #[rstest::rstest]
    #[test]
    fn renamed_and_deleted_carry_their_ids() {
        // Given the two confirmation events.
        let renamed = AppRenamed {
            id: "a".to_owned(),
            title: "T".to_owned(),
        };
        let deleted = AppDeleted { id: "a".to_owned() };

        // When serializing then re-parsing each.
        let renamed_json = serde_json::to_string(&renamed).expect("serialize");
        let renamed_back: AppRenamed = serde_json::from_str(&renamed_json).expect("reparse");
        let deleted_json = serde_json::to_string(&deleted).expect("serialize");
        let deleted_back: AppDeleted = serde_json::from_str(&deleted_json).expect("reparse");

        // Then the fields survive.
        assert_eq!(renamed_back, renamed);
        assert_eq!(deleted_back, deleted);
    }
}
