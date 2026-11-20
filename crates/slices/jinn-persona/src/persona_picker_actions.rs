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

//! The persona picker's row building and lifecycle operations.
//!
//! Pure functions over the picker's own state. They read what they need from
//! the slice's personas cell and the session, mutate the picker, and return
//! what the caller needs to turn into messages — they never reach session
//! state themselves, which is what keeps the picker inside its slice.

use jinn_persona_msg::{PersonaEntry, PersonaPickerState, PersonaRow, persona_row};
use jinn_picker::make_items_with_hooks;
use jinn_slices::Persona;
use jinn_theme::Theme;

/// Builds the picker's rows from the scanned personas.
///
/// The row list is the shared vocabulary's ([`PersonaRow::rows`]); this
/// adapter attaches the theme so the widget's row hook can render. The
/// active persona is marked inside the shared builder, so the menu
/// matches every other persona list in the app.
pub fn build_persona_entries(
    personas: &[Persona],
    active: Option<&str>,
    theme: &Theme,
) -> Vec<PersonaEntry> {
    PersonaRow::rows(personas, active)
        .into_iter()
        .map(|row| PersonaEntry {
            name: row.name,
            description: row.description,
            is_active: row.is_active,
            theme: theme.clone(),
        })
        .collect()
}

/// Wraps entries for the selection widget, wiring the row renderer and the
/// text the filter matches against.
///
/// Without these hooks the widget draws an empty label on every row, so they
/// are part of the picker's definition rather than decoration.
pub fn wrap_entries(entries: Vec<PersonaEntry>) -> Vec<jinn_picker::PickerEntry<PersonaEntry>> {
    make_items_with_hooks(
        entries,
        jinn_picker::PickerItemHooks::new()
            .row(persona_row)
            .search(|entry: &PersonaEntry| format!("{} {}", entry.name, entry.description)),
    )
}

/// Opens the picker: fresh filter and highlight over the given personas.
pub fn open(
    state: &mut PersonaPickerState,
    personas: &[Persona],
    active: Option<&str>,
    theme: &Theme,
) {
    state.selection.reset();
    state.theme = theme.clone();
    state
        .selection
        .set_items(wrap_entries(build_persona_entries(personas, active, theme)));
}
