//! The applets store actor — the catalog and directory owner.
//!
//! Owns the on-disk state of the applets slice: the `applets.json` catalog at
//! the applets root and the one directory per applet beneath it. The
//! in-memory catalog cell is the authority during a run; the file is
//! the persisted form, written through on every mutation so cell and
//! file always agree.
//!
//! Handles three commands:
//!
//! - [`RescanApplets`](jinn_applets_msg::RescanApplets) — reads the directory and
//!   the catalog file, drops entries whose directory is gone, writes the
//!   reconciled file back, updates the catalog cell, and publishes
//!   [`AppletsRescanned`](jinn_applets_msg::AppletsRescanned). The picker's
//!   opener publishes one on every open, so the open picker repaints
//!   with fresh rows without a close/reopen.
//! - [`RenameApplet`](jinn_applets_msg::RenameApplet) — updates the entry's
//!   title in the cell and the file (the directory name, and therefore
//!   the URL, never changes) and publishes `AppletRenamed`.
//! - [`DeleteApplet`](jinn_applets_msg::DeleteApplet) — removes the directory
//!   and the catalog row, writes through, and publishes `AppletDeleted`.
//!
//! A missing or unreadable `applets.json` is the empty-catalog answer,
//! never an error: a first-run user has no applets, and a corrupt file
//! must not fail the launch or the picker.

mod handlers;

use jinn_applets_msg::{AppletDeleted, AppletRenamed, AppletsRescanned};
use jinn_slices::bus::BusService;
use jinn_slices::cell::TypedCell;
use marked_path::{Absolute, MarkedPathBuf};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment.
pub const APPLETS_STORE_ACTOR_NAME: &str = "applets-store";

/// The applets store actor: the catalog and directory owner.
pub struct AppletsStoreActor {
    /// The applets root, resolved from `AppPaths::applets_dir()` at
    /// activation.
    applets_dir: MarkedPathBuf<Absolute>,
    /// The catalog cell — the in-memory authority. Every mutation
    /// updates it and writes the file through in the same handler.
    catalog_cell: TypedCell<jinn_applets_msg::AppletsCatalog>,
    /// The message fabric: publishes the confirmation events and asks
    /// the server actor to bind.
    bus: BusService,
    /// The server actor's registry path, for the open path's ensure ask.
    server_actor: ActorPath,
}

impl ServiceActor for AppletsStoreActor {
    // Never called: spawned via `init_with` (typed deps cannot ride
    // JSON args).
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("AppletsStoreActor spawns via init_with"))
    }
}

/// Dependencies for [`AppletsStoreActor`].
#[derive(Clone)]
pub struct AppletsStoreActorDeps {
    /// The applets root from `AppPaths::applets_dir()`.
    pub applets_dir: MarkedPathBuf<Absolute>,
    /// The catalog cell, resolved by `activate` from the shared cell
    /// catalog. A cell handle is typed state and cannot ride trouper's
    /// JSON args, so it arrives in the deps.
    pub catalog_cell: TypedCell<jinn_applets_msg::AppletsCatalog>,
    /// The message fabric.
    pub bus: BusService,
    /// The server actor's registry path, for the open path's ensure ask.
    pub server_actor: ActorPath,
}

impl AppletsStoreActor {
    /// Spawns the actor. The spawn handshake is the readiness point:
    /// once it returns, the store's command subscriptions are live.
    pub fn spawn(system: &ActorSystem, deps: &AppletsStoreActorDeps) -> ActorPath {
        let path = ActorPath::new(APPLETS_STORE_ACTOR_NAME);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path)
            .init_with({
                let deps = deps.clone();
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            applets_dir: deps.applets_dir,
                            catalog_cell: deps.catalog_cell,
                            bus: deps.bus,
                            server_actor: deps.server_actor,
                        })
                    })
                }
            })
            .handles::<jinn_applets_msg::RescanApplets>()
            .handles::<jinn_applets_msg::RenameApplet>()
            .handles::<jinn_applets_msg::DeleteApplet>()
            .handles::<jinn_applets_msg::OpenApplet>()
            .emits::<AppletsRescanned>()
            .emits::<AppletRenamed>()
            .emits::<AppletDeleted>()
            .mailbox(16, trouper::inbox::OverloadPolicy::Block)
            .start()
    }
}

impl MsgHandler<jinn_applets_msg::RescanApplets> for AppletsStoreActor {
    async fn handle(&mut self, _msg: &jinn_applets_msg::RescanApplets, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_rescan(self).await;
    }
}

impl MsgHandler<jinn_applets_msg::RenameApplet> for AppletsStoreActor {
    async fn handle(&mut self, msg: &jinn_applets_msg::RenameApplet, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_rename(self, msg).await;
    }
}

impl MsgHandler<jinn_applets_msg::DeleteApplet> for AppletsStoreActor {
    async fn handle(&mut self, msg: &jinn_applets_msg::DeleteApplet, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_delete(self, msg).await;
    }
}

impl MsgHandler<jinn_applets_msg::OpenApplet> for AppletsStoreActor {
    async fn handle(&mut self, msg: &jinn_applets_msg::OpenApplet, _ctx: &mut MsgCtx<'_>) {
        handlers::handle_open(self, msg).await;
    }
}
