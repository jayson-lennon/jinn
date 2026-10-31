//! Library membership writes and Library-cell refreshes.
//!
//! Membership is a durable per-session flag on the `sessions` row, mirrored
//! into the in-memory session when one exists. The Library cell holds one
//! [`SessionSummary`] per member, most recently updated first; it is the
//! sidebar Library section's only source of truth, so every store write that
//! changes what a member's row shows (membership, archive, unarchive-on-load)
//! refreshes it here.

use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_store_msg::{
    LoadLibraryEntries, SessionLibraryMembershipChanged, SetSessionLibrary,
};
use trouper::actor::ActorPath;

use crate::session_store_actor::SessionStoreActor;

impl SessionStoreActor {
    /// Applies one membership change: persist the column, mirror it into the
    /// in-memory session when one exists, announce the change, and schedule
    /// a Library-cell refresh.
    ///
    /// The session map write happens through the same single-writer path as
    /// every other live mutation — this actor is the only writer of the
    /// session map, so an in-memory flag changed anywhere else would race it.
    ///
    /// A move *into* the library hides the moved session's row, so before the
    /// announcement goes out the handler checks what the change does to the
    /// Sessions list: if no session would remain listed, a fresh seeded one
    /// joins the map first, and the event publishes into a non-empty list.
    /// (A move *out* only re-adds a row and never empties anything.)
    /// The active session stays on the moved member — the user is looking at
    /// it, and moving it to the Library is not a navigation act.
    pub(crate) async fn handle_set_session_library(&self, payload: &SetSessionLibrary) {
        if let Err(error) = self
            .services
            .session_store
            .set_in_library(&payload.session_id, payload.in_library)
            .await
        {
            tracing::warn!(
                ?error,
                session_id = %payload.session_id,
                in_library = payload.in_library,
                "failed to persist library membership"
            );
            return;
        }
        // The store actor is the session map's single writer, so the
        // in-memory mirror happens here: a loaded session's flag flips in
        // place through the same mutation path every other live write uses.
        // The emptiness repair runs in the same write lock: the event below
        // must never announce a removal that left the visible list empty.
        let mut repair_enablement = None;
        self.state.with_session(|view| {
            if let Some(session) = view.session.map().get_mut(&payload.session_id) {
                session.set_in_library(payload.in_library);
            }
            if payload.in_library && !view.session.map().iter().any(|(_, s)| s.is_listed()) {
                let (fresh, enablement) = self.build_seeded_fresh_session();
                view.session.map().insert(fresh);
                repair_enablement = enablement;
            }
        });
        self.publish(SessionLibraryMembershipChanged {
            session_id: payload.session_id.clone(),
            in_library: payload.in_library,
        })
        .await;
        if let Some(enablement) = repair_enablement {
            self.publish(enablement).await;
        }
        // Detached: the write is durable and announced; the section's rows
        // refresh from the back of this actor's own inbox rather than
        // stalling every queued store message behind a scan.
        self.schedule_library_refresh();
    }

    /// Rebuilds the Library cell from the store's member rows.
    ///
    /// One members-only query — scalar columns, no metadata parse — feeds the
    /// section; a failed read leaves the previous cell contents in place
    /// rather than blanking it.
    pub(crate) async fn refresh_library_cell(&self) {
        let members = match self.services.session_store.library_members().await {
            Ok(members) => members,
            Err(error) => {
                tracing::warn!(?error, "failed to load library entries");
                return;
            }
        };
        let mut members = members;
        members.sort_by_key(|member| std::cmp::Reverse(member.updated_at));
        self.library_cell.update(|library| {
            library.entries = members;
        });
    }

    /// Schedules a Library-cell refresh without holding this actor's
    /// mailbox.
    ///
    /// The refresh is a store read; awaiting it inline made every membership
    /// write stall the store actor for the length of the scan — the same
    /// stall a teardown's post-archive refresh produced. The self-addressed
    /// `LoadLibraryEntries` is enqueued from a detached job instead, so the
    /// write's caller is unblocked the moment the flag is durable, and the
    /// section's rows catch up a tick later. The search-index actor's
    /// self-addressed kick is the same shape. A failed enqueue only means
    /// the actor is stopping, which is fine to drop.
    pub(crate) fn schedule_library_refresh(&self) {
        let path = ActorPath::new(crate::session_store_actor::SESSION_STORE_PATH);
        let system = self.services.trouper_system.clone();
        tokio::spawn(async move {
            let _ = system.tell(path, LoadLibraryEntries).await;
        });
    }
}
