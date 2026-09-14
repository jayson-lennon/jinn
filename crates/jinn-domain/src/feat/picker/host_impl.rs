//! The kernel's [`PickerHost`] implementation — the picker crate's lens onto
//! `AppState`.
//!
//! Typed selection storage is lent as `dyn Any` (specs downcast to the
//! exact `SelectionState<PickerEntry<T>>` they own); anything not yet on
//! the trait flows through `state_any`. This is the *only* place that maps
//! picker ids onto the kernel's typed picker fields, so adding a field
//! migration is a one-match change here.

use jinn_picker::Palette;
use jinn_picker::PickerHost;
use jinn_picker::PickerId;

use crate::common::app_state::AppState;
use crate::feat::picker::registry::PERSONA_ID;
use crate::feat::picker::registry::SKILL_ID;
use crate::feat::ui::picker_states::PickerExt;

/// The host lens over the kernel state. Constructed transiently at
/// dispatch/render with `&mut AppState` — it never outlives the guard.
pub struct AppStatePickerHost<'a> {
    state: &'a mut AppState,
}

impl<'a> AppStatePickerHost<'a> {
    /// Wraps the kernel state.
    #[must_use]
    pub fn new(state: &'a mut AppState) -> Self {
        Self { state }
    }
}

impl PickerHost for AppStatePickerHost<'_> {
    fn selection_state(&mut self, id: PickerId) -> Option<&mut dyn std::any::Any> {
        match id.as_str() {
            PERSONA_ID => Some(self.state.frontend.persona_picker_mut() as &mut dyn std::any::Any),
            SKILL_ID => Some(self.state.frontend.skill_picker_mut() as &mut dyn std::any::Any),
            _ => None,
        }
    }

    fn selection_state_ref(&self, id: PickerId) -> Option<&dyn std::any::Any> {
        match id.as_str() {
            PERSONA_ID => Some(self.state.frontend.persona_picker() as &dyn std::any::Any),
            SKILL_ID => Some(self.state.frontend.skill_picker() as &dyn std::any::Any),
            _ => None,
        }
    }

    fn state_any(&mut self) -> &mut dyn std::any::Any {
        self.state as &mut dyn std::any::Any
    }

    fn state_any_ref(&self) -> &dyn std::any::Any {
        self.state as &dyn std::any::Any
    }

    fn palette(&self) -> Palette {
        let theme = &self.state.frontend.theme;
        // Chrome fields mirror the selection widget's defaults: the legacy
        // pickers never themed borders/filter/separator, and migrated
        // pickers must keep that look.
        Palette {
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

    fn session_id(&self) -> jinn_core_types::SessionId {
        self.state.session.active_session_id().clone()
    }

    fn preview_scroll(&self, id: PickerId) -> usize {
        match id.as_str() {
            // Legacy slot until the skill picker's migration completes.
            SKILL_ID => self.state.frontend.skill_preview_scroll(),
            _ => self.state.frontend.pickers.pickers_scrolls.get(id),
        }
    }

    fn set_preview_scroll(&mut self, id: PickerId, scroll: usize) {
        match id.as_str() {
            SKILL_ID => self.state.frontend.set_skill_preview_scroll(scroll),
            _ => self.state.frontend.pickers.pickers_scrolls.set(id, scroll),
        }
    }

    fn reset_preview_scroll(&mut self, id: PickerId) {
        match id.as_str() {
            SKILL_ID => self.state.frontend.set_skill_preview_scroll(0),
            _ => self.state.frontend.pickers.pickers_scrolls.reset(id),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::registry::PERSONA_ID;
    use jinn_picker::PickerScrolls;

    /// Lent-storage fake: hands out typed selection state by id.
    struct FakeHost {
        states: std::collections::HashMap<String, jinn_selection_widget::SelectionState<crate::feat::persona::PersonaEntry>>,
        scrolls: PickerScrolls,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                states: std::collections::HashMap::new(),
                scrolls: PickerScrolls::default(),
            }
        }
    }

    impl PickerHost for FakeHost {
        fn selection_state(&mut self, id: PickerId) -> Option<&mut dyn std::any::Any> {
            self.states.get_mut(id.as_str()).map(|s| s as &mut dyn std::any::Any)
        }

        fn selection_state_ref(&self, id: PickerId) -> Option<&dyn std::any::Any> {
            self.states.get(id.as_str()).map(|s| s as &dyn std::any::Any)
        }

        fn state_any(&mut self) -> &mut dyn std::any::Any {
            self as &mut dyn std::any::Any
        }

        fn state_any_ref(&self) -> &dyn std::any::Any {
            self as &dyn std::any::Any
        }

        fn palette(&self) -> Palette {
            test_support::test_palette()
        }

        fn session_id(&self) -> jinn_core_types::SessionId {
            jinn_core_types::SessionId::new()
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

    /// A minimal persona entry for storage lends.
    fn test_persona(name: &str) -> crate::feat::persona::PersonaEntry {
        crate::feat::persona::PersonaEntry {
            name: name.to_owned(),
            description: String::new(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        }
    }

    #[test]
    fn selection_state_lends_typed_storage_by_id() {
        // Given a host state whose persona picker holds items.
        let mut state = AppState::default();
        state
            .frontend
            .persona_picker_mut()
            .set_items(vec![test_persona("a")]);

        // When lending the selection state for the persona id.
        let mapped = {
            let mut host = AppStatePickerHost::new(&mut state);
            host.selection_state(PickerId::new(PERSONA_ID))
                .expect("persona is mapped")
                .downcast_ref::<jinn_selection_widget::SelectionState<crate::feat::persona::PersonaEntry>>()
                .is_some()
        };

        // Then the lend downcasts back to the typed selection state.
        assert!(mapped, "persona lend should downcast to its SelectionState");
    }

    #[test]
    fn unmapped_ids_lend_nothing() {
        // Given a default host state.
        let mut state = AppState::default();
        let mut host = AppStatePickerHost::new(&mut state);

        // When lending an id no picker claims.
        // Then nothing is returned.
        assert!(host.selection_state(PickerId::new("nope")).is_none());
    }

    #[test]
    fn skill_scrolls_use_the_legacy_slot_until_migration() {
        // Given a host state.
        let mut state = AppState::default();
        let skill = PickerId::new(SKILL_ID);
        let other = PickerId::new("other");

        // When setting preview scrolls for the skill id and another id.
        let (skill_scroll, other_scroll, stored) = {
            let mut host = AppStatePickerHost::new(&mut state);
            host.set_preview_scroll(skill, 7);
            host.set_preview_scroll(other, 3);
            (
                PickerHost::preview_scroll(&host, skill),
                PickerHost::preview_scroll(&host, other),
                state.frontend.skill_preview_scroll(),
            )
        };

        // Then the skill scroll flows through the legacy field and the
        // other id through the generic map.
        assert_eq!(skill_scroll, 7);
        assert_eq!(stored, 7);
        assert_eq!(other_scroll, 3);
    }
}

/// Test-support helpers for picker feature modules.
#[cfg(test)]
pub(crate) mod test_support {
    use jinn_picker::Palette;

    /// A stable chrome palette matching the selection widget defaults.
    #[must_use]
    pub fn test_palette() -> Palette {
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
}
