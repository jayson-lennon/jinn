//! App-state update command and event — carries atomic diffs for runtime state.

use serde::{Deserialize, Serialize};

use crate::app_state_file::AppStateFile;
use jinn_core_types::ReasoningEffort;
use jinn_core_types::model_selection::ModelSelection;

/// A single atomic app-state update.
///
/// Each variant targets exactly one field in [`AppStateFile`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppStateUpdate {
    /// Set the last-used model.
    SetLastModel(Option<ModelSelection>),
    /// Set the active theme name.
    SetTheme(Option<String>),
    /// Set the active persona name.
    SetPersona(Option<String>),
    /// Set the sidebar width.
    SetSidebarWidth(Option<u16>),
    /// Set the last-selected reasoning effort.
    SetReasoningEffort(Option<ReasoningEffort>),
}

impl AppStateUpdate {
    /// Applies this diff to the given app-state in place.
    pub fn apply(&self, state: &mut AppStateFile) {
        match self {
            Self::SetLastModel(v) => v.clone_into(&mut state.last_model),
            Self::SetTheme(v) => v.clone_into(&mut state.theme_name),
            Self::SetPersona(v) => v.clone_into(&mut state.persona_name),
            Self::SetSidebarWidth(v) => {
                state.sidebar_width = *v;
            }
            Self::SetReasoningEffort(v) => {
                state.reasoning_effort = *v;
            }
        }
    }
}

/// Command to update one or more app-state fields.
///
/// Carries a batch of [`AppStateUpdate`] diffs. The `AppStateActor`
/// loads current state, applies all diffs, saves, and syncs the
/// frontend theme/sidebar/persona fields inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAppState {
    /// The atomic diffs to apply.
    pub updates: Vec<AppStateUpdate>,
}

impl jinn_slices::BusMessage for UpdateAppState {}

jinn_slices::crossing_schema!(UpdateAppState, "UpdateAppState",
trouper::schema::SchemaKind::Command,
description: "Apply a batch of atomic app-state diffs.",
fields: ["updates" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json))]);

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    fn set_last_model_some_applies_to_state() {
        // Given default app-state.
        let mut state = AppStateFile::default();

        // When applying SetLastModel with a model.
        let expected = jinn_core_types::model_selection::ModelSelection::from_single(
            "anthropic/claude-sonnet-4".to_owned(),
        );
        AppStateUpdate::SetLastModel(Some(expected.clone())).apply(&mut state);

        // Then the last model is set.
        assert_eq!(state.last_model, Some(expected));
    }

    #[rstest::rstest]
    fn set_last_model_none_clears_existing() {
        // Given app-state with an existing last model.
        let mut state = AppStateFile {
            last_model: Some(
                jinn_core_types::model_selection::ModelSelection::from_single(
                    "anthropic/claude-sonnet-4".to_owned(),
                ),
            ),
            ..Default::default()
        };

        // When applying SetLastModel(None).
        AppStateUpdate::SetLastModel(None).apply(&mut state);

        // Then the last model is cleared.
        assert!(state.last_model.is_none());
    }

    #[rstest::rstest]
    fn set_sidebar_width_applies_to_state() {
        // Given default app-state.
        let mut state = AppStateFile::default();

        // When applying SetSidebarWidth with a value.
        AppStateUpdate::SetSidebarWidth(Some(40)).apply(&mut state);

        // Then sidebar_width is set.
        assert_eq!(state.sidebar_width, Some(40));
    }

    #[rstest::rstest]
    fn set_theme_applies_to_state() {
        // Given default app-state.
        let mut state = AppStateFile::default();

        // When applying SetTheme with a name.
        AppStateUpdate::SetTheme(Some("dracula".to_owned())).apply(&mut state);

        // Then theme_name is set.
        assert_eq!(state.theme_name.as_deref(), Some("dracula"));
    }

    #[rstest::rstest]
    fn set_persona_applies_to_state() {
        // Given default app-state.
        let mut state = AppStateFile::default();

        // When applying SetPersona with a name.
        AppStateUpdate::SetPersona(Some("default".to_owned())).apply(&mut state);

        assert_eq!(state.persona_name.as_deref(), Some("default"));
    }

    #[rstest::rstest]
    fn set_reasoning_effort_some_applies_to_state() {
        // Given default app-state.
        let mut state = AppStateFile::default();

        // When applying SetReasoningEffort with a value.
        AppStateUpdate::SetReasoningEffort(Some(ReasoningEffort::High)).apply(&mut state);

        // Then reasoning_effort is set.
        assert_eq!(state.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[rstest::rstest]
    fn set_reasoning_effort_none_clears_existing() {
        // Given app-state with an existing reasoning effort.
        let mut state = AppStateFile {
            reasoning_effort: Some(ReasoningEffort::High),
            ..AppStateFile::default()
        };

        // When applying SetReasoningEffort(None).
        AppStateUpdate::SetReasoningEffort(None).apply(&mut state);

        // Then reasoning_effort is cleared.
        assert!(state.reasoning_effort.is_none());
    }
}
