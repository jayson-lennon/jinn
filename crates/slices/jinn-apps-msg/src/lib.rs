//! The apps contract — what crosses the apps slice boundary.
//!
//! The apps slice saves agent-built HTML artifacts under the data dir's
//! `apps/` root and serves them back verbatim over loopback HTTP. The
//! payloads that cross the boundary live here so the picker, the store
//! actor, and the server never share an implementation crate:
//!
//! - [`AppCatalog`] / [`AppEntry`] — the saved-app list, mirroring the
//!   on-disk `apps.json` at the apps root.
//! - [`AppsPickerState`] — the picker cell's payload, with its slot and
//!   scope.
//! - [`RescanApps`], [`RenameApp`], [`DeleteApp`] — the commands the
//!   picker's route actions publish.
//! - [`AppsRescanned`], [`AppRenamed`], [`AppDeleted`] — the events the
//!   store actor answers with.
//!
//! Pure data + pure functions — no actors, no server, no I/O. The
//! promotion path (canvas artifact → saved app) is skill text, not a
//! function here.

mod catalog;
mod messages;
mod picker_state;
mod scope;

pub use catalog::{
    AppCatalog, AppEntry, CATALOG_FILE_NAME, is_valid_app_id, sanitize_app_id,
};
pub use messages::{
    AppDeleted, AppRenamed, AppsRescanned, DeleteApp, RenameApp, RescanApps,
};
pub use picker_state::{
    AppPickerEntry, AppsPickerState, app_row, app_search_text, build_app_entries,
};
pub use scope::{
    RESULTS_VIEWPORT_FALLBACK, apps_catalog_slot, apps_picker_scope, apps_picker_slot,
    apps_rename_scope,
};

#[cfg(test)]
mod tests {
    use super::{AppsRescanned, RescanApps};
    use trouper::schema::{Schema, SchemaKind};

    #[rstest::rstest]
    #[test]
    fn crossing_schema_contract_is_stable() {
        // Given the canonical apps crossing contracts.
        // When reading their schema definitions.
        // Then the command/event names and kinds are stable.
        assert_eq!(<RescanApps as Schema>::schema_id().name(), "RescanApps");
        assert_eq!(
            <AppsRescanned as Schema>::schema_id().name(),
            "AppsRescanned"
        );
        assert!(matches!(<RescanApps as Schema>::schema_def().kind, SchemaKind::Command));
        assert!(matches!(<AppsRescanned as Schema>::schema_def().kind, SchemaKind::Event));
    }
}
