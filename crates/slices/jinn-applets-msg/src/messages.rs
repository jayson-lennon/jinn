//! The applets slice's crossing messages.
//!
//! All discovery and mutation ride the bus as data: the picker's opener
//! publishes [`RescanApplets`] and [`WakeAppletsServer`], the rename and
//! delete actions publish [`RenameApplet`] and [`DeleteApplet`], and the
//! store actor answers with the [`AppletsRescanned`], [`AppletRenamed`],
//! and [`AppletDeleted`] events — which the picker's republisher folds
//! into the catalog cell so an open picker re-renders in place. The
//! server actor answers a wake with the [`AppletsServingChanged`] fact,
//! folded into the picker cell by the same republisher. No request/reply
//! handle exists on the picker path: the picker is the only consumer of
//! the events, and one-way publication keeps the sync route actions off
//! any await.

use jinn_slices::BusMessage;
use serde::{Deserialize, Serialize};

/// Requests a rescan of the applets directory.
///
/// The picker's `<leader>sa` opener publishes this and pushes the picker
/// scope in the same action; the store actor reads disk async and
/// republishes the catalog, so the open picker's first render shows the
/// previous cell contents (the accepted one-frame-stale open) and the
/// repaint lands a few ms later without a close/reopen.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Rescan the saved-applets directory into the catalog.")]
pub struct RescanApplets;

impl BusMessage for RescanApplets {}

/// Announces a finished rescan: the catalog as it now stands.
///
/// Published by the store actor after it has read the directory and
/// `applets.json`, dropped entries whose directory is gone, and written the
/// reconciled file back. The picker's republisher folds this into the
/// catalog cell and rebuilds the rows.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "The reconciled saved-app catalog after a rescan.")]
pub struct AppletsRescanned {
    /// The reconciled catalog, newest first.
    pub catalog: crate::catalog::AppletsCatalog,
}

impl BusMessage for AppletsRescanned {}

/// Renames one saved applet's title.
///
/// The picker's `<c-r>` confirm path publishes this; the store actor
/// updates the catalog entry, writes `applets.json` through, and confirms
/// with [`AppletRenamed`]. The directory name (`id`) is never touched —
/// renaming changes what the picker shows, not any URL the user has.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Rename a saved applet's title.")]
pub struct RenameApplet {
    /// The app's directory id.
    pub id: String,
    /// The new display title.
    pub title: String,
}

impl BusMessage for RenameApplet {}

/// Announces a completed rename.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "A saved applet's title was renamed.")]
pub struct AppletRenamed {
    /// The app's directory id.
    pub id: String,
    /// The title now on record.
    pub title: String,
}

impl BusMessage for AppletRenamed {}

/// Deletes one saved applet.
///
/// The picker's `<c-d>` path publishes this; the store actor removes the
/// directory and the catalog row, writes `applets.json` through, and
/// confirms with [`AppletDeleted`]. The picker stays open.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Delete a saved applet (directory and catalog row).")]
pub struct DeleteApplet {
    /// The app's directory id.
    pub id: String,
}

impl BusMessage for DeleteApplet {}

/// Announces a completed delete.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "A saved applet was deleted.")]
pub struct AppletDeleted {
    /// The id that was removed.
    pub id: String,
}

impl BusMessage for AppletDeleted {}

/// Opens one saved applet in the user's browser.
///
/// The picker's `ENTER` publishes this; the store actor validates the
/// id, ensures the server is bound (the lazy bind happens on the first
/// open), builds `http://<resolved>/applets/<id>/`, and launches the OS
/// opener. Route actions are synchronous, so the ensure-and-launch
/// lives here rather than in the keypress.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Open a saved applet in the user's browser.")]
pub struct OpenApplet {
    /// The app's directory id.
    pub id: String,
}

impl BusMessage for OpenApplet {}

/// Wakes the applets server: bind its listener if it is dormant.
///
/// The picker's `<leader>sa` opener publishes this alongside
/// [`RescanApplets`], so the server binds on the first picker open and
/// the picker's status line can show the resolved address without the
/// user opening an applet first. The server actor owns the applets root,
/// so the command carries nothing. Launch itself stays dormant: no
/// listener binds at activation, only at the first wake or open.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command, PartialEq)]
#[schema(description = "Wake the saved-applet server: bind its listener if dormant.")]
pub struct WakeAppletsServer;

impl BusMessage for WakeAppletsServer {}

/// Announces the applets server's serving phase.
///
/// Published by the server actor on every serving transition — first
/// bind, hot rebind to a changed `[applets] bind`, and a failed rebind
/// going back dormant — and on every wake. A wake against an
/// already-bound server republishes the current fact unchanged, so a
/// fresh picker open always sees the live address. `bind` is the
/// resolved `host:port` (a configured port `0` already resolved by the
/// OS) and is empty while offline. The picker's republisher folds this
/// into the picker cell, whose footer renders it; a dead address can
/// therefore never outlive the transition that ended it.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event, PartialEq)]
#[schema(description = "The applets server's serving phase changed.")]
pub struct AppletsServingChanged {
    /// Whether the server is bound and serving.
    pub serving: bool,
    /// The resolved bind the listener accepted. Empty while offline.
    pub bind: String,
}

impl BusMessage for AppletsServingChanged {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{
        AppletDeleted, AppletRenamed, AppletsRescanned, AppletsServingChanged, DeleteApplet,
        OpenApplet, RenameApplet, RescanApplets, WakeAppletsServer,
    };
    use crate::catalog::AppletsCatalog;
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
            <RescanApplets as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <RenameApplet as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <DeleteApplet as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <OpenApplet as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <WakeAppletsServer as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <AppletsRescanned as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
        assert!(matches!(
            <AppletRenamed as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
        assert!(matches!(
            <AppletDeleted as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
        assert!(matches!(
            <AppletsServingChanged as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
    }

    #[rstest::rstest]
    #[test]
    fn the_wake_command_round_trips_through_json() {
        // Given the parameterless wake command.
        let command = WakeAppletsServer;

        // When serializing then re-parsing.
        let json = serde_json::to_string(&command).expect("serialize");
        let back: WakeAppletsServer = serde_json::from_str(&json).expect("reparse");

        // Then the parse succeeds.
        let _ = back;
    }

    #[rstest::rstest]
    #[test]
    fn the_serving_fact_round_trips_through_json_both_ways() {
        // Given the serving fact, online and offline.
        let serving = AppletsServingChanged {
            serving: true,
            bind: "127.0.0.1:41234".to_owned(),
        };
        let offline = AppletsServingChanged {
            serving: false,
            bind: String::new(),
        };

        // When serializing then re-parsing each.
        let serving_json = serde_json::to_string(&serving).expect("serialize serving");
        let serving_back: AppletsServingChanged =
            serde_json::from_str(&serving_json).expect("reparse serving");
        let offline_json = serde_json::to_string(&offline).expect("serialize offline");
        let offline_back: AppletsServingChanged =
            serde_json::from_str(&offline_json).expect("reparse offline");

        // Then the fields survive.
        assert_eq!(serving_back, serving);
        assert_eq!(offline_back, offline);
    }

    #[rstest::rstest]
    #[test]
    fn rescan_round_trips_through_json() {
        // Given the rescan command (a unit struct).
        let command = RescanApplets;

        // When serializing then re-parsing.
        let json = serde_json::to_string(&command).expect("serialize");
        let back: RescanApplets = serde_json::from_str(&json).expect("reparse");

        // Then the parse succeeds.
        let _ = back;
    }

    #[rstest::rstest]
    #[test]
    fn applets_rescanned_carries_the_catalog() {
        // Given a rescanned event with one entry.
        let event = AppletsRescanned {
            catalog: AppletsCatalog {
                entries: vec![crate::catalog::AppletEntry {
                    id: "a".to_owned(),
                    title: "A".to_owned(),
                    created_at: "1".to_owned(),
                }],
            },
        };

        // When serializing then re-parsing.
        let json = serde_json::to_string(&event).expect("serialize");
        let back: AppletsRescanned = serde_json::from_str(&json).expect("reparse");

        // Then the catalog survives.
        assert_eq!(back, event);
    }

    #[rstest::rstest]
    #[test]
    fn rename_and_delete_round_trip_through_json() {
        // Given a rename and a delete.
        let rename = RenameApplet {
            id: "sales-dashboard".to_owned(),
            title: "Q4 Sales".to_owned(),
        };
        let delete = DeleteApplet {
            id: "sales-dashboard".to_owned(),
        };

        // When serializing then re-parsing each.
        let rename_json = serde_json::to_string(&rename).expect("serialize rename");
        let rename_back: RenameApplet = serde_json::from_str(&rename_json).expect("reparse rename");
        let delete_json = serde_json::to_string(&delete).expect("serialize delete");
        let delete_back: DeleteApplet = serde_json::from_str(&delete_json).expect("reparse delete");

        // Then the fields survive.
        assert_eq!(rename_back, rename);
        assert_eq!(delete_back, delete);
    }

    #[rstest::rstest]
    #[test]
    fn renamed_and_deleted_carry_their_ids() {
        // Given the two confirmation events.
        let renamed = AppletRenamed {
            id: "a".to_owned(),
            title: "T".to_owned(),
        };
        let deleted = AppletDeleted { id: "a".to_owned() };

        // When serializing then re-parsing each.
        let renamed_json = serde_json::to_string(&renamed).expect("serialize");
        let renamed_back: AppletRenamed = serde_json::from_str(&renamed_json).expect("reparse");
        let deleted_json = serde_json::to_string(&deleted).expect("serialize");
        let deleted_back: AppletDeleted = serde_json::from_str(&deleted_json).expect("reparse");

        // Then the fields survive.
        assert_eq!(renamed_back, renamed);
        assert_eq!(deleted_back, deleted);
    }
}
