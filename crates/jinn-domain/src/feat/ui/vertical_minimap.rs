//! Vertical minimap - one block per chat entry showing token-count sizing.
//!
//! Renders colored blocks (`█`) representing chat entries in a single-column
//! display, one entry per row. The color indicates approximate token count.
//! Entries without token counts produce an empty row. Excluded entry types
//! (Actor, empty assistant) produce no row at all.
//! The viewport scrolls to keep the selected entry visible. A `>` arrow overlay
//! on the chat log area points at the selected entry's row.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::common::app_state::AppState;
use crate::protocol::{ChatEntry, ChatEntryKind};
use crate::feat::ui::chat_log::visual_item::VisualItem;

#[cfg(test)]
use crate::feat::ui::chat_log::visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
};

/// Full block character for minimap entries.
const FULL_BLOCK: &str = "\u{2588}";

/// Number of color bands in the minimap gradient.
const MINIMAP_BANDS: usize = 8;

/// Colorblind-friendly palette - 8 colors ramping from perceptually dark to bright.
/// Order: smallest token count → largest token count.
/// Theme-independent: designed for high contrast on dark backgrounds.
const MINIMAP_PALETTE: [Color; MINIMAP_BANDS] = [
    Color::Rgb(39, 12, 77),    // band 0: deep indigo
    Color::Rgb(39, 12, 77),    // band 1: deep indigo
    Color::Rgb(100, 20, 108),  // band 2: violet
    Color::Rgb(156, 43, 99),   // band 3: magenta-rose
    Color::Rgb(208, 74, 67),   // band 4: warm red
    Color::Rgb(243, 125, 22),  // band 5: orange
    Color::Rgb(251, 197, 51),  // band 6: gold
    Color::Rgb(252, 255, 164), // band 7: pale yellow
];

/// Returns the color for a token-count block using linear banding.
///
/// Divides `[0, max_tokens]` into `MINIMAP_BANDS` equal-width bands.
/// Counts exceeding `max_tokens` get the last band color.
#[expect(clippy::expect_used, reason = "infallible")]
fn token_threshold_color(count: u32, max_tokens: u32) -> Color {
    if max_tokens == 0 {
        return MINIMAP_PALETTE[0];
    }
    let band = (u64::from(count) * MINIMAP_BANDS as u64 / u64::from(max_tokens))
        .min((MINIMAP_BANDS - 1) as u64) as usize;
    MINIMAP_PALETTE
        .get(band)
        .copied()
        .unwrap_or(*MINIMAP_PALETTE.first().expect("non-empty"))
}

/// Extension trait for determining whether a visual item should produce
/// a minimap block.
trait MinimapVisibility {
    fn is_minimap_visible(&self, history: &[ChatEntry]) -> bool;
}

impl MinimapVisibility for VisualItem {
    fn is_minimap_visible(&self, history: &[ChatEntry]) -> bool {
        match self {
            VisualItem::CollapsedIgnoredBlock { .. } => true,
            VisualItem::Entry(hist_idx) => {
                let Some(entry) = history.get(*hist_idx) else {
                    return false;
                };
                if entry.is_empty_assistant() {
                    return false;
                }
                !matches!(entry.kind, ChatEntryKind::Actor { .. })
            }
        }
    }
}

/// A visible entry in the minimap (non-excluded).
struct VisibleEntry {
    /// Visual-item index.
    vi_index: usize,
    /// Persisted token count, if computed.
    token_count: Option<u32>,
}

/// Computes the list of visible (non-excluded) entries from visual items.
#[expect(clippy::expect_used, reason = "infallible")]
fn compute_visible_entries(state: &AppState) -> Vec<VisibleEntry> {
    let session = state.active_session();
    let history = session.history();
    let items = session.visual_items_snapshot();

    items
        .iter()
        .enumerate()
        .filter_map(|(vi_idx, item)| {
            if !item.is_minimap_visible(history) {
                return None;
            }
            let ignored = match item {
                VisualItem::CollapsedIgnoredBlock { .. } => false,
                VisualItem::Entry(hist_idx) => {
                    let entry = history.get(*hist_idx).expect("hist_idx from visual_items");
                    !entry.is_in_context()
                }
            };
            let token_count = if ignored {
                None
            } else {
                match item {
                    VisualItem::CollapsedIgnoredBlock { .. } => None,
                    VisualItem::Entry(hist_idx) => {
                        let entry = history.get(*hist_idx).expect("hist_idx from visual_items");
                        entry.token_count
                    }
                }
            };
            Some(VisibleEntry {
                vi_index: vi_idx,
                token_count,
            })
        })
        .collect()
}

fn find_block_index(selected_vi_idx: Option<usize>, visible: &[VisibleEntry]) -> Option<usize> {
    match selected_vi_idx {
        Some(idx) => visible.iter().position(|e| e.vi_index == idx),
        None => visible.len().checked_sub(1),
    }
}
#[expect(
    clippy::allow_attributes,
    reason = "dead_code is a compiler lint, not clippy"
)]
#[allow(dead_code, reason = "available for future use")]
fn compute_minimap_scroll(
    selected_block: usize,
    _total_blocks: usize,
    viewport_height: usize,
) -> usize {
    let midpoint = viewport_height / 2;
    selected_block.saturating_sub(midpoint)
}

pub struct MinimapArrow {
    pub row: u16,
    pub token_count: Option<u32>,
    /// Sum of cached token counts for `is_in_context()` entries strictly
    /// before the cursor's occupied history range.
    pub tokens_above: Option<u32>,
    /// Sum of cached token counts for `is_in_context()` entries strictly
    /// after the cursor's occupied history range.
    pub tokens_below: Option<u32>,
}

pub fn render_vertical_minimap(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    muted_text_color: Color,
) -> Option<MinimapArrow> {
    if state.session.is_loading() {
        return None;
    }

    let visible = compute_visible_entries(state);
    if visible.is_empty() {
        return None;
    }

    let total_blocks = visible.len();
    let viewport_height = area.height as usize;
    if viewport_height == 0 {
        return None;
    }

    let selected_idx = state.active_session().selected_entry_index();
    let selected_block = find_block_index(selected_idx, &visible)?;
    let midpoint = viewport_height / 2;

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport_height);
    for row in 0..viewport_height {
        let block_index = selected_block as isize + row as isize - midpoint as isize;
        if block_index >= 0 && (block_index as usize) < total_blocks {
            let entry = visible.get(block_index as usize)?;
            let span = match entry.token_count {
                Some(count) => Span::styled(
                    FULL_BLOCK.to_owned(),
                    Style::default().fg(token_threshold_color(
                        count,
                        state.frontend.preferences.minimap.max_tokens,
                    )),
                ),
                None => Span::raw(" "),
            };
            lines.push(Line::from(span));
        } else {
            lines.push(Line::from(" "));
        }
    }

    let widget = Paragraph::new(lines);
    frame.render_widget(widget, area);

    render_scroll_arrows(
        frame,
        area,
        selected_block,
        total_blocks,
        viewport_height,
        muted_text_color,
    );

    let arrow_row = midpoint as u16;
    let selected_token_count = visible.get(selected_block).and_then(|e| e.token_count);
    let (cursor_start, cursor_end) = {
        let session = state.active_session();
        let items = session.visual_items_snapshot();
        let history_len = session.history().len();
        // When the user has not yet placed the cursor, fall back to the last
        // visual item so above/below indicators render immediately. This
        // mirrors `find_block_index`'s `None`-arm fallback.
        let effective_idx = selected_idx.or_else(|| items.len().checked_sub(1));
        cursor_history_range(&items, effective_idx, history_len)
    };
    let tokens_above = compute_tokens_above(state, cursor_start);
    let tokens_below = compute_tokens_below(state, cursor_end);

    Some(MinimapArrow {
        row: arrow_row,
        token_count: selected_token_count,
        tokens_above,
        tokens_below,
    })
}

/// Returns the cursor's occupied history range as `[start, end)`.
///
/// - For `Entry(hist_idx)` → `[hist_idx, hist_idx + 1)`.
/// - For `CollapsedIgnoredBlock { start, count }` → `[start, start + count)`.
/// - When no visual item is selected (or the index is out of range) →
///   `[0, history_len)` (full range; both above and below sums become empty).
fn cursor_history_range(
    items: &[VisualItem],
    selected_vi_idx: Option<usize>,
    history_len: usize,
) -> (usize, usize) {
    match selected_vi_idx {
        Some(idx) => match items.get(idx) {
            Some(VisualItem::Entry(hist_idx)) => (*hist_idx, hist_idx.saturating_add(1)),
            Some(VisualItem::CollapsedIgnoredBlock { start, count }) => {
                (*start, start.saturating_add(*count))
            }
            None => (0, history_len),
        },
        None => (0, history_len),
    }
}

/// Sums cached token counts for all `is_in_context()` entries in
/// history range `0..end` (exclusive upper bound). Returns `Some(0)`
/// when the range is empty or all entries are excluded.
fn compute_tokens_above(state: &AppState, end: usize) -> Option<u32> {
    compute_token_sum_in_range(state, 0, end)
}

/// Sums cached token counts for all `is_in_context()` entries in
/// history range `start..history.len()` (exclusive lower bound).
/// Returns `Some(0)` when the range is empty or all entries are excluded.
fn compute_tokens_below(state: &AppState, start: usize) -> Option<u32> {
    let history_len = state.active_session().history().len();
    compute_token_sum_in_range(state, start, history_len)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "trait contract requires Result return"
)]
fn compute_token_sum_in_range(state: &AppState, start: usize, end: usize) -> Option<u32> {
    let session = state.active_session();
    let history = session.history();

    let end = end.min(history.len());
    let start = start.min(end);
    let mut sum: u32 = 0;
    for entry in history.get(start..end).unwrap_or(&[]) {
        if entry.is_in_context()
            && let Some(count) = entry.token_count
        {
            sum = sum.saturating_add(count);
        }
    }
    Some(sum)
}

fn render_scroll_arrows(
    frame: &mut Frame<'_>,
    area: Rect,
    selected_block: usize,
    total_blocks: usize,
    viewport_height: usize,
    muted_text_color: Color,
) {
    let midpoint = viewport_height / 2;
    let has_above = selected_block > midpoint;
    let has_below = selected_block + (viewport_height - midpoint) < total_blocks;

    if has_above {
        let arrow_area = Rect {
            x: area.x,
            y: area.y,
            width: 1,
            height: 1,
        };
        let arrow = Paragraph::new(Line::from(Span::styled(
            "▲",
            Style::default().fg(muted_text_color),
        )));
        frame.render_widget(arrow, arrow_area);
    }

    if has_below {
        let bottom_y = area.y + area.height.saturating_sub(1);
        let arrow_area = Rect {
            x: area.x,
            y: bottom_y,
            width: 1,
            height: 1,
        };
        let arrow = Paragraph::new(Line::from(Span::styled(
            "▼",
            Style::default().fg(muted_text_color),
        )));
        frame.render_widget(arrow, arrow_area);
    }
}

pub fn render_minimap_arrow(
    frame: &mut Frame<'_>,
    chat_log_area: Rect,
    arrow: &MinimapArrow,
    arrow_color: Color,
) {
    if chat_log_area.width == 0 || chat_log_area.height == 0 {
        return;
    }

    let y = chat_log_area.y + arrow.row.min(chat_log_area.height.saturating_sub(1));

    #[expect(
        clippy::single_match_else,
        reason = "different match arms produce different widget layouts"
    )]
    match arrow.token_count {
        Some(count) => {
            let formatted = format_entry_tokens(count);
            let text = format!("{formatted} >");
            let width = text.len() as u16;
            let x = chat_log_area
                .x
                .saturating_add(chat_log_area.width)
                .saturating_sub(width);
            let paragraph = Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(arrow_color),
            )));
            let arrow_area = Rect {
                x,
                y,
                width: width.min(chat_log_area.width),
                height: 1,
            };
            frame.render_widget(paragraph, arrow_area);
        }
        None => {
            let x = chat_log_area.x + chat_log_area.width.saturating_sub(1);
            let paragraph = Paragraph::new(Line::from(Span::styled(
                ">",
                Style::default().fg(arrow_color),
            )));
            let arrow_area = Rect {
                x,
                y,
                width: 1,
                height: 1,
            };
            frame.render_widget(paragraph, arrow_area);
        }
    }

    // Render ▲ token-count line above the arrow (strict-above: cursor excluded).
    if arrow.row > 0 {
        let row = arrow.row - 1;
        let text = match arrow.tokens_above {
            Some(n) => format!("{} ▲", format_entry_tokens(n)),
            _ => "▲".to_owned(),
        };
        let width = text.as_str().width() as u16;
        let x = chat_log_area
            .x
            .saturating_add(chat_log_area.width)
            .saturating_sub(width);
        let area = Rect {
            x,
            y: chat_log_area.y + row,
            width: width.min(chat_log_area.width),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(arrow_color),
            ))),
            area,
        );
    }

    // Render ▼ token-count line below the arrow (strict-below: cursor excluded).
    {
        let row = arrow.row + 1;
        if row < chat_log_area.height {
            let text = match arrow.tokens_below {
                Some(n) => format!("{} ▼", format_entry_tokens(n)),
                _ => "▼".to_owned(),
            };
            let width = text.as_str().width() as u16;
            let x = chat_log_area
                .x
                .saturating_add(chat_log_area.width)
                .saturating_sub(width);
            let area = Rect {
                x,
                y: chat_log_area.y + row,
                width: width.min(chat_log_area.width),
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(arrow_color),
                ))),
                area,
            );
        }
    }
}

fn format_entry_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", f64::from(count) / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", f64::from(count) / 1_000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::protocol::ChatEntry;
    use crate::feat::theme::default_theme;

    #[rstest::rstest]
    fn find_block_index_returns_position_for_existing_entry() {
        let visible = vec![
            VisibleEntry {
                vi_index: 0,
                token_count: None,
            },
            VisibleEntry {
                vi_index: 2,
                token_count: None,
            },
            VisibleEntry {
                vi_index: 5,
                token_count: None,
            },
        ];
        assert_eq!(find_block_index(Some(2), &visible), Some(1));
    }

    #[rstest::rstest]
    fn find_block_index_returns_none_for_excluded_entry() {
        let visible = vec![
            VisibleEntry {
                vi_index: 0,
                token_count: None,
            },
            VisibleEntry {
                vi_index: 2,
                token_count: None,
            },
        ];
        assert!(find_block_index(Some(1), &visible).is_none());
    }

    #[rstest::rstest]
    fn find_block_index_returns_last_when_none() {
        let visible = vec![
            VisibleEntry {
                vi_index: 0,
                token_count: None,
            },
            VisibleEntry {
                vi_index: 2,
                token_count: None,
            },
        ];
        assert_eq!(find_block_index(None, &visible), Some(1));
    }

    #[rstest::rstest]
    fn find_block_index_returns_none_for_empty() {
        let visible: Vec<VisibleEntry> = vec![];
        assert!(find_block_index(Some(0), &visible).is_none());
    }

    #[rstest::rstest]
    fn scroll_is_midpoint_based() {
        assert_eq!(compute_minimap_scroll(4, 5, 10), 0);
    }

    #[rstest::rstest]
    fn scroll_centers_selected() {
        assert_eq!(compute_minimap_scroll(45, 50, 10), 40);
    }

    #[rstest::rstest]
    fn scroll_at_start_is_zero() {
        assert_eq!(compute_minimap_scroll(0, 50, 10), 0);
    }

    #[rstest::rstest]
    fn scroll_at_last_block() {
        assert_eq!(compute_minimap_scroll(49, 50, 10), 44);
    }

    #[rstest::rstest]
    fn scroll_near_midpoint() {
        assert_eq!(compute_minimap_scroll(5, 50, 10), 0);
    }

    fn render_to_buffer(
        state: &AppState,
        width: u16,
        height: u16,
    ) -> (Option<MinimapArrow>, Vec<String>) {
        setup_visual_items(state);
        let (mut terminal, area) = jinn_testutil::setup_term(width, height);
        let theme = default_theme();
        let mut arrow_result = None;
        terminal
            .draw(|frame| {
                arrow_result = render_vertical_minimap(frame, area, state, theme.muted_text);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = jinn_testutil::buffer_rows(buffer, width, height);
        (arrow_result, rows)
    }

    #[rstest::rstest]
    fn empty_history_renders_nothing() {
        let state = AppState::default();
        let (arrow, rows) = render_to_buffer(&state, 1, 10);
        assert!(arrow.is_none());
        assert!(rows[0].trim().is_empty());
    }

    #[rstest::rstest]
    fn single_entry_no_cache_shows_space_at_midpoint() {
        let mut state = AppState::default();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        let (arrow, rows) = render_to_buffer(&state, 1, 10);
        assert!(arrow.is_some());
        assert!(
            !rows[5].contains('\u{2588}'),
            "no block without token count"
        );
    }

    #[rstest::rstest]
    fn single_entry_with_count_shows_block_at_midpoint() {
        let mut state = AppState::default();
        let mut entry = ChatEntry::user("hello world");
        entry.token_count = Some(50);
        state.active_session_mut().push_entry(entry);
        let (arrow, rows) = render_to_buffer(&state, 1, 10);
        assert!(arrow.is_some());
        assert!(rows[5].contains('\u{2588}'), "expected block at midpoint");
    }

    #[rstest::rstest]
    fn arrow_at_midpoint_when_last_entry_selected() {
        let mut state = AppState::default();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        let (arrow, _) = render_to_buffer(&state, 1, 10);
        assert_eq!(arrow.expect("arrow exists").row, 5);
    }

    #[rstest::rstest]
    fn excluded_entries_produce_no_blocks() {
        let mut state = AppState::default();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::actor("bash", "output"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::thinking("reasoning"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        let (arrow, rows) = render_to_buffer(&state, 1, 10);
        assert_eq!(rows.iter().filter(|r| r.contains('\u{2588}')).count(), 0);
        assert_eq!(arrow.expect("arrow").row, 5);
    }

    #[rstest::rstest]
    fn arrow_clamps_to_viewport_height() {
        let mut state = AppState::default();
        for i in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }
        let (arrow, _) = render_to_buffer(&state, 1, 5);
        assert_eq!(arrow.expect("arrow").row, 2);
    }

    #[rstest::rstest]
    fn arrow_renders_greater_than_character() {
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 3,
            token_count: None,
            tokens_above: None,
            tokens_below: None,
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        assert!(rows[3].contains('>'));
    }

    #[rstest::rstest]
    fn scroll_down_arrow_at_bottom() {
        let mut state = AppState::default();
        for i in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }
        state.active_session_mut().set_selected_entry_index(0);
        let (_, rows) = render_to_buffer(&state, 1, 5);
        assert!(rows[4].contains('▼'));
    }

    #[rstest::rstest]
    fn scroll_up_arrow_at_top() {
        let mut state = AppState::default();
        for i in 0..20 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }
        let (_, rows) = render_to_buffer(&state, 1, 5);
        assert!(rows[0].contains('▲'));
    }

    #[rstest::rstest]
    fn no_arrows_when_all_entries_fit() {
        let mut state = AppState::default();
        state.active_session_mut().push_entry(ChatEntry::user("a"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("b"));
        state.active_session_mut().push_entry(ChatEntry::user("c"));
        let (_, rows) = render_to_buffer(&state, 1, 10);
        assert!(!rows.iter().any(|r| r.contains('▲')));
        assert!(!rows.iter().any(|r| r.contains('▼')));
    }

    #[rstest::rstest]
    fn empty_assistant_entry_produces_no_block() {
        let mut state = AppState::default();
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant(""));
        let (arrow, rows) = render_to_buffer(&state, 1, 10);
        assert!(arrow.is_none());
        assert_eq!(rows.iter().filter(|r| r.contains('\u{2588}')).count(), 0);
    }

    fn setup_visual_items(state: &AppState) {
        let session = state.active_session();
        let shown_ignored_blocks = session.shown_ignored_blocks_snapshot();
        let items = build_visual_items(
            session.history(),
            &shown_ignored_blocks,
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        state.active_session().set_visual_items(items);
    }

    #[rstest::rstest]
    fn in_context_entry_with_count_shows_block() {
        let mut state = AppState::default();
        let mut entry = ChatEntry::user("hello world this is a test");
        entry.token_count = Some(500);
        state.active_session_mut().push_entry(entry);
        let (_, rows) = render_to_buffer(&state, 1, 10);
        assert_eq!(rows[5].chars().filter(|&c| c == '\u{2588}').count(), 1);
    }

    #[rstest::rstest]
    fn entry_without_count_shows_space() {
        let mut state = AppState::default();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        let (_, rows) = render_to_buffer(&state, 1, 10);
        assert_eq!(rows[5].chars().filter(|&c| c == '\u{2588}').count(), 0);
    }

    #[rstest::rstest]
    fn token_threshold_band_0_is_blue() {
        // Band 0: [0, 250) - deep indigo
        assert_eq!(token_threshold_color(0, 2000), Color::Rgb(39, 12, 77));
        assert_eq!(token_threshold_color(249, 2000), Color::Rgb(39, 12, 77));
    }

    #[rstest::rstest]
    fn token_threshold_band_1_is_cyan() {
        // Band 1: [250, 500) - deep indigo
        assert_eq!(token_threshold_color(250, 2000), Color::Rgb(39, 12, 77));
        assert_eq!(token_threshold_color(499, 2000), Color::Rgb(39, 12, 77));
    }

    #[rstest::rstest]
    fn token_threshold_band_2_is_green() {
        // Band 2: [500, 750) - violet
        assert_eq!(token_threshold_color(500, 2000), Color::Rgb(100, 20, 108));
        assert_eq!(token_threshold_color(749, 2000), Color::Rgb(100, 20, 108));
    }

    #[rstest::rstest]
    fn token_threshold_band_3_is_yellow_green() {
        // Band 3: [750, 1000) - magenta-rose
        assert_eq!(token_threshold_color(750, 2000), Color::Rgb(156, 43, 99));
        assert_eq!(token_threshold_color(999, 2000), Color::Rgb(156, 43, 99));
    }

    #[rstest::rstest]
    fn token_threshold_band_4_is_gold() {
        // Band 4: [1000, 1250) - warm red
        assert_eq!(token_threshold_color(1000, 2000), Color::Rgb(208, 74, 67));
        assert_eq!(token_threshold_color(1249, 2000), Color::Rgb(208, 74, 67));
    }

    #[rstest::rstest]
    fn token_threshold_band_5_is_red_orange() {
        // Band 5: [1250, 1500) - orange
        assert_eq!(token_threshold_color(1250, 2000), Color::Rgb(243, 125, 22));
        assert_eq!(token_threshold_color(1499, 2000), Color::Rgb(243, 125, 22));
    }

    #[rstest::rstest]
    fn token_threshold_band_6_is_dark_red() {
        // Band 6: [1500, 1750) - gold
        assert_eq!(token_threshold_color(1500, 2000), Color::Rgb(251, 197, 51));
        assert_eq!(token_threshold_color(1749, 2000), Color::Rgb(251, 197, 51));
    }

    #[rstest::rstest]
    fn token_threshold_band_7_is_crimson() {
        // Band 7: [1750, ∞) - pale yellow
        assert_eq!(token_threshold_color(1750, 2000), Color::Rgb(252, 255, 164));
        assert_eq!(token_threshold_color(2000, 2000), Color::Rgb(252, 255, 164));
        assert_eq!(token_threshold_color(9999, 2000), Color::Rgb(252, 255, 164));
    }

    #[rstest::rstest]
    fn token_threshold_custom_max_tokens_adjusts_bands() {
        // With max_tokens=1000, each band is 125 tokens wide.
        assert_eq!(token_threshold_color(0, 1000), Color::Rgb(39, 12, 77));
        assert_eq!(token_threshold_color(124, 1000), Color::Rgb(39, 12, 77));
        assert_eq!(token_threshold_color(125, 1000), Color::Rgb(39, 12, 77));
        assert_eq!(token_threshold_color(999, 1000), Color::Rgb(252, 255, 164));
    }

    #[rstest::rstest]
    fn token_threshold_zero_max_returns_first_band() {
        assert_eq!(token_threshold_color(100, 0), MINIMAP_PALETTE[0]);
    }

    #[rstest::rstest]
    fn format_entry_tokens_small() {
        assert_eq!(format_entry_tokens(42), "42");
    }

    #[rstest::rstest]
    fn format_entry_tokens_k() {
        assert_eq!(format_entry_tokens(1_000), "1.0k");
        assert_eq!(format_entry_tokens(42_500), "42.5k");
    }

    #[rstest::rstest]
    fn format_entry_tokens_m() {
        assert_eq!(format_entry_tokens(1_000_000), "1.0M");
    }

    #[rstest::rstest]
    fn arrow_with_token_count_renders_formatted_count() {
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 3,
            token_count: Some(3000),
            tokens_above: None,
            tokens_below: None,
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        assert!(rows[3].contains('3'));
        assert!(rows[3].contains('>'));
    }

    #[rstest::rstest]
    fn arrow_without_token_count_renders_just_gt() {
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 3,
            token_count: None,
            tokens_above: None,
            tokens_below: None,
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        assert!(rows[3].contains('>'));
        assert!(!rows[3].contains('k'));
    }

    // ── Strict-above / strict-below computation tests ──

    #[rstest::rstest]
    fn tokens_above_and_below_for_single_entry() {
        // Single in-context entry selected: both sides are empty.
        let mut state = AppState::default();
        let mut entry = ChatEntry::user("hello");
        entry.token_count = Some(100);
        state.active_session_mut().push_entry(entry);
        setup_visual_items(&state);
        let items = state.active_session().visual_items_snapshot();
        let history_len = state.active_session().history().len();
        let (start, end) = cursor_history_range(&items, Some(0), history_len);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        // Empty range → Some(0).
        assert_eq!(above, Some(0));
        assert_eq!(below, Some(0));
    }

    #[rstest::rstest]
    fn tokens_above_and_below_skip_excluded_entries() {
        // Two entries: [thinking (excluded), user (in-context, selected)].
        // Above-skipped because thinking is not in context; below-empty.
        let mut state = AppState::default();
        let mut thinking = ChatEntry::thinking("reasoning");
        thinking.token_count = Some(7);
        let mut user = ChatEntry::user("hello");
        user.token_count = Some(200);
        state.active_session_mut().push_entry(thinking);
        state.active_session_mut().push_entry(user);
        setup_visual_items(&state);
        let items = state.active_session().visual_items_snapshot();
        let history_len = state.active_session().history().len();
        // Cursor on the user entry — the last visual item.
        let last_vi = items.len() - 1;
        let (start, end) = cursor_history_range(&items, Some(last_vi), history_len);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        // thinking's 0 cached count is skipped; nothing remains above.
        // No in-context entries above → Some(0); nothing below → Some(0).
        assert_eq!(above, Some(0));
        assert_eq!(below, Some(0));
    }

    #[rstest::rstest]
    fn tokens_above_excludes_cursor_tokens_below_sums_after_cursor() {
        // Three in-context entries [user100, assistant200, user300].
        let mut state = AppState::default();
        let mut e1 = ChatEntry::user("a");
        e1.token_count = Some(100);
        let mut e2 = ChatEntry::assistant("b");
        e2.token_count = Some(200);
        let mut e3 = ChatEntry::user("c");
        e3.token_count = Some(300);
        state.active_session_mut().push_entry(e1);
        state.active_session_mut().push_entry(e2);
        state.active_session_mut().push_entry(e3);
        setup_visual_items(&state);
        let items = state.active_session().visual_items_snapshot();
        let history_len = state.active_session().history().len();

        // Case 1: cursor on the LAST entry (history idx 2, last visual item).
        let last_vi = items.len() - 1;
        let (start, end) = cursor_history_range(&items, Some(last_vi), history_len);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        assert_eq!(above, Some(300)); // e1 + e2
        // Empty range below last cursor → Some(0).
        assert_eq!(below, Some(0));

        // Case 2: cursor on the MIDDLE entry (history idx 1).
        // Find the visual item index for history index 1.
        let mid_vi = items
            .iter()
            .position(|i| matches!(i, VisualItem::Entry(1)))
            .expect("history idx 1 must be a visual item");
        let (start, end) = cursor_history_range(&items, Some(mid_vi), history_len);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        assert_eq!(above, Some(100));
        assert_eq!(below, Some(300));
    }

    #[rstest::rstest]
    fn tokens_above_and_below_are_zero_when_no_counts_computed() {
        let mut state = AppState::default();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));
        state
            .active_session_mut()
            .push_entry(ChatEntry::assistant("world"));
        setup_visual_items(&state);
        let items = state.active_session().visual_items_snapshot();
        let history_len = state.active_session().history().len();
        let last_vi = items.len() - 1;
        let (start, end) = cursor_history_range(&items, Some(last_vi), history_len);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        // Cache is empty: sum is 0 → Some(0). Renderer now shows "0 ▲"/
        // "0 ▼" rather than glyph-alone, because the user wants to see
        // explicit zero counts.
        assert_eq!(above, Some(0));
        assert_eq!(below, Some(0));
    }

    #[rstest::rstest]
    fn collapsed_ignored_block_cursor_excludes_entire_block() {
        // History indices: 0=user(100), 1=thinking(50), 2=thinking(60), 3=thinking(70),
        // 4=user(200), 5=user(300). Cursor sits on the collapsed block covering
        // indices 1..4.
        let mut state = AppState::default();
        let counts = [100u32, 50, 60, 70, 200, 300];
        let entries: Vec<ChatEntry> = vec![
            ChatEntry::user("first"),
            ChatEntry::thinking("t1"),
            ChatEntry::thinking("t2"),
            ChatEntry::thinking("t3"),
            ChatEntry::user("second"),
            ChatEntry::user("third"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, mut entry)| {
            entry.token_count = Some(counts[i]);
            entry
        })
        .collect();
        for entry in entries {
            state.active_session_mut().push_entry(entry);
        }

        // Manually install visual items with a collapsed block.
        let items = vec![
            VisualItem::Entry(0),
            VisualItem::CollapsedIgnoredBlock { start: 1, count: 3 },
            VisualItem::Entry(4),
            VisualItem::Entry(5),
        ];
        state.active_session().set_visual_items(items);

        // Cursor on the collapsed block (vi_idx = 1).
        let items = state.active_session().visual_items_snapshot();
        let history_len = state.active_session().history().len();
        let (start, end) = cursor_history_range(&items, Some(1), history_len);
        assert_eq!(start, 1);
        assert_eq!(end, 4);
        let above = compute_tokens_above(&state, start);
        let below = compute_tokens_below(&state, end);
        // Above excludes the block (only user(0) = 100).
        assert_eq!(above, Some(100));
        // Below excludes the block (user(4) + user(5) = 500).
        assert_eq!(below, Some(500));
    }

    // ── Above/below rendering tests ──

    #[rstest::rstest]
    fn above_and_below_lines_flank_arrow() {
        // Arrow on row 3 with token counts on both sides.
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 3,
            token_count: Some(1000),
            tokens_above: Some(6000),
            tokens_below: Some(1500),
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        // Arrow row has the cursor tokens and `>`.
        assert!(rows[3].contains('1'));
        assert!(rows[3].contains('>'));
        // Row ABOVE has 6.0k and ▲.
        assert!(rows[2].contains('6'));
        assert!(rows[2].contains('▲'));
        assert!(!rows[2].contains('>'));
        // Row BELOW has 1.5k and ▼.
        assert!(rows[4].contains('1'));
        assert!(rows[4].contains('▼'));
        assert!(!rows[4].contains('>'));
    }

    #[rstest::rstest]
    fn below_line_skipped_when_arrow_at_last_row_above_still_renders() {
        // Arrow at row 9 (last) — ▼ cannot fit below, but ▲ on row 8 still renders.
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 9,
            token_count: Some(100),
            tokens_above: Some(500),
            tokens_below: Some(999),
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        // Row 9 has the arrow (>) but NO ▼.
        assert!(rows[9].contains('>'));
        assert!(!rows[9].contains('▼'));
        // Row 8 still has ▲.
        assert!(rows[8].contains('▲'));
    }

    #[rstest::rstest]
    fn above_line_skipped_when_arrow_at_row_zero() {
        // Arrow at row 0 — ▲ cannot fit above, but ▼ on row 1 still renders.
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 0,
            token_count: Some(100),
            tokens_above: Some(500),
            tokens_below: Some(999),
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        // Row 0 has the arrow (>) but NO ▲.
        assert!(rows[0].contains('>'));
        assert!(!rows[0].contains('▲'));
        // Row 1 has ▼.
        assert!(rows[1].contains('▼'));
    }

    #[rstest::rstest]
    fn glyph_alone_rendered_when_no_cached_counts() {
        // tokens_above = None and tokens_below = None: glyph alone on both rows.
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let arrow = MinimapArrow {
            row: 3,
            token_count: None,
            tokens_above: None,
            tokens_below: None,
        };
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        // Row above has ▲ alone (no digit).
        assert!(rows[2].contains('▲'));
        assert!(!rows[2].chars().any(|c| c.is_ascii_digit()));
        // Row below has ▼ alone (no digit).
        assert!(rows[4].contains('▼'));
        assert!(!rows[4].chars().any(|c| c.is_ascii_digit()));
    }

    /// Fix 1: `Some(0)` should render as `"0 ▲"` / `"0 ▼"`, not glyph-alone.
    /// Glyph-alone is reserved for `None` (no cached counts at all).
    #[rstest::rstest]
    fn zero_count_renders_with_digit() {
        let mut state = AppState::default();
        for i in 0..3 {
            state
                .active_session_mut()
                .push_entry(ChatEntry::user(format!("msg {i}")));
        }
        state.active_session_mut().set_selected_entry_index(1);
        // No token cache populated — render_minimap_arrow with a literal arrow.
        let arrow = MinimapArrow {
            row: 3,
            token_count: None,
            tokens_above: Some(0),
            tokens_below: Some(0),
        };
        let (mut terminal, area) = jinn_testutil::setup_term(40, 10);
        let theme = default_theme();
        terminal
            .draw(|frame| {
                render_minimap_arrow(frame, area, &arrow, theme.border_unfocused);
            })
            .unwrap();
        let rows = jinn_testutil::buffer_rows(terminal.backend().buffer(), 40, 10);
        // Row above has `0 ▲` (digit + glyph).
        assert!(rows[2].contains('0'));
        assert!(rows[2].contains('▲'));
        // Row below has `0 ▼` (digit + glyph).
        assert!(rows[4].contains('0'));
        assert!(rows[4].contains('▼'));
    }

    /// Fix 2: when no cursor has been placed (`selected_entry_index() == None`),
    /// the cursor is treated as the last visual item so the above/below
    /// counts are meaningful immediately on first render.
    #[rstest::rstest]
    fn no_cursor_falls_back_to_last_visual_item() {
        let mut state = AppState::default();
        for i in 0..3 {
            let mut entry = ChatEntry::user(format!("msg {i}"));
            entry.token_count = Some((i as u32 + 1) * 100);
            state.active_session_mut().push_entry(entry);
        }
        // Deliberately do NOT call set_selected_entry_index.
        let (arrow, _rows) = render_to_buffer(&state, 1, 10);
        let arrow = arrow.expect("arrow should render even without cursor");
        // History: [100, 200, 300]. Cursor defaults to last → above = 100+200 = 300.
        assert_eq!(arrow.tokens_above, Some(300));
        // Below = no entries after cursor → Some(0).
        assert_eq!(arrow.tokens_below, Some(0));
    }
}
