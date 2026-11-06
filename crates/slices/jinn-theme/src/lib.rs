//! The theme slice — theme discovery and selection state.
//!
//! Owns one cell ([`theme_entries_slot`]) holding
//! [`jinn_theme_msg::ThemeEntries`]: the ordered theme selection the theme
//! picker displays and the app-state actor resolves the persisted theme
//! name against. `activate` scans the user theme directory once — the same
//! behavior the retired themes component had: one bad file never drops
//! the batch.

use marked_path::{Absolute, MarkedPath, MarkedPathBuf};

use jinn_slices::SliceHost;
use jinn_theme::loader::{discover_themes, load_theme_from_file};
use jinn_theme_msg::NamedTheme;
use jinn_theme_msg::ThemeEntries;

pub(crate) mod theme_picker_actions;
pub mod theme_picker_render;
pub mod theme_picker_routes;
#[cfg(test)]
mod theme_picker_tests;
pub mod theme_rescan_actor;
#[cfg(test)]
mod theme_rescan_actor_tests;

pub use jinn_theme_msg::theme_entries_slot;
pub use jinn_theme_msg::theme_picker_scope;
pub use theme_picker_routes::open_from_scope as open_theme_picker_from_scope;

/// Activates the slice: scans the theme directory and re-seeds the
/// catalog-registered theme-entries cell with the ordered results.
///
/// The cell itself belongs to the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`), which registers it with the
/// empty set so every harness has one; the scan is this slice's job, so it
/// writes the scanned value through the resolved handle rather than
/// minting a second cell nobody else can see.
///
/// The built-in default is pinned first; a scanned theme named "default"
/// replaces the built-in's look while keeping its reserved slot; the rest
/// follow in case-insensitive name order. One unloadable file is noted on
/// stderr and skipped — a single bad file never drops the batch.
///
/// # Panics
///
/// Panics if the catalog has not run — the picker and the app-state actor
/// would then resolve no entries at all.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    themes_dir: &MarkedPath<Absolute>,
) {
    let entries = scan(themes_dir);
    let cell = host
        .slices()
        .reader::<ThemeEntries>(&theme_entries_slot())
        .expect("the cell catalog registers the theme-entries slot before any slice activates");
    cell.update(|slot| *slot = entries);
}

/// Re-runs the directory scan and re-seeds the theme-entries cell.
///
/// The live-reload path calls this on `ResourcesChanged`: a theme file
/// added, edited, or removed while jinn runs is reflected in the picker
/// on the next open. The cell is resolved, not minted, so the shared
/// catalog keeps a single entry set; a catalog that never ran is a
/// wiring bug, and here it degrades to a logged no-op rather than a
/// panic (this runs long after launch).
#[must_use]
pub fn rescan(
    slices: &jinn_slices::Slices,
    themes_dir: &MarkedPath<Absolute>,
) -> Option<ThemeEntries> {
    let entries = scan(themes_dir);
    match slices.reader::<ThemeEntries>(&theme_entries_slot()) {
        Some(cell) => {
            cell.update(|slot| *slot = entries.clone());
            Some(entries)
        }
        None => {
            tracing::warn!("theme rescan found no catalog cell; the picker keeps its old entries");
            None
        }
    }
}

/// Registers the theme picker: its overlay, its keys, and its
/// filter hook.
///
/// Split from [`activate`] because the picker needs the same host but is not
/// part of theme discovery. Called from composition right after `activate`,
/// so the picker's rows are seeded from the entries cell `activate` just
/// re-seeded.
///
/// Like [`activate`], the picker's cell belongs to the shared catalog, so
/// this resolves the handle rather than minting one.
///
/// # Panics
///
/// Panics if the catalog has not run — the menu would otherwise render
/// against an absent cell and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate_picker(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .slices()
        .reader::<jinn_theme_msg::ThemePickerState>(&jinn_theme_msg::theme_picker_slot())
        .expect("the cell catalog registers the theme-picker slot before any slice activates");

    let scope = theme_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(theme_picker_render::theme_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_theme_msg::theme_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(theme_picker_render::render_theme_picker),
    );

    // The picker's keys, and the filter's input hook, are this slice's own.
    theme_picker_routes::attach_theme_picker_rows(host.key_routes(), &cell);
    theme_picker_routes::register_theme_picker_input_hook(host.key_routes(), &cell);
}

/// Scans the theme directory into the ordered selection.
///
/// Failures are noted on stderr (host-side diagnostics) and never abort
/// the batch.
#[must_use]
pub fn scan(themes_dir: &MarkedPath<Absolute>) -> ThemeEntries {
    let mut defs = std::collections::BTreeMap::new();
    match discover_themes(themes_dir) {
        Ok(found) => merge_dir(&mut defs, found),
        Err(report) => note_scan_failure(themes_dir, &report),
    }

    let mut entries = vec![NamedTheme {
        name: "default".to_owned(),
        theme: jinn_theme::default_theme(),
    }];
    if let Some(default) = defs.remove("default")
        && let Some(pinned) = entries.first_mut()
    {
        pinned.theme = default;
    }
    entries.extend(
        defs.into_iter()
            .map(|(name, theme)| NamedTheme { name, theme }),
    );
    ThemeEntries { entries }
}

/// Merges one directory's discoveries into the accumulated set.
#[expect(
    clippy::print_stderr,
    reason = "host-side diagnostics: the theme loader runs before tracing exists"
)]
fn merge_dir(
    defs: &mut std::collections::BTreeMap<String, jinn_theme::Theme>,
    found: Vec<(String, MarkedPathBuf<Absolute>)>,
) {
    for (name, path) in found {
        match load_theme_from_file(&path.as_marked_path()) {
            Ok(theme) => {
                defs.insert(name, theme);
            }
            Err(report) => {
                eprintln!("theme-slice: skipping theme `{name}`: {report}");
            }
        }
    }
}

/// Notes a directory scan failure on stderr (host-side diagnostics).
#[expect(
    clippy::print_stderr,
    reason = "host-side diagnostics before tracing is available in the theme loader"
)]
fn note_scan_failure(
    dir: &MarkedPath<Absolute>,
    report: &error_stack::Report<jinn_theme::ThemeError>,
) {
    eprintln!("theme-slice: scan failed for {dir}: {report}");
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use marked_path::MarkedPathAccess;

    /// Writes one loadable theme TOML into `dir`.
    fn write_theme(dir: &MarkedPathBuf<Absolute>, name: &str, accent: &str) {
        std::fs::create_dir_all(dir.as_path()).expect("mkdir");
        let file = dir
            .join(format!("{name}.toml"))
            .expect("typed parent joins a bare name");
        std::fs::write(file.as_path(), format!("focus_accent = \"{accent}\"\n"))
            .expect("write theme");
    }

    #[rstest::rstest]
    fn scan_orders_themes_with_default_first() {
        // Given a user dir with three themes.
        let user = jinn_testutil::tempdir();
        write_theme(&user.path, "beta", "#ff0000");
        write_theme(&user.path, "alpha", "#00ff00");
        write_theme(&user.path, "gamma", "#0000ff");

        // When scanning.
        let entries = scan(&user.path.as_marked_path());

        // Then default is pinned first and the rest follow in
        // case-insensitive name order.
        let names: Vec<_> = entries.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["default", "alpha", "beta", "gamma"]);
    }

    #[rstest::rstest]
    fn a_theme_named_default_replaces_the_builtin() {
        // Given a scanned theme named "default".
        let user = jinn_testutil::tempdir();
        write_theme(&user.path, "default", "#123456");

        // When scanning.
        let entries = scan(&user.path.as_marked_path());

        // Then the pinned first entry carries the scanned look under the
        // reserved "default" name.
        assert_eq!(entries.entries[0].name, "default");
        assert_eq!(
            entries.entries[0].theme.focus_accent,
            "#123456".parse().expect("color")
        );
    }

    #[rstest::rstest]
    fn one_bad_file_never_drops_the_batch() {
        // Given one broken toml and one good toml.
        let user = jinn_testutil::tempdir();
        std::fs::create_dir_all(user.path.as_path()).expect("mkdir");
        std::fs::write(
            user.path
                .join("broken.toml")
                .expect("temp root joins a bare name"),
            "not [valid toml",
        )
        .expect("write");
        write_theme(&user.path, "good", "#00ff00");

        // When scanning.
        let entries = scan(&user.path.as_marked_path());

        // Then only the good theme survives, alongside the pinned default.
        let names: Vec<_> = entries.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["default", "good"]);
    }

    #[rstest::rstest]
    fn missing_directories_yield_default_only() {
        // Given a theme directory that does not exist.
        let root = jinn_testutil::tempdir();
        let user = root
            .path
            .join("nonexistent-themes")
            .expect("temp root joins a bare name");

        // When scanning the directory.
        let entries = scan(&user.as_marked_path());

        // Then the selection is the built-in default alone.
        assert_eq!(entries.entries.len(), 1);
        assert_eq!(entries.entries[0].name, "default");
    }
}
