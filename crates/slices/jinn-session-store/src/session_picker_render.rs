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

//! The session picker's overlay rendering.
//!
//! A tree menu, so it renders through `TreePickerWidget`: that is what nests
//! subagent sessions under their parents.

use jinn_session_store_msg::SessionPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The popup rect for the session picker.
#[must_use]
pub fn session_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the session picker draws with.
#[must_use]
pub fn session_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The session picker's declared binds, in footer order.
#[must_use]
pub fn session_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::session_picker_routes::SESSION_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// The widget the session picker draws, fully configured.
///
/// Shared by the render pass and the tests, so a test's identical second
/// construction is identical by construction rather than by transcription.
#[must_use]
pub fn session_picker_widget<'a>(
    state: &'a SessionPickerState,
    theme: &jinn_theme::Theme,
) -> jinn_selection_widget::TreePickerWidget<
    'a,
    jinn_picker::PickerEntry<jinn_session_store_msg::SessionTreeEntry>,
> {
    let palette = session_picker_palette(theme);
    let keybind = jinn_picker::keybind_line(
        &session_picker_binds(),
        jinn_picker::Tail::Standard,
        &palette,
    );

    // The status line is a hint, not a state readout: `Ctrl+N` creates a
    // session from inside the picker, which is otherwise impossible while a
    // picker holds input.
    let status = Line::from(vec![
        Span::styled(
            "CTRL+N ".to_owned(),
            Style::default().fg(theme.accent_action),
        ),
        Span::styled(
            "to create a new session".to_owned(),
            Style::default().fg(theme.muted_text),
        ),
    ]);

    let footers = vec![status, Line::from(keybind.0)];

    jinn_selection_widget::TreePickerWidget::new(&state.tree)
        .title(Line::from(" Sessions "))
        .title_style(Style::default().fg(theme.popup_title))
        .footers(footers)
        .colors(palette.selection_colors())
        .tree_prefix_color(theme.muted_text)
}

/// Draws the session picker popup for one frame.
pub fn render_session_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader::<SessionPickerState>(&jinn_session_store_msg::session_picker_slot())
    else {
        return;
    };

    // The widget reports the layout it actually solved; the guard must end
    // before the write below (`TypedCell`'s `parking_lot::RwLock` is not
    // reentrant on the same thread).
    let layout = {
        let guard = cell.read();
        let state = &guard;
        session_picker_widget(state, &facts.theme).render(frame, area)
    };

    // Publish the reported row count for the navigation keys.
    cell.update(|state: &mut SessionPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}
