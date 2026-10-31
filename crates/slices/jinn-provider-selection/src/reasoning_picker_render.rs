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

//! The reasoning picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render context
//! deliberately never sees `AppState` (see `RenderFacts`), so this reads
//! `ReasoningPickerState` straight off the registered slot — no `PickerHost`,
//! no kernel borrow, and no picker-kind lookup in the kernel.

use jinn_provider_selection_msg::ReasoningPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the reasoning picker: the shared selection-widget popup.
#[must_use]
pub fn reasoning_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the reasoning picker draws with.
///
/// Mirrors the widget's defaults, matching what the kernel-side spec render
/// produced: the chrome fields are unthemed, the accent fields come from the
/// active theme.
#[must_use]
pub fn reasoning_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The reasoning picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn reasoning_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::reasoning_picker_routes::REASONING_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Draws the reasoning picker popup for one frame.
///
/// The popup's result-row count is not measured here — the widget solves the
/// layout and reports it (see `jinn_selection_widget::PickerLayout`); this
/// writes the reported `inner.height` into the cell so the navigation keys
/// page by what the last frame actually laid out.
pub fn render_reasoning_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_provider_selection_msg::reasoning::reasoning_picker_slot())
    else {
        return;
    };

    let theme = &facts.theme;
    let palette = reasoning_picker_palette(theme);
    let binds = reasoning_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);
    let footers = {
        let guard = cell.read();
        vec![
            crate::reasoning_picker_actions::reasoning_status(&guard, theme),
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
            .title(Line::from(" Reasoning Effort "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .render(frame, area)
    };
    cell.update(|state: &mut ReasoningPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}
