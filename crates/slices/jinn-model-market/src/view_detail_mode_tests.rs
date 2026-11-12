//! Tests for detail mode's render behaviors: the endpoint table through
//! the same table machinery (bordered block, header with the sorted
//! highlight, Default first), the loading line while the fetch is in
//! flight, the title carrying the model, and the full-height geometry
//! the pane's deletion buys.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketDetail;
use jinn_model_market_msg::MarketEndpointRow;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_slices::Slices;
use jinn_slices::cell::TypedCell;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;

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
        let theme = jinn_theme::default_theme();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("test terminal");
        terminal
            .draw(|frame| {
                let facts = jinn_slices::RenderFacts::new(theme.clone(), &self.slices);
                crate::view::render_market_popup(frame, popup, &facts);
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }
}

/// One listing the popup can drill into.
fn listed_row(id: &str) -> MarketRow {
    let (author, slug) = id
        .strip_prefix("openrouter/")
        .and_then(|rest| rest.split_once('/'))
        .unwrap_or(("a", "model"));
    MarketRow {
        id: id.to_owned(),
        author: author.to_owned(),
        slug: slug.to_owned(),
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

/// One endpoint with everything published: the populated-detail shape.
fn full_endpoint() -> MarketEndpointRow {
    MarketEndpointRow {
        tag: "azure/us-east".to_owned(),
        provider_name: "Azure".to_owned(),
        prompt_price: 1.2e-6,
        completion_price: 4.5e-6,
        cache_read_price: Some(0.9e-7),
        latency_p50_ms: Some(320.0),
        throughput_p50: Some(88.0),
        uptime_1d: Some(0.992),
        quantization: None,
        overrides: Vec::new(),
    }
}

/// The detail payload the cell carries once the actor's fetch lands.
fn detail_for(id: &str, endpoints: Vec<MarketEndpointRow>) -> MarketDetail {
    MarketDetail {
        model_id: id.to_owned(),
        endpoints,
    }
}

/// The buffer's full text, cells joined.
fn buffer_text(buffer: &Buffer) -> String {
    buffer.content.iter().map(Cell::symbol).collect()
}

/// A rendered row's text, cells joined across the popup's width.
fn row_text(buffer: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .collect()
}

/// The buffer column where `needle` starts in the row at `y`, counted
/// in cells — the robust lookup for a header text whose row also
/// carries multibyte glyphs (the border, the sort arrow), where byte
/// offsets and cell columns disagree.
fn cell_col_of(buffer: &Buffer, y: u16, width: u16, needle: &str) -> Option<u16> {
    let mut joined = String::new();
    let starts: Vec<usize> = (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .map(|sym| {
            let start = joined.len();
            joined.push_str(sym);
            start
        })
        .collect();
    let at = joined.find(needle)?;
    starts
        .iter()
        .position(|&start| start == at)
        .map(|idx| idx as u16)
}

#[rstest::rstest]
fn detail_mode_renders_the_eight_column_headers() {
    // Given detail mode with its detail landed.
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the header row carries all eight detail columns in order.
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");
    let header = row_text(&buffer, header_y, popup.width);
    for name in [
        "ENDPOINT", "IN /M", "OUT /M", "CACHE /M", "PROJ", "LAT p50", "TP p50", "UP 1d",
    ] {
        assert!(
            header.contains(name),
            "header must carry {name}, got: {header:?}"
        );
    }
}

#[rstest::rstest]
fn the_default_row_is_first_and_labeled_default() {
    // Given detail mode with one endpoint's detail landed.
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the first data row reads `Default` and the endpoint follows
    // with its provider name and tag.
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");
    let first = row_text(&buffer, header_y + 1, popup.width);
    assert!(
        first.contains("Default"),
        "the sentinel is the first row, got: {first:?}"
    );
    let second = row_text(&buffer, header_y + 2, popup.width);
    assert!(
        second.contains("Azure (azure/us-east)"),
        "the endpoint renders name and tag, got: {second:?}"
    );
}

#[rstest::rstest]
fn detail_rows_render_the_endpoint_fields() {
    // Given detail mode with one fully published endpoint.
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the endpoint's prices, percentiles, and uptime render in the
    // column shapes the contract names.
    let text = buffer_text(&buffer);
    assert!(
        text.contains("$1.2000/M") && text.contains("$4.5000/M"),
        "the endpoint prices render in 4-digit dollars, got: {text:?}"
    );
    assert!(
        text.contains("320ms") && text.contains("88 tok/s"),
        "the percentiles render, got: {text:?}"
    );
    assert!(
        text.contains("99.2%"),
        "the uptime renders as the ×100 percentage, got: {text:?}"
    );
    assert!(
        !text.contains("0.992"),
        "the raw fraction never reaches the screen, got: {text:?}"
    );
}

#[rstest::rstest]
fn null_detail_fields_render_em_dashes() {
    // Given an endpoint whose cache price and percentiles are absent
    // (unpublished cache, no API key for the percentiles).
    let harness = Harness::new();
    let mut endpoint = full_endpoint();
    endpoint.cache_read_price = None;
    endpoint.latency_p50_ms = None;
    endpoint.throughput_p50 = None;
    endpoint.uptime_1d = None;
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![endpoint]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the CACHE cell reads the empty-cell placeholder, and no
    // made-up numbers
    // stand in for the missing fields.
    let text = buffer_text(&buffer);
    assert!(
        text.contains(crate::format::EMPTY_CELL),
        "unpublished fields render the empty cell, got: {text:?}"
    );
    assert!(
        !text.contains("320ms") && !text.contains("88 tok/s") && !text.contains("99.2%"),
        "no stale values survive, got: {text:?}"
    );
}

#[rstest::rstest]
fn detail_loading_renders_a_loading_line_and_no_header() {
    // Given detail mode pinned while the data cell holds detail for
    // ANOTHER model — the fetch for the pinned one is still in flight.
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/b/other", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the table area carries the muted loading line — and no
    // header, no rows from the stale detail.
    let text = buffer_text(&buffer);
    assert!(
        text.contains("loading endpoints"),
        "the loading line renders, got: {text:?}"
    );
    assert!(
        !text.contains("ENDPOINT") && !text.contains("Azure"),
        "no header and no stale rows render, got: {text:?}"
    );
}

#[rstest::rstest]
fn title_in_detail_mode_carries_the_model_in_focus_accent() {
    // Given detail mode pinned to `openrouter/a/model`.
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);
    let theme = jinn_theme::default_theme();
    let title = row_text(&buffer, popup.y, popup.width);

    // Then the title reads the prefix and the model name, the
    // `openrouter/` prefix stripped.
    assert!(
        title.contains(" OpenRouter Model Market ") && title.contains("\u{b7} a/model"),
        "the title carries the model, got: {title:?}"
    );
    // And the model-name cells wear the focus accent while the prefix
    // cells wear the popup title color. The name starts after the `·`
    // separator and its following space.
    let separator_x = (popup.x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, popup.y))
                .is_some_and(|c| c.symbol() == "\u{b7}")
        })
        .expect("the separator dot renders");
    let model_cell = (separator_x + 1..popup.x + popup.width)
        .filter_map(|x| buffer.cell((x, popup.y)))
        .find(|c| c.symbol() != " ")
        .expect("the model name renders in the title");
    assert_eq!(
        model_cell.style().fg,
        Some(theme.focus_accent),
        "the model name renders in focus_accent"
    );
    let prefix_cell = buffer
        .cell((popup.x + 2, popup.y))
        .expect("prefix cell exists");
    assert_eq!(
        prefix_cell.style().fg,
        Some(theme.popup_title),
        "the prefix renders in popup_title"
    );
}

#[rstest::rstest]
fn the_title_stays_plain_in_table_mode() {
    // Given table mode with a seeded table.
    let harness = Harness::new();
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);
    let theme = jinn_theme::default_theme();
    let title = row_text(&buffer, popup.y, popup.width);

    // Then the title is exactly the popup's name — no separator dot —
    // and every title cell wears the popup title color.
    assert!(
        title.contains(" OpenRouter Model Market "),
        "the plain title renders, got: {title:?}"
    );
    assert!(!title.contains('\u{b7}'), "no model suffix, got: {title:?}");
    let all_prefix = (popup.x + 2..popup.x + 15)
        .filter_map(|x| buffer.cell((x, popup.y)))
        .all(|c| c.style().fg == Some(theme.popup_title));
    assert!(all_prefix, "the title cells wear popup_title");
}

#[rstest::rstest]
fn the_sorted_detail_header_cell_wears_the_focus_pairing() {
    // Given detail mode on its default sort (PROJ ascending).
    let harness = Harness::new();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/a/model".to_owned()));
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);
    let theme = jinn_theme::default_theme();
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");

    // Then the PROJ header cell wears the focus pairing and carries the
    // ascending arrow.
    let proj_x = cell_col_of(&buffer, header_y, popup.width, "PROJ")
        .map(|col| popup.x + col)
        .expect("the PROJ header renders");
    let proj_cell = buffer.cell((proj_x, header_y)).expect("cell exists");
    assert_eq!(proj_cell.style().fg, Some(theme.gutter_bg));
    assert_eq!(proj_cell.style().bg, Some(theme.focus_accent));
    // And the arrow rides the same cell.
    let arrow_at = usize::from(proj_x - popup.x);
    let cells: Vec<&str> = (0..popup.width)
        .filter_map(|x| buffer.cell((x, header_y)).map(Cell::symbol))
        .collect();
    assert!(
        cells
            .get(arrow_at..)
            .expect("arrow offset within the header row")
            .iter()
            .copied()
            .collect::<String>()
            .starts_with("PROJ \u{2191}"),
        "the ascending arrow rides the sorted cell"
    );
    // And an unsorted header cell carries no background at all.
    let endpoint_x = cell_col_of(&buffer, header_y, popup.width, "ENDPOINT")
        .map(|col| popup.x + col)
        .expect("the ENDPOINT header renders");
    let endpoint_cell = buffer
        .cell((endpoint_x, header_y))
        .expect("ENDPOINT cell exists");
    assert_ne!(
        endpoint_cell.style().bg,
        Some(theme.focus_accent),
        "an unsorted header carries no focus background"
    );
}

#[rstest::rstest]
fn detail_selection_highlights_the_chosen_row() {
    // Given detail mode with the highlight on the second row.
    let harness = Harness::new();
    harness.ui.update(|s| {
        s.enter_detail("openrouter/a/model".to_owned());
        s.detail_selection = 1;
    });
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        let mut lambda = full_endpoint();
        lambda.tag = String::new();
        lambda.provider_name = "Lambda".to_owned();
        lambda.prompt_price = 0.5e-6;
        lambda.completion_price = 1e-6;
        d.detail = Some(detail_for(
            "openrouter/a/model",
            vec![full_endpoint(), lambda],
        ));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);
    let theme = jinn_theme::default_theme();

    // Then the selected row is the Lambda row (the Default row is not
    // highlighted), and the tagless endpoint renders bare-name. The
    // default PROJ-ascending sort puts the cheaper Lambda at index 1,
    // exactly where the highlight is pinned.
    let selected_y = (popup.y..popup.y + popup.height)
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
        selected.contains("Lambda"),
        "the highlight follows detail_selection, got: {selected:?}"
    );
    assert!(
        !selected.contains("()"),
        "an empty tag renders no empty parens, got: {selected:?}"
    );
}

#[rstest::rstest]
fn detail_mode_scrolled_to_the_bottom_sends_the_sentinel_offscreen() {
    // Given detail mode over 12 endpoints (13 rows with the sentinel —
    // past the 5-row window) with the selection pinned to the last
    // endpoint (detail_selection 12).
    let harness = Harness::new();
    let mut provider_index = 0;
    let endpoints: Vec<MarketEndpointRow> = (0..12)
        .map(|i| {
            let mut endpoint = full_endpoint();
            provider_index += 1;
            endpoint.provider_name = format!("Provider{provider_index:02}");
            endpoint.tag = format!("tag{i}");
            endpoint
        })
        .collect();
    harness.ui.update(|s| {
        s.enter_detail("openrouter/a/model".to_owned());
        s.detail_selection = 12;
    });
    harness.data.update(|d| {
        d.rows = vec![listed_row("openrouter/a/model")];
        d.detail = Some(detail_for("openrouter/a/model", endpoints));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 16);

    // When the popup renders.
    let buffer = harness.render(popup, 140, 100);

    // Then the Default sentinel is not on screen — the window sits on
    // the list's tail, exactly as table mode bottoms out — and the
    // highlighted row is the twelfth endpoint.
    let text = buffer_text(&buffer);
    assert!(
        !text.contains("Default"),
        "the sentinel scrolled off the window top, got: {text:?}"
    );
    let theme = jinn_theme::default_theme();
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
        selected.contains("Provider12 (tag11)"),
        "the highlight follows detail_selection, got: {selected:?}"
    );
}

#[rstest::rstest]
fn table_mode_uses_the_full_height_after_the_pane_gone() {
    // Given a 60-row table (well past the viewport) with detail landed
    // for the pinned row — then the round trip back to table mode.
    let harness = Harness::new();
    let rows: Vec<MarketRow> = (0..60)
        .map(|i| listed_row(&format!("openrouter/m/model{i}")))
        .collect();
    harness
        .ui
        .update(|s| s.enter_detail("openrouter/m/model0".to_owned()));
    harness.data.update(|d| {
        d.rows = rows;
        d.detail = Some(detail_for("openrouter/m/model0", vec![full_endpoint()]));
    });
    let popup = ratatui::layout::Rect::new(5, 2, 130, 40);

    // When the popup renders in TABLE mode (the pin cleared — the
    // round trip back).
    harness
        .ui
        .update(jinn_model_market_msg::MarketUiState::leave_detail);
    let buffer = harness.render(popup, 140, 100);
    let status_y = popup.y + popup.height - 3;

    // Then rows render down to the row just above the status line: no
    // band sits reserved under the table.
    let found = (popup.y..status_y)
        .rev()
        .find(|y| row_text(&buffer, *y, popup.width).contains("m/model"))
        .expect("rows render");
    assert_eq!(
        found,
        status_y - 1,
        "a data row renders directly above the status line, got {found:?}"
    );
    // And no endpoint text renders anywhere: the pane is gone.
    let text = buffer_text(&buffer);
    assert!(
        !text.contains("Azure"),
        "no pane text renders in table mode, got: {text:?}"
    );
}
