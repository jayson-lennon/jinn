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

//! The task-list picker's overlay rendering.
//!
//! A tree menu, so it renders through `TreePickerWidget` rather than the flat
//! `SelectionWidget` — that is what gives phases their roots, tasks their
//! indent, and the connector between them.
//!
//! Each frame, the widget reports the `PickerLayout` it solved; the renderer
//! publishes `layout.inner.height` — the true visible row count — into the
//! picker cell's `results_viewport`, so the navigation keys page by what the
//! last frame actually showed.
//!
//! The render context deliberately never sees `AppState` (see `RenderFacts`),
//! so this reads `TaskListPickerState` straight off the registered slot.

use jinn_selection_widget::TreeItem;
use jinn_slices::RenderFacts;
use jinn_tools_msg::TaskListPickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the task-list picker.
#[must_use]
pub fn task_list_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the task-list picker draws with.
#[must_use]
pub fn task_list_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The task-list picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn task_list_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::task_list_picker_routes::TASK_LIST_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Draws the task-list picker popup for one frame.
pub fn render_task_list_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    // The task-list picker's cell — the single home for what it shows. Held
    // across footer construction and the tree render, dropped before the
    // viewport write that follows the render.
    let Some(cell) = facts
        .slices
        .reader::<TaskListPickerState>(&jinn_tools_msg::task_list_picker_slot())
    else {
        return;
    };
    let guard = cell.read();

    // The colors and binds shape the footer rows, and the footer rows decide
    // how many rows the tree gets — so they come first, before the widget
    // renders and reports the layout it solved.
    let theme = &facts.theme;
    let palette = task_list_picker_palette(theme);
    let binds = task_list_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

    // A read-only browser: the status line reports how much it is showing
    // rather than any selection consequence. `items()` is the full tree, so
    // this is the total rather than the filter's current subset — which is
    // what a reader wants from a status line ("24 tasks"), not the count of
    // whatever they happen to have typed.
    let (roots, children) = phase_and_task_counts(&guard);
    let status = Line::from(Span::styled(
        format!("{roots} phases, {children} tasks"),
        Style::default().fg(theme.muted_text),
    ));

    let footers = vec![status, Line::from(keybind.0)];

    // Render the tree and take the layout back — `inner.height` is the true
    // visible row count, chrome and footer block included in the widget's own
    // arithmetic, so the slice mirrors nothing. The guard must drop before the
    // cell write below: `TypedCell`'s `parking_lot::RwLock` is not reentrant,
    // and a same-thread write-while-read deadlocks.
    let layout = {
        let state = &guard;
        jinn_selection_widget::TreePickerWidget::new(&state.tree)
            .title(Line::from(" Task List "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .tree_prefix_color(theme.muted_text)
            .render(frame, area)
    };
    drop(guard);

    // Measure the popup's result rows and publish them for the navigation keys,
    // which need a real row count to keep the highlight on screen. The value is
    // the widget's own report, never a re-derived number.
    cell.update(|state: &mut TaskListPickerState| {
        state.results_viewport = layout.inner.height as usize;
    });
}

use ratatui::text::Span;

/// Counts the tree's roots and its non-root rows.
///
/// Roots are the phases; everything else is a task, so a parent/child split
/// is the only distinction the status line needs.
fn phase_and_task_counts(state: &TaskListPickerState) -> (usize, usize) {
    let mut roots = 0;
    let mut children = 0;
    for item in state.tree.items() {
        if item.entry().parent_id().is_some() {
            children += 1;
        } else {
            roots += 1;
        }
    }
    (roots, children)
}
