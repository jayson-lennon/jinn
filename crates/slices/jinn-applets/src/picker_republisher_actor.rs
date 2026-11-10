//! Republishes the applets picker when the store or server actors report
//! a change.
//!
//! The picker is a slice cell, so neither actor can repaint it
//! directly — they hold no handle to the slice registry. Instead the
//! store publishes [`AppletsRescanned`] / [`AppletRenamed`] /
//! [`AppletDeleted`], and the server publishes
//! [`AppletsServingChanged`], and this listener — spawned by the
//! applets slice with a handle to the picker cell and the catalog cell
//! — folds each event into its cell and repaints the picker's rows.
//!
//! The catalog cell is the seam between the two halves: the store
//! actor writes it (and the disk), the picker reads it (via the
//! scope-enter hook) and this actor repaints from it. An open picker
//! therefore re-renders in place when a rescan lands — no close, no
//! reopen — and its footer's serving phase tracks the server actor's
//! live state, including dropping a dead address on a failed rebind.

use jinn_applets_msg::{
    AppletDeleted, AppletRenamed, AppletsCatalog, AppletsPickerState, AppletsRescanned,
    AppletsServingChanged,
};
use jinn_slices::cell::TypedCell;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// Republishes the picker whenever the store reports a catalog change or
/// the server reports a serving change.
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

    /// Spawns the actor, exactly as composition wires it: subscriptions
    /// on the real builder, so a test that spawns through this cannot
    /// pass against wiring production never runs.
    pub fn spawn(system: &ActorSystem, deps: &AppletsPickerRepublisherActorDeps) -> ActorPath {
        let path = ActorPath::new(Self::PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .init_with({
                let deps = deps.clone();
                move || Box::pin(async move { Ok(Self::new(deps.picker_cell, deps.catalog_cell)) })
            })
            .handles::<AppletsRescanned>()
            .handles::<AppletRenamed>()
            .handles::<AppletDeleted>()
            .handles::<AppletsServingChanged>()
            .start()
    }
}

/// Dependencies for [`AppletsPickerRepublisherActor`].
#[derive(Clone)]
pub struct AppletsPickerRepublisherActorDeps {
    /// The picker's cell, resolved by `activate` from the shared cell
    /// catalog. A cell handle is typed state and cannot ride trouper's
    /// JSON args, so it arrives in the deps.
    pub picker_cell: TypedCell<AppletsPickerState>,
    /// The catalog cell, resolved the same way.
    pub catalog_cell: TypedCell<AppletsCatalog>,
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

impl MsgHandler<AppletsServingChanged> for AppletsPickerRepublisherActor {
    /// Folds the serving fact into the picker cell's phase — online
    /// keeps the resolved bind, offline clears it, so a dead address
    /// leaves the footer the moment its transition lands. The rows are
    /// untouched: a serving change repaints nothing but the footer.
    async fn handle(&mut self, msg: &AppletsServingChanged, _ctx: &mut MsgCtx<'_>) {
        self.picker_cell
            .update(|state| state.apply_serving(msg.serving, &msg.bind));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_applets_msg::{AppletEntry, AppletsPickerState, applets_picker_slot};

    use super::AppletsPickerRepublisherActor;

    /// The republisher spawned through its production builder over a
    /// catalog-seeded registry, so a test cannot pass against wiring
    /// production never runs.
    async fn wired() -> (
        jinn_slices::Slices,
        trouper::system::ActorSystem,
        trouper::actor::ActorPath,
        jinn_slices::cell::TypedCell<AppletsPickerState>,
    ) {
        let slices = jinn_slices::Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let picker_cell = slices
            .reader::<AppletsPickerState>(&applets_picker_slot())
            .expect("the catalog registers the picker cell");
        let catalog_cell = slices
            .reader::<jinn_applets_msg::AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
            .expect("the catalog registers the catalog cell");
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let system = harness.system().clone();
        let path = AppletsPickerRepublisherActor::spawn(
            &system,
            &super::AppletsPickerRepublisherActorDeps {
                picker_cell: picker_cell.clone(),
                catalog_cell: catalog_cell.clone(),
            },
        );
        (slices, system, path, picker_cell)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_serving_fact_flips_the_picker_cell_online() {
        // Given the republisher wired the way activation wires it.
        let (_slices, system, path, cell) = wired().await;

        // When the server actor's online fact publishes.
        system
            .publish(jinn_applets_msg::AppletsServingChanged {
                serving: true,
                bind: "127.0.0.1:41234".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(&system, &path, std::time::Duration::from_secs(5))
            .await;

        // Then the picker cell carries the resolved bind — the footer's
        // next render shows the serving URL.
        let guard = cell.read();
        assert_eq!(
            guard.serving.as_ref().map(|address| address.bind.as_str()),
            Some("127.0.0.1:41234")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_offline_fact_clears_the_picker_cell_phase() {
        // Given the wired republisher whose cell holds a serving phase
        // (a picker open over a live server).
        let (_slices, system, path, cell) = wired().await;
        cell.update(|state| state.apply_serving(true, "127.0.0.1:41234"));

        // When the failed-rebind offline fact publishes.
        system
            .publish(jinn_applets_msg::AppletsServingChanged {
                serving: false,
                bind: String::new(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(&system, &path, std::time::Duration::from_secs(5))
            .await;

        // Then the phase is cleared — no dead address survives in the
        // cell, so the footer reverts to `<server offline>`.
        let guard = cell.read();
        assert!(guard.serving.is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_serving_fact_leaves_the_rows_untouched() {
        // Given the wired republisher whose cell has one rebuilt row.
        let (_slices, system, path, cell) = wired().await;
        let catalog = jinn_applets_msg::AppletsCatalog {
            entries: vec![AppletEntry {
                id: "a".to_owned(),
                title: "Alpha".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            }],
        };
        crate::picker_routes::republish_from_catalog(&cell, &catalog);

        // When a serving fact publishes.
        system
            .publish(jinn_applets_msg::AppletsServingChanged {
                serving: true,
                bind: "127.0.0.1:1".to_owned(),
            })
            .await;
        jinn_testutil::wait::await_inbox_drained(&system, &path, std::time::Duration::from_secs(5))
            .await;

        // Then the rows survive unchanged — a serving change repaints
        // only the footer.
        let guard = cell.read();
        assert_eq!(guard.selection.items().len(), 1);
        assert_eq!(
            guard
                .selection
                .items()
                .first()
                .map(|item| item.entry().id.as_str()),
            Some("a")
        );
    }
}
