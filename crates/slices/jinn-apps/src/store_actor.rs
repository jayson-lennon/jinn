//! The apps store actor — the catalog and directory owner.
//!
//! Owns the on-disk state of the apps slice: the `apps.json` catalog at
//! the apps root and the one directory per app beneath it. The
//! in-memory catalog cell is the authority during a run; the file is
//! the persisted form, written through on every mutation so cell and
//! file always agree.
//!
//! Handles three commands:
//!
//! - [`RescanApps`](jinn_apps_msg::RescanApps) — reads the directory and
//!   the catalog file, drops entries whose directory is gone, writes the
//!   reconciled file back, updates the catalog cell, and publishes
//!   [`AppsRescanned`](jinn_apps_msg::AppsRescanned). The picker's
//!   opener publishes one on every open, so the open picker repaints
//!   with fresh rows without a close/reopen.
//! - [`RenameApp`](jinn_apps_msg::RenameApp) — updates the entry's
//!   title in the cell and the file (the directory name, and therefore
//!   the URL, never changes) and publishes `AppRenamed`.
//! - [`DeleteApp`](jinn_apps_msg::DeleteApp) — removes the directory
//!   and the catalog row, writes through, and publishes `AppDeleted`.
//!
//! A missing or unreadable `apps.json` is the empty-catalog answer,
//! never an error: a first-run user has no apps, and a corrupt file
//! must not fail the launch or the picker.

mod handlers;

use jinn_apps_msg::{AppDeleted, AppRenamed, AppsRescanned};
use jinn_slices::bus::BusService;
use jinn_slices::cell::TypedCell;
use marked_path::{Absolute, MarkedPathBuf};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment.
pub const APPS_STORE_ACTOR_NAME: &str = "apps-store";

/// The apps store actor: the catalog and directory owner.
pub struct AppsStoreActor {
    /// The apps root, resolved from `AppPaths::apps_dir()` at
    /// activation.
    apps_dir: MarkedPathBuf<Absolute>,
    /// The catalog cell — the in-memory authority. Every mutation
    /// updates it and writes the file through in the same handler.
    catalog_cell: TypedCell<jinn_apps_msg::AppCatalog>,
    /// The message fabric: publishes the confirmation events and asks
    /// the server actor to bind.
    bus: BusService,
    /// The server actor's registry path, for the open path's ensure ask.
    server_actor: ActorPath,
}

impl ServiceActor for AppsStoreActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args).
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("AppsStoreActor spawns via init_with"))
    }
}

/// Dependencies for [`AppsStoreActor`].
#[derive(Clone)]
pub struct AppsStoreActorDeps {
    /// The apps root from `AppPaths::apps_dir()`.
    pub apps_dir: MarkedPathBuf<Absolute>,
    /// The catalog cell, resolved by `activate` from the shared cell
    /// catalog. A cell handle is typed state and cannot ride trouper's
    /// JSON args, so it arrives in the deps.
    pub catalog_cell: TypedCell<jinn_apps_msg::AppCatalog>,
    /// The message fabric.
    pub bus: BusService,
    /// The server actor's registry path, for the open path's ensure ask.
    pub server_actor: ActorPath,
}

impl AppsStoreActor {
    /// Spawns the actor. The spawn handshake is the readiness point:
    /// once it returns, the store's command subscriptions are live.
    pub fn spawn(system: &ActorSystem, deps: &AppsStoreActorDeps) -> ActorPath {
        let path = ActorPath::new(APPS_STORE_ACTOR_NAME);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .init_with({
                let deps = deps.clone();
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            apps_dir: deps.apps_dir,
                            catalog_cell: deps.catalog_cell,
                            bus: deps.bus,
                            server_actor: deps.server_actor,
                        })
                    })
                }
            })
            .handles::<jinn_apps_msg::RescanApps>()
            .handles::<jinn_apps_msg::RenameApp>()
            .handles::<jinn_apps_msg::DeleteApp>()
            .handles::<jinn_apps_msg::OpenApp>()
            .emits::<AppsRescanned>()
            .emits::<AppRenamed>()
            .emits::<AppDeleted>()
            .mailbox(16, trouper::inbox::OverloadPolicy::Block)
            .start()
    }
}

impl MsgHandler<jinn_apps_msg::RescanApps> for AppsStoreActor {
    async fn handle(&mut self, _msg: &jinn_apps_msg::RescanApps, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_rescan(self).await;
    }
}

impl MsgHandler<jinn_apps_msg::RenameApp> for AppsStoreActor {
    async fn handle(&mut self, msg: &jinn_apps_msg::RenameApp, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_rename(self, msg).await;
    }
}

impl MsgHandler<jinn_apps_msg::DeleteApp> for AppsStoreActor {
    async fn handle(&mut self, msg: &jinn_apps_msg::DeleteApp, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_delete(self, msg).await;
    }
}

impl MsgHandler<jinn_apps_msg::OpenApp> for AppsStoreActor {
    async fn handle(&mut self, msg: &jinn_apps_msg::OpenApp, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_open(self, msg).await;
    }
}
