//! Runner dispatch for TUI and headless modes.

use error_stack::{Report, ResultExt};

use crate::app::AppError;
#[cfg(debug_assertions)]
use crate::headless::HeadlessApp;

/// Mode-specific runner.
///
/// Each variant owns the state needed for its execution mode.
/// The [`Runner::run`] method delegates to the appropriate event loop.
pub enum Runner {
    /// Terminal UI mode.
    Tui(Box<jinn_tui::TuiApp>),
    /// Headless (non-interactive) mode.
    #[cfg(debug_assertions)]
    Headless(Box<HeadlessApp>),
}

impl Runner {
    /// Returns a handle to the root supervisor actor ref, for coordinated shutdown.
    ///
    /// Returns `None` in modes that don't have an actor system (e.g. headless without services).
    pub fn root_supervisor(
        &self,
    ) -> Option<jinn_domain::common::root_supervisor::RootSupervisorRef> {
        match self {
            Runner::Tui(app) => Some(app.services.root_supervisor.clone()),
            #[cfg(debug_assertions)]
            Runner::Headless(app) => Some(app.root_supervisor()),
        }
    }

    /// Returns the trouper system handle, for the graceful shutdown sweep
    /// at exit.
    ///
    /// Returns `None` in modes that don't have a trouper fabric.
    pub fn trouper_system(&self) -> Option<trouper::system::ActorSystem> {
        match self {
            Runner::Tui(app) => Some(app.services.trouper_system.clone()),
            #[cfg(debug_assertions)]
            Runner::Headless(app) => Some(app.trouper_system()),
        }
    }

    /// Runs the selected mode to completion.
    ///
    /// For TUI mode, runs the terminal event loop.
    /// For headless mode, runs until settled, prints history, and shuts down.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner fails.
    pub fn run(self) -> Result<(), Report<AppError>> {
        match self {
            Runner::Tui(app) => {
                jinn_tui::run(*app).change_context(AppError)?;
            }
            #[cfg(debug_assertions)]
            Runner::Headless(app) => {
                app.shutdown();
                app.print_history();
            }
        }
        Ok(())
    }
}
