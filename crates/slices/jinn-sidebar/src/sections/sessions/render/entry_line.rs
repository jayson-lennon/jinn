//! Pure helper functions for building a single session entry line.
//!
//! Each function is pure (no side effects, no `&mut self`) and takes explicit
//! parameters so it can be unit-tested in isolation.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use throbber_widgets_tui::ThrobberState;
use unicode_segmentation::UnicodeSegmentation;

use crate::sections::session_row_style::selected_row_style;
use crate::sections::sessions::state::{SessionEntry, SessionEntryKind};
use jinn_theme::Theme;

use super::super::{ACTIVE_PREFIX, INACTIVE_PREFIX};
use super::truncate::truncate_str;

/// Builds the animated throbber indicator span for a session entry.
///
/// Returns a blank space when the session is idle, an animated braille
/// character when the session is working, and an animated left-eighth block
/// while the session's archive or teardown is in flight.
///
/// A session being disposed is always idle — the archive and close validators
/// both reject a busy session — so the two animations never share a row.
///
/// Both spinners wear [`Theme::streaming`], the theme's busy/active color, so
/// they read the same in a green gruvbox as in a sky-blue catppuccin.
///
/// `pub(crate)` because the library section's rows lead with the same cell:
/// both lists must read "this one is working" identically.
pub(crate) fn indicator_span(
    is_idle: bool,
    is_in_flight: bool,
    throbber_state: &ThrobberState,
    theme: &Theme,
) -> Span<'static> {
    if is_idle {
        in_flight_span(is_in_flight, throbber_state, theme)
    } else {
        busy_span(throbber_state, theme)
    }
}

/// The braille spinner shown while a session is streaming or running tools.
fn busy_span(throbber_state: &ThrobberState, theme: &Theme) -> Span<'static> {
    let set = throbber_widgets_tui::symbols::throbber::BRAILLE_EIGHT;
    let ch = throbber_symbol(&set, throbber_state);
    Span::styled(ch.to_owned(), Style::default().fg(theme.streaming))
}

/// The growing block shown while a session's disposal is in flight.
fn in_flight_span(
    is_in_flight: bool,
    throbber_state: &ThrobberState,
    theme: &Theme,
) -> Span<'static> {
    if !is_in_flight {
        return Span::raw(" ");
    }
    let set = throbber_widgets_tui::symbols::throbber::HORIZONTAL_BLOCK;
    let ch = throbber_symbol(&set, throbber_state);
    Span::styled(ch.to_owned(), Style::default().fg(theme.streaming))
}

/// Resolves a throbber symbol from an unbounded, possibly negative index.
///
/// [`ThrobberState::index`] counts upward without wrapping, so the modulo and
/// the negative correction are both required to stay in bounds.
#[expect(clippy::expect_used, reason = "idx modulo len is always in bounds")]
fn throbber_symbol(
    set: &throbber_widgets_tui::symbols::throbber::Set,
    throbber_state: &ThrobberState,
) -> &'static str {
    let len = set.symbols.len() as i8;
    let mut idx = throbber_state.index() % len;
    if idx < 0 {
        idx += len;
    }
    set.symbols
        .get(idx as usize)
        .copied()
        .expect("idx modulo len")
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
/// This answers only "what color is this row's text when nothing is selected"
/// — selection is a line-level band applied by [`assemble_session_line`], not
/// a property of the title. Priority: in-flight → tinted, error → red block,
/// active → subagent/bot/primary text, default → subagent/bot/
/// muted text. Subagent sessions use [`Theme::subagent_fg`] wherever a regular
/// session would use muted text, so machine-spawned sessions read as a
/// different kind.
pub(crate) fn entry_title_style(entry: &SessionEntry, theme: &Theme) -> Style {
    // The in-flight tint outranks every other state, including selection:
    // selection no longer restyles the title at all. The tint's background is
    // what the row wears while its disposal runs; the band is painted on top
    // of it by the line, so the row reads as selected without losing the tint.
    if entry.is_in_flight {
        return in_flight_style(theme);
    }
    let base = if entry.is_subagent {
        theme.subagent_fg
    } else if entry.is_bot {
        theme.bot_fg
    } else {
        theme.muted_text
    };
    let active = if entry.is_subagent {
        theme.subagent_fg
    } else if entry.is_bot {
        theme.bot_fg
    } else {
        theme.primary_text
    };
    if entry.last_entry_is_error {
        // A red block: the row's own foreground is the sidebar background, so
        // the block is a literal inversion of the panel. Selection overrides
        // it — selected is handled by the line, not here.
        Style::default().fg(theme.gutter_bg).bg(Color::Red)
    } else if entry.is_active {
        Style::default().fg(active)
    } else {
        Style::default().fg(base)
    }
}

/// The wash drawn behind a row whose session has a disposal in flight.
fn in_flight_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.in_flight_fg)
        .bg(theme.in_flight_bg)
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
/// Marks a bot still being composed, which will not dispatch a turn.
const BOT_PREPPING_SYMBOL: &str = "⏸ ";
/// Marks a bot that runs when its parent's turn completes.
const BOT_PARENT_TRIGGER_SYMBOL: &str = "⇉ ";

/// Renders a session entry line (indicator + arrow + tree + styled title).
fn assemble_session_line(
    entry: &SessionEntry,
    is_selected: bool,
    max_title_len: usize,
    throbber_state: &ThrobberState,
    theme: &Theme,
) -> Line<'static> {
    let indicator = indicator_span(entry.is_idle, entry.is_in_flight, throbber_state, theme);
    let arrow = arrow_span(entry.is_active, theme);
    let tree = tree_prefix(entry);
    let tree_len = tree.graphemes(true).count();
    let style = entry_title_style(entry, theme);
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
    // Like the subagent symbol, these sit beside the title rather than
    // inside it: the title is what a rename replaces, and a mode marker
    // must not be reachable by the rename key.
    let prepping_symbol = if entry.is_bot_prepping {
        BOT_PREPPING_SYMBOL
    } else {
        ""
    };
    // A separate glyph, not a second mark on the pause: "cannot run yet" and
    // "runs on its own" are independent facts, and a bot can be in
    // either state without the other being false.
    let parent_trigger_symbol = if entry.bot_fires_on_parent_completion {
        BOT_PARENT_TRIGGER_SYMBOL
    } else {
        ""
    };
    let symbol_len = (subagent_symbol.graphemes(true).count())
        + term_symbol.graphemes(true).count()
        + prepping_symbol.graphemes(true).count()
        + parent_trigger_symbol.graphemes(true).count();
    let budget = max_title_len.saturating_sub(tree_len);
    let display_title = {
        let title_budget = budget.saturating_sub(symbol_len);
        truncate_str(&entry.title, title_budget)
    };
    let title_width = display_title.graphemes(true).count();
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
    if !prepping_symbol.is_empty() {
        spans.push(Span::styled(
            prepping_symbol.to_owned(),
            Style::default().fg(theme.bot_paused),
        ));
    }
    if !parent_trigger_symbol.is_empty() {
        spans.push(Span::styled(
            parent_trigger_symbol.to_owned(),
            Style::default().fg(theme.bot_parent_trigger),
        ));
    }
    if !term_symbol.is_empty() {
        spans.push(Span::styled(
            term_symbol.to_owned(),
            Style::default().fg(theme.success),
        ));
    }
    spans.push(Span::styled(display_title, style));
    // Selection replaces every span's style, not just the title's: a span
    // with its own background (the error block, the in-flight wash) would
    // otherwise win over the band for its cells, and selection overrides
    // every session state. A trailing band-styled pad carries the band to
    // the row's last cell — `Paragraph` does not extend a line's style into
    // the cells past the last grapheme, so the pad is what makes the band
    // full-width.
    if is_selected {
        let band = selected_row_style(theme);
        let used = tree_len + symbol_len + title_width + 3; // indicator(1) + gap(1) + arrow(1)
        let pad_width = max_title_len.saturating_sub(used) + 4; // back to full area width
        let mut spans = spans
            .into_iter()
            .map(|span| span.style(band))
            .collect::<Vec<_>>();
        spans.push(Span::styled(" ".repeat(pad_width), band));
        return Line::from(spans).style(band);
    }
    // Re-style every span so the wash runs the full width of the row rather
    // than only behind the title, and so the indicator, arrow, tree connector
    // and status glyphs read as part of the same tinted row.
    if entry.is_in_flight {
        spans = spans
            .into_iter()
            .map(|span| span.style(in_flight_style(theme)))
            .collect::<Vec<_>>();
    }
    Line::from(spans)
}
