//! Comprehensive tests for tree prefix formatting and entry line assembly.
//!
//! Each test verifies one specific tree rendering behavior using BDD style.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jiff::Timestamp;
use throbber_widgets_tui::ThrobberState;

use crate::sections::sessions::render::entry_line::{assemble_entry_line, tree_prefix};
use crate::sections::sessions::state::{SessionEntry, SessionEntryKind};
use jinn_domain::common::app_state::AppState;
use jinn_domain::protocol::SessionId;

use unicode_segmentation::UnicodeSegmentation;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tree_entry(
    depth: usize,
    ancestor_continuations: Vec<bool>,
    is_last_child: bool,
    is_subagent: bool,
) -> SessionEntry {
    SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Test".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth,
        ancestor_continuations,
        is_last_child,
        is_subagent,
        has_live_term: false,
    }
}

fn default_theme() -> jinn_domain::feat::theme::Theme {
    AppState::default_with_scope_focus().frontend.theme
}

fn idle_throbber() -> ThrobberState {
    ThrobberState::default()
}

// ---------------------------------------------------------------------------
// tree_prefix - root entries
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_prefix_is_empty_for_root_entry() {
    // Given a root entry (depth 0).
    let entry = tree_entry(0, vec![], true, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is empty.
    assert_eq!(prefix, "");
}

// ---------------------------------------------------------------------------
// tree_prefix - depth 1
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_prefix_last_child_at_depth_1() {
    // Given a last child at depth 1.
    let entry = tree_entry(1, vec![true], true, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is └─ (root continuation skipped).
    assert_eq!(prefix, "└─ ");
}

#[rstest::rstest]
fn tree_prefix_not_last_child_at_depth_1() {
    // Given a non-last child at depth 1.
    let entry = tree_entry(1, vec![true], false, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is ├─ (root continuation skipped).
    assert_eq!(prefix, "├─ ");
}

// ---------------------------------------------------------------------------
// tree_prefix - depth 2
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_prefix_last_child_with_continuing_ancestor() {
    // Given a last child at depth 2 with a continuing intermediate ancestor.
    let entry = tree_entry(2, vec![true, true], true, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is │  └─ (root skipped, intermediate continues).
    assert_eq!(prefix, "│  └─ ");
}

#[rstest::rstest]
fn tree_prefix_last_child_with_non_continuing_ancestor() {
    // Given a last child at depth 2 with a non-continuing intermediate ancestor.
    let entry = tree_entry(2, vec![true, false], true, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is    └─ (root skipped, intermediate is space).
    assert_eq!(prefix, "   └─ ");
}

#[rstest::rstest]
fn tree_prefix_not_last_child_with_continuing_ancestor() {
    // Given a non-last child at depth 2 with a continuing intermediate ancestor.
    let entry = tree_entry(2, vec![true, true], false, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then the prefix is │  ├─ (root skipped, intermediate continues).
    assert_eq!(prefix, "│  ├─ ");
}

// ---------------------------------------------------------------------------
// tree_prefix - deep chains
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_prefix_deep_chain_not_last() {
    // Given a non-last child at depth 5 with mixed continuations.
    let entry = tree_entry(5, vec![true, true, false, true, true], false, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then continuation chars are correct: │     │  │  ├─ (root skipped, 4 intermediate segments).
    assert_eq!(prefix, "│     │  │  ├─ ");
}

#[rstest::rstest]
fn tree_prefix_deep_chain_last() {
    // Given a last child at depth 5 with mixed continuations.
    let entry = tree_entry(5, vec![true, true, false, true, true], true, false);

    // When computing tree prefix.
    let prefix = tree_prefix(&entry);

    // Then continuation chars are correct: │     │  │  └─ (root skipped, 4 intermediate segments).
    assert_eq!(prefix, "│     │  │  └─ ");
}

// ---------------------------------------------------------------------------
// assemble_entry_line - tree prefix inclusion
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn assembled_line_includes_tree_prefix_for_non_root() {
    // Given a child entry at depth 1.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Child".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 1,
        ancestor_continuations: vec![true],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the line contains the \u{2514}\u{2500} tree characters.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("\u{2514}\u{2500} "),
        "line should contain \u{2514}\u{2500} , got: {text}"
    );
}

#[rstest::rstest]
fn assembled_line_has_no_tree_prefix_for_root() {
    // Given a root entry.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Root".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the line has 4 spans: indicator, space, arrow, title (no tree prefix span).
    assert_eq!(
        line.spans.len(),
        4,
        "root entry should have 4 spans, got {}",
        line.spans.len()
    );
}

#[rstest::rstest]
fn assembled_line_has_tree_prefix_span_for_child() {
    // Given a child entry at depth 1.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Child".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 1,
        ancestor_continuations: vec![true],
        is_last_child: false,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the line has 5 spans: indicator, space, arrow, tree prefix, title.
    assert_eq!(
        line.spans.len(),
        5,
        "child entry should have 5 spans, got {}",
        line.spans.len()
    );
}

// ---------------------------------------------------------------------------
// assemble_entry_line - title truncation with tree prefix
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn title_is_truncated_more_at_higher_depth() {
    // Given two entries with the same title but different depths.
    let root = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "A very long session title that should be truncated".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let child = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "A very long session title that should be truncated".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 3,
        ancestor_continuations: vec![true, true, true],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();
    let max_len = 20;

    // When assembling entry lines with limited space.
    let root_line = assemble_entry_line(&root, false, max_len, &idle_throbber(), &theme);
    let child_line = assemble_entry_line(&child, false, max_len, &idle_throbber(), &theme);

    // Then the root title span is longer than the child title span.
    // Root has 4 spans: indicator, space, arrow, title.
    // Child has 5 spans: indicator, space, arrow, tree prefix, title.
    let root_title = &root_line.spans[3].content;
    let child_title = &child_line.spans[4].content;
    assert!(
        root_title.len() > child_title.len(),
        "root title ({root_title}) should be longer than child title ({child_title})"
    );
}

// ---------------------------------------------------------------------------
// assemble_entry_line - active arrow at depth > 0
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn active_arrow_shows_at_depth_greater_than_zero() {
    // Given an active child entry at depth 2.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Deep Child".to_owned(),
        is_active: true,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 2,
        ancestor_continuations: vec![true, true],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the arrow span contains ▸.
    let arrow_span = &line.spans[2];
    assert!(
        arrow_span.content.contains('▸'),
        "arrow should contain ▸, got: {}",
        arrow_span.content
    );
}

// ---------------------------------------------------------------------------
// assemble_entry_line - tree prefix color
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn tree_prefix_uses_muted_text_color() {
    // Given a child entry at depth 1.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Child".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 1,
        ancestor_continuations: vec![true],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the tree prefix span (index 3) uses muted_text color.
    let tree_span = &line.spans[3];
    assert_eq!(
        tree_span.style.fg,
        Some(theme.muted_text),
        "tree prefix should use muted_text color"
    );
}

// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn non_judge_child_at_depth_1_uses_grapheme_count_for_tree() {
    // Given a non-judge child at depth 1 with a long title.
    // Tree prefix "├─ " = 3 graphemes (but 7 bytes).
    let mut entry = tree_entry(1, vec![true], false, false);
    entry.title = "ABCDEFGHIJ".to_owned();
    let theme = default_theme();
    // max_title_len = 7 → budget = 7 - 3 (tree prefix graphemes) = 4 graphemes for title.
    // Title "ABCDEFGHIJ" truncated to 3 + "…" = 4 graphemes.
    let max_title_len = 7;

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, max_title_len, &idle_throbber(), &theme);

    // Then the title span has exactly 4 graphemes.
    let title_span = &line.spans[4].content;
    let grapheme_count = title_span.graphemes(true).count();
    assert_eq!(
        grapheme_count, 4,
        "title span should have 4 graphemes, got {grapheme_count}: {title_span}"
    );
    assert!(
        title_span.ends_with('…'),
        "truncated title should end with ellipsis, got: {title_span}"
    );
}

// ---------------------------------------------------------------------------
// assemble_entry_line - subagent symbol
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn sidebar_marks_child_with_symbol() {
    // Given a subagent session entry.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Explore".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: Some(SessionId::new()),
        depth: 1,
        ancestor_continuations: vec![true],
        is_last_child: true,
        is_subagent: true,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then the line ends with the subagent symbol span.
    let last_span = line.spans.last().expect("spans");
    assert_eq!(
        last_span.content.as_ref(),
        "Explore",
        "the symbol precedes the title, so the title is last"
    );
    // And the title span just before it is the diamond symbol in the
    // subagent color.
    let symbol_span = &line.spans[line.spans.len() - 2];
    assert_eq!(
        symbol_span.content.as_ref(),
        "⋄ ",
        "subagent line must carry the diamond symbol before its title"
    );
    assert_eq!(
        symbol_span.style.fg,
        Some(theme.subagent_fg),
        "symbol should use subagent_fg color"
    );
}

#[rstest::rstest]
fn sidebar_omits_symbol_for_regular_session() {
    // Given a regular (non-subagent) session entry.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Root".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then no span carries the subagent symbol.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !text.contains('⋄'),
        "regular session must not show the symbol, got: {text}"
    );
}

#[rstest::rstest]
fn sidebar_shows_live_term_symbol_for_session_with_terminal() {
    // Given a session entry with a live interactive_term terminal.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Term".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent: false,
        has_live_term: true,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then a span carries the live-term symbol in the success color.
    let term_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "◼ ")
        .expect("live-term line must carry the ◼ symbol");
    assert_eq!(term_span.style.fg, Some(theme.success));
}

#[rstest::rstest]
fn sidebar_omits_live_term_symbol_for_session_without_terminal() {
    // Given a session entry with no live terminal.
    let entry = SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "Plain".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent: false,
        has_live_term: false,
    };
    let theme = default_theme();

    // When assembling the entry line.
    let line = assemble_entry_line(&entry, false, 30, &idle_throbber(), &theme);

    // Then no span carries the live-term symbol.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !text.contains('◼'),
        "session without a terminal must not show the symbol, got: {text}"
    );
}

#[rstest::rstest]
fn sidebar_live_term_symbol_consumes_truncation_budget() {
    // Given two entries with a long title and a live term each, narrow budget.
    let make = |is_subagent: bool| SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "A very long session title that will be truncated".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent,
        has_live_term: true,
    };
    let plain = make(false);
    let sub = make(true);
    let theme = default_theme();
    let max_len = 12;

    // When assembling both lines.
    let plain_line = assemble_entry_line(&plain, false, max_len, &idle_throbber(), &theme);
    let sub_line = assemble_entry_line(&sub, false, max_len, &idle_throbber(), &theme);

    // Then the subagent line's title is exactly one symbol-width shorter
    // than the plain line's (it carries the extra ⋄ symbol).
    let plain_title_len: usize = plain_line
        .spans
        .last()
        .map_or(0, |s| s.content.as_ref().graphemes(true).count());
    let sub_title_len: usize = sub_line
        .spans
        .last()
        .map_or(0, |s| s.content.as_ref().graphemes(true).count());
    let symbol_len = "⋄ ".graphemes(true).count();
    assert_eq!(
        sub_title_len,
        plain_title_len - symbol_len,
        "each extra symbol eats its width from the title budget"
    );
    // And both lines show the term symbol.
    for (name, line) in [("plain", &plain_line), ("sub", &sub_line)] {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('◼'), "{name} line must show the term symbol");
    }
}

#[rstest::rstest]
fn sidebar_symbol_consumes_truncation_budget() {
    // Given two subagent entries with a long title, one narrow viewport.
    let make = |is_subagent: bool| SessionEntry {
        kind: SessionEntryKind::Session,
        id: SessionId::new(),
        title: "A very long session title that will be truncated".to_owned(),
        is_active: false,
        created_at: Timestamp::now(),
        is_idle: true,
        last_entry_is_error: false,
        parent_id: None,
        depth: 0,
        ancestor_continuations: vec![],
        is_last_child: true,
        is_subagent,
        has_live_term: false,
    };
    let plain = make(false);
    let subagent = make(true);
    let theme = default_theme();
    let max_len = 12;

    // When assembling both entry lines.
    let plain_line = assemble_entry_line(&plain, false, max_len, &idle_throbber(), &theme);
    let sub_line = assemble_entry_line(&subagent, false, max_len, &idle_throbber(), &theme);

    // Then the subagent's title span is shorter: the symbol consumed part of
    // the same budget (no overflow past max_len). The title is always the
    // last span for both lines.
    let plain_title = plain_line.spans.last().expect("spans").content.clone();
    let sub_title = sub_line.spans.last().expect("spans").content.clone();
    let plain_title = plain_title.graphemes(true).count();
    let sub_title = sub_title.graphemes(true).count();
    let symbol_len = "⋄ ".graphemes(true).count();
    assert!(
        sub_title + symbol_len <= max_len,
        "title ({sub_title}) + symbol ({symbol_len}) must fit the budget {max_len}"
    );
    assert_eq!(
        sub_title,
        plain_title - symbol_len,
        "the symbol must reduce the title budget by its own width"
    );
}
