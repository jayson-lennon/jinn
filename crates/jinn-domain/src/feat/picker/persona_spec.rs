//! The persona picker's spec — behavior authored once in the builder.
//!
//! The persona picker keeps no snapshot: ESC simply closes (no revert).
//! Open resets the picker and asks the session actor for fresh entries;
//! confirm sets the active persona, binds it to the session, persists via
//! messages, and closes.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::PickerEntry;
use jinn_picker::StatusCtx;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::common::app_state::AppState;
use crate::feat::context::protocol::command::LoadPersonaPickerEntries;
use crate::feat::ui::picker_states::PickerExt;

use crate::feat::preferences_actor::protocol::app_state_command::{
    AppStateUpdate, UpdateAppState,
};
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;

/// The kernel entry this picker's items wrap in storage.
pub use crate::feat::persona::PersonaEntry;

/// Builds the persona picker's spec.
#[must_use]
pub fn persona_spec() -> PickerSpec<PersonaEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::PERSONA_ID))
        .title(" Personas ")
        .on_open(|ctx| {
            // Fresh filter + selection each open; the session actor fills
            // the picker with persona entries.
            ctx.selection::<jinn_selection_widget::SelectionState<PickerEntry<PersonaEntry>>>()
                .map(|picker| picker.reset());
            PickerOutcome::new_message(LoadPersonaPickerEntries)
        })
        .on_confirm(confirm_persona)
        .status(persona_status)
}


/// Enter on the persona picker: set the active persona, bind it to the
/// session, close, and persist both updates via messages.
fn confirm_persona(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let (persona_name, session_id) = {
        let state = ctx
            .state_any()
            .downcast_mut::<AppState>()
            .expect("domain host lends AppState");
        let Some(entry) = state.frontend.persona_picker().selected_item() else {
            return PickerOutcome::empty();
        };
        let persona_name = entry.entry().name.clone();

        // Find the matching persona and set it as active.
        let persona = state
            .context
            .personas()
            .iter()
            .find(|p| p.name == persona_name)
            .cloned();
        if let Some(p) = persona {
            state.context.set_active_persona(Some(p));
        }

        // Also update the active session's persona binding.
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_persona_name(persona_name.clone());
        (persona_name, session_id)
    };

    PickerOutcome::empty()
        .with_message(UpdateAppState {
            updates: vec![AppStateUpdate::SetPersona(Some(persona_name))],
        })
        .with_message(MarkSessionInteracted { session_id })
        .close()
}

/// The status line: the currently active persona (blank when none).
fn persona_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = ctx
        .state_any_ref()
        .downcast_ref::<AppState>()
        .expect("domain host lends AppState");
    let theme = &state.frontend.theme;
    let active_name = state.context.active_persona().map_or("none", |p| p.name.as_str());
    Some(Line::from(vec![
        ratatui::text::Span::styled(
            "Active: ".to_owned(),
            Style::default().fg(theme.muted_text),
        ),
        ratatui::text::Span::styled(
            active_name.to_owned(),
            Style::default().fg(theme.primary_text),
        ),
    ]))
}



#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::common::app_state::FocusScope;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::registry::PERSONA_ID;
    use crate::feat::session::ChatSessionState;
    use crate::protocol::PickerKind;
    use jinn_picker::ActionCtx;

    fn state_with_open_picker() -> AppState {
        let mut state = AppState::default();
        state.session.insert(ChatSessionState::new());
        state.session.set_active(state.session.active_session_id().clone());
        state
            .frontend
            .scope_stack
            .push(FocusScope::Picker { kind: PickerKind::Persona });
        state
    }

    fn persona(name: &str) -> crate::feat::persona::Persona {
        crate::feat::persona::Persona {
            name: name.to_owned(),
            description: String::new(),
            body: String::new(),
        }
    }

    fn wrap(state: &mut AppState, entries: Vec<PersonaEntry>) {
        let items = {
            let registry = crate::feat::picker::registry::build_picker_registry();
            registry
                .make_items(PERSONA_ID, entries)
                .expect("persona spec is registered")
        };
        state.frontend.persona_picker_mut().set_items(items);
    }

    fn test_entry(name: &str) -> PersonaEntry {
        PersonaEntry {
            name: name.to_owned(),
            description: "desc".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        }
    }

    #[test]
    fn on_open_emits_load_command_and_resets_state() {
        // Given an open persona picker with a dirty filter.
        let mut state = state_with_open_picker();
        wrap(
            &mut state,
            vec![test_entry("a"), test_entry("b")],
        );
        let spec = {
            let registry = crate::feat::picker::registry::build_picker_registry();
            registry.get(PERSONA_ID).expect("persona spec registered")
        };

        // When running the open hook.
        let mut host = AppStatePickerHost::new(&mut state);
        let mut ctx = ActionCtx::new(jinn_picker::PickerId::new(PERSONA_ID), &mut host);
        let outcome = spec.run_open(&mut ctx);
        drop(ctx);
        drop(host);

        // Then the load command is emitted.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("LoadPersonaPickerEntries")),
            "open must request persona entries; got {:?}",
            outcome.message_names
        );
        // And the picker stays open.
        assert!(!outcome.close);
    }

    #[test]
    fn on_confirm_sets_persona_and_session_binding_then_closes() {
        // Given an open persona picker with "writer" selected.
        let mut state = state_with_open_picker();
        state.context.set_personas(vec![persona("coder"), persona("writer")]);
        wrap(&mut state, vec![test_entry("coder"), test_entry("writer")]);
        state.frontend.persona_picker_mut().move_down(1);
        state.frontend.persona_picker_mut().move_down(1);

        // When running the confirm hook through the registry.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let spec = registry.get(PERSONA_ID).expect("persona spec registered");
        let outcome = {
            let mut host = AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(jinn_picker::PickerId::new(PERSONA_ID), &mut host);
            spec.run_confirm(&mut ctx)
        };

        // Then the active persona and the session binding are set.
        assert_eq!(
            state.context.active_persona().map(|p| p.name.as_str()),
            Some("writer"),
        );
        assert_eq!(
            state.active_session().profile().persona_name,
            "writer",
        );
        // And the outcome closes the picker and persists both updates.
        assert!(outcome.close, "confirm must close the picker");
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("UpdateAppState")),
            "confirm must emit UpdateAppState: {:?}",
            outcome.message_names
        );
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("MarkSessionInteracted")),
            "confirm must emit MarkSessionInteracted: {:?}",
            outcome.message_names
        );
    }

    #[test]
    fn on_confirm_with_no_selection_is_a_no_op() {
        // Given an open persona picker with no items.
        let mut state = state_with_open_picker();
        let registry = crate::feat::picker::registry::build_picker_registry();
        let spec = registry.get(PERSONA_ID).expect("persona spec registered");

        // When running the confirm hook.
        let outcome = {
            let mut host = AppStatePickerHost::new(&mut state);
            let mut ctx = ActionCtx::new(jinn_picker::PickerId::new(PERSONA_ID), &mut host);
            spec.run_confirm(&mut ctx)
        };

        // Then nothing is emitted and the picker stays open.
        assert!(outcome.messages.is_empty());
        assert!(!outcome.close);
    }

    #[test]
    fn status_line_names_the_active_persona() {
        // Given an app state with "coder" active.
        let state = {
            let mut state = AppState::default();
            state.context.set_active_persona(Some(persona("coder")));
            state
        };
        let registry = crate::feat::picker::registry::build_picker_registry();
        let spec = registry.get(PERSONA_ID).expect("persona spec registered");
        let host = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
        let ctx = jinn_picker::StatusCtx::new(jinn_picker::PickerId::new(PERSONA_ID), &host);

        // When reading the status line.
        let line = spec.status_line(&ctx).expect("status declared");

        // Then it names the active persona.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "Active: coder");
    }

    #[test]
    fn status_line_says_none_when_no_persona_active() {
        // Given an app state with no active persona.
        let state = AppState::default();
        let registry = crate::feat::picker::registry::build_picker_registry();
        let spec = registry.get(PERSONA_ID).expect("persona spec registered");
        let host = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
        let ctx = jinn_picker::StatusCtx::new(jinn_picker::PickerId::new(PERSONA_ID), &host);

        // When reading the status line.
        let line = spec.status_line(&ctx).expect("status declared");

        // Then it reads "Active: none".
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "Active: none");
    }
}
