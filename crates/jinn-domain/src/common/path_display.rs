//! Shared path-display transforms.
//!
//! Path strings shown to the user in multiple places (the status bar, the CWD
//! input popup) must use one consistent shortening rule — replacing a leading
//! `$HOME` prefix with `~`. Centralizing the transform here keeps those displays
//! from drifting.

use std::path::Path;

/// Shorten a path for display: replace the home directory prefix with `~`.
///
/// Paths under `$HOME` collapse to `~/…` (or just `~` when the path *is* the home
/// directory); any other path is returned unchanged as a display string. Falls
/// back to the raw path when `dirs::home_dir()` cannot be determined.
///
/// This is a pure display transform — the [`crate::feat::cwd_input::resolve`]
/// logic expands `~` back when resolving user input, so shortened paths round-trip.
#[must_use]
pub fn shorten_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        let display = relative.display().to_string();
        if display.is_empty() {
            return "~".to_owned();
        }
        return format!("~/{display}");
    }
    path.display().to_string()
}
