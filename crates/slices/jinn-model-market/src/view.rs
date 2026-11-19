//! The model-market popup's overlay rendering: the centered full-table
//! geometry, the two-state table (table mode ⇄ detail mode), the filter
//! line, the status line, the keybind footer, and the `?` help card the
//! view draws below itself while it is toggled on.
//!
//! The render pass reads the cells only — no IO, no actor pokes, no
//! messages published from render. PROJ is computed here from the
//! fixed [`PROJECTION_MIX`] the `-msg` crate names.
//!
//! Each mode's shape has one source of truth: [`MARKET_COLUMNS`] for
//! the table, [`DETAIL_COLUMNS`] for detail mode. The header, every
//! data row, and the layout constraints all derive from them, so a
//! header cannot sit over the wrong data column — and the sorted-cell
//! styling factors through one helper, so the two headers cannot drift.

use std::ops::Range;

use jinn_model_market_msg::DetailSort;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::PROJECTION_MIX;
use jinn_model_market_msg::SortColumn;
use jinn_slices::RenderFacts;
use jinn_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Cell;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Row;
use ratatui::widgets::Table;
use ratatui::widgets::TableState;

use crate::format;
use crate::help::market_help_card;
use crate::routes::MARKET_BINDINGS;
use crate::ui_state;
use crate::ui_state::DetailRow;

/// The rows the status line and footer occupy at the popup's bottom —
/// the table's own rect ends above them so the selection highlight can
/// never render underneath. The footer is two rows: the binding set
/// overflows one row at common widths, and a deterministic split beats
/// a wrapping layout that moves with the terminal's width.
const STATUS_ROWS: u16 = 3;
/// The rows the filter line and its separator occupy at the popup's
/// top, above the table's header.
const FILTER_ROWS: u16 = 2;
/// How many of [`MARKET_BINDINGS`]' entries the footer's upper row
/// carries; the rest render on the second row. Thirteen bindings split
/// 7/6, which fits both halves at the narrowest popup width.
const FOOTER_FIRST_ROW: usize = 7;

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;

#[cfg(test)]
#[path = "view_detail_mode_tests.rs"]
mod view_detail_mode_tests;

/// One table column: its header, which [`SortColumn`] keys its sort,
/// whether the sort marker rides this column, its width, and the cell
/// renderer for a listing row. A renderer receives the row, the UI
/// state (the filter's match ranges), and the theme — the MODEL column
/// needs all three to draw its variant suffix dimmed under the filter's
/// highlight.
struct MarketColumn {
    header: &'static str,
    sort: SortColumn,
    /// The active-sort arrow renders on this column. `false` for a
    /// display-only column sharing another column's sort key.
    keyed: bool,
    width: u16,
    cell: fn(&MarketRow, &MarketUiState, &Theme) -> Line<'static>,
}

/// The columns in table order — the same order as the `h`/`l` sort
/// cycle, [`SORT_CYCLE`], plus the FLAGS display column. FLAGS exists
/// because `#` (no ECI) is a display fact, not a sortable key: it
/// renders beside the ECI column it annotates.
const MARKET_COLUMNS: &[MarketColumn] = &[
    MarketColumn {
        header: "PROJ",
        sort: SortColumn::Proj,
        keyed: true,
        width: 10,
        cell: |row, ui, _theme| {
            Line::from(format::dollars(ui_state::projected_cents(
                row,
                ui.active_mix,
            )))
        },
    },
    MarketColumn {
        header: "IN/M",
        sort: SortColumn::In,
        keyed: true,
        width: 10,
        cell: |row, _ui, _theme| Line::from(format::price_per_million(row.prompt_price)),
    },
    MarketColumn {
        header: "OUT/M",
        sort: SortColumn::Out,
        keyed: true,
        width: 10,
        cell: |row, _ui, _theme| Line::from(format::price_per_million(row.completion_price)),
    },
    MarketColumn {
        header: "CACHE/M",
        sort: SortColumn::Cache,
        keyed: true,
        width: 10,
        cell: |row, _ui, _theme| Line::from(format::cache_column(row.cache_read_price)),
    },
    MarketColumn {
        header: "ECI",
        sort: SortColumn::Eci,
        keyed: true,
        width: 7,
        cell: |row, _ui, _theme| Line::from(format::eci_column(row.eci)),
    },
    MarketColumn {
        header: "VALS",
        sort: SortColumn::Vals,
        keyed: true,
        width: 7,
        cell: |row, _ui, _theme| Line::from(format::vals_column(row.vals)),
    },
    MarketColumn {
        header: "MODEL",
        sort: SortColumn::Model,
        keyed: true,
        width: 38,
        cell: model_column,
    },
    MarketColumn {
        header: "FLAGS",
        sort: SortColumn::Model,
        keyed: false,
        width: 5,
        cell: |row, _ui, _theme| Line::from(flags_column(row)),
    },
];

/// One detail-mode column: the same shape as a [`MarketColumn`], over
/// [`DetailRow`]s. A renderer receives the row, the pinned model's
/// listing row (the Default sentinel's price source — `None` when a
/// refresh emptied the table mid-mode), and the theme.
struct DetailColumn {
    header: &'static str,
    sort: DetailSort,
    keyed: bool,
    width: u16,
    cell: fn(&DetailRow, Option<&MarketRow>, &MarketUiState, &Theme) -> Line<'static>,
}

/// The detail-mode columns in header order — the same order as the
/// `h`/`l` detail sort cycle, [`DETAIL_SORT_CYCLE`]. Pricing overrides
/// are documented in the help card, never rendered: the columns are the
/// endpoint's own headline prices.
const DETAIL_COLUMNS: &[DetailColumn] = &[
    DetailColumn {
        header: "ENDPOINT",
        sort: DetailSort::Endpoint,
        keyed: true,
        width: 30,
        cell: |row, _model, _ui, _theme| Line::from(row.display_name()),
    },
    DetailColumn {
        header: "IN /M",
        sort: DetailSort::In,
        keyed: true,
        width: 10,
        cell: |row, model, _ui, _theme| {
            Line::from(format::price_per_million(detail_prices(row, model).0))
        },
    },
    DetailColumn {
        header: "OUT /M",
        sort: DetailSort::Out,
        keyed: true,
        width: 10,
        cell: |row, model, _ui, _theme| {
            Line::from(format::price_per_million(detail_prices(row, model).1))
        },
    },
    DetailColumn {
        header: "CACHE /M",
        sort: DetailSort::Cache,
        keyed: true,
        width: 10,
        cell: |row, model, _ui, _theme| {
            Line::from(format::cache_column(detail_prices(row, model).2))
        },
    },
    DetailColumn {
        header: "PROJ",
        sort: DetailSort::Proj,
        keyed: true,
        width: 10,
        cell: |row, model, ui, _theme| {
            Line::from(format::dollars(detail_projected_cents(row, model, ui)))
        },
    },
    DetailColumn {
        header: "LAT p50",
        sort: DetailSort::Lat,
        keyed: true,
        width: 9,
        cell: |row, _model, _ui, _theme| {
            Line::from(match row {
                DetailRow::Default => format::EMPTY_CELL.to_owned(),
                DetailRow::Endpoint(endpoint) => endpoint
                    .latency_p50_ms
                    .map_or_else(|| format::EMPTY_CELL.to_owned(), |ms| format!("{ms:.0}ms")),
            })
        },
    },
    DetailColumn {
        header: "TP p50",
        sort: DetailSort::Tp,
        keyed: true,
        width: 10,
        cell: |row, _model, _ui, _theme| {
            Line::from(match row {
                DetailRow::Default => format::EMPTY_CELL.to_owned(),
                DetailRow::Endpoint(endpoint) => endpoint.throughput_p50.map_or_else(
                    || format::EMPTY_CELL.to_owned(),
                    |t| format!("{t:.0} tok/s"),
                ),
            })
        },
    },
    DetailColumn {
        header: "UP 1d",
        sort: DetailSort::Up,
        keyed: true,
        width: 7,
        cell: |row, _model, _ui, _theme| {
            Line::from(match row {
                DetailRow::Default => format::EMPTY_CELL.to_owned(),
                DetailRow::Endpoint(endpoint) => endpoint.uptime_1d.map_or_else(
                    || format::EMPTY_CELL.to_owned(),
                    |frac| format!("{:.1}%", frac * 100.0),
                ),
            })
        },
    },
];

/// The row's `(prompt, completion, cache)` display prices: an
/// endpoint's own, or the sentinel's from the model's listing row.
/// A sentinel with no listing row left prices at NaN, which both
/// formatters render as `NaN` — an unreachable-in-practice fallback
/// (the pin lives only while the table that listed the model still
/// does), never a panic.
fn detail_prices(row: &DetailRow, model: Option<&MarketRow>) -> (f64, f64, Option<f64>) {
    match row {
        DetailRow::Default => match model {
            Some(listing) => (
                listing.prompt_price,
                listing.completion_price,
                listing.cache_read_price,
            ),
            None => (f64::NAN, f64::NAN, None),
        },
        DetailRow::Endpoint(endpoint) => (
            endpoint.prompt_price,
            endpoint.completion_price,
            endpoint.cache_read_price,
        ),
    }
}

/// The row's projected $/turn: the endpoint's own prices, or the
/// sentinel's from the model's listing row. A sentinel with no listing
/// row left projects `NaN` — same fallback as [`detail_prices`].
fn detail_projected_cents(row: &DetailRow, model: Option<&MarketRow>, ui: &MarketUiState) -> f64 {
    let (prompt, completion, cache) = detail_prices(row, model);
    ui_state::projected_cents_for(prompt, completion, cache, ui.active_mix)
}

/// The popup rect: a centered table window, ~90% of the frame with
/// minimums so a small terminal still draws a usable table.
#[must_use]
pub fn market_overlay_rect(area: &Rect) -> Option<Rect> {
    if area.width < 40 || area.height < 12 {
        return None;
    }
    let width = ((area.width * 9) / 10).max(40);
    let height = ((area.height * 8) / 10).max(12);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Some(Rect::new(x, y, width, height))
}

/// Draws the popup for one frame: the opaque surface, the filter line,
/// the mode's table (or the detail fetch's loading line), the status
/// line, the footer, then the `?` help card when it is toggled on.
pub fn render_market_popup(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(ui_cell) = facts
        .slices
        .reader::<MarketUiState>(&crate::market_ui_slot())
    else {
        return;
    };
    let Some(data_cell) = facts
        .slices
        .reader::<MarketDataState>(&crate::market_data_slot())
    else {
        return;
    };

    // Publish the measured result-row count for the paging keys and the
    // frame's resolved projection mix before reading (the project
    // picker's measurement pattern). Both modes page over the same
    // geometry: the rows between the filter's separator and the status
    // rows.
    let theme = facts.theme.clone();
    let measured = results_viewport(area);
    let usage = usage_mix_inputs(facts);
    ui_cell.update(|state: &mut MarketUiState| {
        state.results_viewport = measured;
        state.active_mix = ui_state::resolve_mix(state.mix_source, usage);
    });

    let ui = ui_cell.read();
    let data = data_cell.read();

    // The popup is an opaque overlay: nothing behind it may bleed
    // through, so the surface is cleared before anything draws into it
    // (the selection widget's render order).
    frame.render_widget(Clear, area);

    // The block draws alone — the table renders inside its inner area
    // without a block of its own, because the table's rect excludes the
    // status rows the block would otherwise hand it. The title names
    // the popup; detail mode appends the drilled-in model's name.
    let border = if ui.filter_focused {
        theme.picker_highlight_bg
    } else {
        theme.muted_text
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans(&ui, &data, &theme)))
        .border_style(Style::default().fg(border));
    frame.render_widget(&block, area);
    let inner = block.inner(area);

    // The filter line: `> ` replacing the old `filter:` prompt, the
    // typed text, and a cursor block while focused. Drawn before the
    // table so the table's auto-scroll can never repaint it.
    let filter_style = if ui.filter_focused {
        Style::default().fg(theme.primary_text)
    } else {
        Style::default().fg(theme.muted_text)
    };
    let cursor = if ui.filter_focused { "\u{2588}" } else { "" };
    let filter_line = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(
        Line::from(Span::styled(
            format!(" > {}{cursor}", ui.filter),
            filter_style,
        )),
        filter_line,
    );
    let separator_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    frame.render_widget(
        Line::from(Span::styled(
            "\u{2500}".repeat(usize::from(inner.width)),
            Style::default().fg(theme.muted_text),
        )),
        separator_area,
    );

    // The table renders without its block into the rect between the
    // filter's separator and the status rows. Scoping `TableState`'s
    // auto-scroll to exactly the visible rows is what keeps the
    // selection highlight above the status line on long lists — the
    // full-area rect let auto-scroll park the highlight under rows the
    // status line and footer paint over afterwards. Both modes share
    // the geometry; only the row source, the column table, and the
    // selection driving auto-scroll differ.
    let table_area = Rect::new(
        inner.x,
        inner.y + FILTER_ROWS,
        inner.width,
        inner.height.saturating_sub(FILTER_ROWS + STATUS_ROWS),
    );
    render_mode_table(frame, table_area, &data, &ui, &theme);

    render_status_line(frame, inner, &data, &ui, &theme);
    render_footer(frame, inner, &theme);

    // The help card draws last, over the table behind it — the same
    // order the properties popup's help overlay composes in: surface
    // first, then the card that floats over it.
    if let Some(card) = market_help_card(&ui, area, inner, frame.area(), &theme) {
        card.draw(frame);
    }
}

/// The mode's table: table mode's listing table, detail mode's endpoint
/// table, or the muted loading line while a detail fetch is in flight.
fn render_mode_table(
    frame: &mut Frame<'_>,
    table_area: Rect,
    data: &MarketDataState,
    ui: &MarketUiState,
    theme: &Theme,
) {
    // The data-row window: the table area less its one-line header —
    // the row count the widget can actually show. The pivot math
    // windows against this, never against the header-inclusive height.
    let viewport = table_area.height.saturating_sub(1) as usize;
    if ui.detail_model.is_some() {
        match ui_state::sorted_detail_rows(data, ui) {
            Some(rows) => {
                let model = model_listing_row(data, ui);
                let header = detail_header_row(ui, theme);
                let rendered: Vec<Row<'static>> = rows
                    .iter()
                    .enumerate()
                    .map(|(idx, row)| {
                        detail_table_row(row, model, idx == ui.detail_selection, ui, theme)
                    })
                    .collect();
                let table = Table::new(
                    rendered,
                    DETAIL_COLUMNS.iter().map(|c| Constraint::Length(c.width)),
                )
                .header(header)
                .column_spacing(1);
                let mut table_state = TableState::default()
                    .with_offset(centered_offset(ui.detail_selection, rows.len(), viewport))
                    .with_selected(Some(ui.detail_selection));
                frame.render_stateful_widget(table, table_area, &mut table_state);
            }
            None => {
                // Detail mode without its detail: the fetch is in flight
                // (or landed for another model and was re-requested). A
                // muted line where the table will be — no header, no
                // stale rows.
                let muted = Style::default().fg(theme.muted_text);
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled("loading endpoints\u{2026}", muted))),
                    table_area,
                );
            }
        }
        return;
    }

    let visible = ui_state::visible_rows(data, ui);
    let header = header_row(ui, theme);
    let rows: Vec<Row<'static>> = visible
        .iter()
        .enumerate()
        .map(|(idx, row)| table_row(row, idx == ui.selection, ui, theme))
        .collect();

    let table = Table::new(
        rows,
        MARKET_COLUMNS.iter().map(|c| Constraint::Length(c.width)),
    )
    .header(header)
    .column_spacing(1);

    // The window centers the selection and clamps to the list's ends,
    // derived fresh each frame — the same pivot the sidebar's layout
    // uses — so scrolling back up moves the cursor within the window
    // instead of dragging the window edge along with it.
    let mut table_state = TableState::default()
        .with_offset(centered_offset(ui.selection, visible.len(), viewport))
        .with_selected(Some(ui.selection));
    frame.render_stateful_widget(table, table_area, &mut table_state);
}

/// The pinned model's listing row — the Default sentinel's price
/// source — or `None` when the table no longer carries it.
fn model_listing_row<'a>(data: &'a MarketDataState, ui: &MarketUiState) -> Option<&'a MarketRow> {
    let pinned = ui.detail_model.as_ref()?;
    data.rows.iter().find(|row| row.id == *pinned)
}

/// The title's spans: ` OpenRouter Model Market ` in the popup-title accent, and —
/// in detail mode — `· {author/slug[:variant]}` appended with the model
/// name in the focus accent, both bold.
fn title_spans(ui: &MarketUiState, data: &MarketDataState, theme: &Theme) -> Vec<Span<'static>> {
    let prefix = Span::styled(
        " OpenRouter Model Market ",
        Style::default()
            .fg(theme.popup_title)
            .add_modifier(Modifier::BOLD),
    );
    let Some(pinned) = ui.detail_model.as_ref() else {
        return vec![prefix];
    };
    let name = pinned_model_name(data, pinned);
    vec![
        prefix,
        Span::styled(
            "\u{b7} ",
            Style::default()
                .fg(theme.popup_title)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            name,
            Style::default()
                .fg(theme.focus_accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

/// The pinned model's display name: `author/slug[:variant]`, the
/// `openrouter/` prefix stripped — from the listing row when the table
/// still carries it, else from the pinned id itself.
fn pinned_model_name(data: &MarketDataState, pinned: &str) -> String {
    let listing = data
        .rows
        .iter()
        .find(|row| row.id == pinned)
        .map(ui_state::row_name);
    listing.unwrap_or_else(|| {
        pinned
            .strip_prefix("openrouter/")
            .unwrap_or(pinned)
            .to_owned()
    })
}

/// One sorted-header cell: the column's name, the direction arrow when
/// it holds the active sort, and the focus pairing on the whole cell —
/// name and arrow together. The one styling both header builders share,
/// so the two tables cannot drift.
fn header_cell(text: &str, sorted: bool, sort_desc: bool, theme: &Theme) -> Cell<'static> {
    let marker = if sort_desc { " \u{2193}" } else { " \u{2191}" };
    let cell_text = if sorted {
        format!("{text}{marker}")
    } else {
        text.to_owned()
    };
    let style = if sorted {
        Style::default()
            .fg(theme.gutter_bg)
            .bg(theme.focus_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    Cell::from(cell_text).style(style)
}

/// The table-mode header row, with the active sort column marked and
/// highlighted.
///
/// Derived from [`MARKET_COLUMNS`] so a header cannot sit over the
/// wrong data column, and the marker cannot land on a display-only
/// column. The sorted column's cell wears the focus pairing (background
/// `focus_accent`, foreground `gutter_bg`) via [`header_cell`]; the
/// rest stay plain bold.
fn header_row(ui: &MarketUiState, theme: &Theme) -> Row<'static> {
    let cells: Vec<Cell<'static>> = MARKET_COLUMNS
        .iter()
        .map(|col| {
            header_cell(
                col.header,
                col.keyed && col.sort == ui.sort_col,
                ui.sort_desc,
                theme,
            )
        })
        .collect();
    Row::new(cells)
}

/// The detail-mode header row: the same styling over
/// [`DETAIL_COLUMNS`] and the mode's own sort state.
fn detail_header_row(ui: &MarketUiState, theme: &Theme) -> Row<'static> {
    let cells: Vec<Cell<'static>> = DETAIL_COLUMNS
        .iter()
        .map(|col| {
            header_cell(
                col.header,
                col.keyed && col.sort == ui.detail_sort_col,
                ui.detail_sort_desc,
                theme,
            )
        })
        .collect();
    Row::new(cells)
}

/// One table row: the contract's columns for one listing, with the
/// fuzzy-filter match highlighted in the MODEL column.
fn table_row(row: &MarketRow, selected: bool, ui: &MarketUiState, theme: &Theme) -> Row<'static> {
    let cells: Vec<Cell<'static>> = MARKET_COLUMNS
        .iter()
        .map(|col| Cell::from((col.cell)(row, ui, theme)))
        .collect();
    let mut rendered = Row::new(cells);
    if selected {
        rendered = rendered.style(
            Style::default()
                .fg(theme.primary_text)
                .bg(theme.picker_selected_bg),
        );
    }
    rendered
}

/// One detail-mode row: the mode's columns for the sentinel or one
/// endpoint, with the same selection styling as the table.
fn detail_table_row(
    row: &DetailRow,
    model: Option<&MarketRow>,
    selected: bool,
    ui: &MarketUiState,
    theme: &Theme,
) -> Row<'static> {
    let cells: Vec<Cell<'static>> = DETAIL_COLUMNS
        .iter()
        .map(|col| Cell::from((col.cell)(row, model, ui, theme)))
        .collect();
    let mut rendered = Row::new(cells);
    if selected {
        rendered = rendered.style(
            Style::default()
                .fg(theme.primary_text)
                .bg(theme.picker_selected_bg),
        );
    }
    rendered
}

/// The MODEL column as a line: `author/slug` in the row's base style,
/// then the `:variant` suffix dimmed — the card's MODEL entry calls the
/// suffix a dimmed one, so the table renders one. The filter's match
/// highlight rides over both parts: a matched glyph wears the highlight
/// whatever segment it sits in, exactly as the unsplit render did.
fn model_column(row: &MarketRow, ui: &MarketUiState, theme: &Theme) -> Line<'static> {
    // Built from the fields rather than sliced off `row_name`: the
    // segments are the name's two halves, and `base.len()` is where the
    // name's byte offsets split between them.
    let base = format!("{}/{}", row.author, row.slug);
    let suffix = match &row.variant {
        Some(variant) => format!(":{variant}"),
        None => String::new(),
    };
    let base_len = base.len();
    let dim = Style::default().fg(theme.muted_text);
    let ranges = ui_state::match_ranges(row, ui);

    let mut spans = highlighted_segment(
        &base,
        Style::default(),
        ranges.as_deref(),
        0,
        theme.picker_highlight_bg,
    );
    spans.extend(highlighted_segment(
        &suffix,
        dim,
        ranges.as_deref(),
        base_len,
        theme.picker_highlight_bg,
    ));
    Line::from(spans)
}

/// One segment of the MODEL column as styled spans: `text` starts at
/// byte `name_offset` within the row's display name, so the filter's
/// match ranges (offsets into the whole name) rebasing to the segment
/// is a subtraction. A range cannot straddle a segment boundary — every
/// range is one byte wide and the boundary sits on a character edge.
fn highlighted_segment(
    text: &str,
    base_style: Style,
    ranges: Option<&[Range<usize>]>,
    name_offset: usize,
    highlight_bg: Color,
) -> Vec<Span<'static>> {
    let Some(ranges) = ranges else {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(text.to_owned(), base_style)]
        };
    };
    if text.is_empty() {
        return Vec::new();
    }
    let segment_end = name_offset + text.len();
    let within: Vec<Range<usize>> = ranges
        .iter()
        .filter(|r| r.start >= name_offset && r.end <= segment_end)
        .map(|r| (r.start - name_offset)..(r.end - name_offset))
        .collect();
    jinn_selection_widget::highlight_text_with_bg(text, base_style, &within, highlight_bg)
        .into_iter()
        .map(|span| Span::styled(span.content.into_owned(), span.style))
        .collect()
}

/// The FLAGS column: `†` pricing overrides, `f` `:free`, `#` missing
/// score data (no ECI match, no vals match — one flag per missing
/// source, so a row lacking both carries two).
fn flags_column(row: &MarketRow) -> String {
    let mut flags = String::new();
    if row.has_overrides {
        flags.push('\u{2020}');
    }
    if row.is_free {
        flags.push('f');
    }
    if row.eci.is_none() {
        flags.push('#');
    }
    if row.vals.is_none() {
        flags.push('#');
    }
    flags
}

/// The status line, drawn on the popup's bottom-inner row: the
/// "updated X ago" stamps (rendered from the stamps, no network) and
/// the degraded-mode/error line.
fn render_status_line(
    frame: &mut Frame<'_>,
    inner: Rect,
    data: &MarketDataState,
    ui: &MarketUiState,
    theme: &Theme,
) {
    let status_y = inner.y + inner.height.saturating_sub(STATUS_ROWS);
    let status_area = Rect::new(inner.x, status_y, inner.width, 1);
    let muted = Style::default().fg(theme.muted_text);

    let mut spans = Vec::new();
    if let Some(stamp) = data.models_fetched_at {
        spans.push(Span::styled(
            format!("models updated {} \u{b7} ", format::age_stamp(stamp)),
            muted,
        ));
    }
    // The active projection mix: which mix the PROJ column and its sort
    // are computed under, so a flipped source is visible at a glance.
    // Resolved to the fixed display whenever the value equals the
    // constant (including the usage fallback on an unmeasured tree).
    // The source word alone carries the green (`success`) — the numbers
    // stay muted beside it.
    let (input, hit, output) = ui.active_mix;
    let (source_word, numbers) =
        if input == PROJECTION_MIX.0 && hit == PROJECTION_MIX.1 && output == PROJECTION_MIX.2 {
            (
                "fixed",
                format!(
                    " {}M·{}%·{}k",
                    PROJECTION_MIX.0 / 1_000_000,
                    (PROJECTION_MIX.1 * 100.0) as u32,
                    PROJECTION_MIX.2 / 1_000
                ),
            )
        } else {
            (
                "tree",
                format!(
                    " {}M·{}%·{}k",
                    input / 1_000_000,
                    (hit * 100.0) as u32,
                    output / 1_000
                ),
            )
        };
    spans.push(Span::styled("mix: ", muted));
    spans.push(Span::styled(
        source_word,
        Style::default().fg(theme.success),
    ));
    spans.push(Span::styled(format!("{numbers} · "), muted));
    if data.models_loading || data.endpoints_loading || data.eci_loading || data.vals_loading {
        spans.push(Span::styled(" \u{b7} loading\u{2026}", muted));
    }
    if let Some(error) = &data.last_error {
        spans.push(Span::styled(
            format!(" \u{b7} {error}"),
            Style::default().fg(theme.accent_action),
        ));
    }
    frame.render_widget(Line::from(spans), status_area);
}

/// The keybind footer on the popup's last two inner rows, rendered from
/// [`MARKET_BINDINGS`] — the same table the bind test checks, so the
/// footer cannot advertise a key the popup does not bind.
///
/// The binding set does not fit one row at common widths, and a
/// wrap-at-width layout would move entries between rows as the terminal
/// resizes. The split is therefore deterministic: the first
/// [`FOOTER_FIRST_ROW`] entries on the upper row, the rest below, in
/// display order. At any popup width the same entry sits on the same
/// row. The picker pairing: keys in `accent_action`, labels in
/// `muted_text`.
fn render_footer(frame: &mut Frame<'_>, inner: Rect, theme: &Theme) {
    let footer_y = inner.y + inner.height.saturating_sub(2);
    let key_style = Style::default().fg(theme.accent_action);
    let desc_style = Style::default().fg(theme.muted_text);
    let (upper, lower) = MARKET_BINDINGS.split_at(FOOTER_FIRST_ROW);
    for (row_idx, bindings) in [upper, lower].into_iter().enumerate() {
        let row_y = footer_y + row_idx as u16;
        let area = Rect::new(inner.x, row_y, inner.width, 1);
        let mut spans = Vec::new();
        for (idx, (key, desc)) in bindings.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::styled(" \u{b7} ", desc_style));
            }
            spans.push(Span::styled((*key).to_owned(), key_style));
            spans.push(Span::styled(format!(" {desc}"), desc_style));
        }
        frame.render_widget(Line::from(spans), area);
    }
}

/// The popup's result-row count for a frame of the given size: the popup
/// height less both borders, the filter line, its separator, the
/// header, the status line, and the two footer rows. At least 1, so
/// paging never windows against zero. Both modes page over this same
/// geometry.
#[must_use]
pub fn results_viewport(popup: Rect) -> usize {
    block_inner(popup).saturating_sub(6).max(1) as usize
}

/// The first visible row index for a selection, centering it in a
/// viewport of `viewport` rows and clamping to the list's ends — the
/// sidebar layout's offset expression over the popup's table. Derived
/// fresh each frame from the selection alone, so a list that shrinks
/// or grows never leaves a stale window behind.
#[must_use]
pub fn centered_offset(selection: usize, row_count: usize, viewport: usize) -> usize {
    selection
        .saturating_sub(viewport / 2)
        .min(row_count.saturating_sub(viewport))
}

/// The frame's usage numbers for [`ui_state::resolve_mix`], read from
/// the render facts the kernel seeds each frame. `None` when any fact
/// is missing or unparseable — an absent usage degrades to the fixed
/// mix, never to a wrong one.
fn usage_mix_inputs(facts: &RenderFacts) -> Option<(u64, u64, u64, u64)> {
    let sent = facts
        .fact("session.tree-effective-sent")?
        .parse::<u64>()
        .ok()?;
    let received = facts
        .fact("session.tree-total-received")?
        .parse::<u64>()
        .ok()?;
    let cached = facts
        .fact("session.tree-cached-total")?
        .parse::<u64>()
        .ok()?;
    let measured = facts
        .fact("session.tree-measured-sent")?
        .parse::<u64>()
        .ok()?;
    Some((sent, received, cached, measured))
}

/// The popup's inner area: the rect less its border. Shared by the
/// viewport measurement so every subtraction names the same chrome.
fn block_inner(popup: Rect) -> u16 {
    popup.height.saturating_sub(2)
}
