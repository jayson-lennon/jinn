//! The theme slice's bus events.

use jinn_slices::BusMessage;
use serde::Deserialize;
use serde::Serialize;

/// A theme-directory re-scan completed and the theme-entries cell was
/// re-seeded with the fresh scan.
///
/// Published by the theme rescan actor after the cell write lands, so a
/// declarant of this event can re-resolve against the new entries
/// without racing the rescan — the same event-chained shape the persona
/// slice uses. Carries nothing: declarants read the cell under the
/// well-known slot, which the publisher has already replaced.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Theme directory re-scan completed.")]
pub struct ThemeEntriesReloaded;

impl BusMessage for ThemeEntriesReloaded {}
