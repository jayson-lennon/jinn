//! Theme rescan actor — re-scans the theme directory on
//! `ResourcesChanged`, re-seeds the theme-entries cell, and publishes
//! `ThemeEntriesReloaded`.
//!
//! The theme slice scans once at activation into the shared catalog's
//! theme-entries cell; this actor repeats that scan when the
//! config-watch slice reports a resource change. The rescan body is
//! the slice's own `rescan` function — the actor is the bus listener
//! around it, and the published event is the ordered handoff: the
//! app-state actor re-resolves the running theme on `ThemeEntriesReloaded`
//! rather than on `ResourcesChanged`, so the fresh scan is guaranteed
//! to be in the cell before it reads. It also resolves the running
//! theme against the fresh scan: a stored theme that stopped resolving
//! keeps rendering unchanged (the picker falls back at next selection),
//! so this only notes the fact on the row.

use marked_path::Absolute;
use marked_path::MarkedPathBuf;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::builder::spawn_service_builder;
use trouper::context::MsgCtx;
use trouper::prelude::ActorPath;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_kernel::common::services::Services;
use jinn_slices::NoteTone;
use jinn_slices::ServiceStatusUpdate;

/// The theme rescan actor's static trouper path.
pub const THEME_RESCAN_PATH: &str = "theme-rescan-actor";

/// The theme rescan actor.
pub struct ThemeRescanActor {
    /// Runtime services — the themes dir comes from the shared paths
    /// and the row note goes through the bus.
    services: Services,
}

impl ServiceActor for ThemeRescanActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects services via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ThemeRescanActor is spawned via init_with"),
        )
    }
}

impl ThemeRescanActor {
    /// Spawns the actor at its static path, subscribing to
    /// `ResourcesChanged` and emitting the fresh-scan event plus the row
    /// note it may publish.
    pub fn spawn(system: &ActorSystem, services: Services) -> ActorPath {
        spawn_service_builder::<Self>(system)
            .at(ActorPath::new(THEME_RESCAN_PATH))
            .init_with({
                move || {
                    let services = services.clone();
                    Box::pin(async move { Ok(Self { services }) })
                }
            })
            // One declarant: this actor is the theme listener for the
            // config-watch slice's resource event.
            .handles::<jinn_boot_msg::ResourcesChanged>()
            .emits::<jinn_theme_msg::ThemeEntriesReloaded>()
            .emits::<ServiceStatusUpdate>()
            .start()
    }

    /// Re-scans the theme directory through the slice's rescan body,
    /// then publishes the fresh-scan event.
    ///
    /// The cell re-seed is the whole effect for the picker: it reads
    /// entries on open, and a removed theme file simply stops being
    /// listed. The event is what the app-state actor chains on — it
    /// re-resolves the running theme against the freshly re-seeded
    /// cell, and because it subscribes to *this* event rather than to
    /// `ResourcesChanged` directly, the rescan is guaranteed to have
    /// landed before it reads. The running theme keeps rendering from
    /// its already-loaded colors either way; the row note only reports
    /// the refresh.
    async fn rescan_and_note(&self, ctx: &mut MsgCtx<'_>) {
        let themes_dir: MarkedPathBuf<Absolute> = self.services.paths.themes_dir();
        let entries = crate::rescan(&self.services.slices, &themes_dir.as_marked_path());
        let refreshed = entries.is_some();
        // Publish after the cell write: subscribers of the event may
        // read the cell immediately.
        ctx.publish(jinn_theme_msg::ThemeEntriesReloaded);
        ctx_note(self, refreshed).await;
    }
}

/// Publishes the row note for a completed rescan.
///
/// Extracted so the handler body stays a two-step recipe; the note is
/// best-effort — a row that fails to publish must not fail the rescan.
async fn ctx_note(actor: &ThemeRescanActor, refreshed: bool) {
    let (note, tone) = if refreshed {
        (Some("themes re-scanned".to_owned()), Some(NoteTone::Muted))
    } else {
        (
            Some("theme rescan found no catalog cell".to_owned()),
            Some(NoteTone::Warning),
        )
    };
    // Best-effort: a failed row note must not fail the rescan.
    actor
        .services
        .bus
        .publish(ServiceStatusUpdate {
            name: "themes".to_owned(),
            description: None,
            status_message: note,
            note_tone: tone,
        })
        .await;
}

impl MsgHandler<jinn_boot_msg::ResourcesChanged> for ThemeRescanActor {
    async fn handle(&mut self, _msg: &jinn_boot_msg::ResourcesChanged, ctx: &mut MsgCtx<'_>) {
        // The event carries no payload (the watcher cannot know which
        // slice cares about which directory), so this rescans on every
        // resource change. The scan is cheap and idempotent when the
        // themes dir is not the one that changed.
        self.rescan_and_note(ctx).await;
    }
}
