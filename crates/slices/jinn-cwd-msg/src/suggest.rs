//! The cwd popup's suggestion request and its pure path helpers.
//!
//! [`SuggestCwdCompletion`] is the command the popup's input hook mints on
//! every edit; [`CwdSuggestActor`] is its one declarant. The helpers here are
//! the pure half of the suggestion pipeline: splitting the typed input into
//! (parent, segment) at the cursor and resolving that parent against the
//! popup's open-time base with the same tilde/relative semantics the confirm
//! resolver uses. No filesystem I/O lives in this module — the actor does the
//! one `read_dir`, on a blocking thread.

// The `expect`s below are typed-path re-typing joins, covered by the
// crate-level family policy in `cwd_input_state.rs` (home, base, and
// normalized values are absolute by construction).
#![allow(
    clippy::expect_used,
    reason = "typed-path re-typing joins on absolute-by-construction values, per the crate's family policy"
)]

use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};

use serde::{Deserialize, Serialize};

use jinn_slices::BusMessage;

/// Command: suggest a directory completion for the cwd popup's current input.
///
/// `request_id` is the staleness token. The actor writes its ghost only when
/// this matches `cwd_input.expected_request_id`, so an earlier, slow listing
/// cannot overwrite a newer request's answer.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Suggest a directory completion for the cwd popup.")]
pub struct SuggestCwdCompletion {
    /// The directory whose subdirectories are the suggestion candidates —
    /// the lexical parent of the segment before the cursor, already
    /// resolved absolute. Exactly one directory per request; never recursed.
    pub parent_dir: MarkedPathBuf<Absolute>,
    /// The segment the user is typing (the text after the input's last
    /// separator, before the cursor). Empty for a trailing separator.
    pub segment: String,
    /// Monotonic id tying this request to the expected reply slot.
    pub request_id: u64,
}

impl BusMessage for SuggestCwdCompletion {}

/// The text before the cursor ends in a path separator.
///
/// `is_separator` is platform-aware: both `/` and `\` count on Windows, only
/// `/` on Unix, matching the confirm resolver's parsing.
#[must_use]
pub fn ends_with_separator(text: &str) -> bool {
    text.chars()
        .next_back()
        .is_some_and(std::path::is_separator)
}

/// Splits the text before the cursor into its path parent and the segment
/// being typed.
///
/// The split point is the last separator in the text *before the cursor*,
/// so a cursor parked mid-word splits at the word's own start (`~/co^mpleted`
/// → parent `~/`, segment `co`). The separator itself stays with the parent.
/// Text after the cursor is ignored — the suggestion extends at the cursor.
///
/// Returns `(parent_text, segment)`; the parent is empty when the input has
/// no separator at all (a bare relative segment).
#[must_use]
pub fn split_at_cursor(text_before_cursor: &str) -> (&str, &str) {
    let split = text_before_cursor
        .char_indices()
        .rev()
        .find(|(_, ch)| std::path::is_separator(*ch))
        .map_or(0, |(idx, ch)| idx + ch.len_utf8());
    text_before_cursor.split_at(split)
}

/// Resolves a raw parent text against the popup's open-time base, expanding
/// `~` against `home`, with the same semantics as the confirm resolver
/// ([`crate::resolve_cwd_input_with_home`], minus canonicalization).
///
/// `tilde` → home; a bare `~` plus separator → home; absolute-on-this-
/// platform → itself (the marker's verdict); anything else → joined against
/// `base` (relative or `.`/`..` navigation). Lexical normalization collapses
/// `.` and `..` so `~/x/../y` and `base/../y` list the directory the user
/// navigated to.
///
/// The home is threaded in (never read here) so the popup's suggestion and
/// confirm paths agree on expansion under test injection.
///
/// # Panics
///
/// Panics only on the typed-path re-typing invariants documented at the
/// top of this crate: expanding a tilde against an absolute home, joining
/// a non-absolute parent against an absolute base, and normalizing an
/// absolute path all keep the marker absolute — a failure is programmer
/// error, matching the crate family's `expect` policy.
#[must_use]
pub fn resolve_parent_dir(
    parent_text: &str,
    base: &MarkedPath<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> MarkedPathBuf<Absolute> {
    if parent_text == "~" {
        return home.clone();
    }
    if let Some(rest) = parent_text.strip_prefix('~') {
        let mut chars = rest.chars();
        if let Some(sep) = chars.next()
            && std::path::is_separator(sep)
        {
            return home
                .join(chars.as_str())
                .expect("expanding a tilde keeps the home absolute");
        }
    }
    let absolute = match MarkedPathBuf::<Absolute>::new(parent_text) {
        Ok(typed) => typed,
        Err(_) => base
            .join(parent_text)
            .expect("joining a non-absolute parent keeps the base absolute"),
    };
    // `..` segments collapse against the join result before listing, so the
    // suggestion names the directory the user navigated to, not the literal
    // parent-of text. Re-typing an already-absolute path introduces no new
    // prefix.
    #[expect(
        clippy::disallowed_types,
        reason = "reassembly seam: collects the components of an already-typed absolute path; the reassembled value is re-typed immediately and can introduce no new prefix"
    )]
    let normalized: std::path::PathBuf = {
        use std::path::Component;
        let mut components: Vec<Component<'_>> = Vec::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => match components.last() {
                    Some(Component::Normal(_)) => {
                        components.pop();
                    }
                    Some(Component::RootDir | Component::Prefix(_)) => {}
                    _ => components.push(component),
                },
                _ => components.push(component),
            }
        }
        components.into_iter().map(Component::as_os_str).collect()
    };
    MarkedPathBuf::<Absolute>::new(normalized).expect("normalizing an absolute path stays absolute")
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
    use jinn_testutil::volume_root;

    /// A base/home fixture pair under one temp root, so tilde and relative
    /// cases both start from portable absolute paths.
    fn base_and_home() -> (MarkedPathBuf<Absolute>, MarkedPathBuf<Absolute>) {
        let root = jinn_testutil::tempdir();
        let base = root.path.join("work").expect("temp root joins a bare name");
        let home = root.path.join("home").expect("temp root joins a bare name");
        (base, home)
    }

    #[rstest::rstest]
    #[case("~/", "~/", "")]
    #[case("~/co", "~/", "co")]
    #[case("foo/bar/ba", "foo/bar/", "ba")]
    #[case("bare", "", "bare")]
    #[case("/", "/", "")]
    fn split_divides_at_the_last_separator_before_the_cursor(
        #[case] text: &str,
        #[case] expected_parent: &str,
        #[case] expected_segment: &str,
    ) {
        // Given the text before the cursor.
        // When splitting at the cursor.
        let (parent, segment) = split_at_cursor(text);

        // Then the parent keeps the separator and the segment is the rest.
        assert_eq!(parent, expected_parent);
        assert_eq!(segment, expected_segment);
    }

    #[rstest::rstest]
    #[case("~/com", "~/", "com")]
    #[case("~/completed-projects", "~/", "completed-projects")]
    fn split_ignores_text_after_the_cursor(
        #[case] before: &str,
        #[case] expected_parent: &str,
        #[case] expected_segment: &str,
    ) {
        // Given a cursor parked mid-word: the before-cursor half is all
        // the split sees (text after the cursor is not part of it).
        // When splitting the before-cursor half.
        let (parent, segment) = split_at_cursor(before);

        // Then the segment is the before-cursor part only.
        assert_eq!(parent, expected_parent);
        assert_eq!(segment, expected_segment);
    }

    #[rstest::rstest]
    #[case::native(true)]
    #[case::other_separator(false)]
    fn trailing_separator_is_detected_via_is_separator(#[case] native: bool) {
        // Given a text ending in one separator spelling.
        let text = if native {
            format!("a{}", std::path::MAIN_SEPARATOR)
        } else {
            "a\\".to_owned()
        };

        // When checking for a trailing separator.
        let detected = ends_with_separator(&text);

        // Then the platform-native spelling is always a separator, and the
        // other spelling counts as one only where the platform says so
        // (`\` is a separator on Windows, a literal on Unix) — the same
        // parsing the confirm resolver gets from `is_separator`.
        assert_eq!(detected, native || cfg!(windows));
    }

    #[rstest::rstest]
    fn tilde_resolves_to_home() {
        // Given a base and home.
        let (base, home) = base_and_home();

        // When resolving the tilde parent.
        let dir = resolve_parent_dir("~", &base.as_marked_path(), &home);

        // Then it is the home.
        assert_eq!(dir, home);
    }

    #[rstest::rstest]
    fn tilde_with_separator_resolves_under_home() {
        // Given a base and home, with the native separator after the tilde.
        let (base, home) = base_and_home();
        let text = format!("~{}code", std::path::MAIN_SEPARATOR);

        // When resolving it.
        let dir = resolve_parent_dir(&text, &base.as_marked_path(), &home);

        // Then it is home/code.
        assert_eq!(dir, home.join("code").expect("home joins a bare name"));
    }

    #[rstest::rstest]
    fn relative_parent_resolves_against_base() {
        // Given a base and home.
        let (base, home) = base_and_home();

        // When resolving a relative parent with a `..` in it.
        let dir = resolve_parent_dir("a/b/../c", &base.as_marked_path(), &home);

        // Then the dot segments collapse against the base.
        let expected = base.join("a").expect("a").join("c").expect("c");
        assert_eq!(dir, expected);
    }

    #[rstest::rstest]
    fn dotdot_above_base_clamps_at_its_root() {
        // Given a base under the temp root.
        let (base, home) = base_and_home();

        // When resolving `..`s that outnumber the base's components — one
        // `..` per normal component above the root, derived from the base
        // itself so the fixture holds at any temp-dir depth (the temp dir
        // is one component under the root on Linux but several on Windows).
        let depth = base
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .count();
        let ups = vec![".."; depth].join(std::path::MAIN_SEPARATOR_STR);
        let dir = resolve_parent_dir(&ups, &base.as_marked_path(), &home);

        // Then it clamps at the root instead of panicking.
        let root = volume_root();
        assert_eq!(dir, root);
    }

    #[rstest::rstest]
    fn absolute_parent_passes_through() {
        // Given a base and home.
        let (base, home) = base_and_home();

        // When resolving an absolute-on-this-platform parent (built from
        // the volume root — never spelled with a separator literal).
        let abs = volume_root().join("etc").expect("root joins a bare name");
        let dir = resolve_parent_dir(&abs.to_string_lossy(), &base.as_marked_path(), &home);

        // Then it passes through unchanged.
        assert_eq!(dir, abs);
    }
}
