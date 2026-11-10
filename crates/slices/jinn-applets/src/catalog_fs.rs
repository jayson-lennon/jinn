//! The applets root's on-disk catalog: scan, persist, and the index page.
//!
//! One module owns everything that reads or writes `applets.json` at
//! the applets root, plus the page served at `/`:
//!
//! - [`scan_catalog`] — the reconciled catalog: entries whose directory
//!   is gone are dropped, an absent or corrupt `applets.json` reads as
//!   empty. The store actor runs it on every rescan; the server runs it
//!   on every `/` request, so the index never lists a dead applet.
//! - [`persist_catalog`] — the write-through side of the same rule: the
//!   file mirrors the cell after every mutation.
//! - [`index_html`] — the barebones `Index of /` page at the server's
//!   root URL: one link per saved applet, its title, nothing else. The
//!   on-disk truth (a fresh scan, not the picker's cell), because a
//!   browser pointed at the raw address may outlive the jinn process
//!   that served the last rescan.
//!
//! Pure functions over the applets root — no actors, no cell handles.

use marked_path::MarkedPathAccess as _;

/// Reads the applets root into a reconciled catalog.
///
/// The directory is the truth for *existence* (an entry whose directory
/// is gone is dropped), `applets.json` is the truth for *metadata* (title
/// and created_at; a directory without a row is not listed — the skill
/// registers saves through the catalog, so an unlisted directory is a
/// half-finished copy, not an applet).
#[must_use]
pub fn scan_catalog(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> jinn_applets_msg::AppletsCatalog {
    let mut catalog: jinn_applets_msg::AppletsCatalog =
        std::fs::read_to_string(catalog_path(applets_dir).as_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

    catalog
        .entries
        .retain(|entry| jinn_applets_msg::is_valid_app_id(&entry.id));
    catalog.entries.retain(|entry| {
        applets_dir
            .join_all([entry.id.as_str()])
            .is_ok_and(|dir| dir.as_path().is_dir())
    });
    catalog.sort_recency();
    catalog
}

/// Writes the catalog file, creating the applets root when absent.
///
/// A failure is logged, never panicked: a read-only or vanished applets
/// root degrades to an in-memory catalog, and the next rescan retries.
pub fn persist_catalog(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    catalog: &jinn_applets_msg::AppletsCatalog,
) {
    if let Err(error) = std::fs::create_dir_all(applets_dir.as_path()) {
        tracing::error!(%error, "failed to create the applets directory");
        return;
    }
    let json = match serde_json::to_string_pretty(catalog) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(%error, "failed to serialize the applets catalog");
            return;
        }
    };
    if let Err(error) = std::fs::write(catalog_path(applets_dir).as_path(), json) {
        tracing::error!(%error, "failed to write the applets catalog");
    }
}

/// The catalog file's path at the applets root.
///
/// # Panics
///
/// Panics only if the bare file name stops joining — unreachable while
/// `CATALOG_FILE_NAME` stays separator-free.
#[expect(
    clippy::panic,
    reason = "the bare file name joining is a compile-time invariant; a failure is a code bug, not a runtime state"
)]
fn catalog_path(
    applets_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
) -> marked_path::MarkedPathBuf<marked_path::Absolute> {
    let Ok(path) = applets_dir.join(jinn_applets_msg::CATALOG_FILE_NAME) else {
        panic!("catalog file name did not type-check")
    };
    path
}

/// The `/` page: a barebones `Index of /` listing, one link per saved
/// applet.
///
/// Apache-directory-listing style — plain HTML, no styling, no script,
/// nothing injected into anything the applets serve. Entries carry the
/// applet's title as the link text and link to its trailing-slash URL,
/// so relative asset paths inside the applet keep resolving. An empty
/// root still renders the heading, with no links under it.
#[must_use]
pub fn index_html(catalog: &jinn_applets_msg::AppletsCatalog) -> String {
    use std::fmt::Write as _;

    let mut page = String::from(
        "<!DOCTYPE html>\n<html>\n<head><title>Index of /</title></head>\n<body>\n<h1>Index of /</h1>\n<hr>\n<ul>\n",
    );
    for entry in &catalog.entries {
        // `write!` into the page: the id is a validated directory name
        // and the title is display text, so the plain-link shape needs
        // no escaping layer of its own.
        let id = &entry.id;
        let title = &entry.title;
        let _ = writeln!(page, "<li><a href=\"/applets/{id}/\">{title}</a></li>");
    }
    page.push_str("</ul>\n<hr>\n</body>\n</html>\n");
    page
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "test assertions need expect/unwrap"
    )]

    use super::{index_html, persist_catalog, scan_catalog};
    use jinn_applets_msg::{AppletEntry, AppletsCatalog};
    use marked_path::MarkedPathAccess as _;

    /// A temp applets root with one applet directory on disk.
    fn root_with_app(
        id: &str,
    ) -> (
        jinn_testutil::JinnTempDir,
        marked_path::MarkedPathBuf<marked_path::Absolute>,
    ) {
        let dir = jinn_testutil::retained_tempdir();
        let applets = dir.path.join("applets").expect("join");
        let app = applets.join_all([id]).expect("join");
        std::fs::create_dir_all(app.as_path()).expect("mkdir");
        std::fs::write(
            app.join("index.html").expect("join").as_path(),
            "<html></html>",
        )
        .expect("seed entry");
        let typed = marked_path::MarkedPathBuf::<marked_path::Absolute>::new(applets.as_path())
            .expect("absolute");
        (dir, typed)
    }

    fn entry(id: &str, created_at: &str) -> AppletEntry {
        AppletEntry {
            id: id.to_owned(),
            title: id.to_owned(),
            created_at: created_at.to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn missing_catalog_file_scans_as_empty() {
        // Given an applets root with an applet directory but no applets.json.
        let (_dir, applets) = root_with_app("solo");

        // When scanning.
        let catalog = scan_catalog(&applets);

        // Then the catalog is empty — the file is the metadata truth,
        // and a half-registered directory is not an applet.
        assert_eq!(catalog.entries.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn corrupt_catalog_file_scans_as_empty() {
        // Given an applets root whose applets.json is garbage.
        let (_dir, applets) = root_with_app("solo");
        std::fs::write(
            applets.as_path().join(jinn_applets_msg::CATALOG_FILE_NAME),
            "not json at all {{{",
        )
        .expect("write garbage");

        // When scanning.
        let catalog = scan_catalog(&applets);

        // Then the catalog is empty — a corrupt file must not fail.
        assert_eq!(catalog.entries.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn rescan_drops_entries_whose_directory_is_gone() {
        // Given a catalog row whose directory was deleted out from
        // under it, and one whose directory is live.
        let (_dir, applets) = root_with_app("live");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("live", "2026-01-02T00:00:00Z"),
                entry("ghost", "2026-01-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then only the live entry survives.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["live"]);
    }

    #[rstest::rstest]
    #[test]
    fn rescan_orders_entries_newest_first() {
        // Given a catalog written oldest-first.
        let (_dir, applets) = root_with_app("old");
        let second = applets.join_all(["new"]).expect("join");
        std::fs::create_dir_all(second.as_path()).expect("mkdir");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("old", "2026-01-01T00:00:00Z"),
                entry("new", "2026-06-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then the newest leads.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[rstest::rstest]
    #[test]
    fn persist_then_scan_round_trips_the_catalog() {
        // Given a catalog persisted to disk.
        let (_dir, applets) = root_with_app("a");
        let second = applets.join_all(["b"]).expect("join");
        std::fs::create_dir_all(second.as_path()).expect("mkdir");
        let catalog = AppletsCatalog {
            entries: vec![
                entry("a", "2026-01-01T00:00:00Z"),
                entry("b", "2026-02-01T00:00:00Z"),
            ],
        };
        persist_catalog(&applets, &catalog);

        // When scanning it back.
        let scanned = scan_catalog(&applets);

        // Then the entries survive (order normalized to recency).
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[rstest::rstest]
    #[test]
    fn invalid_ids_are_dropped_by_the_scan() {
        // Given a catalog file carrying a traversal id.
        let (_dir, applets) = root_with_app("ok");
        let raw = r#"{"entries":[
            {"id":"ok","title":"Ok","created_at":"2026-01-01T00:00:00Z"},
            {"id":"..","title":"Escape","created_at":"2026-01-02T00:00:00Z"}
        ]}"#;
        std::fs::write(
            applets.as_path().join(jinn_applets_msg::CATALOG_FILE_NAME),
            raw,
        )
        .expect("write catalog");

        // When scanning.
        let scanned = scan_catalog(&applets);

        // Then the traversal row is gone.
        let ids: Vec<&str> = scanned.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["ok"]);
    }

    #[rstest::rstest]
    #[test]
    fn index_lists_each_applet_as_a_link_to_its_url() {
        // Given a catalog with two applets.
        let catalog = AppletsCatalog {
            entries: vec![
                AppletEntry {
                    id: "sales-dashboard".to_owned(),
                    title: "Q4 Sales".to_owned(),
                    created_at: "2026-01-02T00:00:00Z".to_owned(),
                },
                AppletEntry {
                    id: "quiz".to_owned(),
                    title: "Biology Quiz".to_owned(),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                },
            ],
        };

        // When building the index page.
        let page = index_html(&catalog);

        // Then every entry is a plain link from the title to the
        // applet's trailing-slash URL.
        assert!(
            page.contains("<a href=\"/applets/sales-dashboard/\">Q4 Sales</a>"),
            "got: {page}"
        );
        assert!(
            page.contains("<a href=\"/applets/quiz/\">Biology Quiz</a>"),
            "got: {page}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn index_renders_the_barebones_heading_even_when_empty() {
        // Given an applets root with no saved applets.
        let catalog = AppletsCatalog::default();

        // When building the index page.
        let page = index_html(&catalog);

        // Then the page is the plain `Index of /` heading with no links.
        assert!(page.contains("<h1>Index of /</h1>"), "got: {page}");
        assert!(!page.contains("<a "), "got: {page}");
    }

    #[rstest::rstest]
    #[test]
    fn index_order_follows_the_catalog() {
        // Given a catalog already ordered newest-first.
        let catalog = AppletsCatalog {
            entries: vec![
                entry("new", "2026-06-01T00:00:00Z"),
                entry("old", "2026-01-01T00:00:00Z"),
            ],
        };

        // When building the index page.
        let page = index_html(&catalog);

        // Then the links appear in the catalog's order — the store owns
        // ordering, the index does not re-sort.
        let new_pos = page.find("/applets/new/").expect("new renders");
        let old_pos = page.find("/applets/old/").expect("old renders");
        assert!(new_pos < old_pos);
    }
}
