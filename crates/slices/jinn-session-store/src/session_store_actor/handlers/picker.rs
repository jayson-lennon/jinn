//! Session picker hydration from the store.

use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_store_msg::{LoadSessionPickerEntries, SessionPickerEntriesLoaded};

use crate::session_store_actor::SessionStoreActor;

impl SessionStoreActor {
    /// Loads session tree entries from storage into the session picker, then
    /// announces the write.
    pub(crate) async fn handle_load_session_picker_entries(
        &self,
        _payload: &LoadSessionPickerEntries,
    ) {
        let (store, theme) = {
            let state = self.state.read();
            (
                self.services.session_store.clone(),
                state.frontend.theme.clone(),
            )
        };
        let entries = crate::session_entries::load_session_entries_from_store(&store, &theme).await;
        // Into the picker's own cell, not the kernel's frontend state: the
        // picker is this slice's, and its rows are its state.
        self.session_picker_cell.update(|picker| {
            crate::session_picker_actions::load(picker, entries);
        });
        // Strictly after the cell write: a reader woken by this event sees
        // the fresh rows. The terminal never needs it — it re-reads the cell
        // every frame — but the web UI's Archive picker is push-only, and
        // this is how its rows reach the page.
        self.publish(SessionPickerEntriesLoaded).await;
    }
}
