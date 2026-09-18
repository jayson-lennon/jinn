//! Preferences updated event - carries the full preferences after a save.

use serde::{Deserialize, Serialize};

use crate::UserPreferences;

/// Emitted after preferences have been persisted to disk.
///
/// Carries the full [`UserPreferences`] - the source of truth after save.
/// Listeners replace their cached copy wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferencesUpdated {
    /// The full preferences as persisted to disk.
    pub preferences: UserPreferences,
}

impl jinn_slices::BusMessage for PreferencesUpdated {}
