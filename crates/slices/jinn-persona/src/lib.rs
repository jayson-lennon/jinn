//! The persona slice — persona discovery and selection state.
//!
//! Owns one cell ([`personas_slot`]) holding [`jinn_persona_msg::Personas`]:
//! the markdown personas scanned from the user persona directory at
//! activation. Activation also hands the scanned set back
//! to composition, which publishes the kernel's `PersonasLoaded` event —
//! the same contract the retired `persona-loader` component fulfilled over
//! the wire — so the session actor's consumer code is unchanged.

use marked_path::{Absolute, MarkedPathBuf};

use jinn_slices::Persona;
use jinn_slices::SliceHost;

pub mod parse;
mod persona_picker_actions;
pub mod persona_picker_render;
mod persona_picker_routes;
#[cfg(test)]
mod persona_picker_tests;
pub mod persona_rescan_actor;
#[cfg(test)]
mod persona_rescan_actor_tests;
pub use jinn_persona_msg::persona_picker_scope;

pub use jinn_persona_msg::Personas;
pub use jinn_persona_msg::personas_slot;
pub use parse::PersonaParseError;
pub use parse::parse_persona_content;
pub use parse::parse_persona_file;

/// Activates the slice: scans the persona directory, re-seeds the
/// catalog-registered personas cell with the result, and returns the
/// scanned set for composition to publish as `PersonasLoaded` after the
/// session actor has subscribed.
///
/// The cell itself belongs to the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`), which registers it with an
/// empty set so every harness has one; the scan is this slice's job, so it
/// writes the scanned value through the resolved handle rather than
/// minting a second cell nobody else can see.
///
/// Scan semantics (the inherited ones): `.md` files only, one
/// unparseable file is noted on stderr and skipped — a single bad file
/// never drops the batch.
///
/// # Panics
///
/// Panics if the catalog has not run — the picker and the session actor's
/// consumers would then read an empty persona set forever.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    personas_dir: &MarkedPathBuf<Absolute>,
) -> Personas {
    let entries = scan(personas_dir);
    let scanned = Personas { entries };
    let cell = host
        .slices()
        .reader::<Personas>(&personas_slot())
        .expect("the cell catalog registers the personas slot before any slice activates");
    cell.update(|slot| *slot = scanned.clone());
    scanned
}

/// Registers the persona picker: its overlay, its keys, and its
/// filter hook, and the scope-enter hook that seeds the menu.
///
/// Split from [`activate`] because the picker needs the same host but is not
/// part of persona discovery. Called from composition right after `activate`.
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
        .reader::<jinn_persona_msg::PersonaPickerState>(&jinn_persona_msg::persona_picker_slot())
        .expect("the cell catalog registers the persona-picker slot before any slice activates");

    let scope = persona_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(persona_picker_render::persona_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_persona_msg::persona_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(persona_picker_render::render_persona_picker),
    );

    // The picker's keys, its filter's input hook, and the scope-enter hook
    // that builds the menu are all this slice's own.
    persona_picker_routes::attach_persona_picker_rows(host.key_routes(), &cell);
    persona_picker_routes::register_persona_picker_input_hook(host.key_routes(), &cell);
    persona_picker_routes::register_persona_picker_enter_hook(host.key_routes(), &cell);
}

/// Scans the persona directory into the name-sorted persona set.
///
/// Failures are noted on stderr (host-side diagnostics) and never abort
/// the batch.
#[must_use]
pub fn scan(personas_dir: &MarkedPathBuf<Absolute>) -> Vec<Persona> {
    let mut defs = std::collections::BTreeMap::new();
    merge_dir(&mut defs, personas_dir);
    defs.into_values().collect()
}

/// Merges one directory's persona files into the accumulated set.
fn merge_dir(
    defs: &mut std::collections::BTreeMap<String, Persona>,
    dir: &MarkedPathBuf<Absolute>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let Ok(path) = MarkedPathBuf::<Absolute>::new(&path) else {
            continue;
        };
        match parse::parse_persona_file(&path) {
            Ok(persona) => {
                // The retired coordinator's translation dropped
                // empty-name definitions individually — preserve that
                // contract: a blank-name file is skipped, the batch
                // continues.
                if persona.name.trim().is_empty() {
                    continue;
                }
                defs.insert(persona.name.clone(), persona);
            }
            Err(report) => {
                let reason = report.current_context();
                tracing::warn!(file = %path, ?reason, "skipping unparseable persona file");
            }
        }
    }
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

    /// Writes one parseable persona file into `dir`.
    fn write_persona(dir: &MarkedPathBuf<Absolute>, file: &str, name: &str, description: &str) {
        std::fs::create_dir_all(dir.as_path()).expect("mkdir");
        let file_path = dir
            .join(format!("{file}.md"))
            .expect("typed parent joins a bare name");
        std::fs::write(
            file_path.as_path(),
            format!("+++\nname = \"{name}\"\ndescription = \"{description}\"\n+++\n\n{name} body."),
        )
        .expect("write persona");
    }

    #[rstest::rstest]
    fn scan_returns_personas_sorted_by_name() {
        // Given a directory with persona files written out of name order.
        let dir = jinn_testutil::tempdir();
        write_persona(&dir.path, "beta", "beta", "B");
        write_persona(&dir.path, "alpha", "alpha", "A");
        write_persona(&dir.path, "gamma", "gamma", "G");

        // When scanning.
        let personas = scan(&dir.path);

        // Then personas are sorted by name.
        let names: Vec<_> = personas.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[rstest::rstest]
    fn same_name_persona_is_deduplicated_by_file_name() {
        // Given two files declaring the same persona name with different
        // descriptions (BTreeMap insert-overwrite: the later file wins).
        let dir = jinn_testutil::tempdir();
        write_persona(&dir.path, "a-first", "dup", "First");
        write_persona(&dir.path, "b-second", "dup", "Second");

        // When scanning.
        let personas = scan(&dir.path);

        // Then one persona survives.
        assert_eq!(personas.len(), 1);
    }

    #[rstest::rstest]
    fn one_bad_file_never_drops_the_batch() {
        // Given one broken and one valid persona file.
        let user = jinn_testutil::tempdir();
        std::fs::create_dir_all(user.path.as_path()).expect("mkdir");
        std::fs::write(
            user.path
                .join("invalid.md")
                .expect("temp root joins a bare name"),
            "Not a valid persona file.",
        )
        .expect("write");
        write_persona(&user.path, "valid", "valid", "V");

        // When scanning.
        let personas = scan(&user.path);

        // Then only the valid persona survives.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "valid");
    }

    #[rstest::rstest]
    fn non_md_files_are_ignored() {
        // Given a .txt file with valid persona content.
        let user = jinn_testutil::tempdir();
        std::fs::create_dir_all(user.path.as_path()).expect("mkdir");
        std::fs::write(
            user.path
                .join("notes.txt")
                .expect("temp root joins a bare name"),
            "+++\nname = \"hidden\"\ndescription = \"H\"\n+++\n\nBody.",
        )
        .expect("write");

        // When scanning.
        let personas = scan(&user.path);

        // Then nothing is found.
        assert!(personas.is_empty());
    }

    #[rstest::rstest]
    fn empty_name_persona_is_skipped_but_batch_continues() {
        // Given a blank-name persona and a valid one.
        let user = jinn_testutil::tempdir();
        std::fs::create_dir_all(user.path.as_path()).expect("mkdir");
        std::fs::write(
            user.path
                .join("blank.md")
                .expect("temp root joins a bare name"),
            "+++\nname = \"\"\ndescription = \"B\"\n+++\n\nBody.",
        )
        .expect("write");
        write_persona(&user.path, "valid", "valid", "V");

        // When scanning.
        let personas = scan(&user.path);

        // Then only the named persona survives.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "valid");
    }

    #[rstest::rstest]
    fn missing_directory_yields_empty_set() {
        // Given a directory that does not exist.
        let root = jinn_testutil::tempdir();
        let missing = root
            .path
            .join("nonexistent-personas")
            .expect("temp root joins a bare name");

        // When scanning it for personas.
        let personas = scan(&missing);

        // Then the set is empty.
        assert!(personas.is_empty());
    }
}
