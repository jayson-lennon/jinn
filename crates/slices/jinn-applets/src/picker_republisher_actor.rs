//! Republishes the applets picker when the store actor reports a catalog
//! change.
//!
//! The picker is a slice cell, so the store actor cannot repaint it
//! directly — it holds no handle to the slice registry. Instead the
//! store publishes [`AppletsRescanned`] / [`AppletRenamed`] / [`AppletDeleted`],
//! and this listener — spawned by the applets slice with handles to both
//! the picker cell and the catalog cell — folds the event into the
//! catalog cell and rebuilds the picker's rows.
//!
//! The catalog cell is the seam between the two halves: the store
//! actor writes it (and the disk), the picker reads it (via the
//! scope-enter hook) and this actor repaints from it. An open picker
//! therefore re-renders in place when a rescan lands — no close, no
//! reopen.

use jinn_applets_msg::{
    AppletDeleted, AppletRenamed, AppletsCatalog, AppletsPickerState, AppletsRescanned,
};
use jinn_slices::cell::TypedCell;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

/// Republishes the picker whenever the store reports a catalog change.
#[derive(Debug)]
pub struct AppletsPickerRepublisherActor {
    /// The picker's cell, repainted on every store event.
    picker_cell: TypedCell<AppletsPickerState>,
    /// The catalog cell, updated from each event so the cell mirrors
    /// what the store published.
    catalog_cell: TypedCell<AppletsCatalog>,
}

impl AppletsPickerRepublisherActor {
    /// The registry path this actor is published under.
    pub const PATH: &'static str = "jinn.applets.picker-republisher";

    /// Builds a republisher over the picker and catalog cells.
    #[must_use]
    pub fn new(
        picker_cell: TypedCell<AppletsPickerState>,
        catalog_cell: TypedCell<AppletsCatalog>,
    ) -> Self {
        Self {
            picker_cell,
            catalog_cell,
        }
    }
}

impl ServiceActor for AppletsPickerRepublisherActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by signature; this impl never awaits"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: spawned via `init_with` (typed deps cannot ride
        // JSON args).
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("AppletsPickerRepublisherActor spawns via init_with"))
    }
}

impl MsgHandler<AppletsRescanned> for AppletsPickerRepublisherActor {
    /// Folds the rescanned catalog into the catalog cell and repaints
    /// the picker's rows, preserving the highlight on the applet the user
    /// was looking at.
    async fn handle(&mut self, msg: &AppletsRescanned, _ctx: &mut MsgCtx<'_>) {
        self.catalog_cell.update(|cell| *cell = msg.catalog.clone());
        crate::picker_routes::republish_from_catalog(&self.picker_cell, &msg.catalog);
    }
}

impl MsgHandler<AppletRenamed> for AppletsPickerRepublisherActor {
    /// Updates the catalog cell's title and repaints. The store actor
    /// already wrote the cell and the file; this handles the run where
    /// the republisher's own cell write is the one that lands (idempotent
    /// with the store's, and the repaint is the point).
    async fn handle(&mut self, msg: &AppletRenamed, _ctx: &mut MsgCtx<'_>) {
        self.catalog_cell.update(|catalog| {
            if let Some(entry) = catalog.entries.iter_mut().find(|entry| entry.id == msg.id) {
                entry.title.clone_from(&msg.title);
            }
        });
        let catalog = self.catalog_cell.read();
        crate::picker_routes::republish_from_catalog(&self.picker_cell, &catalog);
    }
}

impl MsgHandler<AppletDeleted> for AppletsPickerRepublisherActor {
    /// Drops the catalog cell's row and repaints — the deleted app
    /// disappears from the open picker immediately.
    async fn handle(&mut self, msg: &AppletDeleted, _ctx: &mut MsgCtx<'_>) {
        self.catalog_cell.update(|catalog| {
            catalog.entries.retain(|entry| entry.id != msg.id);
        });
        let catalog = self.catalog_cell.read();
        crate::picker_routes::republish_from_catalog(&self.picker_cell, &catalog);
    }
}
