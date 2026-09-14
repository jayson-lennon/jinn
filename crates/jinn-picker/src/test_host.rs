//! Test support: a fake [`PickerHost`] lending in-memory selection storage.
//!
//! The fake mirrors the kernel's lending shape — storage lives in a map and
//! is handed out as `dyn Any` — so crate tests exercise the exact seam the
//! domain implements.

use std::any::Any;
use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_selection_widget::PickerItem;
use jinn_selection_widget::SelectionState;

use crate::host::Palette;
use crate::host::PickerHost;
use crate::id::PickerId;
use crate::scroll::PickerScrolls;

/// A test `PickerHost`: typed selection storage per picker id, scroll
/// tracking, and a recording palette.
pub(crate) struct FakeHost {
    states: HashMap<PickerId, Box<dyn Any>>,
    scrolls: PickerScrolls,
    kernel_probe: usize,
}

impl FakeHost {
    pub(crate) fn new() -> Self {
        Self {
            states: HashMap::new(),
            scrolls: PickerScrolls::default(),
            kernel_probe: 0,
        }
    }

    /// Registers (or replaces) the selection storage for `id`.
    pub(crate) fn set_selection<T>(&mut self, id: PickerId, state: SelectionState<T>)
    where
        T: PickerItem,
    {
        self.states.insert(id, Box::new(state));
    }

    /// Borrow of the registered selection storage for `id`.
    pub(crate) fn selection<T>(&self, id: PickerId) -> Option<&SelectionState<T>>
    where
        T: PickerItem,
    {
        self.states
            .get(&id)?
            .downcast_ref::<SelectionState<T>>()
    }
}

impl PickerHost for FakeHost {
    fn selection_state(&mut self, id: PickerId) -> Option<&mut dyn Any> {
        Some(self.states.get_mut(&id)?.as_mut())
    }

    fn selection_state_ref(&self, id: PickerId) -> Option<&dyn Any> {
        Some(self.states.get(&id)?.as_ref())
    }

    fn state_any(&mut self) -> &mut dyn Any {
        &mut self.kernel_probe
    }

    fn state_any_ref(&self) -> &dyn Any {
        &self.kernel_probe
    }

    fn palette(&self) -> Palette {
        Palette {
            border: ratatui::style::Color::DarkGray,
            filter_text: ratatui::style::Color::White,
            separator: ratatui::style::Color::DarkGray,
            footer: ratatui::style::Color::DarkGray,
            highlight_bg: ratatui::style::Color::DarkGray,
            muted_text: ratatui::style::Color::Gray,
            accent_action: ratatui::style::Color::LightRed,
            popup_title: ratatui::style::Color::Cyan,
            primary_text: ratatui::style::Color::White,
        }
    }

    fn session_id(&self) -> SessionId {
        SessionId::new()
    }

    fn preview_scroll(&self, id: PickerId) -> usize {
        self.scrolls.get(id)
    }

    fn set_preview_scroll(&mut self, id: PickerId, scroll: usize) {
        self.scrolls.set(id, scroll);
    }

    fn reset_preview_scroll(&mut self, id: PickerId) {
        self.scrolls.reset(id);
    }
}
