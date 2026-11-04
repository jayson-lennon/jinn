//! The saved-app catalog — what the picker shows and the server serves.
//!
//! [`AppCatalog`] is the apps slice's data cell payload: one [`AppEntry`]
//! per saved app directory. The on-disk `apps.json` at the apps root has
//! the same shape; the catalog cell is the in-memory authority during a
//! run and the file is written through on every mutation, so the two
//! always agree. A missing or unreadable `apps.json` deserializes to an
//! empty catalog — a first-run user has no apps and that is a state,
//! never an error.

use serde::{Deserialize, Serialize};

/// The file name of the on-disk catalog at the apps root.
pub const CATALOG_FILE_NAME: &str = "apps.json";

/// One saved app: a directory of static files under the apps root.
///
/// `id` is the directory's name and the URL path segment
/// (`/apps/<id>/`); it is stable across renames — the title is the
/// display name, so renaming rewrites neither the directory nor any
/// link the user already has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// The app's directory name under the apps root, and its URL path
    /// segment. Directory-name safe by construction (see
    /// [`sanitize_app_id`]).
    pub id: String,
    /// The display name the picker shows and `<c-r>` edits.
    pub title: String,
    /// When the app was saved (RFC 3339), for recency ordering.
    pub created_at: String,
}

/// The catalog cell's payload: every saved app, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppCatalog {
    /// The saved apps, recency-ordered (newest first).
    pub entries: Vec<AppEntry>,
}

impl AppCatalog {
    /// Looks up one entry by id.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&AppEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Sorts the entries newest first (the picker's row order).
    pub fn sort_recency(&mut self) {
        self.entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    }
}

/// Reduces a requested app title to a directory-name-safe id.
///
/// Lowercases, keeps only ASCII alphanumerics, `-`, and `_`; every other
/// run collapses to one `-`; the result is trimmed of leading and
/// trailing `-` and capped at 64 bytes. An input that reduces to
/// nothing yields the fallback `app`, so a saved app always has a
/// servable URL segment.
#[must_use]
pub fn sanitize_app_id(title: &str) -> String {
    const MAX_ID_BYTES: usize = 64;
    const FALLBACK: &str = "app";

    let lowered = title.to_lowercase();
    let mut id = String::with_capacity(lowered.len().min(MAX_ID_BYTES));
    let mut last_was_dash = true; // a leading dash must not survive
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            id.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            id.push('-');
            last_was_dash = true;
        }
    }
    let mut id = id.trim_matches('-').to_owned();
    if id.len() > MAX_ID_BYTES {
        // Truncate on a char boundary: 64 bytes of ASCII ids here means
        // the cut only lands mid-char for non-ASCII input, which the
        // filter above already excluded — the floor is defensive.
        let mut cut = MAX_ID_BYTES;
        while !id.is_char_boundary(cut) {
            cut -= 1;
        }
        id.truncate(cut);
    }
    let id = id.trim_matches('-').to_owned();
    if id.is_empty() { FALLBACK.to_owned() } else { id }
}

/// Rejects an app id that could escape the apps root or collide with the
/// catalog file.
///
/// A valid id is one or more ASCII alphanumerics, `-`, `_`, or `.` — but
/// never `.` or `..` alone (traversal), never containing `/` or `\`
/// (path separators), never starting with `.` (hidden/special), and
/// never the catalog file's own name. Every id the store mints passes
/// [`sanitize_app_id`]; this check is the gate for anything read back
/// from disk or the wire.
#[must_use]
pub fn is_valid_app_id(id: &str) -> bool {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id == CATALOG_FILE_NAME
        || id.starts_with('.')
    {
        return false;
    }
    id.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{AppCatalog, AppEntry, is_valid_app_id, sanitize_app_id};

    fn entry(id: &str, title: &str, created_at: &str) -> AppEntry {
        AppEntry {
            id: id.to_owned(),
            title: title.to_owned(),
            created_at: created_at.to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn catalog_serializes_to_the_apps_json_shape() {
        // Given a catalog with one entry.
        let catalog = AppCatalog {
            entries: vec![entry("sales-dashboard", "Sales Dashboard", "2026-11-01T10:00:00Z")],
        };

        // When serializing.
        let json = serde_json::to_string(&catalog).expect("serialize");

        // Then the file's shape is the flat entry list.
        assert_eq!(
            json,
            r#"{"entries":[{"id":"sales-dashboard","title":"Sales Dashboard","created_at":"2026-11-01T10:00:00Z"}]}"#
        );
    }

    #[rstest::rstest]
    #[test]
    fn catalog_round_trips_through_json() {
        // Given a catalog with two entries.
        let catalog = AppCatalog {
            entries: vec![
                entry("a", "A", "2026-11-01T10:00:00Z"),
                entry("b", "B", "2026-11-02T10:00:00Z"),
            ],
        };

        // When serializing then re-parsing.
        let json = serde_json::to_string(&catalog).expect("serialize");
        let back: AppCatalog = serde_json::from_str(&json).expect("reparse");

        // Then the entries survive.
        assert_eq!(back, catalog);
    }

    #[rstest::rstest]
    #[test]
    fn empty_catalog_deserializes_from_an_empty_entries_list() {
        // Given the file body an empty apps.json carries.
        // When parsing.
        let catalog: AppCatalog = serde_json::from_str(r#"{"entries": []}"#).expect("parse");

        // Then the catalog is empty — the first-run state.
        assert!(catalog.entries.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn entry_lookup_finds_by_id() {
        // Given a catalog with two entries.
        let catalog = AppCatalog {
            entries: vec![entry("a", "A", "1"), entry("b", "B", "2")],
        };

        // When looking up by id.
        // Then the right entry comes back, and a miss is None.
        assert_eq!(catalog.entry("b").map(|e| e.title.as_str()), Some("B"));
        assert!(catalog.entry("c").is_none());
    }

    #[rstest::rstest]
    #[test]
    fn recency_sort_puts_newest_first() {
        // Given a catalog written oldest-first.
        let mut catalog = AppCatalog {
            entries: vec![entry("old", "Old", "2026-01-01T00:00:00Z"), entry("new", "New", "2026-06-01T00:00:00Z")],
        };

        // When sorting by recency.
        catalog.sort_recency();

        // Then the newest leads — the picker's row order.
        assert_eq!(catalog.entries[0].id, "new");
        assert_eq!(catalog.entries[1].id, "old");
    }

    #[rstest::rstest]
    #[case("Sales Dashboard", "sales-dashboard")]
    #[case("Hello, World!", "hello-world")]
    #[case("  already-idle  ", "already-idle")]
    #[case("multiple   spaces & things", "multiple-spaces-things")]
    #[case("under_scores", "under_scores")]
    fn sanitize_reduces_titles_to_url_safe_ids(#[case] title: &str, #[case] expected: &str) {
        assert_eq!(sanitize_app_id(title), expected);
    }

    #[rstest::rstest]
    #[test]
    fn sanitize_of_symbols_falls_back() {
        // Given a title with no id-worthy characters at all.
        // When sanitizing.
        // Then the fallback keeps the app servable.
        assert_eq!(sanitize_app_id("!!!"), "app");
        assert_eq!(sanitize_app_id(""), "app");
    }

    #[rstest::rstest]
    #[test]
    fn sanitize_caps_the_id_length() {
        // Given a very long title.
        let long = "a".repeat(500);

        // When sanitizing.
        let id = sanitize_app_id(&long);

        // Then the id is capped at 64 bytes.
        assert_eq!(id.len(), 64);
    }

    #[rstest::rstest]
    #[case("fine-app", true)]
    #[case("App_2", true)]
    #[case("dots.ok", true)]
    #[case("", false)]
    #[case(".", false)]
    #[case("..", false)]
    #[case(".hidden", false)]
    #[case("a/b", false)]
    #[case("a\\b", false)]
    #[case("apps.json", false)]
    #[case("with space", false)]
    fn id_validation_gates_traversal_and_catalog_collisions(#[case] id: &str, #[case] valid: bool) {
        assert_eq!(is_valid_app_id(id), valid, "id: {id}");
    }
}
