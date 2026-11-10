//! The applets contract — what crosses the applets slice boundary.
//!
//! The applets slice saves agent-built HTML artifacts under the data dir's
//! `applets/` root and serves them back verbatim over HTTP (loopback by
//! default). The payloads that cross the boundary live here so the
//! picker, the store actor, and the server never share an
//! implementation crate:
//!
//! - [`AppletsCatalog`] / [`AppletEntry`] — the saved-app list, mirroring the
//!   on-disk `applets.json` at the applets root.
//! - [`AppletsPickerState`] — the picker cell's payload, with its slot and
//!   scope.
//! - [`RescanApplets`], [`RenameApplet`], [`DeleteApplet`],
//!   [`WakeAppletsServer`] — the commands the picker's route actions
//!   publish.
//! - [`AppletsRescanned`], [`AppletRenamed`], [`AppletDeleted`],
//!   [`AppletsServingChanged`] — the events the store and server actors
//!   answer with.
//!
//! Pure data + pure functions — no actors, no server, no I/O. The
//! promotion path (canvas artifact → saved applet) is skill text, not a
//! function here.

mod catalog;
mod messages;
mod picker_state;
mod scope;

pub use catalog::{
    AppletEntry, AppletsCatalog, CATALOG_FILE_NAME, is_valid_app_id, sanitize_app_id,
};
pub use messages::{
    AppletDeleted, AppletRenamed, AppletsRescanned, AppletsServingChanged, DeleteApplet,
    OpenApplet, RenameApplet, RescanApplets, WakeAppletsServer,
};
pub use picker_state::{
    AppletPickerEntry, AppletsPickerState, ServingAddress, applet_row, applet_search_text,
    applets_server_status_line, build_applet_entries,
};
pub use scope::{
    RESULTS_VIEWPORT_FALLBACK, applets_catalog_slot, applets_picker_scope, applets_picker_slot,
    applets_rename_scope,
};

#[cfg(test)]
mod tests {
    use super::{AppletsRescanned, AppletsServingChanged, RescanApplets, WakeAppletsServer};
    use trouper::schema::{Schema, SchemaKind};

    #[rstest::rstest]
    #[test]
    fn crossing_schema_contract_is_stable() {
        // Given the canonical applets crossing contracts.
        // When reading their schema definitions.
        // Then the command/event names and kinds are stable.
        assert_eq!(
            <RescanApplets as Schema>::schema_id().name(),
            "RescanApplets"
        );
        assert_eq!(
            <AppletsRescanned as Schema>::schema_id().name(),
            "AppletsRescanned"
        );
        assert_eq!(
            <WakeAppletsServer as Schema>::schema_id().name(),
            "WakeAppletsServer"
        );
        assert_eq!(
            <AppletsServingChanged as Schema>::schema_id().name(),
            "AppletsServingChanged"
        );
        assert!(matches!(
            <RescanApplets as Schema>::schema_def().kind,
            SchemaKind::Command
        ));
        assert!(matches!(
            <AppletsRescanned as Schema>::schema_def().kind,
            SchemaKind::Event
        ));
    }
}
