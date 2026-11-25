//! The session archive region's tests: the open pull lists loaded AND
//! archived sessions tree-nested with a project column and a
//! loaded/archived tag, the filter narrows server-side with the TUI
//! picker's own matching (a matching child surfaces its parent, `<mark>`
//! runs), the result is capped, and the conformance gate holds the
//! region to its pull-only declaration.
//!
//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_core_types::SessionId;

use super::ROW_CAP;
use super::SessionArchiveRegion;
use crate::contract;
use crate::sessions_list_tests::archived_summary;
use crate::sessions_list_tests::harness;
use crate::sessions_list_tests::harness_with;
use crate::sessions_list_tests::seed_child;
use crate::sessions_list_tests::seed_titled;

/// The archive session ids in render order, read off the rows'
/// `data-webui-archive-session` attribute.
fn archive_ids(html: &str) -> Vec<String> {
    let attr = format!("{}=\"", contract::ARCHIVE_SESSION_DATA_ATTR);
    html.split(&attr)
        .skip(1)
        .filter_map(|rest| rest.split('"').next().map(str::to_owned))
        .collect()
}

/// The row fragment for one session id: from its marker through the end
/// of the row's closing `</a>`.
fn row_fragment<'a>(html: &'a str, id: &str) -> &'a str {
    let marker = format!("{}=\"{id}\"", contract::ARCHIVE_SESSION_DATA_ATTR);
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("session {id} renders a row: {html}"));
    // The first piece that keeps its closing tag is the rest of the row.
    let (_, from_row) = html.split_at(start);
    from_row.split_inclusive("</a>").next().unwrap_or(from_row)
}

/// A live session's `padding-left` under its tree depth.
fn indent_of(html: &str, id: &str) -> String {
    let row = row_fragment(html, id);
    let after_indent = row
        .split("padding-left: ")
        .nth(1)
        .expect("the row carries an indent");
    after_indent
        .split([';', '"'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

// ── Conformance ─────────────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn the_region_conforms_with_pull_route_only_declared() {
    // Given the shipped registry.
    // When the conformance gate runs.
    let gaps = crate::region::check_region(&SessionArchiveRegion);

    // Then it finds nothing — the region declares exactly what it
    // carries (a pull route; no settle render, SSE event, or page
    // fill).
    assert!(
        gaps.is_empty(),
        "the archive region must conform, got: {gaps:?}"
    );
}

// ── The open pull: loaded AND archived, tree-nested ──────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_open_pull_lists_a_loaded_session() {
    // Given one live session and an empty store.
    let (state, shared) = harness(Vec::new());
    let loaded = seed_titled(&state, "Loaded one");

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then the loaded session's row is in it.
    assert!(
        archive_ids(&html).contains(&loaded.to_string()),
        "the loaded session lists: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_open_pull_lists_an_archived_session() {
    // Given an archived summary in the store.
    let archived = SessionId::new();
    let (_state, shared) = harness(vec![archived_summary(archived.clone(), "Archived one")]);

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then the archived session's row is in it — the picker's whole
    // reason for existing.
    assert!(
        archive_ids(&html).contains(&archived.to_string()),
        "the archived session lists: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_archived_row_renders_the_archived_tag() {
    // Given an archived summary in the store.
    let archived = SessionId::new();
    let (_state, shared) = harness(vec![archived_summary(archived.clone(), "Archived one")]);

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then its row carries the "archived" tag, not "loaded".
    let row = row_fragment(&html, &archived.to_string());
    assert!(row.contains(">archived<"), "the tag reads archived: {row}");
    assert!(!row.contains(">loaded<"), "no loaded tag: {row}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_loaded_row_renders_the_loaded_tag() {
    // Given one live session.
    let (state, shared) = harness(Vec::new());
    let loaded = seed_titled(&state, "Loaded one");

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then its row carries the "loaded" tag.
    let row = row_fragment(&html, &loaded.to_string());
    assert!(row.contains(">loaded<"), "the tag reads loaded: {row}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_child_indents_deeper_than_its_root() {
    // Given a live root with a live subagent child.
    let (state, shared) = harness(Vec::new());
    let root = seed_titled(&state, "root session");
    let child = seed_child(&state, &root, "subagent child");

    // When pulling the modal unfiltered.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then the child's row indents further than the root's — the
    // picker's tree, unfiltered too.
    let root_indent = indent_of(&html, &root.to_string());
    let child_indent = indent_of(&html, &child.to_string());
    let root_px: i64 = root_indent
        .trim_end_matches("px")
        .parse()
        .unwrap_or_else(|_| panic!("root indent is px: {root_indent}"));
    let child_px: i64 = child_indent
        .trim_end_matches("px")
        .parse()
        .unwrap_or_else(|_| panic!("child indent is px: {child_indent}"));
    assert!(
        child_px > root_px,
        "the child indents deeper: root {root_px}px, child {child_px}px"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_renders_the_filter_input_bound_to_its_signal() {
    // Given any store.
    let (_state, shared) = harness(Vec::new());

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then the filter input is present and bound to the archive
    // filter signal.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::ARCHIVE_FILTER_ID)),
        "the filter input renders: {html}"
    );
    assert!(
        html.contains(&format!("data-bind:{}", contract::SIGNAL_ARCHIVE_FILTER)),
        "the input binds the filter signal: {html}"
    );
}

// ── The filter: the picker's own matching ───────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_filter_narrows_the_rows_server_side() {
    // Given a live "Fix the leak" and an archived "Aardvark notes".
    let aardvark = SessionId::new();
    let (state, shared) = harness(vec![archived_summary(aardvark.clone(), "Aardvark notes")]);
    let leak = seed_titled(&state, "Fix the leak");

    // When pulling the rows with a filter only the archived title
    // fuzzy-matches.
    let html = super::rows_fragment(&shared, "ardvark")
        .await
        .expect("the rows pull renders");

    // Then only the archived session's row renders.
    assert_eq!(
        archive_ids(&html),
        vec![aardvark.to_string()],
        "only the match renders (leak is {leak}): {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_matching_child_surfaces_its_parent() {
    // Given a live root "tts" whose child carries a title the root's
    // own title does not contain.
    let (state, shared) = harness(Vec::new());
    let root = seed_titled(&state, "tts");
    let child = seed_child(&state, &root, "please set up kokoro for me");

    // When filtering on a term only the child's title matches.
    let html = super::rows_fragment(&shared, "kokoro")
        .await
        .expect("the rows pull renders");

    // Then the root renders alongside its matched child — the picker's
    // ancestor-chain expansion.
    let ids = archive_ids(&html);
    assert!(
        ids.contains(&root.to_string()),
        "the parent surfaces: {html}"
    );
    assert!(
        ids.contains(&child.to_string()),
        "the child renders: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_non_matching_filter_renders_no_rows() {
    // Given one session.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "Fix the leak");

    // When filtering for a string no title contains.
    let html = super::rows_fragment(&shared, "qqqqzzzz")
        .await
        .expect("the rows pull renders");

    // Then no row renders — an empty rows container, not an error.
    assert!(archive_ids(&html).is_empty(), "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_marks_the_matched_runs() {
    // Given one session.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "Fix the leak");

    // When filtering on a contiguous substring of its title.
    let html = super::rows_fragment(&shared, "leak")
        .await
        .expect("the rows pull renders");

    // Then the matched run renders inside `<mark>`.
    assert!(
        html.contains("<mark>leak</mark>"),
        "the match is marked: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_empty_filter_renders_rows_unmarked() {
    // Given one session.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "Fix the leak");

    // When pulling the rows with an empty filter.
    let html = super::rows_fragment(&shared, "")
        .await
        .expect("the rows pull renders");

    // Then the row renders with no `<mark>` — nothing matched.
    assert!(!html.contains("<mark>"), "unmarked: {html}");
}

// ── The cap ─────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_unfiltered_pull_caps_the_rows() {
    // Given three more archived sessions than the cap.
    let summaries: Vec<_> = (0..ROW_CAP + 3)
        .map(|n| archived_summary(SessionId::new(), &format!("Archived {n:04}")))
        .collect();
    let (_state, shared) = harness(summaries);

    // When pulling the modal unfiltered.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then exactly the cap's worth of rows renders.
    assert_eq!(archive_ids(&html).len(), ROW_CAP, "the rows are capped");
}

#[rstest::rstest]
#[tokio::test]
async fn a_filtered_pull_caps_the_rows() {
    // Given three more archived sessions than the cap, all matching
    // one filter term.
    let summaries: Vec<_> = (0..ROW_CAP + 3)
        .map(|n| archived_summary(SessionId::new(), &format!("zeta {n:04}")))
        .collect();
    let (_state, shared) = harness(summaries);

    // When filtering on the shared term.
    let html = super::rows_fragment(&shared, "zeta")
        .await
        .expect("the rows pull renders");

    // Then exactly the cap's worth of rows renders — the cap holds
    // under a filter too.
    assert_eq!(
        archive_ids(&html).len(),
        ROW_CAP,
        "the rows are capped: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_both_live_and_in_the_store_renders_once() {
    // Given a live session whose id the store ALSO holds a row for —
    // live truth wins, and the merge must not render it twice.
    let (_state, shared) = harness_with(
        |state| seed_titled(state, "Live title"),
        |loaded| vec![archived_summary(loaded.clone(), "Stale store title")],
    );

    // When pulling the modal.
    let html = super::open_fragment(&shared)
        .await
        .expect("the open pull renders");

    // Then that session renders exactly once (the harness's default
    // session lists too — it is a different session), under its live
    // title.
    assert!(
        html.matches("Live title").count() == 1,
        "the live session renders once: {html}"
    );
    assert!(
        !html.contains("Stale store title"),
        "the stale row loses: {html}"
    );
}
