//! Session-store actor — persistence, loading, startup hydration, and archiving.
//!
//! This actor owns the store-facing half of session management. It mutates
//! shared session state and then releases the state lock before publishing or
//! persisting.

mod handlers;

use jinn_boot_msg::EnvironmentLoaded;
use jinn_chat_log_view_msg::{ArmLayoutDeadline, LayoutChatSession};
use jinn_kernel::Services;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::services::BusService;
use jinn_kernel::common::state::State;
use jinn_session_store_msg::PersistSession;
use jinn_session_store_msg::SessionLoadRequested;
use jinn_session_store_msg::{ArchiveSession, ArchiveSessionTree};
use jinn_session_store_msg::{
    LoadLibraryEntries, LoadSessionPickerEntries, LoadSessionTree, StartupHydrationFinished,
};
use jinn_session_store_msg::{SessionForkRequested, SetSessionLibrary};
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::hydrate::{HydrateCompleted, HydrateSession};

/// The session-store actor's static trouper path.
pub const SESSION_STORE_PATH: &str = "session-store";

/// The session-store actor's mailbox capacity.
///
/// Store operations can touch the full session snapshot and SQLite. Blocking
/// delivery prevents a large history from being dropped during a burst of
/// explicit persistence requests.
pub const SESSION_STORE_MAILBOX_CAPACITY: usize = 65_536;

/// Dependencies for [`SessionStoreActor`].
#[derive(Clone)]
pub struct SessionStoreActorDeps {
    /// Application-wide services containing the session store and bus.
    pub services: Services,
    /// Shared application state.
    pub state: State,
    /// The session picker's cell, so the history read can install rows into
    /// the menu that shows them instead of the kernel's frontend state.
    pub session_picker_cell:
        jinn_slices::cell::TypedCell<jinn_session_store_msg::SessionPickerState>,
    /// The Library section's cell, so membership and archive writes can
    /// refresh the list the sidebar renders from.
    pub library_cell: jinn_slices::cell::TypedCell<jinn_session_store_msg::LibraryState>,
}

/// Actor that owns session persistence, loading, hydration, and archiving.
pub struct SessionStoreActor {
    services: Services,
    state: State,
    /// The session picker's cell, so the history read can install rows into
    /// the menu that shows them instead of the kernel's frontend state.
    session_picker_cell: jinn_slices::cell::TypedCell<jinn_session_store_msg::SessionPickerState>,
    /// The Library section's cell, so membership and archive writes can
    /// refresh the list the sidebar renders from.
    library_cell: jinn_slices::cell::TypedCell<jinn_session_store_msg::LibraryState>,
    /// Loads dispatched to the hydration pool that have not reported back.
    ///
    /// The startup handler no longer knows when hydration ends — it dispatches
    /// and returns — so the flag that drives the sidebar's hydration indicator
    /// is cleared by the last completion instead.
    pending_hydrations: usize,
    /// Frozen tree node reads dispatched by the startup sweep, still in flight.
    ///
    /// Counted separately from the unarchived pass: the two waves overlap in
    /// time, and one shared counter would reach zero twice — clearing the
    /// hydration flag during the frozen wave and re-firing the sweep.
    pending_frozen_hydrations: usize,
}

impl BusPublish for SessionStoreActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl ServiceActor for SessionStoreActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("SessionStoreActor is spawned via init_with"),
        )
    }
}

impl SessionStoreActor {
    /// Spawns the store actor and installs its eight typed subscriptions.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: SessionStoreActorDeps) -> ActorPath {
        let path = ActorPath::new(SESSION_STORE_PATH);
        Self::spawn_hydration_pool(system, &deps.services);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services: deps.services,
                            state: deps.state,
                            session_picker_cell: deps.session_picker_cell,
                            library_cell: deps.library_cell,
                            pending_hydrations: 0,
                            pending_frozen_hydrations: 0,
                        })
                    })
                }
            })
            .handles::<SessionLoadRequested>()
            .handles::<LoadSessionPickerEntries>()
            .handles::<LoadLibraryEntries>()
            .handles::<SetSessionLibrary>()
            .handles::<LoadSessionTree>()
            .handles::<SessionForkRequested>()
            .handles::<PersistSession>()
            .handles::<ArchiveSession>()
            .handles::<ArchiveSessionTree>()
            .handles::<EnvironmentLoaded>()
            .handles::<HydrateCompleted>()
            // A successful load hands the chat log to the layout workers
            // instead of clearing the load guard, so the chat log's loading
            // indication stays up until it has been measured. The flush gate
            // drops any outbound type not declared here.
            .emits::<LayoutChatSession>()
            .emits::<ArmLayoutDeadline>()
            // Membership changes tell the sidebar's Sessions and Library
            // sections that their lists are stale.
            .emits::<jinn_session_store_msg::SessionLibraryMembershipChanged>()
            // Every activation announces itself: the disk-load branch
            // publishes through `restore_loaded_session`, the in-memory
            // branch through `on_activate_in_memory`. Without this
            // declaration the schema table carries no edge for the
            // activation fact this actor produces.
            .emits::<jinn_kernel::protocol::system::ActiveSessionChanged>()
            // Hydration jobs leave this actor; without this the flush gate
            // drops them and the sidebar never fills in.
            .emits::<HydrateSession>()
            // The end of startup hydration is announced here, strictly after
            // the hydration flag is cleared. Without this declaration a
            // push-only surface (the the web UI rail) would spin forever: the
            // flush gate drops an undeclared type silently.
            .emits::<StartupHydrationFinished>()
            .mailbox(
                SESSION_STORE_MAILBOX_CAPACITY,
                trouper::inbox::OverloadPolicy::Block,
            )
            .start();
        path
    }

    /// Spawns the hydration worker pool the startup path dispatches to.
    ///
    /// Spawned from here rather than by the caller so the pool exists wherever
    /// the store actor does: a `send_to_any` with no worker to receive it would
    /// drop every hydration job, and the sidebar would never finish filling in.
    fn spawn_hydration_pool(system: &ActorSystem, services: &Services) {
        for index in 0..crate::hydrate_worker::HYDRATE_WORKER_POOL_SIZE {
            crate::hydrate_worker::HydrateWorkerActor::spawn(
                system,
                index,
                crate::hydrate_worker::HydrateWorkerActorDeps {
                    session_store: services.session_store.clone(),
                },
            );
        }
    }
}

impl MsgHandler<SessionLoadRequested> for SessionStoreActor {
    async fn handle(&mut self, msg: &SessionLoadRequested, ctx: &mut MsgCtx<'_>) {
        self.on_load_requested(ctx, msg).await;
    }
}

impl MsgHandler<LoadSessionPickerEntries> for SessionStoreActor {
    async fn handle(&mut self, msg: &LoadSessionPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_session_picker_entries(msg).await;
    }
}

impl MsgHandler<LoadLibraryEntries> for SessionStoreActor {
    async fn handle(&mut self, _msg: &LoadLibraryEntries, _ctx: &mut MsgCtx<'_>) {
        self.refresh_library_cell().await;
    }
}

impl MsgHandler<SetSessionLibrary> for SessionStoreActor {
    async fn handle(&mut self, msg: &SetSessionLibrary, _ctx: &mut MsgCtx<'_>) {
        self.handle_set_session_library(msg).await;
    }
}

impl MsgHandler<LoadSessionTree> for SessionStoreActor {
    async fn handle(&mut self, msg: &LoadSessionTree, ctx: &mut MsgCtx<'_>) {
        self.on_load_tree_requested(ctx, msg).await;
    }
}

impl MsgHandler<SessionForkRequested> for SessionStoreActor {
    async fn handle(&mut self, msg: &SessionForkRequested, ctx: &mut MsgCtx<'_>) {
        self.on_session_fork_requested(ctx, msg).await;
    }
}

impl MsgHandler<PersistSession> for SessionStoreActor {
    async fn handle(&mut self, msg: &PersistSession, _ctx: &mut MsgCtx<'_>) {
        self.handle_persist_session(msg).await;
    }
}

impl MsgHandler<ArchiveSession> for SessionStoreActor {
    async fn handle(&mut self, msg: &ArchiveSession, _ctx: &mut MsgCtx<'_>) {
        self.handle_archive_session(msg).await;
    }
}

impl MsgHandler<ArchiveSessionTree> for SessionStoreActor {
    async fn handle(&mut self, msg: &ArchiveSessionTree, _ctx: &mut MsgCtx<'_>) {
        self.handle_archive_session_tree(msg).await;
    }
}

impl MsgHandler<EnvironmentLoaded> for SessionStoreActor {
    async fn handle(&mut self, msg: &EnvironmentLoaded, ctx: &mut MsgCtx<'_>) {
        self.on_environment_loaded(&msg.config, ctx).await;
    }
}
