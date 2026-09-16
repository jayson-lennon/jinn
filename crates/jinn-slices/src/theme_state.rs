//! The theme slice's shared cell vocabulary.
//!
//! [`ThemeEntries`] lives in the slice-surface layer (not in the slice
//! crate) because the *readers* include kernel-resident code: the theme
//! picker spec (the picker framework's specs live in the kernel by
//! design) and the app-state actor's persisted-theme resolution. The
//! writer — the activation-time directory scan — lives in the slice
//! crate. Both import this one type; neither depends on the other.

use crate::SlotKey;
use jinn_theme::Theme;

/// One selectable theme: its display name and resolved colors.
///
/// A tuple rather than `jinn_theme::ThemeEntry` — that type sits behind
/// the `entry` feature (pulling the picker widget stack) which this
/// vocabulary layer does not enable.
#[derive(Debug, Clone)]
pub struct NamedTheme {
    /// The theme's display name ("default", "gruvbox-dark", …).
    pub name: String,
    /// The resolved theme colors.
    pub theme: Theme,
}

/// The theme slice's cell payload.
///
/// The ordered selection the theme picker displays: the built-in
/// "default" pinned first (a scanned theme named "default" replaces the
/// built-in's look while keeping its reserved slot), the rest sorted
/// case-insensitively by name. Written once at slice activation; read by
/// the picker's open hook and the app-state actor's persisted-name
/// resolution.
#[derive(Debug, Default, Clone)]
pub struct ThemeEntries {
    /// The ordered theme selection.
    pub entries: Vec<NamedTheme>,
}

impl ThemeEntries {
    /// Looks up one theme by exact name.
    #[must_use]
    pub fn theme(&self, name: &str) -> Option<&Theme> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| &e.theme)
    }
}

/// The slot key the theme slice's cell lives under.
#[must_use]
pub fn theme_entries_slot() -> SlotKey {
    SlotKey::builtin("theme", "entries")
}
