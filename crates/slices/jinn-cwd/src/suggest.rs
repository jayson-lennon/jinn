//! Pure suggestion ranking for the cwd popup's ghost.
//!
//! One function, no I/O: given the segment the user is typing and one
//! directory's entries (already read by the actor), pick the directory that
//! becomes the ghost. Ranking is `SkimMatcherV2` score descending, ties
//! breaking to the shorter name then alphabetically; an empty segment ghosts
//! the alphabetically-first directory, and a segment that case-insensitively
//! equals an existing directory's name ghosts only that directory so repeated
//! Tab walks down the tree.

use fuzzy_matcher::FuzzyMatcher as _;
use fuzzy_matcher::skim::SkimMatcherV2;

/// One candidate directory, as the actor's listing hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostCandidate {
    /// The entry's file name (no separator).
    pub name: String,
    /// Whether the entry is a directory — non-directories never ghost.
    pub is_dir: bool,
}

/// The ghost for a typed segment over a directory's entries.
///
/// * Empty segment → the alphabetically-first directory (so repeated Tab
///   walks down into the tree).
/// * A segment that case-insensitively equals a directory's name → that
///   name (the ghost is then its separator; Tab accepts and re-suggests
///   inside it).
/// * Otherwise → the best-scoring fuzzy match among directories (ties:
///   shorter name, then alphabetical).
///
/// Returns `None` when nothing matches — the popup ghosts nothing and Tab
/// is a consumed no-op.
#[must_use]
pub fn suggest_ghost(segment: &str, entries: &[GhostCandidate]) -> Option<String> {
    if segment.is_empty() {
        return entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| e.name.as_str())
            .min()
            .map(str::to_owned);
    }

    // Exact-name walk: a case-insensitive full match wins outright so Tab
    // descends (`~/` → first child → its first child …) instead of scoring
    // the already-typed name against its siblings.
    if let Some(exact) = entries
        .iter()
        .filter(|e| e.is_dir)
        .find(|e| e.name.eq_ignore_ascii_case(segment))
    {
        return Some(exact.name.clone());
    }

    let matcher = SkimMatcherV2::default();
    entries
        .iter()
        .filter(|e| e.is_dir)
        .filter_map(|e| {
            matcher
                .fuzzy_match(&e.name, segment)
                .map(|score| (score, e))
        })
        .max_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.name.len().cmp(&b.1.name.len()))
                .then_with(|| a.1.name.cmp(&b.1.name))
        })
        .map(|(_, e)| e.name.clone())
}

/// The ghost text to render after the cursor for `winner` against the typed
/// `segment`.
///
/// When the winner extends the segment as a case-insensitive prefix, the
/// ghost is the remainder only (`co` + `completed-projects` →
/// `mpleted-projects`); otherwise the whole name is ghosted for type-over
/// (`mkn` + `mkdir` → `mkdir`). The caller appends the separator.
#[must_use]
pub fn ghost_remainder(segment: &str, winner: &str) -> String {
    let extends = segment.len() <= winner.len()
        && winner
            .get(..segment.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(segment));
    if extends {
        winner.get(segment.len()..).unwrap_or_default().to_owned()
    } else {
        winner.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;

    /// Candidate list fixture: `name` `/` `dir|file` per entry.
    fn candidates(specs: &[&str]) -> Vec<GhostCandidate> {
        specs
            .iter()
            .map(|spec| {
                let (name, kind) = spec.split_once('/').expect("fixture is name/kind");
                GhostCandidate {
                    name: name.to_owned(),
                    is_dir: kind == "dir",
                }
            })
            .collect()
    }

    #[rstest::rstest]
    fn empty_segment_ghosts_the_alphabetically_first_directory() {
        // Given a directory listing with a file and three subdirectories.
        let entries = candidates(&["zeta/dir", "alpha/dir", "mid/dir", "afile/file"]);

        // When suggesting for an empty segment.
        let ghost = suggest_ghost("", &entries);

        // Then the first directory alphabetically wins, files excluded.
        assert_eq!(ghost.as_deref(), Some("alpha"));
    }

    #[rstest::rstest]
    fn empty_segment_with_no_directories_ghosts_nothing() {
        // Given a listing holding only a file.
        let entries = candidates(&["afile/file"]);

        // When suggesting for an empty segment.
        let ghost = suggest_ghost("", &entries);

        // Then there is no ghost.
        assert_eq!(ghost, None);
    }

    #[rstest::rstest]
    fn higher_fuzzy_score_wins() {
        // Given directories where one is a much better fuzzy match for the
        // segment.
        let entries = candidates(&["mkdir/dir", "mknods/dir", "models/dir"]);

        // When suggesting for a typo'd segment.
        let ghost = suggest_ghost("mkn", &entries);

        // Then the closest match wins over alphabetical order.
        assert_eq!(ghost.as_deref(), Some("mknods"));
    }

    #[rstest::rstest]
    fn equal_scores_break_to_the_shorter_name() {
        // Given two directories with the same fuzzy score shape and a
        // third that cannot compete (`zig` subsequence-fails `mn`).
        let entries = candidates(&["mnx/dir", "mn/dir", "zig/dir"]);

        // When suggesting for a segment both candidates match equally.
        let ghost = suggest_ghost("mn", &entries);

        // Then the shorter name wins the tie.
        assert_eq!(ghost.as_deref(), Some("mn"));
    }

    #[rstest::rstest]
    fn files_never_ghost() {
        // Given a listing whose best fuzzy match for the segment is a file.
        let entries = candidates(&["mknods/dir", "mkn-file/file"]);

        // When suggesting for a segment.
        let ghost = suggest_ghost("mkn", &entries);

        // Then the directory wins even when the file scores as high or
        // higher — ghosts navigate, they do not name files.
        assert_eq!(ghost.as_deref(), Some("mknods"));
    }

    #[rstest::rstest]
    fn exact_name_case_insensitive_ghosts_only_that_directory() {
        // Given a listing holding the directory the segment fully names.
        let entries = candidates(&["Projects/dir", "proj/dir", "other/dir"]);

        // When suggesting with the name in a different case.
        let ghost = suggest_ghost("projects", &entries);

        // Then that directory ghosts (Tab accepts it and re-suggests inside
        // it) rather than scoring it against siblings.
        assert_eq!(ghost.as_deref(), Some("Projects"));
    }

    #[rstest::rstest]
    fn prefix_extension_ghosts_the_remainder() {
        // Given a winner that extends the typed segment.
        // When computing the display remainder.
        let remainder = ghost_remainder("co", "completed-projects");

        // Then the ghost is the untyped part only.
        assert_eq!(remainder, "mpleted-projects");
    }

    #[rstest::rstest]
    fn prefix_extension_is_case_insensitive() {
        // Given a winner whose case differs from the typed segment.
        // When computing the display remainder.
        let remainder = ghost_remainder("CO", "Completed-Projects");

        // Then the remainder starts past the typed length.
        assert_eq!(remainder, "mpleted-Projects");
    }

    #[rstest::rstest]
    fn non_matching_segment_type_over_ghosts_the_whole_name() {
        // Given a winner that does not extend the typed segment (a typo).
        // When computing the display remainder.
        let remainder = ghost_remainder("mkn", "mkdir");

        // Then the whole winner name ghosts for type-over.
        assert_eq!(remainder, "mkdir");
    }

    #[rstest::rstest]
    fn segment_longer_than_winner_ghosts_the_whole_name() {
        // Given a typed segment longer than the winner name.
        // When computing the display remainder.
        let remainder = ghost_remainder("completed-projects-x", "completed-projects");

        // Then the whole winner name ghosts (no out-of-bounds slicing).
        assert_eq!(remainder, "completed-projects");
    }
}
