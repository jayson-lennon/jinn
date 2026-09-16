//! Audit popup overlay - renders the context-change history of the
//! currently-selected chat entry.
//!
//! Activated by pressing `a` in Normal mode (toggles
//! `FrontendState::audit_popup_visible`). The popup is right-aligned to the
//! chat-log area and vertically anchored to the top of the selected entry.
//! It tracks the cursor live as the user navigates.

use jinn_domain::RenderCtx;
use jinn_domain::common::focus::FocusScope;
use jinn_domain::feat::ui::chat_log::audit_popup::{audit_popup_rect, format_audit_lines};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Render the audit popup, if it should be visible.
///
/// Conditions for visibility:
/// - `audit_popup_visible` is true
/// - No higher-priority overlay is active
/// - An entry is selected
///
/// `chat_log_area` is the chat-log content area (right-aligned anchor and
// bottom-edge clamp).
pub(super) fn render_audit_popup(
    frame: &mut Frame<'_>,
    chat_log_area: Rect,
    ctx: &RenderCtx,
    rects: &mut Vec<Rect>,
) {
    if !ctx.state.frontend.audit_popup_visible {
        return;
    }

    // Suppress when a higher-priority overlay is active.
    if overlay_active(ctx) {
        return;
    }

    let Some(entry) = ctx.state.active_session().selected_entry() else {
        return;
    };

    let lines = format_audit_lines(entry, &ctx.state.frontend.theme);

    // Resolve the screen Y of the selected entry's top edge.
    // Returns None until the render pipeline has populated cached fields for
    // this frame; in that case we simply skip rendering for this frame.
    let Some(entry_top_y) = ctx
        .state
        .active_session()
        .selected_entry_screen_y(chat_log_area.y)
    else {
        return;
    };

    let rect = audit_popup_rect(chat_log_area, entry_top_y, lines.len());

    // Clear underlying buffer so the popup is opaque.
    frame.render_widget(Clear, rect);

    // Render the popup body. The block draws a full rounded rectangle around
    // the popup; the header line is the first body line (so it scrolls with content).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(
            Style::default()
                .bg(ctx.state.frontend.theme.infopopup_bg)
                .fg(ctx.state.frontend.theme.infopopup_border),
        );
    let paragraph = Paragraph::new(lines).style(
        Style::default()
            .bg(ctx.state.frontend.theme.infopopup_bg)
            .fg(ctx.state.frontend.theme.infopopup_fg),
    );
    frame.render_widget(paragraph.block(block), rect);

    rects.push(rect);
}

/// Returns true if a higher-priority overlay is currently active.
fn overlay_active(ctx: &RenderCtx) -> bool {
    match ctx.state.frontend.scope() {
        FocusScope::Picker { .. } | FocusScope::ArgInput | FocusScope::RenameSessionInput => true,
        FocusScope::Dynamic(id) => id.slice() == "sidebar" && id.name() == "sessions",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::manual_assert,
        clippy::panic,
        clippy::map_unwrap_or,
        clippy::redundant_closure_for_method_calls,
        clippy::collapsible_if,
        reason = "test code, panics are acceptable"
    )]
    //! Render-level tests for the audit popup overlay.
    //!
    //! These tests exercise the integration of:
    //! - `format_audit_lines` (domain pure function)
    //! - `selected_entry_screen_y` (session helper)
    //! - `audit_popup_rect` (rect computation)
    //! - The widget stack (Clear + Block + Paragraph)
    //! - The `rects.push()` registration for mouse-selectable regions.
    //!
    //! Together they pin the contract that the popup paints at the computed
    //! rect with the expected text and is registered as a selectable region.
    use jinn_domain::FocusScope;
    use jinn_domain::RenderCtx;
    use jinn_domain::feat::session::chat_entry::{ChangeSource, ChatEntry, ContextOverride};
    use jinn_domain::feat::ui::chat_log::audit_popup::AUDIT_POPUP_WIDTH;
    use jinn_testutil::setup_term;
    use ratatui::layout::Rect;

    use super::render_audit_popup;

    /// Build an app with one user entry that has one audit event, with the
    /// audit popup toggle ON.
    async fn app_with_audit_visible() -> crate::TuiApp {
        let app = crate::TuiApp::test_builder().build().await;
        let mut entry = ChatEntry::user("hello");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .audit_popup_visible = true;
        app.core
            .state
            .write_test_no_cap()
            .active_session_mut()
            .push_entry(entry);
        app
    }

    /// Returns the audit popup rect by probing `selectable_rects` for a
    /// rect of width `AUDIT_POPUP_WIDTH`. Returns `None` if not found.
    ///
    /// Probes the rightmost column of the chat-log area across all visible
    /// rows; the audit popup is right-aligned to the chat-log area so its
    /// right edge sits at `chat_log_area.x + chat_log_area.width - 1`. The
    /// first probe hit yields the popup rect.
    fn find_audit_popup_rect(app: &crate::TuiApp) -> Option<ratatui::layout::Rect> {
        // Scan every cell of a 80x24 frame and return the first rect of width
        // AUDIT_POPUP_WIDTH that we find. This is slower than a single probe but
        // robust to layout changes (e.g. sidebar width, content area offsets).
        for y in 0..24 {
            for x in 0..80 {
                if let Some(rect) = app.selectable_rects.find_for_position(x, y) {
                    if rect.width == AUDIT_POPUP_WIDTH {
                        return Some(rect);
                    }
                }
            }
        }
        None
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_audit_popup_paints_header_and_body_at_computed_rect() {
        // Given a state with audit visible and one excluded entry, with
        // pre-populated line ranges (simulating what render_chat_log does).
        let app = app_with_audit_visible().await;
        let (mut terminal, _area) = setup_term(120, 24);

        // Pre-populate the line-range cache that render_chat_log normally fills.
        // The selected entry occupies wrapped-line 0..=0 (one line).
        {
            let mut wstate = app.core.state.write_test_no_cap();
            let session = wstate.active_session_mut();
            session.set_entry_line_ranges(vec![(0, 0)]);
            session.set_rendered_scroll_offset(0);
            session.set_viewport_height(24);
            session.set_blank_count(0);
        }

        // The chat-log area we render against. Wide enough to fit the
        // 70-col popup with room to spare.
        let chat_log_area = Rect::new(30, 0, 70, 24);

        // When rendering the popup directly.
        let mut rects: Vec<Rect> = Vec::new();
        terminal
            .draw(|frame| {
                let guard = app.core.state.read();
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&guard, &slices, &views);
                render_audit_popup(frame, chat_log_area, &ctx, &mut rects);
            })
            .unwrap();

        // Then exactly one rect is registered, matching the popup width.
        assert_eq!(
            rects.len(),
            1,
            "exactly one popup rect should be registered"
        );
        let popup = rects[0];
        assert_eq!(popup.width, AUDIT_POPUP_WIDTH, "popup width");
        assert_eq!(
            popup.x + popup.width,
            chat_log_area.x + chat_log_area.width,
            "popup right edge should align to chat-log right edge"
        );

        // And the popup height accommodates Metadata section + audit section + 2 borders
        // (3 Metadata lines + 1 audit header + 1 audit body = 5 content lines + 2 borders = 7).
        assert_eq!(
            popup.height, 7,
            "popup height should be content + 2 borders"
        );

        // And the rendered buffer contains the Metadata title on the first body
        // line (popup.y + 1, since row 0 is the top border).
        let buffer = terminal.backend().buffer();
        let metadata_y = popup.y + 1;
        let metadata_row: String = (popup.x..popup.x + popup.width)
            .filter_map(|x| buffer.cell((x, metadata_y)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            metadata_row.contains("Metadata"),
            "metadata title row at y={metadata_y}: {metadata_row:?}"
        );

        // And the Sent line appears on the second body line (popup.y + 2).
        let sent_y = popup.y + 2;
        let sent_row: String = (popup.x..popup.x + popup.width)
            .filter_map(|x| buffer.cell((x, sent_y)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            sent_row.contains("Sent:"),
            "sent row at y={sent_y}: {sent_row:?}"
        );

        // And the rendered buffer contains the audit header text on the fourth
        // body line (popup.y + 4, after Metadata title + Sent + blank).
        let audit_header_y = popup.y + 4;
        let audit_header_row: String = (popup.x..popup.x + popup.width)
            .filter_map(|x| {
                buffer
                    .cell((x, audit_header_y))
                    .map(|c| c.symbol().to_owned())
            })
            .collect();
        assert!(
            audit_header_row.contains("audit")
                && audit_header_row.contains("1 events")
                && audit_header_row.contains("ForcedExclude"),
            "audit header row at y={audit_header_y}: {audit_header_row:?}"
        );

        // And the rendered buffer contains the event body text on the fifth
        // body line (popup.y + 5).
        let body_y = popup.y + 5;
        let body_row: String = (popup.x..popup.x + popup.width)
            .filter_map(|x| buffer.cell((x, body_y)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            body_row.contains("user") && body_row.contains("Default"),
            "body row at y={body_y}: {body_row:?}"
        );
    }

    /// Render the audit popup into an 80×24 terminal and return a snapshot
    /// of the rendered buffer together with the popup's screen rect.
    ///
    /// Mirrors the paint test's setup: one excluded entry, audit visible,
    /// pre-populated line ranges, popup rendered right-aligned in a 70-col
    /// chat-log area.
    async fn render_popup_buffer() -> (ratatui::buffer::Buffer, Rect) {
        let app = app_with_audit_visible().await;
        let (mut terminal, _area) = setup_term(100, 24);

        {
            let mut wstate = app.core.state.write_test_no_cap();
            let session = wstate.active_session_mut();
            session.set_entry_line_ranges(vec![(0, 0)]);
            session.set_rendered_scroll_offset(0);
            session.set_viewport_height(24);
            session.set_blank_count(0);
        }

        let chat_log_area = Rect::new(30, 0, 70, 24);

        let mut rects: Vec<Rect> = Vec::new();
        terminal
            .draw(|frame| {
                let guard = app.core.state.read();
                let slices = jinn_slices::Slices::new();
                let views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&guard, &slices, &views);
                render_audit_popup(frame, chat_log_area, &ctx, &mut rects);
            })
            .unwrap();

        let popup = rects
            .into_iter()
            .next()
            .expect("audit popup rect should be registered");
        (terminal.backend().buffer().clone(), popup)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_audit_popup_paints_vertical_borders_on_every_content_row() {
        let (buffer, popup) = render_popup_buffer().await;

        // Then every content row (strictly between the top and bottom border
        // rows) is bounded by the vertical border glyph on both edges.
        for y in (popup.y + 1)..(popup.y + popup.height - 1) {
            let left = buffer.cell((popup.x, y)).map(|c| c.symbol()).unwrap_or("");
            let right = buffer
                .cell((popup.x + popup.width - 1, y))
                .map(|c| c.symbol())
                .unwrap_or("");
            assert_eq!(left, "│", "missing left border at ({}, {})", popup.x, y);
            assert_eq!(
                right,
                "│",
                "missing right border at ({}, {})",
                popup.x + popup.width - 1,
                y
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_audit_popup_paints_four_rounded_corners() {
        // Given the audit popup rendered into a buffer.
        let (buffer, popup) = render_popup_buffer().await;

        let top = popup.y;
        let bottom = popup.y + popup.height - 1;
        let left = popup.x;
        let right = popup.x + popup.width - 1;

        // Then the four corners are the rounded border glyphs.
        assert_eq!(
            buffer.cell((left, top)).map(|c| c.symbol()),
            Some("╭"),
            "top-left corner"
        );
        assert_eq!(
            buffer.cell((right, top)).map(|c| c.symbol()),
            Some("╮"),
            "top-right corner"
        );
        assert_eq!(
            buffer.cell((left, bottom)).map(|c| c.symbol()),
            Some("╰"),
            "bottom-left corner"
        );
        assert_eq!(
            buffer.cell((right, bottom)).map(|c| c.symbol()),
            Some("╯"),
            "bottom-right corner"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_audit_popup_skips_when_visibility_off() {
        // Given a state with audit popup visibility OFF.
        let mut app = crate::TuiApp::test_builder().build().await;
        let mut entry = ChatEntry::user("hello");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        // audit_popup_visible stays at default (false)
        app.core
            .state
            .write_test_no_cap()
            .active_session_mut()
            .push_entry(entry);
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .unwrap();

        // Then no audit popup rect is registered (findable via probing).
        assert!(
            find_audit_popup_rect(&app).is_none(),
            "no audit popup rect should be registered when visibility is off"
        );

        // And the rendered buffer does not contain audit text anywhere.
        let buffer = terminal.backend().buffer();
        for y in 0..24 {
            for x in 0..80 {
                if let Some(cell) = buffer.cell((x, y)) {
                    let s = cell.symbol();
                    if s.contains("audit") && s.contains("events") {
                        panic!(
                            "audit text should not appear when popup is off (found at ({x}, {y}))"
                        );
                    }
                }
            }
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn render_audit_popup_skips_when_picker_overlay_active() {
        // Given a state with audit visible BUT a Picker overlay on top.
        let mut app = app_with_audit_visible().await;
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .scope_push(FocusScope::Picker {
                kind: jinn_domain::PickerKind::Provider,
            });
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .unwrap();

        // Then no audit popup rect was registered (suppressed by overlay).
        assert!(
            find_audit_popup_rect(&app).is_none(),
            "audit popup should be suppressed when Picker overlay is active"
        );
    }
}
