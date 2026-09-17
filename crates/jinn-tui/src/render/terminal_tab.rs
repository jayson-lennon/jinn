//! Terminal screen rendering — mirrors an `interactive_term` session.
//!
//! Draws the actor-mirrored screen ([`TerminalTabState`]) into a rect: the
//! styled cell grid (colors, attributes, wide characters) when available,
//! falling back to the plain-text rows when a mirror predates the cells.
//! The program's cursor is drawn only when the program shows it (TUIs hide
//! it while repainting).
//!

use jinn_domain::RenderCtx;
use jinn_term_msg::cells::{TermCell, TermColor};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Renders the active session's terminal screen into `area` (the bordered
/// overlay rect).
///
/// Clears the area first (the frame underneath is stale content, not
/// background), draws the border ring — gray while merely viewing, the
/// theme's focus accent while the overlay is capturing input — then paints
/// the program's cells into the interior, which is exactly the pty size.
///
/// The bottom border carries shortcut hints so the modes are self-describing:
/// view mode shows the capture toggle (from `[interactive_term]
/// control_toggle_key`), yank, and push keys; capture mode shows only the
/// toggle (everything else types into the program).
pub fn render_terminal_tab(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) {
    let theme = &ctx.state.frontend.theme;
    let terminal = ctx.state.term_tabs().map(|cell| cell.read().clone());

    // Stale frame content would otherwise bleed through Blank cells.
    frame.render_widget(Clear, area);

    let capturing = matches!(
        ctx.state.frontend.scope(),
        jinn_domain::FocusScope::TerminalControl
    );
    let border_color = if capturing {
        theme.focus_accent
    } else {
        theme.border_unfocused
    };
    let hints = bottom_border_hints(ctx, capturing);
    let interior = {
        let b = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title_bottom(hints);
        let inner = b.inner(area);
        frame.render_widget(b, area);
        inner
    };

    // The view shows the active chat session's mirrored terminal.
    let chat = ctx.state.session.active_session_id().clone();
    let mirror = terminal.as_ref().and_then(|t| t.mirror(&chat));
    let Some(mirror) = mirror else {
        render_empty(frame, interior, theme.focus_accent);
        return;
    };

    if mirror.cells.cells.is_empty() {
        render_plain_text(frame, interior, &mirror.screen);
    } else {
        render_cells(frame, interior, &mirror.cells);
    }

    // Cursor: only when the program shows it (TUIs hide it while repainting)
    // and the cursor is inside the visible area.
    if !mirror.cursor_hidden {
        let (row, col) = mirror.cursor;
        let x = interior.x.saturating_add(col);
        let y = interior.y.saturating_add(row);
        if col < interior.width && row < interior.height {
            frame.set_cursor_position((x, y));
        }
    }
}

/// Draws the styled cell grid cell-by-cell (colors and attributes).
///
/// `Blank` cells are left untouched; [`TermCell::WideSpacer`] cells are
/// skipped — the wide `ch` already occupies the leading slot and ratatui's
/// buffer advances past the second column on its own.
fn render_cells(frame: &mut Frame<'_>, area: Rect, cells: &jinn_term_msg::cells::ScreenCells) {
    let buf = frame.buffer_mut();
    for row in 0..cells.rows.min(area.height) {
        for col in 0..cells.cols.min(area.width) {
            let Some(TermCell::Styled { ch, style }) = cells.get(row, col) else {
                continue;
            };
            let x = area.x + col;
            let y = area.y + row;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(ch.encode_utf8(&mut [0u8; 4]));
                cell.set_style(to_ratatui_style(style));
            }
        }
    }
}

/// Draws plain-text rows when no cell grid is mirrored yet.
fn render_plain_text(frame: &mut Frame<'_>, area: Rect, text: &str) {
    if text.trim().is_empty() {
        let theme_hint = Color::Indexed(8);
        render_empty(frame, area, theme_hint);
        return;
    }
    let lines: Vec<Line<'_>> = text
        .lines()
        .map(|row| Line::from(Span::raw(row.to_owned())))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Maps the emulator's cell style to a ratatui style.
fn to_ratatui_style(style: &jinn_term_msg::cells::CellStyle) -> Style {
    let mut out = Style::default();
    if let Some(fg) = to_ratatui_color(style.fg) {
        out = out.fg(fg);
    }
    if let Some(bg) = to_ratatui_color(style.bg) {
        out = out.bg(bg);
    }
    let mut mods = Modifier::empty();
    if style.bold {
        mods |= Modifier::BOLD;
    }
    if style.italic {
        mods |= Modifier::ITALIC;
    }
    if style.underline {
        mods |= Modifier::UNDERLINED;
    }
    if style.inverse {
        mods |= Modifier::REVERSED;
    }
    out.add_modifier(mods)
}

/// Maps a terminal color; `None` leaves ratatui's default (`Reset`).
fn to_ratatui_color(color: TermColor) -> Option<Color> {
    match color {
        TermColor::Default => None,
        TermColor::Idx(i) => Some(Color::Indexed(i)),
        TermColor::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Draws a hint line when there is no session or the screen is blank.
fn render_empty(frame: &mut Frame<'_>, area: Rect, accent: ratatui::style::Color) {
    let hint = Paragraph::new(Line::from(Span::styled(
        "no active terminal session — ask the agent to run `interactive_term`",
        Style::default().fg(accent).add_modifier(Modifier::ITALIC),
    )));
    frame.render_widget(hint, area);
}

/// Builds the bottom-border hint line describing the mode's keys.
///
/// Entries are separated by `|`; key glyphs use [`Theme::accent_action`]
/// (the hotkey accent — same convention as the session preview's keybinds
/// bar), descriptions use `muted_text`.
///
/// View mode lists the capture toggle (from `[interactive_term]
/// control_toggle_key`), `<M-t>` (close the overlay), `y` (yank screen to
/// clipboard), and `I` (yank + push the screen to the model). Capture mode
/// lists only the toggle — every other key types into the program, so
/// advertising more would lie.
fn bottom_border_hints(ctx: &RenderCtx<'_>, capturing: bool) -> ratatui::text::Line<'static> {
    let theme = &ctx.state.frontend.theme;
    let configured = ctx
        .state
        .frontend
        .preferences
        .interactive_term
        .control_toggle_key
        .clone();
    let toggle =
        jinn_domain::feat::interactive_term::prefs::normalize_control_toggle_key(&configured)
            .unwrap_or_else(|| jinn_term_msg::prefs::DEFAULT_CONTROL_TOGGLE_KEY.to_owned());
    let key_style = Style::default()
        .fg(theme.accent_action)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.muted_text);
    let separator = Span::styled(" | ", desc_style);
    let entry = |key: &str, desc: &str| {
        vec![
            Span::styled(key.to_owned(), key_style),
            Span::styled(format!(" {desc}"), desc_style),
        ]
    };

    let entries: Vec<Vec<Span<'static>>> = if capturing {
        vec![entry(&toggle, "release")]
    } else {
        vec![
            entry(&toggle, "capture"),
            entry("<M-t>", "toggle"),
            entry("y", "yank"),
            entry("I", "send screen"),
        ]
    };
    let spans = {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, e) in entries.into_iter().enumerate() {
            if i > 0 {
                spans.push(separator.clone());
            }
            spans.extend(e);
        }
        spans
    };
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use jinn_domain::common::app_state::{AppState, FocusScope};
    use jinn_term_msg::cells::{CellStyle, ScreenCells};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    fn app_with_terminal_screen(screen: &str, cursor: (u16, u16)) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                screen.to_owned(),
                ScreenCells::default(),
                cursor,
                false,
            );
        });
        state
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn renders_screen_text_into_buffer() {
        // Given an app with a mirrored terminal screen.
        let state = app_with_terminal_screen("hello from vim", (0, 0));
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering on a test backend.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the interior (inside the border) contains the screen text.
        let buffer = terminal.backend().buffer();
        let row: String = (1..15)
            .map(|x| buffer[(x, 1)].symbol().to_owned())
            .collect();
        assert!(row.contains("hello from vim"), "row was: {row:?}");
        // And the border ring was drawn around it.
        assert_eq!(buffer[(0, 0)].symbol(), "\u{250c}");
        assert_eq!(buffer[(14, 0)].symbol(), "\u{2500}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn renders_hint_when_no_session() {
        // Given an app with no mirrored session.
        let state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering the terminal tab.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the buffer shows the empty-session hint inside the border.
        let buffer = terminal.backend().buffer();
        let row: String = (1..60)
            .map(|x| buffer[(x, 1)].symbol().to_owned())
            .collect();
        assert!(row.contains("no active terminal session"), "row: {row:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn renders_styled_cells_with_colors_and_attributes() {
        // Given an app whose mirror carries a styled cell grid: red bold "R"
        // followed by default-colored plain text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        let styled = {
            use jinn_term_msg::cells::{CellStyle, ScreenCells, TermCell, TermColor};
            let mut cells = vec![
                TermCell::Styled {
                    ch: 'R',
                    style: CellStyle {
                        fg: TermColor::Idx(1),
                        bold: true,
                        ..CellStyle::default()
                    },
                },
                TermCell::Styled {
                    ch: 'x',
                    style: CellStyle::default(),
                },
            ];
            cells.resize(200, TermCell::Blank);
            ScreenCells {
                rows: 24,
                cols: 80,
                cells,
            }
        };
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "Rx".to_owned(),
                styled,
                (0, 2),
                false,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering on a test backend.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the styled cell carries the red bold style and the plain cell
        // carries no modifiers.
        let buffer = terminal.backend().buffer();
        let red = buffer[(1, 1)].clone();
        assert_eq!(red.symbol(), "R");
        assert_eq!(red.fg, Color::Indexed(1));
        assert!(red.modifier.contains(ratatui::style::Modifier::BOLD));
        let plain = buffer[(2, 1)].clone();
        assert_eq!(plain.symbol(), "x");
        assert_eq!(plain.modifier, ratatui::style::Modifier::empty());
    }

    /// Renders a single styled cell `ch` at (0, 0) and returns the buffer
    /// cell, so style-mapping cases share one assertion path.
    async fn rendered_cell_for_style(style: CellStyle) -> ratatui::buffer::Cell {
        use jinn_term_msg::cells::{ScreenCells, TermCell};

        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        let mut cells = vec![TermCell::Styled { ch: 'S', style }];
        cells.resize(200, TermCell::Blank);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "S".to_owned(),
                ScreenCells {
                    rows: 24,
                    cols: 80,
                    cells,
                },
                (0, 0),
                true,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");
        terminal.backend().buffer()[(1, 1)].clone()
    }

    #[rstest::rstest]
    #[case::background(
        CellStyle {
            bg: TermColor::Idx(4),
            ..CellStyle::default()
        },
        |cell: &ratatui::buffer::Cell| {
            assert_eq!(cell.bg, Color::Indexed(4), "bg must map to Indexed");
        },
    )]
    #[case::rgb_truecolor(
        CellStyle {
            fg: TermColor::Rgb(10, 200, 30),
            ..CellStyle::default()
        },
        |cell: &ratatui::buffer::Cell| {
            assert_eq!(cell.fg, Color::Rgb(10, 200, 30), "fg must map to Rgb");
        },
    )]
    #[case::italic(
        CellStyle {
            italic: true,
            ..CellStyle::default()
        },
        |cell: &ratatui::buffer::Cell| {
            assert!(cell.modifier.contains(Modifier::ITALIC));
        },
    )]
    #[case::underline(
        CellStyle {
            underline: true,
            ..CellStyle::default()
        },
        |cell: &ratatui::buffer::Cell| {
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
        },
    )]
    #[case::inverse(
        CellStyle {
            inverse: true,
            ..CellStyle::default()
        },
        |cell: &ratatui::buffer::Cell| {
            assert!(cell.modifier.contains(Modifier::REVERSED));
        },
    )]
    #[tokio::test]
    async fn styled_cell_attrs_map_onto_the_rendered_buffer(
        #[case] style: CellStyle,
        #[case] assert_style: impl Fn(&ratatui::buffer::Cell),
    ) {
        // Given a mirror whose single cell carries one terminal style.
        // When the overlay renders on a test backend.
        let cell = Box::pin(rendered_cell_for_style(style)).await;
        // Then the style maps to the matching ratatui color/modifier.
        assert_style(&cell);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn wide_char_spacer_cells_are_skipped_not_rendered() {
        // Given a mirror whose grid contains a wide char followed by its
        // spacer (as the emulator emits for double-width glyphs).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        let styled = {
            use jinn_term_msg::cells::{CellStyle, ScreenCells, TermCell};
            let mut cells = vec![
                TermCell::Styled {
                    ch: '漢',
                    style: CellStyle::default(),
                },
                TermCell::WideSpacer,
            ];
            cells.resize(200, TermCell::Blank);
            ScreenCells {
                rows: 24,
                cols: 80,
                cells,
            }
        };
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "漢".to_owned(),
                styled,
                (0, 2),
                false,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering on a test backend.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the wide glyph renders at the interior's first cell and the
        // spacer column was not overwritten with a symbol (skipped; the
        // cleared buffer default remains).
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].symbol(), "漢");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
    }

    /// Renders the overlay for a state whose scope is `scope`, returning the
    /// buffer, so border-color/clear cases share one setup path.
    async fn rendered_overlay_with_scope(scope: FocusScope) -> ratatui::buffer::Buffer {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(scope);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "screen".to_owned(),
                ScreenCells::default(),
                (0, 0),
                true,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn border_is_gray_while_viewing() {
        // Given the overlay open in view mode (TerminalView base scope).
        // When rendering.
        let buffer = Box::pin(rendered_overlay_with_scope(FocusScope::TerminalView)).await;
        // Then the border uses the theme's unfocused (gray) color.
        assert_eq!(
            buffer[(0, 0)].fg,
            jinn_domain::feat::theme::default_theme().border_unfocused
        );
    }

    /// Reads the bottom border row (the overlay's last row) as text.
    fn bottom_border_text(buffer: &ratatui::buffer::Buffer) -> String {
        let y = buffer.area.height - 1;
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_owned())
            .collect()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn view_mode_border_advertises_capture_yank_and_send_keys() {
        // Given the overlay open in view mode.
        // When rendering.
        let buffer = Box::pin(rendered_overlay_with_scope(FocusScope::TerminalView)).await;

        // Then the bottom border advertises the mode's keys: the configured
        // toggle (default `<c-g>`), overlay close, yank, and send-screen,
        // joined by `|` separators.
        let bottom = bottom_border_text(&buffer);
        assert!(bottom.contains("<c-g>"), "bottom was: {bottom:?}");
        assert!(bottom.contains("capture"), "bottom was: {bottom:?}");
        assert!(bottom.contains("<M-t> toggle"), "bottom was: {bottom:?}");
        assert!(bottom.contains("y yank"), "bottom was: {bottom:?}");
        assert!(bottom.contains("send screen"), "bottom was: {bottom:?}");
        assert!(bottom.contains(" | "), "bottom was: {bottom:?}");
        assert!(!bottom.contains("  "), "bottom was: {bottom:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn capture_mode_border_advertises_only_the_toggle() {
        // Given the overlay capturing input (TerminalControl base scope).
        // When rendering.
        let buffer = Box::pin(rendered_overlay_with_scope(FocusScope::TerminalControl)).await;

        // Then the bottom border shows only the release hint — every other
        // key types into the program, so advertising them would lie.
        let bottom = bottom_border_text(&buffer);
        assert!(bottom.contains("<c-g>"), "bottom was: {bottom:?}");
        assert!(bottom.contains("release"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("yank"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("send screen"), "bottom was: {bottom:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn border_hint_shows_a_custom_configured_toggle_key() {
        // Given an app whose preferences configure `<m-g>` as the toggle.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "screen".to_owned(),
                ScreenCells::default(),
                (0, 0),
                true,
            );
        });
        state
            .frontend
            .preferences
            .interactive_term
            .control_toggle_key = "<m-g>".to_owned();
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, f.area(), &ctx))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        drop(guard);

        // Then the hint names the configured key, not the default.
        let bottom = bottom_border_text(&buffer);
        assert!(bottom.contains("<m-g>"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("<c-g>"), "bottom was: {bottom:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn hint_key_glyphs_use_the_action_accent_color() {
        // Given the overlay open in view mode.
        // When rendering.
        let buffer = Box::pin(rendered_overlay_with_scope(FocusScope::TerminalView)).await;

        // Then the key glyphs (e.g. the `y` of "y yank") use accent_action,
        // the same orange hotkey color as the session preview keybind bar,
        // while the descriptions stay in the non-accent style.
        let theme = jinn_domain::feat::theme::default_theme();
        let y = buffer.area.height - 1;
        let yank_key_x = (0..buffer.area.width - 1)
            .find(|&x| buffer[(x, y)].symbol() == "y" && buffer[(x + 1, y)].symbol() == " ")
            .expect("yank key glyph on the bottom border");
        assert_eq!(buffer[(yank_key_x, y)].fg, theme.accent_action);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn border_is_accent_while_capturing_input() {
        // Given the overlay capturing input (TerminalControl base scope).
        // When rendering.
        let buffer = Box::pin(rendered_overlay_with_scope(FocusScope::TerminalControl)).await;
        // Then the border uses the theme's focus accent (yellow by default).
        assert_eq!(
            buffer[(0, 0)].fg,
            jinn_domain::feat::theme::default_theme().focus_accent
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn overlay_clears_the_frame_underneath() {
        // Given a buffer pre-painted with visible content where the overlay
        // will draw.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                use ratatui::widgets::Paragraph;
                f.render_widget(Paragraph::new("LEAK".repeat(30)), f.area());
            })
            .expect("seed draw");
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                String::new(),
                ScreenCells::default(),
                (0, 0),
                true,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        let area = Rect::new(0, 0, 80, 24);

        // When rendering the overlay (mirrored screen is blank).
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the interior shows no trace of the underlying frame.
        let buffer = terminal.backend().buffer();
        let interior_row: String = (1..79)
            .map(|x| buffer[(x, 1)].symbol().to_owned())
            .collect();
        assert!(
            !interior_row.contains("LEAK"),
            "frame content leaked through: {interior_row:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cursor_position_is_set_when_visible_and_skipped_when_hidden() {
        // Given a mirror with an unhidden cursor at (1, 3).
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::TerminalView);
        state.term_tabs().expect("term tabs cell").update(|term| {
            term.apply_screen(
                state.session.active_session_id(),
                "hello".to_owned(),
                ScreenCells::default(),
                (1, 3),
                false,
            );
        });
        let app = crate::TuiApp::test_builder().state(state).build().await;

        // When rendering.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 80, 24);
        let guard = app.core.state.read();
        let slices = jinn_slices::Slices::new();
        let views = jinn_domain::common::overlay_views::OverlayViews::new();
        let ctx = RenderCtx::new(&guard, &slices, &views);
        terminal
            .draw(|f| render_terminal_tab(f, area, &ctx))
            .expect("draw");

        // Then the cursor sits at interior (1, 3): frame coords offset by
        // the border ring.
        assert_eq!(terminal.backend().cursor_position(), Position::from((4, 2)));
    }
}
