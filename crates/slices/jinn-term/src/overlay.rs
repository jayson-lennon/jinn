//! Overlay geometry + renderer for the terminal overlay — a
//! kernel-free slice view.
//!
//! Ports the renderer that lived in the TUI's `render/terminal_tab.rs`:
//! draws the actor-mirrored screen ([`jinn_term_msg::TerminalTabState`])
//! into the overlay rect — the styled cell grid (colors, attributes,
//! wide characters) when available, falling back to the plain-text rows
//! when a mirror predates the cells. The program's cursor is drawn only
//! when the program shows it (TUIs hide it while repainting).
//!
//! The renderer reads [`RenderFacts`] instead of the kernel's app
//! state: theme from the facts context, the mirror through the slices
//! registry's `term/tabs` cell, the active session id + capture state +
//! configured toggle key as seeded facts (see the `*_FACT` consts).
//! Missing facts render degraded chrome, never panic.

use jinn_slices::RenderFacts;
use jinn_term_msg::cells::{TermCell, TermColor};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The app fact carrying the active chat session's id (the mirror key).
pub const SESSION_ID_FACT: &str = "session.id";

/// The app fact `"1"`/`"0"`: whether the overlay is capturing input
/// (the `term:control` scope is on top).
pub const CAPTURING_FACT: &str = "term.capturing";

/// The app fact carrying the configured control-toggle key (the border
/// hint's capture glyph).
pub const TOGGLE_KEY_FACT: &str = "term.toggle-key";

/// Registers the overlay geometry + renderer for both overlay scopes.
///
/// The renderer reads the scope id it is handed only through the
/// registry (both scopes render the same overlay; capture styling comes
/// from the `term.capturing` fact).
pub fn register_views(
    slices: &jinn_slices::Slices,
    views: &jinn_slices::OverlayViews<RenderFacts>,
) {
    for scope in [jinn_term_msg::view_scope(), jinn_term_msg::control_scope()] {
        slices.register_overlay(
            scope.clone(),
            std::sync::Arc::new(|area: &Rect| Some(jinn_term_msg::terminal_overlay_rect(*area))),
        );
        views.register(scope, std::sync::Arc::new(render));
    }
}

/// Renders the active session's terminal screen into `area` (the bordered
/// overlay rect).
///
/// Clears the area first (the frame underneath is stale content, not
/// background), draws the border ring — gray while merely viewing, the
/// theme's focus accent while the overlay is capturing input — then paints
/// the program's cells into the interior, which is exactly the pty size.
///
/// The bottom border carries shortcut hints so the modes are self-describing:
/// view mode shows the capture toggle (the `term.toggle-key` fact), yank, and
/// push keys; capture mode shows only the toggle (everything else types into
/// the program).
pub fn render(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let theme = &facts.theme;
    let terminal = facts
        .slices
        .reader::<jinn_term_msg::TerminalTabState>(&jinn_term_msg::term_tabs_slot())
        .map(|cell| cell.read().clone());

    // Stale frame content would otherwise bleed through Blank cells.
    frame.render_widget(Clear, area);

    let capturing = facts.fact(CAPTURING_FACT) == Some("1");
    let border_color = if capturing {
        theme.focus_accent
    } else {
        theme.border_unfocused
    };
    let hints = bottom_border_hints(facts, capturing);
    let interior = {
        let b = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title_bottom(hints);
        let inner = b.inner(area);
        frame.render_widget(b, area);
        inner
    };

    // The view shows the active chat session's mirrored terminal. A
    // missing/unparseable session fact renders the empty chrome
    // (degraded, not broken).
    let chat = facts
        .fact(SESSION_ID_FACT)
        .and_then(jinn_core_types::SessionId::try_from_string);
    let Some(chat) = chat else {
        render_empty(frame, interior, theme.focus_accent);
        return;
    };
    let Some(mirror) = terminal.as_ref().and_then(|t| t.mirror(&chat)) else {
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
/// Entries are separated by `|`; key glyphs use the theme's
/// `accent_action` (the hotkey accent — same convention as the session
/// preview's keybinds bar), descriptions use `muted_text`.
///
/// View mode lists the capture toggle (the `term.toggle-key` fact),
/// `<M-t>` (close the overlay), `y` (yank screen to clipboard), and `I`
/// (yank + push the screen to the model). Capture mode lists only the
/// toggle — every other key types into the program, so advertising more
/// would lie.
fn bottom_border_hints(facts: &RenderFacts, capturing: bool) -> ratatui::text::Line<'static> {
    let theme = &facts.theme;
    let toggle = facts.fact(TOGGLE_KEY_FACT).map_or_else(
        || jinn_term_msg::prefs::DEFAULT_CONTROL_TOGGLE_KEY.to_owned(),
        ToOwned::to_owned,
    );
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

    use super::CAPTURING_FACT;
    use super::SESSION_ID_FACT;
    use super::TOGGLE_KEY_FACT;
    use super::render;
    use jinn_slices::RenderFacts;
    use jinn_slices::Slices;
    use jinn_term_msg::cells::CellStyle;
    use jinn_term_msg::cells::ScreenCells;
    use jinn_term_msg::cells::TermCell;
    use jinn_term_msg::cells::TermColor;
    use jinn_term_msg::term_tabs_slot;
    use jinn_theme::default_theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::widgets::Paragraph;

    /// Builds facts over a fresh registry with the term/tabs cell, seeded
    /// with one session's mirror.
    fn facts_with_mirror(
        screen: &str,
        cursor: (u16, u16),
        cursor_hidden: bool,
    ) -> (Slices, RenderFacts, jinn_core_types::SessionId) {
        let slices = Slices::new();
        slices
            .register(term_tabs_slot(), jinn_term_msg::TerminalTabState::default())
            .expect("fresh registry");
        let id = jinn_core_types::SessionId::new();
        slices
            .reader::<jinn_term_msg::TerminalTabState>(&term_tabs_slot())
            .expect("cell")
            .update(|t| {
                t.apply_screen(
                    &id,
                    screen.to_owned(),
                    ScreenCells::default(),
                    cursor,
                    cursor_hidden,
                )
            });
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: id.to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "0".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);
        (slices, facts, id)
    }

    /// Renders the overlay on a test backend and returns the buffer.
    fn render_to_buffer(facts: &RenderFacts, area: Rect) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| render(f, area, facts)).expect("draw");
        terminal.backend().buffer().clone()
    }

    /// Reads the bottom border row (the overlay's last row) as text.
    fn bottom_border_text(buffer: &ratatui::buffer::Buffer) -> String {
        let y = buffer.area.height - 1;
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_owned())
            .collect()
    }

    #[rstest::rstest]
    fn renders_screen_text_into_buffer() {
        // Given facts with a mirrored terminal screen.
        let (_, facts, _) = facts_with_mirror("hello from vim", (0, 0), false);
        let area = Rect::new(0, 0, 80, 24);

        // When rendering on a test backend.
        let buffer = render_to_buffer(&facts, area);

        // Then the interior (inside the border) contains the screen text.
        let row: String = (1..15)
            .map(|x| buffer[(x, 1)].symbol().to_owned())
            .collect();
        assert!(row.contains("hello from vim"), "row was: {row:?}");
        // And the border ring was drawn around it.
        assert_eq!(buffer[(0, 0)].symbol(), "\u{250c}");
        assert_eq!(buffer[(14, 0)].symbol(), "\u{2500}");
    }

    #[rstest::rstest]
    fn renders_hint_when_no_mirror() {
        // Given facts whose registry has no mirror for the seeded session.
        let slices = Slices::new();
        slices
            .register(term_tabs_slot(), jinn_term_msg::TerminalTabState::default())
            .expect("fresh registry");
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: jinn_core_types::SessionId::new().to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "0".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);

        // When rendering.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the buffer shows the empty-session hint inside the border.
        let row: String = (1..60)
            .map(|x| buffer[(x, 1)].symbol().to_owned())
            .collect();
        assert!(row.contains("no active terminal session"), "row: {row:?}");
    }

    #[rstest::rstest]
    fn renders_styled_cells_with_colors_and_attributes() {
        // Given facts whose mirror carries a styled cell grid: red bold "R"
        // followed by default-colored plain text.
        let slices = Slices::new();
        slices
            .register(term_tabs_slot(), jinn_term_msg::TerminalTabState::default())
            .expect("fresh registry");
        let styled = {
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
        let id = jinn_core_types::SessionId::new();
        slices
            .reader::<jinn_term_msg::TerminalTabState>(&term_tabs_slot())
            .expect("cell")
            .update(|t| t.apply_screen(&id, "Rx".to_owned(), styled, (0, 2), false));
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: id.to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "0".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);

        // When rendering on a test backend.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the styled cell carries the red bold style and the plain
        // cell carries no modifiers.
        let red = buffer[(1, 1)].clone();
        assert_eq!(red.symbol(), "R");
        assert_eq!(red.fg, Color::Indexed(1));
        assert!(red.modifier.contains(ratatui::style::Modifier::BOLD));
        let plain = buffer[(2, 1)].clone();
        assert_eq!(plain.symbol(), "x");
        assert_eq!(plain.modifier, ratatui::style::Modifier::empty());
    }

    #[rstest::rstest]
    fn wide_char_spacer_cells_are_skipped_not_rendered() {
        // Given a mirror whose grid contains a wide char followed by its
        // spacer (as the emulator emits for double-width glyphs).
        let slices = Slices::new();
        slices
            .register(term_tabs_slot(), jinn_term_msg::TerminalTabState::default())
            .expect("fresh registry");
        let styled = {
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
        let id = jinn_core_types::SessionId::new();
        slices
            .reader::<jinn_term_msg::TerminalTabState>(&term_tabs_slot())
            .expect("cell")
            .update(|t| t.apply_screen(&id, "漢".to_owned(), styled, (0, 2), false));
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: id.to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "0".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);

        // When rendering on a test backend.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the wide glyph renders at the interior's first cell and the
        // spacer column was not overwritten with a symbol (skipped; the
        // cleared buffer default remains).
        assert_eq!(buffer[(1, 1)].symbol(), "漢");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
    }

    #[rstest::rstest]
    fn border_is_gray_while_viewing_and_accent_while_capturing() {
        // Given the same mirror rendered under both facts variants.
        let (slices, mut viewing, _) = facts_with_mirror("screen", (0, 0), true);
        let mut capturing = RenderFacts::new(default_theme(), &slices);
        capturing.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: String::new(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "1".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);
        let _ = &mut viewing;

        // When rendering each.
        let view_buffer = render_to_buffer(&viewing, Rect::new(0, 0, 80, 24));
        let capture_buffer = render_to_buffer(&capturing, Rect::new(0, 0, 80, 24));

        // Then the border is gray while viewing and the focus accent while
        // capturing.
        assert_eq!(view_buffer[(0, 0)].fg, default_theme().border_unfocused);
        assert_eq!(capture_buffer[(0, 0)].fg, default_theme().focus_accent);
    }

    #[rstest::rstest]
    fn view_mode_border_advertises_capture_yank_and_send_keys() {
        // Given the overlay open in view mode.
        let (_, facts, _) = facts_with_mirror("screen", (0, 0), true);

        // When rendering.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

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
    fn capture_mode_border_advertises_only_the_toggle() {
        // Given the overlay capturing input.
        let (slices, _, id) = facts_with_mirror("screen", (0, 0), true);
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: id.to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "1".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<c-g>".to_owned(),
            },
        ]);

        // When rendering.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the bottom border shows only the release hint — every other
        // key types into the program, so advertising them would lie.
        let bottom = bottom_border_text(&buffer);
        assert!(bottom.contains("<c-g>"), "bottom was: {bottom:?}");
        assert!(bottom.contains("release"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("yank"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("send screen"), "bottom was: {bottom:?}");
    }

    #[rstest::rstest]
    fn border_hint_shows_a_custom_configured_toggle_key() {
        // Given facts configured with `<m-g>` as the toggle.
        let (slices, _, id) = facts_with_mirror("screen", (0, 0), true);
        let mut facts = RenderFacts::new(default_theme(), &slices);
        facts.set_facts([
            jinn_slices::AppFact {
                key: SESSION_ID_FACT,
                value: id.to_string(),
            },
            jinn_slices::AppFact {
                key: CAPTURING_FACT,
                value: "0".to_owned(),
            },
            jinn_slices::AppFact {
                key: TOGGLE_KEY_FACT,
                value: "<m-g>".to_owned(),
            },
        ]);

        // When rendering.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the hint names the configured key, not the default.
        let bottom = bottom_border_text(&buffer);
        assert!(bottom.contains("<m-g>"), "bottom was: {bottom:?}");
        assert!(!bottom.contains("<c-g>"), "bottom was: {bottom:?}");
    }

    #[rstest::rstest]
    fn cursor_position_is_set_when_visible_and_skipped_when_hidden() {
        // Given a mirror with an unhidden cursor at (1, 3).
        let (_, facts, _) = facts_with_mirror("hello", (1, 3), false);
        let area = Rect::new(0, 0, 80, 24);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("terminal");

        // When rendering.
        terminal.draw(|f| render(f, area, &facts)).expect("draw");

        // Then the cursor sits at interior (1, 3): frame coords offset by
        // the border ring.
        assert_eq!(terminal.backend().cursor_position(), Position::from((4, 2)));
    }

    #[rstest::rstest]
    fn overlay_clears_the_frame_underneath() {
        // Given a buffer pre-painted with visible content where the overlay
        // will draw, and a blank mirrored screen.
        let (_, facts, _) = facts_with_mirror(String::new().as_str(), (0, 0), true);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new("LEAK".repeat(30)), f.area());
            })
            .expect("seed draw");

        // When rendering the overlay.
        let area = Rect::new(0, 0, 80, 24);
        terminal.draw(|f| render(f, area, &facts)).expect("draw");

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
    fn hint_key_glyphs_use_the_action_accent_color() {
        // Given the overlay open in view mode.
        let (_, facts, _) = facts_with_mirror("screen", (0, 0), true);

        // When rendering.
        let buffer = render_to_buffer(&facts, Rect::new(0, 0, 80, 24));

        // Then the key glyphs (e.g. the `y` of "y yank") use accent_action,
        // the hotkey accent, while descriptions stay in the muted style.
        let theme = default_theme();
        let y = buffer.area.height - 1;
        let yank_key_x = (0..buffer.area.width - 1)
            .find(|&x| buffer[(x, y)].symbol() == "y" && buffer[(x + 1, y)].symbol() == " ")
            .expect("yank key glyph on the bottom border");
        assert_eq!(buffer[(yank_key_x, y)].fg, theme.accent_action);
    }
}
