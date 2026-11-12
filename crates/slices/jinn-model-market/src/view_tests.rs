//! Tests for the popup view's own render behaviors: the `?` help card
//! appearing with the popup's draw pass, and the MODEL column's dimmed
//! variant suffix — the card calls the suffix a dimmed one, so the
//! table must render one.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_slices::Slices;
use jinn_slices::cell::TypedCell;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::widgets::Paragraph;

/// A registered pair of cells over a fresh registry, exactly as the
/// composition catalog seeds them — the render pass resolves readers
/// through the facts context.
struct Harness {
    slices: Slices,
    ui: TypedCell<MarketUiState>,
    data: TypedCell<MarketDataState>,
}

impl Harness {
    fn new() -> Self {
        let slices = Slices::new();
        let ui = slices
            .register(crate::market_ui_slot(), MarketUiState::default())
            .expect("fresh registry has no UI slot yet");
        let data = slices
            .register(crate::market_data_slot(), MarketDataState::default())
            .expect("fresh registry has no data slot yet");
        Self { slices, ui, data }
    }

    /// Renders one frame at the given popup rect against a test backend
    /// and returns the buffer.
    fn render(&self, popup: ratatui::layout::Rect, width: u16, height: u16) -> Buffer {
        self.render_seeded(popup, width, height, |_facts| {})
    }

    /// Renders one frame with a hook for seeding render facts (the
    /// usage numbers the mix resolution reads).
    fn render_seeded(
        &self,
        popup: ratatui::layout::Rect,
        width: u16,
        height: u16,
        seed: impl FnOnce(&mut jinn_slices::RenderFacts),
    ) -> Buffer {
        let theme = jinn_theme::default_theme();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("test terminal");
        terminal
            .draw(|frame| {
                let mut facts = jinn_slices::RenderFacts::new(theme.clone(), &self.slices);
                seed(&mut facts);
                crate::view::render_market_popup(frame, popup, &facts);
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }
}

/// One listing with a variant, dimmed-suffix territory.
fn variant_row() -> MarketRow {
    MarketRow {
        id: "openrouter/z/mod:free".to_owned(),
        author: "z".to_owned(),
        slug: "mod".to_owned(),
        variant: Some("free".to_owned()),
        prompt_price: 1e-6,
        completion_price: 2e-6,
        cache_read_price: None,
        has_overrides: false,
        context_length: Some(128_000),
        eci: None,
        eci_ci: None,
        is_free: true,
    }
}

/// One plain listing without a variant — table filler that keeps the
/// variant row unselected (the selection row's style overrides the
/// cell foreground, hiding the dim). Priced cheaper so the default
/// PROJ-ascending sort keeps the variant row off the selection.
fn plain_row() -> MarketRow {
    MarketRow {
        id: "openrouter/a/plain".to_owned(),
        author: "a".to_owned(),
        slug: "plain".to_owned(),
        variant: None,
        prompt_price: 1e-9,
        completion_price: 1e-9,
        cache_read_price: None,
        has_overrides: false,
        context_length: Some(128_000),
        eci: None,
        eci_ci: None,
        is_free: false,
    }
}

/// The buffer's full text, cells joined.
fn buffer_text(buffer: &Buffer) -> String {
    buffer.content.iter().map(Cell::symbol).collect()
}

/// A rendered table row's text, cells joined across the popup's width.
fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .collect()
}

#[rstest::rstest]
fn hidden_help_draws_no_card_pixels() {
    // Given a wired popup with help hidden (the opener default).
    let harness = Harness::new();
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);

    // When the popup renders over a tall terminal.
    let buffer = harness.render(popup, 90, 100);

    // Then no help card text is anywhere on the screen.
    let text = buffer_text(&buffer);
    assert!(
        !text.contains("OpenRouter Model Market help"),
        "a hidden help must not draw, got: {text:?}"
    );
}

#[rstest::rstest]
fn table_mode_renders_no_em_dash() {
    // Given a wired popup whose data carries an empty-cell placeholder
    // (a None ECI renders the plain-hyphen EMPTY_CELL glyph).
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = vec![variant_row(), plain_row()];
    });

    // When the popup renders in table mode.
    let buffer = harness.render(ratatui::layout::Rect::new(5, 2, 80, 10), 90, 100);

    // Then no em-dash is on screen — the empty-cell glyph is the plain
    // hyphen, and nothing else the table draws uses the character.
    let text = buffer_text(&buffer);
    assert!(
        !text.contains('\u{2014}'),
        "the popup displays no em-dash, got: {text:?}"
    );
}

#[rstest::rstest]
fn detail_mode_renders_no_em_dash() {
    // Given a wired popup pinned to a model whose detail carries an
    // endpoint row with every optional field absent (all the `-`
    // placeholders render).
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = vec![plain_row()];
        d.detail = Some(jinn_model_market_msg::MarketDetail {
            model_id: "openrouter/a/plain".to_owned(),
            endpoints: vec![jinn_model_market_msg::MarketEndpointRow {
                tag: "free".to_owned(),
                provider_name: "Chutes".to_owned(),
                prompt_price: 0.0,
                completion_price: 0.0,
                cache_read_price: None,
                latency_p50_ms: None,
                throughput_p50: None,
                uptime_1d: None,
                quantization: None,
                overrides: Vec::new(),
            }],
        });
        d.endpoints_fetched_at = Some(jiff::Timestamp::from_second(0).expect("epoch"));
    });
    harness.ui.update(|s| {
        s.detail_model = Some("openrouter/a/plain".to_owned());
    });

    // When the popup renders in detail mode.
    let buffer = harness.render(ratatui::layout::Rect::new(5, 2, 80, 10), 90, 100);

    // Then no em-dash is on screen — every absent optional renders the
    // plain-hyphen placeholder.
    let text = buffer_text(&buffer);
    assert!(
        !text.contains('\u{2014}'),
        "the popup displays no em-dash, got: {text:?}"
    );
}

#[rstest::rstest]
fn visible_help_draws_the_card_below_the_popup() {
    // Given a wired popup with the help toggled on.
    let harness = Harness::new();
    harness.ui.update(|s| s.help_visible = true);
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);

    // When the popup renders over a terminal with room below it.
    let buffer = harness.render(popup, 90, 100);

    // Then the card is on screen directly below the popup's bottom
    // border: its top border flush with the popup's edge, its title
    // one row inside that.
    let title_row = row_text(&buffer, popup.y + popup.height + 1, popup.width);
    assert!(
        title_row.contains("OpenRouter Model Market help"),
        "the card draws below the popup, got: {title_row:?}"
    );
}

#[rstest::rstest]
fn visible_help_over_a_full_height_popup_still_reaches_the_screen() {
    // Given the standard centered popup (no room below it) with the
    // help toggled on.
    let harness = Harness::new();
    harness.ui.update(|s| s.help_visible = true);
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = crate::view::market_overlay_rect(&area).expect("frame fits the popup");

    // When the popup renders.
    let buffer = harness.render(popup, 120, 40);

    // Then the card's fallback placement still puts its text on screen.
    let text = buffer_text(&buffer);
    assert!(
        text.contains("OpenRouter Model Market help"),
        "the card falls back to the terminal top, got: {text:?}"
    );
}

#[rstest::rstest]
fn model_column_dims_the_variant_suffix() {
    // Given a popup with a variant listing below the selected row.
    let harness = Harness::new();
    harness.ui.update(|s| s.selection = 0);
    harness.data.update(|d| {
        d.rows = vec![plain_row(), variant_row()];
    });
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);

    // When the popup renders.
    let buffer = harness.render(popup, 90, 100);

    // Then the suffix glyphs wear the muted foreground while the base
    // name does not: the card's "dimmed :variant" is what the table
    // shows. The suffix cell is found by scanning the row's buffer
    // cells for the variant's colon — string indexes and buffer
    // positions need not agree for that. Data rows start below the
    // filter line, its separator, and the header.
    let theme = jinn_theme::default_theme();
    let name = "z/mod:free";
    let y = (popup.y + 4..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains(name))
        .expect("the variant row renders");
    let colon_x = (0..popup.width)
        .find(|x| buffer.cell((*x, y)).is_some_and(|c| c.symbol() == ":"))
        .expect("the variant suffix renders");
    let suffix_cell = buffer.cell((colon_x, y)).expect("suffix cell exists");
    assert_eq!(
        suffix_cell.style().fg,
        Some(theme.muted_text),
        "the :variant suffix renders dimmed"
    );
    let base_cell = buffer.cell((colon_x - 1, y)).expect("base cell exists");
    assert_ne!(
        base_cell.style().fg,
        Some(theme.muted_text),
        "the author/slug base stays undimmed"
    );
}

#[rstest::rstest]
fn filter_match_on_a_suffix_glyph_highlights_over_the_dim() {
    // Given a popup filtered to the variant text itself.
    let harness = Harness::new();
    harness.ui.update(|s| s.filter = "free".to_owned());
    harness.data.update(|d| {
        d.rows = vec![variant_row()];
    });
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);

    // When the popup renders.
    let buffer = harness.render(popup, 90, 100);

    // Then the matched suffix glyphs carry the shared highlight
    // background — the dim yields to the filter's highlight, as it
    // does for the base name. Data rows start below the filter line,
    // its separator, and the header.
    let theme = jinn_theme::default_theme();
    let y = (popup.y + 4..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("z/mod:free"))
        .expect("the filtered row renders");
    let highlighted: String = (0..popup.width)
        .filter(|x| {
            buffer
                .cell((*x, y))
                .is_some_and(|c| c.style().bg == Some(theme.picker_highlight_bg))
        })
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .collect();
    assert_eq!(highlighted, "free", "the suffix match highlights");
}

#[rstest::rstest]
fn the_popup_paints_an_opaque_surface_over_what_is_behind_it() {
    // Given a frame with filler text covering the whole terminal. The
    // filler glyph `~` cannot occur in the popup's own text (prices,
    // stamps, the mix display, and key names never render one).
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = vec![plain_row()];
    });
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);

    // When the frame draws the filler first and the popup over it.
    let theme = jinn_theme::default_theme();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 100)).expect("terminal");
    terminal
        .draw(|frame| {
            let filler = ratatui::text::Line::from("~".repeat(90 * 100));
            for y in 0..100u16 {
                frame.render_widget(
                    Paragraph::new(filler.clone()),
                    ratatui::layout::Rect::new(0, y, 90, 1),
                );
            }
            let facts = jinn_slices::RenderFacts::new(theme.clone(), &harness.slices);
            crate::view::render_market_popup(frame, popup, &facts);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();

    // Then no filler glyph survives anywhere inside the popup rect —
    // the surface is opaque.
    for y in popup.y..popup.y + popup.height {
        let bleeds = (popup.x..popup.x + popup.width)
            .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
            .any(|sym| sym.contains('~'));
        assert!(!bleeds, "filler bleeds through at y={y}");
    }
}

#[rstest::rstest]
fn focused_filter_renders_its_own_line_and_the_title_stays_plain() {
    // Given a popup with the filter focused and "son" typed.
    let harness = Harness::new();
    harness.ui.update(|s| {
        s.filter_focused = true;
        s.filter = "son".to_owned();
    });

    // When the popup renders.
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);
    let buffer = harness.render(popup, 90, 100);

    // Then the filter line sits at the inner area's first row with the
    // glyph, the text, and the cursor block, the separator sits under
    // it, and the title row is the popup's plain name — the filter
    // never renders into the titlebar.
    let title = row_text(&buffer, popup.y, popup.width);
    assert!(
        title.contains(" OpenRouter Model Market "),
        "the title stays plain, got: {title:?}"
    );
    assert!(
        !title.contains('>'),
        "the filter never renders into the titlebar"
    );
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);
    assert!(
        filter_line.contains(" > son") && filter_line.contains('\u{2588}'),
        "the focused filter line shows the glyph, text, and cursor, got: {filter_line:?}"
    );
    let separator: String = (popup.x + 1..popup.x + popup.width - 1)
        .filter_map(|x| buffer.cell((x, popup.y + 2)).map(Cell::symbol))
        .collect();
    let trimmed = separator.trim();
    assert!(
        !trimmed.is_empty() && trimmed.chars().all(|c| c == '\u{2500}'),
        "a separator of dashes sits under the filter line, got: {separator:?}"
    );
}

#[rstest::rstest]
fn unfocused_filter_shows_its_text_without_the_cursor() {
    // Given a popup holding filter text with the table focused.
    let harness = Harness::new();
    harness.ui.update(|s| {
        s.filter = "son".to_owned();
    });

    // When the popup renders.
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);
    let buffer = harness.render(popup, 90, 100);

    // Then the filter line still shows the glyph and text — the filter
    // stays applied — but no cursor block, and the title stays plain.
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);
    assert!(
        filter_line.contains(" > son"),
        "the applied filter text renders after the glyph, got: {filter_line:?}"
    );
    assert!(
        !filter_line.contains('\u{2588}'),
        "an unfocused filter shows no cursor, got: {filter_line:?}"
    );
}

#[rstest::rstest]
fn the_filter_line_renders_the_glyph_alone_when_empty() {
    // Given a popup with an empty, focused filter.
    let harness = Harness::new();
    harness.ui.update(|s| s.filter_focused = true);

    // When the popup renders.
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);
    let buffer = harness.render(popup, 90, 100);

    // Then the filter line reads the prompt glyph with just the cursor.
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);
    assert!(
        filter_line.contains(" > \u{2588}"),
        "the empty focused filter reads the glyph and cursor, got: {filter_line:?}"
    );
}

#[rstest::rstest]
fn the_filter_line_renders_the_glyph_in_detail_mode_too() {
    // Given detail mode holding filter text, filter unfocused.
    let harness = Harness::new();
    harness.ui.update(|s| {
        s.filter = "son".to_owned();
        s.enter_detail("openrouter/a/model".to_owned());
    });
    harness.data.update(|d| {
        d.rows = vec![variant_row()];
    });

    // When the popup renders.
    let popup = ratatui::layout::Rect::new(5, 2, 80, 10);
    let buffer = harness.render(popup, 90, 100);
    let title = row_text(&buffer, popup.y, popup.width);

    // Then the filter line carries the same glyph — the filter line is
    // global to the popup — and the titlebar stays plain.
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);
    assert!(
        filter_line.contains(" > son"),
        "the glyph renders in detail mode too, got: {filter_line:?}"
    );
    assert!(
        !title.contains('>'),
        "the titlebar stays plain, got: {title:?}"
    );
}

/// Helper unit tests live beside the render tests so the geometry
/// helpers read as one section; these are pure — no harness.
mod centered_offset_tests {
    use crate::view::centered_offset;

    #[rstest::rstest]
    #[test]
    fn offset_centers_a_mid_list_selection_with_slack_both_sides() {
        // Given a 12-row list in a 6-row window.
        // When the offset derives for selection 5.
        let offset = centered_offset(5, 12, 6);

        // Then the selection sits mid-window: offset 2 puts it third of
        // six visible rows.
        assert_eq!(offset, 2);
    }

    #[rstest::rstest]
    #[test]
    fn offset_clamps_to_zero_while_the_selection_is_in_the_top_half() {
        // Given a tall list, the selection still inside the top half.
        // When the offset derives for selection 1 in a 6-row window.
        let offset = centered_offset(1, 12, 6);

        // Then the window starts at the list's top — centering never
        // scrolls above row 0.
        assert_eq!(offset, 0);
    }

    #[rstest::rstest]
    #[test]
    fn offset_clamps_to_the_bottom_so_no_blank_rows_render() {
        // Given the selection at the list's very bottom.
        // When the offset derives for selection 11 of 12 rows.
        let offset = centered_offset(11, 12, 6);

        // Then the window is the last six rows exactly — no blank
        // rows below the last one.
        assert_eq!(offset, 6);
    }

    #[rstest::rstest]
    #[test]
    fn offset_is_zero_when_the_list_fits_the_window() {
        // Given a list shorter than the window.
        // When the offset derives for its last row.
        let offset = centered_offset(2, 3, 6);

        // Then the window starts at row 0 — nothing to scroll.
        assert_eq!(offset, 0);
    }

    #[rstest::rstest]
    #[test]
    fn offset_is_zero_for_an_empty_list() {
        // Given an empty list at selection 0 (the route actions reset
        // to zero on an empty projection).
        // When the offset derives.
        let offset = centered_offset(0, 0, 6);

        // Then the window starts at 0 — no panic, nothing to show.
        assert_eq!(offset, 0);
    }
}

/// One listing with a distinct index in its slug — table filler whose
/// fetch order survives the PROJ-ascending stable sort (identical
/// prices), so `m/model{i}` is visible row `i`.
fn numbered_row(i: usize) -> MarketRow {
    MarketRow {
        id: format!("openrouter/m/model{i}"),
        author: "m".to_owned(),
        slug: format!("model{i}"),
        variant: None,
        prompt_price: 1e-6,
        completion_price: 2e-6,
        cache_read_price: Some(1e-7),
        has_overrides: false,
        context_length: Some(128_000),
        eci: None,
        eci_ci: None,
        is_free: false,
    }
}

/// The first on-screen data row's text, scanning the table band from
/// the top — the window's opening row.
fn first_data_row(buffer: &Buffer, popup: ratatui::layout::Rect) -> String {
    (popup.y + 5..popup.y + popup.height)
        .map(|y| row_text(buffer, y, popup.width))
        .find(|text| text.contains("m/model"))
        .expect("a data row renders")
}

#[rstest::rstest]
fn scrolling_up_from_the_bottom_keeps_the_window_still() {
    // Given a 12-row list (past the 5-row window) with the selection on
    // its last row — the window bottom-clamped.
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = (0..12).map(numbered_row).collect();
    });
    harness.ui.update(|s| s.selection = 11);
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders, and then renders again one up-step later
    // (the selection now on the second-to-last row).
    let at_bottom = harness.render(popup, 140, 100);
    harness.ui.update(|s| s.selection = 10);
    let after_up_step = harness.render(popup, 140, 100);

    // Then both frames open on the same model row — the up-step moved
    // the cursor within the window instead of dragging the window edge.
    assert_eq!(
        first_data_row(&at_bottom, popup),
        first_data_row(&after_up_step, popup),
        "the window holds still across the up-step from the bottom"
    );
}

#[rstest::rstest]
fn a_frame_one_up_step_from_the_bottom_shows_the_cursor_strictly_inside() {
    // Given the same 12-row list with the selection one up-step off
    // the bottom (selection 10 of 12).
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = (0..12).map(numbered_row).collect();
    });
    harness.ui.update(|s| s.selection = 10);
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);
    let theme = jinn_theme::default_theme();

    // Then the highlighted row is model10 and it sits strictly above
    // the band's last data row — slack remains below the cursor, so
    // the cursor rose inside the window. The row under the cursor
    // (model11) still renders. The band's last data row is the row
    // just above the status line: popup.y + height − 4.
    let selected_y = (popup.y + 5..popup.y + popup.height)
        .find(|y| {
            (popup.x..popup.x + popup.width).any(|x| {
                buffer
                    .cell((x, *y))
                    .is_some_and(|c| c.bg == theme.picker_selected_bg)
            })
        })
        .expect("a selected row renders");
    let selected = row_text(&buffer, selected_y, popup.width);
    assert!(
        selected.contains("m/model10"),
        "the highlight follows the selection, got: {selected:?}"
    );
    assert!(
        selected_y < popup.y + popup.height - 4,
        "the cursor keeps slack below it, highlighted at y={selected_y}"
    );
    let below = row_text(&buffer, selected_y + 1, popup.width);
    assert!(
        below.contains("m/model11"),
        "the row under the cursor still renders, got: {below:?}"
    );
}
