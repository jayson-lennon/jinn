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
use crate::feat::picker::registry::ENDPOINT_ID;
use crate::feat::picker::registry::MCP_SERVER_ID;
use crate::feat::picker::registry::PERSONA_ID;
use crate::feat::picker::registry::PLUGIN_ID;
use crate::feat::picker::registry::PROJECT_ID;
use crate::feat::picker::registry::PROVIDER_ID;
use crate::feat::picker::registry::REASONING_EFFORT_ID;
use crate::feat::picker::registry::SESSION_ID;
use crate::feat::picker::registry::SESSION_LIFECYCLE_ID;
use crate::feat::picker::registry::SKILL_ID;
use crate::feat::picker::registry::TASK_LIST_ID;
use crate::feat::picker::registry::THEME_ID;
use crate::feat::picker::registry::TOOL_ID;
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
            THEME_ID => Some(self.state.frontend.theme_picker_mut() as &mut dyn std::any::Any),
            TOOL_ID => Some(self.state.frontend.tool_picker_mut() as &mut dyn std::any::Any),
            MCP_SERVER_ID => {
                Some(self.state.frontend.mcp_server_picker_mut() as &mut dyn std::any::Any)
            }
            SESSION_LIFECYCLE_ID => {
                Some(self.state.frontend.session_lifecycle_picker_mut() as &mut dyn std::any::Any)
            }
            REASONING_EFFORT_ID => {
                Some(self.state.frontend.reasoning_effort_picker_mut() as &mut dyn std::any::Any)
            }
            PLUGIN_ID => Some(self.state.frontend.plugin_picker_mut() as &mut dyn std::any::Any),
            TASK_LIST_ID => {
                Some(self.state.frontend.task_list_picker_mut() as &mut dyn std::any::Any)
            }
            SESSION_ID => Some(self.state.frontend.session_picker_mut() as &mut dyn std::any::Any),
            PROVIDER_ID => Some(&mut self.state.provider.provider_picker as &mut dyn std::any::Any),
            ENDPOINT_ID => {
                Some(self.state.frontend.endpoint_picker_mut() as &mut dyn std::any::Any)
            }
            PROJECT_ID => Some(self.state.frontend.project_picker_mut() as &mut dyn std::any::Any),
            _ => None,
        }
    }

    fn selection_state_ref(&self, id: PickerId) -> Option<&dyn std::any::Any> {
        match id.as_str() {
            PERSONA_ID => Some(self.state.frontend.persona_picker() as &dyn std::any::Any),
            SKILL_ID => Some(self.state.frontend.skill_picker() as &dyn std::any::Any),
            THEME_ID => Some(self.state.frontend.theme_picker() as &dyn std::any::Any),
            TOOL_ID => Some(self.state.frontend.tool_picker() as &dyn std::any::Any),
            MCP_SERVER_ID => Some(self.state.frontend.mcp_server_picker() as &dyn std::any::Any),
            SESSION_LIFECYCLE_ID => {
                Some(self.state.frontend.session_lifecycle_picker() as &dyn std::any::Any)
            }
            REASONING_EFFORT_ID => {
                Some(self.state.frontend.reasoning_effort_picker() as &dyn std::any::Any)
            }
            PLUGIN_ID => Some(self.state.frontend.plugin_picker() as &dyn std::any::Any),
            TASK_LIST_ID => Some(self.state.frontend.task_list_picker() as &dyn std::any::Any),
            SESSION_ID => Some(self.state.frontend.session_picker() as &dyn std::any::Any),
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
        self.state.frontend.pickers.pickers_scrolls.get(id)
    }

    fn set_preview_scroll(&mut self, id: PickerId, scroll: usize) {
        self.state.frontend.pickers.pickers_scrolls.set(id, scroll);
    }

    fn reset_preview_scroll(&mut self, id: PickerId) {
        self.state.frontend.pickers.pickers_scrolls.reset(id);
    }

    fn preview_cache(&self, id: PickerId) -> Option<jinn_picker::SharedPreviewCache> {
        match id.as_str() {
            SKILL_ID => Some(
                std::sync::Arc::clone(&self.state.frontend.caches.skill_preview_cache)
                    as jinn_picker::SharedPreviewCache,
            ),
            _ => None,
        }
    }
}

/// Read-only lens over [`AppState`] for the render path, where no mutable
/// access exists (the render pass holds only a read guard). Read-side host
/// operations are answered; mutable lends are not.
pub struct AppStateRenderHost<'a> {
    state: &'a AppState,
}

impl<'a> AppStateRenderHost<'a> {
    /// Wraps the render pass's state snapshot.
    #[must_use]
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

impl PickerHost for AppStateRenderHost<'_> {
    fn selection_state(&mut self, _id: PickerId) -> Option<&mut dyn std::any::Any> {
        None // render never mutates through this lens
    }

    fn selection_state_ref(&self, id: PickerId) -> Option<&dyn std::any::Any> {
        match id.as_str() {
            PERSONA_ID => Some(self.state.frontend.persona_picker() as &dyn std::any::Any),
            SKILL_ID => Some(self.state.frontend.skill_picker() as &dyn std::any::Any),
            THEME_ID => Some(self.state.frontend.theme_picker() as &dyn std::any::Any),
            TOOL_ID => Some(self.state.frontend.tool_picker() as &dyn std::any::Any),
            MCP_SERVER_ID => Some(self.state.frontend.mcp_server_picker() as &dyn std::any::Any),
            SESSION_LIFECYCLE_ID => {
                Some(self.state.frontend.session_lifecycle_picker() as &dyn std::any::Any)
            }
            REASONING_EFFORT_ID => {
                Some(self.state.frontend.reasoning_effort_picker() as &dyn std::any::Any)
            }
            PLUGIN_ID => Some(self.state.frontend.plugin_picker() as &dyn std::any::Any),
            TASK_LIST_ID => Some(self.state.frontend.task_list_picker() as &dyn std::any::Any),
            SESSION_ID => Some(self.state.frontend.session_picker() as &dyn std::any::Any),
            PROVIDER_ID => Some(&self.state.provider.provider_picker as &dyn std::any::Any),
            ENDPOINT_ID => Some(self.state.frontend.endpoint_picker() as &dyn std::any::Any),
            PROJECT_ID => Some(self.state.frontend.project_picker() as &dyn std::any::Any),
            _ => None,
        }
    }

    fn state_any(&mut self) -> &mut dyn std::any::Any {
        unreachable!("AppStateRenderHost is read-only; specs must not call state_any in render")
    }

    fn state_any_ref(&self) -> &dyn std::any::Any {
        self.state
    }

    fn palette(&self) -> Palette {
        let theme = &self.state.frontend.theme;
        // Chrome fields mirror the selection widget's defaults, matching
        // [`AppStatePickerHost::palette`].
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
        self.state.frontend.pickers.pickers_scrolls.get(id)
    }

    fn set_preview_scroll(&mut self, _id: PickerId, _scroll: usize) {
        // Read-only lens: render never stores scrolls.
    }

    fn reset_preview_scroll(&mut self, _id: PickerId) {
        // Read-only lens: render never clears scrolls.
    }

    fn preview_cache(&self, id: PickerId) -> Option<jinn_picker::SharedPreviewCache> {
        match id.as_str() {
            SKILL_ID => Some(
                std::sync::Arc::clone(&self.state.frontend.caches.skill_preview_cache)
                    as jinn_picker::SharedPreviewCache,
            ),
            _ => None,
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

    fn test_persona(name: &str) -> crate::feat::persona::PersonaEntry {
        crate::feat::persona::PersonaEntry {
            name: name.to_owned(),
            description: String::new(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn selection_state_lends_typed_storage_by_id() {
        // Given a host state whose persona picker holds items.
        let mut state = AppState::default_with_scope_focus();
        let items = {
            let registry = crate::feat::picker::registry::build_picker_registry();
            registry
                .make_items(
                    crate::feat::picker::registry::PERSONA_ID,
                    vec![test_persona("a")],
                )
                .expect("persona spec is registered")
        };
        state.frontend.persona_picker_mut().set_items(items);

        // When lending the selection state for the persona id.
        let mapped = {
            let mut host = AppStatePickerHost::new(&mut state);
            host.selection_state(PickerId::new(PERSONA_ID))
                .expect("persona is mapped")
                .downcast_ref::<jinn_selection_widget::SelectionState<
                    jinn_picker::PickerEntry<crate::feat::persona::PersonaEntry>,
                >>()
                .is_some()
        };

        // Then the lend downcasts back to the wrapped selection storage.
        assert!(
            mapped,
            "persona lend should downcast to its wrapped SelectionState"
        );
    }

    #[rstest::rstest]
    #[test]
    fn unmapped_ids_lend_nothing() {
        // Given a default host state.
        let mut state = AppState::default_with_scope_focus();
        let mut host = AppStatePickerHost::new(&mut state);

        // When lending an id no picker claims.
        // Then nothing is returned.
        assert!(host.selection_state(PickerId::new("nope")).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn skill_scrolls_use_the_legacy_slot_until_migration() {
        // Given a host state.
        let mut state = AppState::default_with_scope_focus();
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
                state.frontend.pickers.pickers_scrolls.get(skill),
            )
        };

        // Then both scrolls live in the shared map, keyed by picker id.
        assert_eq!(skill_scroll, 7);
        assert_eq!(stored, 7);
        assert_eq!(other_scroll, 3);
    }
}
