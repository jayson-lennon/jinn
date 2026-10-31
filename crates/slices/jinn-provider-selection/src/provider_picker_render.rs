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

//! The model picker's overlay rendering.

use jinn_provider_selection_msg::ProviderPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The popup rect for the model picker.
#[must_use]
pub fn provider_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the model picker draws with.
#[must_use]
pub fn provider_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The model picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn provider_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::provider_picker_routes::PROVIDER_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// The status line: the model cache's age plus the live alloy-mode state
/// (`N selected` while alloy is on). The keybind row above it advertises the
/// keys; this line carries the dynamic state.
#[must_use]
pub fn provider_status(
    selected_count: usize,
    alloy_mode: bool,
    cache_age: Option<jiff::Timestamp>,
    theme: &jinn_theme::Theme,
) -> Line<'static> {
    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);

    let mut spans = Vec::new();
    if let Some(ts) = cache_age {
        spans.push(Span::styled(
            format!(
                "updated {} ago",
                crate::endpoint_picker_actions::format_age(ts)
            ),
            gray,
        ));
    }
    if alloy_mode {
        spans.push(Span::styled(
            format!("alloy \u{00b7} {selected_count} selected"),
            orange,
        ));
    } else {
        spans.push(Span::styled("single model".to_owned(), gray));
    }
    Line::from(spans)
}

/// Draws the model picker popup for one frame.
///
/// The popup's result-row count is not measured here — the widget solves the
/// layout and reports it (see `jinn_selection_widget::PickerLayout`); this
/// writes the reported `inner.height` into the cell so the navigation keys
/// page by what the last frame actually laid out.
pub fn render_provider_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_provider_selection_msg::provider_picker_slot())
    else {
        return;
    };

    let theme = &facts.theme;
    let palette = provider_picker_palette(theme);
    let keybind = jinn_picker::keybind_line(
        &provider_picker_binds(),
        jinn_picker::Tail::Standard,
        &palette,
    );

    // Alloy mode and the cache stamp live in the shared provider cell, which
    // the render context reads too — so the status line stays live without app
    // state.
    let provider_cell: Option<
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    > = facts
        .slices
        .reader(&jinn_provider_selection_msg::provider_state_slot());
    let (alloy_mode, cache_age) = provider_cell.map_or((false, None), |cell| {
        let guard = cell.read();
        (
            guard.is_alloy_mode(),
            guard.model_cache.as_ref().and_then(|c| c.last_updated_at),
        )
    });

    let footers = {
        let guard = cell.read();
        let state: &ProviderPickerState = &guard;
        let selected_count = crate::provider_picker_actions::selected_count(state);
        vec![
            provider_status(selected_count, alloy_mode, cache_age, theme),
            Line::from(keybind.0),
        ]
    };

    // The widget solves the layout and reports it; the guard ends before the
    // cell write because `TypedCell`'s `parking_lot::RwLock` is not
    // reentrant and a same-thread write-while-read would deadlock. The
    // layout is `Copy`, so it outlives the guard cleanly.
    let layout = {
        let guard = cell.read();
        jinn_selection_widget::SelectionWidget::new(&guard.selection)
            .title(Line::from(" Model "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .render(frame, area)
    };
    cell.update(|state: &mut ProviderPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}
