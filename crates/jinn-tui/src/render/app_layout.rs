//! Application layout computation and minimum size constants.

use ratatui::layout::{Constraint, Layout, Rect};

/// Minimum sidebar width in columns.
pub const MIN_SIDEBAR_WIDTH: u16 = 15;

/// Minimum terminal width.
pub const MIN_WIDTH: u16 = 40;
/// Minimum terminal height.
pub const MIN_HEIGHT: u16 = 15;

/// Top-level application layout areas.
pub struct AppLayout {
    /// The left column: chat + indicator + queue + input + status bar.
    pub main: Rect,
    /// The vertical minimap column (1 char wide, same height as chat log area).
    pub minimap: Rect,
    /// The dictation meter strip: the minimap column from the streaming
    /// indicator row down to the terminal bottom edge (chat layout only).
    pub meter_strip: Rect,
    /// The right column: sidebar (full height).
    pub sidebar: Rect,
    /// The vertical border between minimap and sidebar (1 column wide).
    pub border: Rect,
    /// The tab bar area (1 row at the very top of the main column).
    pub tab_bar: Rect,
    // Sub-areas of the main column:
    /// The content area (chat log + indicator + queue + bottom line).
    pub content: Rect,
    /// The input box area (dynamic height, chat tab only).
    pub input: Rect,
    /// The status bar area (1 row at very bottom).
    pub status_bar: Rect,
}

impl AppLayout {
    /// Returns `true` if the given area meets minimum size requirements.
    #[must_use]
    pub const fn meets_min_size(area: Rect) -> bool {
        area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
    }

    /// Computes the layout for the given terminal area.
    ///
    /// Layout structure:
    /// ```text
    /// main column | minimap(1) | border | sidebar (full height)
    ///   content   |   ▲        |        |
    ///             |   █        |        |
    ///             |   ▼        |        |
    ///   input     |            |        |
    ///   status    |            |        |
    /// ```
    ///
    /// `input_lines` is the number of visual lines the input box needs
    /// (used for dynamic multi-line input height).
    ///
    /// `max_input_height` caps the input box height (e.g., 50% of terminal).
    #[must_use]
    pub fn new(area: Rect, input_lines: u16, max_input_height: u16, sidebar_width: u16) -> Self {
        // Horizontal split: main column | minimap(1) | border(1) | sidebar
        let sidebar_width = sidebar_width
            .min(area.width.saturating_sub(MIN_WIDTH))
            .max(MIN_SIDEBAR_WIDTH);
        let border_width: u16 = 1;
        let minimap_width: u16 = 1;
        let main_width = area
            .width
            .saturating_sub(sidebar_width)
            .saturating_sub(border_width)
            .saturating_sub(minimap_width);

        let main = Rect {
            x: area.x,
            y: area.y,
            width: main_width,
            height: area.height,
        };

        let input_height = (1 + input_lines.max(1)).min(max_input_height);
        let [tab_bar, content, input, status_bar] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(2),
        ])
        .areas(main);

        // Minimap column: same height as the chat log area (content minus
        // the bottom indicator line and chat-bottom-line).
        let bottom_lines: u16 = 2;
        let chat_log_height = content.height.saturating_sub(bottom_lines);
        let minimap = Rect {
            x: main.x + main.width,
            y: content.y,
            width: minimap_width,
            height: chat_log_height,
        };

        // Dictation meter strip: the minimap column from the streaming
        // indicator row (first of the content area's two bottom rows, see
        // chat_tab's `bottom_lines`) to the terminal bottom edge. It spans
        // the input-box and status-bar bands, which paint nothing in this
        // column, and its top anchor rides the indicator row, so the strip
        // resizes automatically as the input box grows.
        let indicator_row_y = content.y + content.height.saturating_sub(bottom_lines);
        let meter_strip = Rect {
            x: minimap.x,
            y: indicator_row_y,
            width: minimap_width,
            height: (area.y + area.height).saturating_sub(indicator_row_y),
        };

        let border = Rect {
            x: main.x + main.width + minimap_width,
            y: area.y,
            width: border_width,
            height: area.height,
        };
        let sidebar = Rect {
            x: main.x + main.width + minimap_width + border_width,
            y: area.y,
            width: sidebar_width,
            height: area.height,
        };

        Self {
            main,
            minimap,
            meter_strip,
            sidebar,
            border,
            tab_bar,
            content,
            input,
            status_bar,
        }
    }
}
/// Full-terminal dynamic-tab layout: a 1-row tab bar on top, everything else
/// is full-width content (no sidebar, border, minimap, input, or status bar).
/// Used by any slice whose registered tab descriptor selects full-width
/// framing; named after the dashboard, the first slice to use it.
pub struct TabLayout {
    /// The tab bar area (1 row at the very top).
    pub tab_bar: Rect,
    /// The content area (everything below the tab bar, full terminal width).
    pub content: Rect,
}

impl TabLayout {
    /// Computes the full-width tab layout for the given terminal area.
    ///
    /// Structure:
    /// ```text
    /// tab_bar (1 row)
    /// content (everything below, full width)
    /// ```
    #[must_use]
    pub fn new(area: Rect) -> Self {
        let [tab_bar, content] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        Self { tab_bar, content }
    }
}

/// The active application layout, selected by the base focus scope.
///
/// Chat mode uses the multi-column [`AppLayout`]; a dynamic tab uses the
/// full-terminal [`TabLayout`]. Branching on this enum guarantees the tab
/// path physically cannot render chat-only chrome (sidebar, status bar,
/// input, border) — those fields simply do not exist on [`TabLayout`].
///
/// Named `AppFrameLayout` (not `Layout`) to avoid collision with ratatui's
/// [`ratatui::layout::Layout`], which `AppLayout::new` and
/// [`TabLayout::new`] call for `Layout::vertical` splits.
pub enum AppFrameLayout {
    /// Chat tab layout: main column + minimap + border + sidebar, with
    /// content, input, and status-bar sub-areas.
    Chat(AppLayout),
    /// Dynamic tab layout: tab bar + full-width content only.
    Tab(TabLayout),
}

impl AppFrameLayout {
    /// Computes the layout for the given terminal area, branching on the
    /// base focus scope.
    ///
    /// `input_lines` and `sidebar_width` are ignored in tab mode.
    /// `max_input_height` likewise. The terminal is an overlay and does not
    /// participate in the frame layout.
    #[must_use]
    pub fn new(
        area: Rect,
        input_lines: u16,
        max_input_height: u16,
        sidebar_width: u16,
        is_dashboard: bool,
    ) -> Self {
        if is_dashboard {
            Self::Tab(TabLayout::new(area))
        } else {
            Self::Chat(AppLayout::new(
                area,
                input_lines,
                max_input_height,
                sidebar_width,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]
    use super::*;
    use ratatui::layout::Rect;

    #[rstest::rstest]
    fn meets_min_size() {
        // Given a 40x15 area.
        let area = Rect::new(0, 0, 40, 15);

        // When checking meets_min_size.
        let result = AppLayout::meets_min_size(area);

        // Then it returns true.
        assert!(result);
    }

    #[rstest::rstest]
    fn too_small() {
        // Given a 10x5 area.
        let area = Rect::new(0, 0, 10, 5);

        // When checking meets_min_size.
        let result = AppLayout::meets_min_size(area);

        // Then it returns false.
        assert!(!result);
    }

    #[rstest::rstest]
    fn includes_status_bar() {
        // Given a 40x14 area.
        let area = Rect::new(0, 0, 40, 14);

        // When computing the chat layout for it.
        let layout = AppLayout::new(area, 1, area.height / 2, 30);

        // Then the status bar sits below the input and reaches the bottom edge.
        assert_eq!(layout.status_bar.height, 2);
        assert!(layout.status_bar.y > layout.input.y);
        assert_eq!(layout.status_bar.y + layout.status_bar.height, area.height);
    }

    #[rstest::rstest]
    fn tab_layout_uses_full_width() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);

        // When computing the full-width tab layout.
        let layout = TabLayout::new(area);

        // Then the tab bar is the top row, full width.
        assert_eq!(layout.tab_bar, Rect::new(0, 0, 80, 1));
        // And the content fills everything below, full width.
        assert_eq!(layout.content, Rect::new(0, 1, 80, 23));
    }

    #[rstest::rstest]
    fn meter_strip_anchors_the_indicator_row_to_the_term_bottom() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);
        let layout = AppLayout::new(area, 1, area.height / 2, 30);

        // When reading the meter strip off the layout.
        let strip = layout.meter_strip;

        // Then it is the minimap column, starting at the indicator row
        // (the first of content's two bottom rows) and reaching the
        // terminal bottom edge.
        assert_eq!(strip.x, layout.minimap.x);
        assert_eq!(strip.width, 1);
        assert_eq!(strip.y, layout.content.y + layout.content.height - 2);
        assert_eq!(strip.y + strip.height, area.y + area.height);
        // And it does not overlap the minimap (strip starts below it).
        assert_eq!(strip.y, layout.minimap.y + layout.minimap.height);
    }

    #[rstest::rstest]
    fn meter_strip_grows_when_the_input_adds_lines() {
        // Given an 80x24 area laid out with a 1-line input.
        let area = Rect::new(0, 0, 80, 24);
        let narrow = AppLayout::new(area, 1, area.height / 2, 30);

        // When the input grows to 4 visual lines.
        let tall = AppLayout::new(area, 4, area.height / 2, 30);

        // Then the strip is taller: its top anchor moved up with the
        // indicator row while its bottom stayed on the terminal edge.
        assert!(tall.meter_strip.height > narrow.meter_strip.height);
        assert_eq!(tall.meter_strip.height, narrow.meter_strip.height + (4 - 1));
        assert_eq!(tall.meter_strip.y + tall.meter_strip.height, area.bottom());
        assert_eq!(tall.meter_strip.y, tall.content.y + tall.content.height - 2);
    }

    #[rstest::rstest]
    fn frame_layout_chat_variant_matches_the_direct_chat_layout() {
        // Given an 80x24 area and the direct chat layout for it.
        let area = Rect::new(0, 0, 80, 24);
        let chat_direct = AppLayout::new(area, 1, area.height / 2, 30);

        // When building the frame layout in chat mode.
        let chat_via_frame = AppFrameLayout::new(area, 1, area.height / 2, 30, false);

        // Then the frame yields a Chat layout whose panes match the direct one.
        let AppFrameLayout::Chat(frame_chat) = chat_via_frame else {
            panic!("expected Chat layout for is_dashboard=false");
        };
        assert_eq!(frame_chat.content, chat_direct.content);
        assert_eq!(frame_chat.sidebar, chat_direct.sidebar);
        assert_eq!(frame_chat.status_bar, chat_direct.status_bar);
    }

    #[rstest::rstest]
    fn frame_layout_dashboard_variant_is_full_width() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);

        // When building the frame layout in dashboard mode.
        let frame = AppFrameLayout::new(area, 1, area.height / 2, 30, true);

        // Then it yields a Tab layout spanning the full width, not the chat layout.
        let AppFrameLayout::Tab(dash) = frame else {
            panic!("expected Tab layout for is_dashboard=true");
        };
        assert_eq!(dash.content.width, area.width);
        assert_eq!(dash.tab_bar, Rect::new(0, 0, 80, 1));
    }
}
