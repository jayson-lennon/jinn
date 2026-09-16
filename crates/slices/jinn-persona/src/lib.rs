//! The persona slice — persona discovery and selection state.
//!
//! Owns one cell ([`personas_slot`]) holding [`jinn_persona_msg::Personas`]:
//! the markdown personas scanned from the user and system persona
//! directories at activation. Activation also hands the scanned set back
//! to composition, which publishes the kernel's `PersonasLoaded` event —
//! the same contract the retired `persona-loader` plugin fulfilled over
//! the wire — so the session actor's consumer code is unchanged.

use std::path::Path;

use jinn_persona_msg::Persona;
use jinn_slices::SliceHost;

pub mod parse;

pub use jinn_persona_msg::Personas;
pub use jinn_persona_msg::personas_slot;
pub use parse::PersonaParseError;
pub use parse::parse_persona_content;
pub use parse::parse_persona_file;

/// Activates the slice: scans both persona directories, mints the
/// personas cell, and returns the scanned set for composition to publish
/// as `PersonasLoaded` after the session actor has subscribed.
///
/// Scan semantics (the retired plugin's): `.md` files only, one
/// unparseable file is noted on stderr and skipped — a single bad file
/// never drops the batch — and the user directory shadows the system
/// directory for same-name personas.
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a
/// wiring bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    personas_dir: &Path,
) -> Personas {
    let entries = scan(personas_dir);
    let scanned = Personas { entries };
    let _cell = host
        .register_cell(personas_slot(), scanned.clone())
        .expect("personas slot is registered exactly once at wiring");
    scanned
}

/// Scans both directories into the name-sorted persona set.
///
/// The user directory shadows the system directory for same-name
/// personas (matching `load_theme`'s user-first rule), so the system dir
/// is merged first and the user dir's personas overwrite them. Failures
/// are noted on stderr (host-side diagnostics) and never abort the batch.
#[must_use]
pub fn scan(personas_dir: &Path) -> Vec<Persona> {
    let mut defs = std::collections::BTreeMap::new();
    merge_dir(&mut defs, personas_dir);
    defs.into_values().collect()
}

/// Merges one directory's persona files into the accumulated set.
fn merge_dir(defs: &mut std::collections::BTreeMap<String, Persona>, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
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
                tracing::warn!(file = %path.display(), ?reason, "skipping unparseable persona file");
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

    /// Writes one parseable persona file into `dir`.
    fn write_persona(dir: &Path, file: &str, name: &str, description: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(
            dir.join(format!("{file}.md")),
            format!("+++\nname = \"{name}\"\ndescription = \"{description}\"\n+++\n\n{name} body."),
        )
        .expect("write persona");
    }

    #[rstest::rstest]
    fn scan_returns_personas_sorted_by_name() {
        // Given a directory with persona files written out of name order.
        let dir = tempfile::tempdir().expect("tmpdir");
        write_persona(dir.path(), "beta", "beta", "B");
        write_persona(dir.path(), "alpha", "alpha", "A");
        write_persona(dir.path(), "gamma", "gamma", "G");

        // When scanning.
        let personas = scan(dir.path());

        // Then personas are sorted by name.
        let names: Vec<_> = personas.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[rstest::rstest]
    fn same_name_persona_is_deduplicated_by_file_name() {
        // Given two files declaring the same persona name with different
        // descriptions (BTreeMap insert-overwrite: the later file wins).
        let dir = tempfile::tempdir().expect("tmpdir");
        write_persona(dir.path(), "a-first", "dup", "First");
        write_persona(dir.path(), "b-second", "dup", "Second");

        // When scanning.
        let personas = scan(dir.path());

        // Then one persona survives.
        assert_eq!(personas.len(), 1);
    }

    #[rstest::rstest]
    fn one_bad_file_never_drops_the_batch() {
        // Given one broken and one valid persona file.
        let user = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(user.path()).expect("mkdir");
        std::fs::write(user.path().join("invalid.md"), "Not a valid persona file.").expect("write");
        write_persona(user.path(), "valid", "valid", "V");

        // When scanning.
        let personas = scan(user.path());

        // Then only the valid persona survives.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "valid");
    }

    #[rstest::rstest]
    fn non_md_files_are_ignored() {
        // Given a .txt file with valid persona content.
        let user = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(user.path()).expect("mkdir");
        std::fs::write(
            user.path().join("notes.txt"),
            "+++\nname = \"hidden\"\ndescription = \"H\"\n+++\n\nBody.",
        )
        .expect("write");

        // When scanning.
        let personas = scan(user.path());

        // Then nothing is found.
        assert!(personas.is_empty());
    }

    #[rstest::rstest]
    fn empty_name_persona_is_skipped_but_batch_continues() {
        // Given a blank-name persona and a valid one.
        let user = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(user.path()).expect("mkdir");
        std::fs::write(
            user.path().join("blank.md"),
            "+++\nname = \"\"\ndescription = \"B\"\n+++\n\nBody.",
        )
        .expect("write");
        write_persona(user.path(), "valid", "valid", "V");

        // When scanning.
        let personas = scan(user.path());

        // Then only the named persona survives.
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "valid");
    }

    #[rstest::rstest]
    fn missing_directory_yields_empty_set() {
        // Given a directory that does not exist.
        let personas = scan(Path::new("/nonexistent-personas"));

        // Then the set is empty.
        assert!(personas.is_empty());
    }
}
