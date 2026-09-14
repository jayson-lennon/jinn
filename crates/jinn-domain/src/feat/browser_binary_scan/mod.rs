//! Browser binary scan actor — verifies the configured browser binary at startup.
//!
//! Runs once on [`EnvironmentLoaded`] (program start only, not
//! per-session). It resolves the configured [`BrowserBinary`] via
//! [`resolve_browser_binary`] and publishes the result on the bus.
//!
//! ## Why events, not direct dashboard writes
//!
//! `frontend.dashboard` is sole-owned by
//! `DashboardCanvasActor` (the dashboard slice's canvas actor).
//! To honour the per-sub-struct ownership rule, this actor does **not** write
//! to the dashboard. It publishes a generic
//! [`ServiceStatusUpdate`](jinn_slices::ServiceStatusUpdate)
//! carrying the display label alongside [`BrowserBinaryVerified`] (the domain
//! fact); the dashboard owner applies it to the `web-fetch` row.

use std::sync::Arc;

use kameo::actor::ActorRef;
use kameo::prelude::{Actor, Context, Message};
use serde::{Deserialize, Serialize};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::Services;
use crate::common::services::bus_service::BusService;
use crate::feat::browser::BrowserBinary;
use crate::init::env_init_actor::EnvironmentLoaded;

pub mod binary_resolver;

pub use binary_resolver::{
    BinaryFamily, BinaryLocator, ResolvedBrowser, SystemBinaryLocator, resolve_browser_binary,
};

/// Dashboard row name for the web-fetch actor — whose Notes column surfaces
/// the resolved browser backend (Chrome/Chromium/Bundled).
const WEB_FETCH_ENTRY_NAME: &str = "web-fetch";

/// Dependencies for [`BrowserBinaryScanActor`].
#[derive(Clone)]
pub struct BrowserBinaryScanActorDeps {
    /// Runtime services and bus access.
    pub deps: ActorDeps,
    /// The configured binary selection (read once from prefs at spawn time).
    pub config: BrowserBinary,
    /// Filesystem seam; [`SystemBinaryLocator`] in production, injectable in
    /// tests. Defaults to [`SystemBinaryLocator`] when constructed via
    /// [`BrowserBinaryScanActorDeps::new`].
    pub locator: Arc<dyn BinaryLocator + Send + Sync>,
}

impl BrowserBinaryScanActorDeps {
    /// Production deps with the system filesystem locator.
    #[must_use]
    pub fn new(deps: ActorDeps, config: BrowserBinary) -> Self {
        Self {
            deps,
            config,
            locator: Arc::new(SystemBinaryLocator),
        }
    }
}

/// Verifies the configured browser binary is reachable at program start.
///
/// Subscribes to [`EnvironmentLoaded`] only. On the event, resolves the binary
/// on a blocking thread and publishes the outcome. It does not hold shared
/// [`State`](crate::common::state::State) — the resolution result is
/// communicated entirely via bus events.
pub struct BrowserBinaryScanActor {
    /// Runtime services.
    #[expect(dead_code, reason = "retained for future use / logging access")]
    services: Services,
    /// Bus service for publishing events.
    bus: BusService,
    /// The configured binary selection.
    config: BrowserBinary,
    /// Filesystem seam; `SystemBinaryLocator` in production, injectable in tests.
    locator: Arc<dyn BinaryLocator + Send + Sync>,
}

impl BusPublish for BrowserBinaryScanActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl Actor for BrowserBinaryScanActor {
    type Args = BrowserBinaryScanActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let bus = args.deps.services.bus.clone();
        bus.subscribe::<EnvironmentLoaded, _>(&actor_ref).await;

        Ok(Self {
            services: args.deps.services,
            bus,
            config: args.config,
            locator: args.locator,
        })
    }
}

impl Message<EnvironmentLoaded> for BrowserBinaryScanActor {
    type Reply = ();

    async fn handle(&mut self, _msg: EnvironmentLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        let config = self.config;
        let locator = self.locator.clone();
        let result =
            tokio::task::spawn_blocking(move || resolve_browser_binary(config, locator.as_ref()))
                .await;

        match result {
            Ok(resolved) => {
                tracing::info!(
                    family = ?resolved.family,
                    path = ?resolved.path,
                    version = ?resolved.version_major,
                    note = ?resolved.fallback_note,
                    "browser binary resolved"
                );
                let verified = BrowserBinaryVerified {
                    family: resolved.family,
                    path: resolved.path,
                    version_major: resolved.version_major,
                    fallback_note: resolved.fallback_note,
                };
                let label = verified.display_label();
                self.publish(verified).await;
                // The web-fetch row's lifecycle is owned by the
                // actor-lifecycle events (the scan actor IS web-fetch); this
                // projection only fills the Notes column.
                self.publish(jinn_slices::ServiceStatusUpdate {
                    name: WEB_FETCH_ENTRY_NAME.to_owned(),
                    description: None,
                    lifecycle: None,
                    status_message: Some(label),
                })
                .await;
            }
            Err(join_err) => {
                tracing::error!("browser binary scan task panicked: {join_err}");
            }
        }
    }
}

/// Emitted when the configured browser binary has been resolved.
///
/// Resolution is infallible (Chrome → system Chromium → bundled); this event
/// always fires. `family` identifies what will actually run, `path` is `None`
/// for the bundled binary, and `fallback_note` explains any substitution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserBinaryVerified {
    /// The resolved binary family.
    pub family: BinaryFamily,
    /// The resolved executable path, or `None` for the bundled binary.
    pub path: Option<std::path::PathBuf>,
    /// The detected major version (e.g. `"138"`), or `None` when undetectable
    /// or the binary is bundled.
    pub version_major: Option<String>,
    /// Human-readable note when resolution fell back from the requested family.
    pub fallback_note: Option<String>,
}

impl crate::common::bus::BusMessage for BrowserBinaryVerified {}

impl BrowserBinaryVerified {
    /// Builds the dashboard Notes string for the resolved browser binary.
    ///
    /// Format: `"<family> <version>"` (or the bundled/undetected variants),
    /// optionally suffixed with `" — <path>"` when a path is known, and
    /// optionally prefixed with `"<note>: "` when resolution fell back.
    #[must_use]
    pub fn display_label(&self) -> String {
        let label = match self.family {
            BinaryFamily::Bundled => "Chromium (bundled, version undetected)".to_owned(),
            _ => {
                let family = self.family_display();
                match &self.version_major {
                    Some(v) => format!("{family} {v}"),
                    None => format!(
                        "{family} {} (version undetected)",
                        jinn_web_fetch::stealth::CHROME_MAJOR
                    ),
                }
            }
        };

        let with_path = match &self.path {
            Some(p) => format!("{label} — {}", p.display()),
            None => label,
        };

        match &self.fallback_note {
            Some(note) => format!("{note}: {with_path}"),
            None => with_path,
        }
    }

    /// Returns the capitalized family name for display.
    fn family_display(&self) -> &'static str {
        match self.family {
            BinaryFamily::Chrome => "Chrome",
            BinaryFamily::Chromium => "Chromium",
            BinaryFamily::Bundled => "Bundled",
        }
    }
}

#[cfg(test)]
mod tests;
