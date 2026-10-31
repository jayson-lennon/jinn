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

//! The persona picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render context
//! deliberately never sees `AppState` (see `RenderFacts`), so this reads
//! `PersonaPickerState` straight off the registered slot — no `PickerHost`, no
//! kernel borrow, and no picker-kind lookup in the kernel.

use jinn_persona_msg::PersonaPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the persona picker: the shared selection-widget popup.
#[must_use]
pub fn persona_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the persona picker draws with.
///
/// Mirrors the widget's defaults, matching what the kernel-side spec render
/// produced: the chrome fields are unthemed, the accent fields come from the
/// active theme.
#[must_use]
pub fn persona_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The persona picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn persona_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::persona_picker_routes::PERSONA_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Draws the persona picker popup for one frame.
pub fn render_persona_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_persona_msg::persona_picker_slot())
    else {
        return;
    };

    let theme = &facts.theme;
    let palette = persona_picker_palette(theme);
    let binds = persona_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

    // Render under the read guard, then publish the layout after the block
    // ends: `TypedCell`'s `parking_lot::RwLock` is not reentrant, and a
    // same-thread write while the read guard is alive deadlocks.
    let layout = {
        let guard = cell.read();
        let state: &PersonaPickerState = &guard;
        let footers = vec![Line::from(String::new()), Line::from(keybind.0)];

        jinn_selection_widget::SelectionWidget::new(&state.selection)
            .title(Line::from(" Personas "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .render(frame, area)
    };

    // The widget reports the rects it actually solved; the navigation keys
    // page against that row count. This is the slice-owned equivalent of the
    // kernel's per-frame viewport write — measured by the widget itself
    // rather than mirrored arithmetic.
    cell.update(|state: &mut PersonaPickerState| {
        state.results_viewport = layout.inner.height as usize;
    });
}
