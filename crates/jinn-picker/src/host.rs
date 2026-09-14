//! The host seam — the crate's sole lens onto the kernel's state.

use jinn_core_types::SessionId;
use jinn_selection_widget::SelectionColors;
use ratatui::style::Color;

use crate::id::PickerId;

/// Theme-dependent colors the crate renders with, decoupled from the
/// kernel's `Theme` type (mirrors `SelectionColors` plus the footer accents
/// the keybind line needs).
#[derive(Debug, Clone)]
pub struct Palette {
    /// Popup border color.
    pub border: Color,
    /// Filter input text color.
    pub filter_text: Color,
    /// Separator line color.
    pub separator: Color,
    /// Footer text color.
    pub footer: Color,
    /// Fuzzy match highlight background color.
    pub highlight_bg: Color,
    /// Muted footer/status text (descriptions, separators between binds).
    pub muted_text: Color,
    /// Key tokens in the keybind line.
    pub accent_action: Color,
    /// Popup title color (when the theme styles titles distinctly).
    pub popup_title: Color,
    /// Primary list text color.
    pub primary_text: Color,
}

/// The minimal state surface a picker spec may read or write.
///
/// Implemented by the kernel: typed storage is lent as `dyn Any` (specs
/// downcast to the exact `SelectionState<PickerEntry<T>>` they own), and
/// anything not yet on this trait flows through [`PickerHost::state_any`].
/// Operations shared by two or more migrated specs graduate onto named
/// methods permanently.
///
/// Domain-authored spec closures capture what they need at builder time and
/// borrow this host through the action contexts; the crate never depends on
/// the kernel crate.
pub trait PickerHost {
    /// Mutable lend of the picker's selection storage (the kernel dispatches
    /// `PickerId` → its typed field and hands it out as `dyn Any`).
    fn selection_state(&mut self, id: PickerId) -> Option<&mut dyn std::any::Any>;

    /// Read-only lend of the picker's selection storage.
    fn selection_state_ref(&self, id: PickerId) -> Option<&dyn std::any::Any>;

    /// The full kernel state, for spec-authored downcasts — the sanctioned
    /// escape hatch while the pilot migrates; shared operations graduate
    /// onto named trait methods instead of staying here forever.
    fn state_any(&mut self) -> &mut dyn std::any::Any;

    /// Read-only full kernel state.
    fn state_any_ref(&self) -> &dyn std::any::Any;

    /// The live theme colors for this frame.
    fn palette(&self) -> Palette;

    /// The active session's identifier.
    fn session_id(&self) -> SessionId;

    /// The stored preview scroll for `id`.
    fn preview_scroll(&self, id: PickerId) -> usize;

    /// Stores the preview scroll for `id`.
    fn set_preview_scroll(&mut self, id: PickerId, scroll: usize);

    /// Clears the preview scroll for `id` (next read returns 0).
    fn reset_preview_scroll(&mut self, id: PickerId);
}

impl Palette {
    /// Converts to the selection widget's color bundle (shared fields only).
    #[must_use]
    pub fn selection_colors(&self) -> SelectionColors {
        SelectionColors {
            border: self.border,
            filter_text: self.filter_text,
            separator: self.separator,
            footer: self.footer,
            highlight_bg: self.highlight_bg,
        }
    }
}
