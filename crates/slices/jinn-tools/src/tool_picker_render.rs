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

//! The tool picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render context
//! deliberately never sees `AppState` (see `RenderFacts`), so this reads
//! `ToolPickerState` straight off the registered slot — no `PickerHost`, no
//! kernel borrow, and no picker-kind lookup in the kernel.

use jinn_slices::RenderFacts;
use jinn_theme::Theme;
use jinn_tools_msg::ToolPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The popup rect for the tool picker: the shared selection-widget popup.
#[must_use]
pub fn tool_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the tool picker draws with.
///
/// Mirrors the widget's defaults, matching what the kernel-side spec render
/// produced: the chrome fields are unthemed, the accent fields come from the
/// active theme.
#[must_use]
pub fn tool_picker_palette(theme: &Theme) -> jinn_picker::Palette {
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

/// The tool picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn tool_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::tool_picker_routes::TOOL_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// The status line: the live enabled count and the pending context cost,
/// e.g. `3/7 enabled · 4.2k ctx`.
///
/// The cost figure is the picker's *pending* state — what the next prompt pays
/// for the enabled toolset if the picker were confirmed right now — counted
/// with the assembler's shaping at the last open or toggle.
#[must_use]
pub fn tool_status(state: &ToolPickerState, theme: &Theme) -> Line<'static> {
    let (enabled, total) = crate::tool_picker_actions::enabled_count(state);
    let cost = jinn_picker::format_compact(state.context_tokens);
    Line::from(Span::styled(
        format!("{enabled}/{total} enabled · {cost} ctx"),
        Style::default().fg(theme.muted_text),
    ))
}

/// Draws the tool picker popup for one frame.
///
/// The state is read under one guard that spans footer construction and the
/// widget's render, and the cell's `results_viewport` is written only after
/// that guard drops — `TypedCell`'s `parking_lot::RwLock` is not reentrant,
/// and same-thread write-while-read deadlocks.
pub fn render_tool_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts.slices.reader(&jinn_tools_msg::tool_picker_slot()) else {
        return;
    };

    let layout = {
        let guard = cell.read();
        let state: &ToolPickerState = &guard;

        let theme = &facts.theme;
        let palette = tool_picker_palette(theme);
        let binds = tool_picker_binds();
        let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);
        let footers = vec![tool_status(state, theme), Line::from(keybind.0)];

        let widget = jinn_selection_widget::SelectionWidget::new(&state.selection)
            .title(Line::from(" Tools "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors());

        // What the widget solved for this frame; `inner` is the rect the
        // result rows rendered into, so `.height` is the visible row count.
        widget.render(frame, area)
    };

    // Publish the layout for the navigation keys and the toggle's advance,
    // which need a real row count to keep the highlight on screen. This is
    // the slice-owned equivalent of the kernel's per-frame viewport write.
    cell.update(|state: &mut ToolPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}
