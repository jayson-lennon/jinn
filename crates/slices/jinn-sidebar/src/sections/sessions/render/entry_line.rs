//! Pure helper functions for building a single session entry line.
//!
//! Each function is pure (no side effects, no `&mut self`) and takes explicit
//! parameters so it can be unit-tested in isolation.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use throbber_widgets_tui::ThrobberState;
use unicode_segmentation::UnicodeSegmentation;

use crate::sections::sessions::state::{SessionEntry, SessionEntryKind};
use jinn_domain::feat::theme::Theme;

use super::super::{ACTIVE_PREFIX, INACTIVE_PREFIX};
use super::truncate::truncate_str;

/// Builds the animated throbber indicator span for a session entry.
///
/// Returns a blank space when the session is idle, or an animated braille
/// character when the session is working.
#[expect(clippy::expect_used, reason = "idx modulo len is always in bounds")]
pub(crate) fn indicator_span(is_idle: bool, throbber_state: &ThrobberState) -> Span<'static> {
    if is_idle {
        Span::raw(" ")
    } else {
        let set = throbber_widgets_tui::symbols::throbber::BRAILLE_EIGHT;
        let mut idx = throbber_state.index();
        let len = set.symbols.len() as i8;
        idx %= len;
        if idx < 0 {
            idx += len;
        }
        let ch = set.symbols.get(idx as usize).expect("idx modulo len");
        Span::styled(ch.to_string(), Style::default().fg(Color::Cyan))
    }
}

/// Builds the arrow prefix span indicating whether a session is active.
///
/// Active sessions display `▸ `, inactive sessions display `  ` (aligned).
pub(crate) fn arrow_span(is_active: bool, theme: &Theme) -> Span<'static> {
    if is_active {
        Span::styled(
            ACTIVE_PREFIX.to_owned(),
            Style::default().fg(theme.primary_text),
        )
    } else {
        Span::styled(INACTIVE_PREFIX.to_owned(), Style::default())
    }
}

/// Computes the title style based on entry state.
///
/// Priority: error+selected → red+reversed, error → red, selected → reversed,
/// active → subagent/primary text, default → subagent/muted text. Subagent
/// sessions use [`Theme::subagent_fg`] wherever a regular session would use
/// muted text, so machine-spawned sessions read as a different kind.
pub(crate) fn entry_title_style(entry: &SessionEntry, is_selected: bool, theme: &Theme) -> Style {
    let base = if entry.is_subagent {
        theme.subagent_fg
    } else {
        theme.muted_text
    };
    let active = if entry.is_subagent {
        theme.subagent_fg
    } else {
        theme.primary_text
    };
    if entry.last_entry_is_error {
        if is_selected {
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::Red)
        }
    } else if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if entry.is_active {
        Style::default().fg(active)
    } else {
        Style::default().fg(base)
    }
}

/// Builds the tree connector prefix for a session entry.
///
/// For root entries (depth 0), returns an empty string.
/// For non-root entries, constructs non-compacted 3-char-wide segments:
/// - Skips `ancestor_continuations[0]` (root-level) since roots have no prefix.
/// - For each intermediate ancestor level: `│  ` if continuing, `   ` if not
/// - For the entry's own level: `├─ ` if has younger siblings, `└─ ` if last child
pub(crate) fn tree_prefix(entry: &SessionEntry) -> String {
    if entry.depth == 0 {
        return String::new();
    }
    // Skip ancestor_continuations[0] - the root-level continuation.
    // Roots have no tree prefix, so there's nothing for that │ to connect to.
    let mut prefix = String::with_capacity(entry.depth * 3);
    for &continues in entry.ancestor_continuations.get(1..).unwrap_or(&[]) {
        prefix.push_str(if continues { "│  " } else { "   " });
    }
    prefix.push_str(if entry.is_last_child {
        "└─ "
    } else {
        "├─ "
    });
    prefix
}

/// Assembles a complete session entry line from its components.
///
/// Combines the throbber indicator, arrow prefix, tree connector prefix,
/// and styled truncated title into a single [`Line`] ready for rendering.
pub(crate) fn assemble_entry_line(
    entry: &SessionEntry,
    is_selected: bool,
    max_title_len: usize,
    throbber_state: &ThrobberState,
    theme: &Theme,
) -> Line<'static> {
    match entry.kind {
        SessionEntryKind::Session => {
            assemble_session_line(entry, is_selected, max_title_len, throbber_state, theme)
        }
    }
}

/// Glyph marking a subagent session, rendered before its title.
const SUBAGENT_SYMBOL: &str = "⋄ ";
/// Marks a session with a live `interactive_term` terminal.
pub(crate) const LIVE_TERM_SYMBOL: &str = "◼ ";

/// Renders a session entry line (indicator + arrow + tree + styled title).
fn assemble_session_line(
    entry: &SessionEntry,
    is_selected: bool,
    max_title_len: usize,
    throbber_state: &ThrobberState,
    theme: &Theme,
) -> Line<'static> {
    let indicator = indicator_span(entry.is_idle, throbber_state);
    let arrow = arrow_span(entry.is_active, theme);
    let tree = tree_prefix(entry);
    let tree_len = tree.graphemes(true).count();
    let style = entry_title_style(entry, is_selected, theme);
    // The subagent symbol is part of the title's rendered width so the
    // truncation budget accounts for it.
    let subagent_symbol = if entry.is_subagent {
        SUBAGENT_SYMBOL
    } else {
        ""
    };
    let term_symbol = if entry.has_live_term {
        LIVE_TERM_SYMBOL
    } else {
        ""
    };
    let symbol_len =
        (subagent_symbol.graphemes(true).count()) + term_symbol.graphemes(true).count();
    let budget = max_title_len.saturating_sub(tree_len);
    let display_title = {
        let title_budget = budget.saturating_sub(symbol_len);
        truncate_str(&entry.title, title_budget)
    };
    let mut spans = vec![indicator, Span::raw(" "), arrow];
    if !tree.is_empty() {
        spans.push(Span::styled(tree, Style::default().fg(theme.muted_text)));
    }
    if !subagent_symbol.is_empty() {
        spans.push(Span::styled(
            subagent_symbol.to_owned(),
            Style::default().fg(theme.subagent_fg),
        ));
    }
    if !term_symbol.is_empty() {
        spans.push(Span::styled(
            term_symbol.to_owned(),
            Style::default().fg(theme.success),
        ));
    }
    spans.push(Span::styled(display_title, style));
    Line::from(spans)
}
