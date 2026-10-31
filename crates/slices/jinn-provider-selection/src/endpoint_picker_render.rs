// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The OpenRouter endpoint picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render context
//! deliberately never sees `AppState` (see `RenderFacts`), so this reads
//! `EndpointPickerState` straight off the registered slot — no `PickerHost`, no
//! kernel borrow, and no picker-kind lookup in the kernel.

use jinn_provider_selection_msg::EndpointPickerState;
use jinn_provider_selection_msg::endpoint::picker_entry::EndpointEntry;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

/// The popup rect for the endpoint picker: the shared selection-widget popup.
#[must_use]
pub fn endpoint_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the endpoint picker draws with.
///
/// Mirrors the widget's defaults, matching what the kernel-side spec render
/// produced: the chrome fields are unthemed, the accent fields come from the
/// active theme.
#[must_use]
pub fn endpoint_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
    jinn_picker::Palette {
        border: ratatui::style::Color::DarkGray,
        filter_text: ratatui::style::Color::White,
        separator: ratatui::style::Color::DarkGray,
        footer: ratatui::style::Color::DarkGray,
        highlight_bg: ratatui::style::Color::DarkGray,
        muted_text: theme.muted_text,
        accent_action: theme.accent_action,
        popup_title: theme.popup_title,
        primary_text: theme.primary_text,
    }
}

/// The endpoint picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn endpoint_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::endpoint_picker_routes::ENDPOINT_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Renders one picker row: `● name  (tag)` with the active marker bold.
///
/// The filter's matches are highlighted within the name.
#[must_use]
pub fn endpoint_row(entry: &EndpointEntry, ctx: &jinn_picker::RowCtx<'_>) -> Line<'static> {
    let theme = &entry.theme;
    let active_marker = Span::styled(
        if entry.is_active { "\u{25cf} " } else { "  " },
        if entry.is_active {
            Style::default()
                .fg(theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    );

    let label_style = if ctx.is_selected {
        Style::default()
            .fg(theme.primary_text)
            .bg(theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let suffix = if entry.tag.is_empty() {
        "(auto-route)".to_owned()
    } else {
        format!("({})", entry.tag)
    };

    let name_spans = if ctx.match_ranges.is_empty() {
        vec![Span::styled(
            format!("{}  ", entry.provider_name),
            label_style,
        )]
    } else {
        let mut spans = jinn_selection_widget::highlight_text_with_bg(
            &entry.provider_name,
            label_style,
            ctx.match_ranges,
            theme.picker_highlight_bg,
        );
        spans.push(Span::styled("  ".to_owned(), label_style));
        spans
    };

    let mut all_spans = vec![active_marker];
    all_spans.extend(name_spans);
    all_spans.push(Span::styled(suffix, label_style));
    Line::from(all_spans)
}

/// Renders the preview pane for the selected endpoint: its routing tag,
/// uptime, quantization, and pricing — or a one-line explanation for the
/// auto-route sentinel.
#[must_use]
pub fn endpoint_preview(
    entry: &EndpointEntry,
    _ctx: &jinn_picker::PreviewCtx<'_>,
) -> Vec<Line<'static>> {
    // The auto-route sentinel has no metadata; show a one-line explanation.
    if entry.tag.is_empty() {
        return vec![
            Line::from("Let OpenRouter choose the upstream each turn.")
                .style(Style::default().fg(entry.theme.muted_text)),
        ];
    }

    let gray = Style::default().fg(entry.theme.muted_text);
    let primary = Style::default().fg(entry.theme.primary_text);
    let row = |label: &str, value: &str| {
        Line::from(vec![
            Span::styled(format!("{label}: "), gray),
            Span::styled(value.to_owned(), primary),
        ])
    };

    let uptime = entry
        .uptime_30m
        .map_or_else(|| "unknown".to_owned(), |u| format!("{u:.1}%"));
    let quant = entry
        .quantization
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let prompt = entry
        .prompt_price
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let completion = entry
        .completion_price
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let max_tokens = entry
        .max_completion_tokens
        .map_or_else(|| "unknown".to_owned(), |n| n.to_string());

    vec![
        row("Tag", &entry.tag),
        row("Uptime (30m)", &uptime),
        row("Quantization", &quant),
        row("Prompt price", &prompt),
        row("Completion price", &completion),
        row("Max completion", &max_tokens),
    ]
}

/// The status line: the pinned upstream (or auto-route) plus the fetch state.
///
/// While a fetch is in flight it shows a spinner-style indicator; otherwise
/// the cache's age, or that nothing has been fetched yet.
#[must_use]
pub fn endpoint_status(
    state: &EndpointPickerState,
    theme: &jinn_theme::Theme,
    loading: bool,
    fetched_at: Option<jiff::Timestamp>,
) -> Line<'static> {
    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);

    let pinned_name = crate::endpoint_picker_actions::active_routing_label(state)
        .unwrap_or_else(|| "auto-route".to_owned());

    let mut spans = vec![
        Span::styled("Routing: ".to_owned(), gray),
        Span::styled(pinned_name, Style::default().fg(theme.primary_text)),
        Span::styled("  ".to_owned(), gray),
    ];

    if loading {
        spans.push(Span::styled("fetching\u{2026}".to_owned(), orange));
    } else if let Some(ts) = fetched_at {
        spans.push(Span::styled(
            format!("fetched {}", crate::endpoint_picker_actions::format_age(ts)),
            gray,
        ));
    } else {
        spans.push(Span::styled("no fetch yet".to_owned(), gray));
    }

    Line::from(spans)
}

/// Draws the endpoint picker popup for one frame.
///
/// The popup's result-row count is not measured here — the widget solves the
/// layout (including its wide/narrow split decision) and reports it (see
/// `jinn_selection_widget::PickerLayout`); this writes the reported
/// `inner.height` into the cell so the navigation keys page by what the last
/// frame actually laid out.
pub fn render_endpoint_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_provider_selection_msg::endpoint_picker_slot())
    else {
        return;
    };

    let theme = &facts.theme;
    let palette = endpoint_picker_palette(theme);
    let binds = endpoint_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

    // The fetch state lives in the provider cell, which the render context can
    // also read — so the status line stays live without app state.
    let provider_cell: Option<
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    > = facts
        .slices
        .reader(&jinn_provider_selection_msg::provider_state_slot());
    let (loading, fetched_at) = provider_cell.map_or((false, None), |cell| {
        let guard = cell.read();
        (guard.endpoint_loading, guard.endpoint_fetched_at)
    });

    let (footers, preview_scroll) = {
        let guard = cell.read();
        let state: &EndpointPickerState = &guard;
        let footers = vec![
            endpoint_status(state, theme, loading, fetched_at),
            Line::from(keybind.0),
        ];
        (footers, state.preview_scroll)
    };

    // `PreviewSelectionWidget`, not `SelectionWidget`: the endpoint menu has a
    // detail pane (routing tag, uptime, quantization, pricing). The plain
    // widget has no pane to draw it in, so the rows rendered bare.
    //
    // The widget solves the layout — its wide/narrow split included — and
    // reports it; the guard ends before the cell write because `TypedCell`'s
    // `parking_lot::RwLock` is not reentrant and a same-thread
    // write-while-read would deadlock. The layout is `Copy`, so it outlives
    // the guard cleanly.
    let (layout, report) = {
        let guard = cell.read();
        let mut report = None;
        let layout = jinn_selection_widget::PreviewSelectionWidget::new(&guard.selection)
            .title(Line::from(" OpenRouter Endpoint "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .preview_scroll(preview_scroll)
            .preview_report(&mut report)
            .render(frame, area);
        (layout, report)
    };
    cell.update(|state: &mut EndpointPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
        // The pane's measured geometry is this frame's clamp bound; clamping
        // the stored offset here is what keeps a stale oversized scroll —
        // including one held over from a shorter detail list — from
        // surviving past the first frame that shows the new content.
        if let Some(report) = report {
            state.preview_max_scroll = report.max_scroll();
            state.preview_scroll = state.preview_scroll.min(state.preview_max_scroll);
        }
    });
}
