//! Tests for the `?` help card: the widget-level visibility contract,
//! the TestBackend render, and the attribution Epoch AI's license
//! requires — checked in the shipped license file.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_model_market_msg::MarketUiState;
use jinn_theme::default_theme;
use ratatui::layout::Rect;

use super::help_lines;
use super::market_help_card;
use super::wrap_line;

/// A popup frame with room for the card below it: a terminal, the
/// popup's rect, and the popup's inner rect.
struct PopupFrame {
    terminal: Rect,
    popup: Rect,
    inner: Rect,
}

/// The standard test frame: the overlay geometry the view itself uses.
///
/// The popup is ~80% of the terminal, so the card — a taller document
/// than the room left below — resolves to its fallback here. Tests that
/// need the below-popup placement build [`small_popup_frame`].
fn popup_frame() -> PopupFrame {
    let terminal = Rect::new(0, 0, 90, 40);
    let popup = crate::view::market_overlay_rect(&terminal).expect("90×40 fits the popup");
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    PopupFrame {
        terminal,
        popup,
        inner,
    }
}

/// A short popup with the whole terminal below it free: the geometry
/// that shows the card's preferred below-the-popup placement.
fn small_popup_frame() -> PopupFrame {
    let terminal = Rect::new(0, 0, 90, 100);
    let popup = Rect::new(5, 2, 80, 10);
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    PopupFrame {
        terminal,
        popup,
        inner,
    }
}

/// The card over the standard frame, or `None` when hidden.
fn card_for(ui: &MarketUiState) -> Option<super::HelpCard> {
    let frame = popup_frame();
    market_help_card(
        ui,
        frame.popup,
        frame.inner,
        frame.terminal,
        &default_theme(),
    )
}

#[rstest::rstest]
fn hidden_help_state_yields_no_card() {
    // Given a default UI state — help hidden, as the opener resets it.
    let ui = MarketUiState::default();

    // When the card is resolved for the frame.
    let card = card_for(&ui);

    // Then there is nothing to draw.
    assert!(card.is_none(), "a hidden help must not produce a card");
}

#[rstest::rstest]
fn visible_help_state_places_the_card_below_the_popup() {
    // Given a UI state with the help toggled on, over a frame with room
    // below the popup.
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };
    let frame = small_popup_frame();

    // When the card is resolved for the frame.
    let card = market_help_card(
        &ui,
        frame.popup,
        frame.inner,
        frame.terminal,
        &default_theme(),
    )
    .expect("a visible help produces a card");

    // Then the card starts flush with the popup's bottom border.
    assert_eq!(
        card.area.y,
        frame.popup.y + frame.popup.height,
        "the card sits directly below the popup"
    );
    // And it is as wide as the popup's inner rect, at its x.
    assert_eq!(card.area.width, frame.inner.width);
    assert_eq!(card.area.x, frame.inner.x);
}

#[rstest::rstest]
fn card_too_tall_for_the_room_drops_to_the_terminal_top() {
    // Given a terminal barely taller than the popup, leaving no room
    // below for the card.
    let terminal = Rect::new(0, 0, 90, 16);
    let popup = crate::view::market_overlay_rect(&terminal).expect("90×16 fits the popup");
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };

    // When the card is resolved.
    let card =
        market_help_card(&ui, popup, inner, terminal, &default_theme()).expect("card resolves");

    // Then it draws from the terminal's top row rather than floating
    // mid-screen or over the popup.
    assert_eq!(
        card.area.y, terminal.y,
        "with no room below, the card falls back to the terminal top"
    );
}

#[rstest::rstest]
fn drawn_card_renders_its_border_title_and_sections() {
    // Given a visible help card over a tall frame so the whole card is
    // on screen.
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };
    let card = card_for(&ui).expect("a visible help produces a card");
    let frame = popup_frame();

    // When it draws against a test backend.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        frame.terminal.width,
        120,
    ))
    .expect("test terminal");
    terminal.draw(|f| card.draw(f)).expect("draw");

    // Then the screen carries the border, the title, and exactly the
    // two sections the card documents — no key inventory, no status
    // line, no data-sources *section* (the sources ride one muted
    // line at the bottom).
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        text.contains("OpenRouter Model Market help"),
        "title, got: {text:?}"
    );
    for section in ["Columns", "Detail mode"] {
        assert!(text.contains(section), "section {section}, got: {text:?}");
    }
    for gone in [
        "Keys",
        "Status line",
        "Data sources:",
        "<leader>so",
        "[m]",
        "Detail pane",
    ] {
        assert!(
            !text.contains(gone),
            "the card must not carry {gone:?}, got: {text:?}"
        );
    }
}

#[rstest::rstest]
fn drawn_card_documents_the_columns_and_flags() {
    // Given a visible help card over a tall frame so the whole card is
    // on screen.
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };
    let card = card_for(&ui).expect("a visible help produces a card");
    let frame = popup_frame();

    // When it draws against a test backend.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        frame.terminal.width,
        120,
    ))
    .expect("test terminal");
    terminal.draw(|f| card.draw(f)).expect("draw");

    // Then the rendered text names every column and the flag glyphs.
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    for fact in [
        "MODEL", "IN/M", "OUT/M", "CACHE/M", "PROJ", "ECI", "VALS", "FLAGS", "\u{2020}", "#",
    ] {
        assert!(text.contains(fact), "card must document {fact:?}");
    }
}

#[rstest::rstest]
fn drawn_card_names_both_data_sources() {
    // Given a visible help card over a tall frame so the whole card is
    // on screen.
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };
    let card = card_for(&ui).expect("a visible help produces a card");
    let frame = popup_frame();

    // When it draws against a test backend.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        frame.terminal.width,
        120,
    ))
    .expect("test terminal");
    terminal.draw(|f| card.draw(f)).expect("draw");
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    // Then the sources line names both score providers.
    assert!(
        text.contains("Epoch Capabilities Index") && text.contains("epoch.ai"),
        "the ECI source is named, got: {text:?}"
    );
    assert!(
        text.contains("Vals Index") && text.contains("vals.ai"),
        "the vals source is named, got: {text:?}"
    );
}

#[rstest::rstest]
fn drawn_card_documents_the_detail_mode_semantics() {
    // Given a visible help card over a tall frame so the whole card is
    // on screen.
    let ui = MarketUiState {
        help_visible: true,
        ..MarketUiState::default()
    };
    let card = card_for(&ui).expect("a visible help produces a card");
    let frame = popup_frame();

    // When it draws against a test backend.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        frame.terminal.width,
        120,
    ))
    .expect("test terminal");
    terminal.draw(|f| card.draw(f)).expect("draw");

    // Then the rendered text names the detail columns, the DEFAULT
    // sentinel with its auto-pick semantics, and the mix toggle — and
    // no pane-era wording survives.
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    for fact in [
        "ENDPOINT",
        "LAT p50",
        "TP p50",
        "UP 1d",
        "DEFAULT",
        "auto-picks",
        "mix",
    ] {
        assert!(text.contains(fact), "card must document {fact:?}");
    }
    for gone in ["ECI \u{b1}", "Detail pane"] {
        assert!(
            !text.contains(gone),
            "the pane-era fact {gone:?} must be gone, got: {text:?}"
        );
    }
}

#[rstest::rstest]
fn attribution_file_carries_the_required_wording() {
    // Given the shipped third-party license file, whitespace-normalized
    // (the wording must be verbatim, not its line layout).
    let file = include_str!("../../../../THIRD_PARTY_LICENSES");
    let normalized = file.split_whitespace().collect::<Vec<_>>().join(" ");

    // When checked for the required facts.
    // Then it names the source and the license, and carries Epoch's
    // required attribution sentence verbatim.
    assert!(normalized.contains("Epoch AI"), "source named");
    assert!(normalized.contains("CC BY 4.0"), "license named");
    assert!(
        normalized.contains(
            "Epoch AI, 'Capabilities & benchmarking'. Published online at epoch.ai. \
             Retrieved from 'https://epoch.ai/benchmarks/use-this-data' [online resource]. \
             Accessed 15 Oct 2026."
        ),
        "the verbatim attribution sentence must ship unmodified"
    );
    // And it says what we ship and where runtime copies live.
    assert!(normalized.contains("eci_scores.csv"), "seed files named");
    assert!(
        normalized.contains("~/.cache/jinn/scores/eci/"),
        "runtime cache dir named"
    );
}

#[rstest::rstest]
fn wrap_line_breaks_long_lines_within_the_width() {
    // Given one long line and a narrow width.
    let line = ratatui::text::Line::from("the projected cost of one turn under the mix");
    let width = 18;

    // When the line wraps.
    let rows = wrap_line(line, width);

    // Then every produced row fits the width.
    for row in &rows {
        let len: usize = row.spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(
            len <= usize::from(width),
            "row of {len} exceeds width {width}: {row:?}"
        );
    }
    // And the words survive the wrap in order — reconstructed by
    // appending spans, dropping only a trailing row-edge space.
    let mut joined = String::new();
    for row in &rows {
        if !joined.is_empty() {
            joined.push(' ');
        }
        let last = row.spans.len().saturating_sub(1);
        for (idx, span) in row.spans.iter().enumerate() {
            if idx == last {
                joined.push_str(span.content.trim_end_matches(' '));
            } else {
                joined.push_str(&span.content);
            }
        }
    }
    assert_eq!(joined, "the projected cost of one turn under the mix");
}

#[rstest::rstest]
fn wrap_line_keeps_a_blank_line_one_row() {
    // Given an authored blank line.
    let line = ratatui::text::Line::from("");

    // When it wraps.
    let rows = wrap_line(line, 40);

    // Then it still occupies exactly one row: the author's spacing.
    assert_eq!(rows.len(), 1);
}

#[rstest::rstest]
fn help_lines_render_within_a_reasonable_popup_width() {
    // Given the card's authored lines.
    let theme = default_theme();

    // When every line wraps to the standard frame's text width.
    let frame = popup_frame();
    let text_width = frame.inner.width.saturating_sub(super::CARD_BORDER_CELLS);
    let rows: Vec<ratatui::text::Line<'static>> = help_lines(&theme)
        .into_iter()
        .flat_map(|line| wrap_line(line, text_width))
        .collect();

    // Then no wrapped row overflows the width it will draw at.
    for row in &rows {
        let len: usize = row.spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(
            len <= usize::from(text_width),
            "row of {len} exceeds text width {text_width}: {row:?}"
        );
    }
}

#[rstest::rstest]
#[test]
fn help_names_both_data_sources_on_one_line() {
    // Given the help card's lines for the default theme.
    let lines = help_lines(&default_theme());

    // When the data-sources line is found.
    let sources = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .find(|t| t.contains("Scores:"))
        .expect("a data-sources line exists");

    // Then it names both providers with their hosts.
    assert!(
        sources.contains("epoch.ai") && sources.contains("vals.ai"),
        "both data sources are named, got: {sources:?}"
    );
    // And the line is a single row — the card grows by content, not
    // layout.
    assert_eq!(
        lines
            .iter()
            .filter(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    .contains("Scores:")
            })
            .count(),
        1,
        "exactly one sources line"
    );
}
