//! App-state updated event — carries the full state after a save.

use serde::{Deserialize, Serialize};

use crate::AppStateFile;

/// Emitted after app-state has been persisted to disk.
///
/// Carries the full [`AppStateFile`] — the source of truth after save.
/// Listeners replace their cached copy wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStateUpdated {
    /// The full app-state as persisted to disk.
    pub state: AppStateFile,
}

impl jinn_slices::BusMessage for AppStateUpdated {}
