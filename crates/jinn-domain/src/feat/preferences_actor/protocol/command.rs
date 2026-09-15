//! Preferences update command - carries a batch of atomic preference diffs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::feat::preferences_actor::UserPreferences;
use crate::feat::project::ProjectConfig;
/// A single atomic preference update.
///
/// Each variant targets exactly one field in [`UserPreferences`].
/// When a new field is added to `UserPreferences`, a corresponding
/// variant must be added here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PreferenceUpdate {
    /// Set the compaction model (provider/model for summarization).
    /// `None` means fall back to the session model.
    SetCompactionModel(Option<String>),
    /// Set the pruner-accumulation token threshold.
    SetAccumulationThreshold(u32),
    /// Add a project directory to the curated list. No-op if already present
    /// (dedupe by path).
    AddProject(PathBuf),
    /// Remove a project directory from the curated list by path. No-op if absent.
    RemoveProject(PathBuf),
}

impl PreferenceUpdate {
    /// Applies this diff to the given preferences in place.
    pub fn apply(&self, prefs: &mut UserPreferences) {
        match self {
            Self::SetCompactionModel(v) => prefs.compaction.model.clone_from(v),
            Self::SetAccumulationThreshold(v) => {
                prefs.auto_prune.accumulation_threshold_tokens = *v;
            }
            Self::AddProject(path) => {
                if !prefs.projects.iter().any(|c| &c.path == path) {
                    prefs.projects.push(ProjectConfig {
                        path: path.clone(),
                        command_policy: Vec::new(),
                    });
                }
            }
            Self::RemoveProject(path) => {
                prefs.projects.retain(|c| &c.path != path);
            }
        }
    }
}

/// Command to update one or more preference fields.
///
/// Carries a batch of [`PreferenceUpdate`] diffs. The `PreferencesActor`
/// loads current prefs, applies all diffs, saves, and emits
/// [`PreferencesUpdated`](super::event::PreferencesUpdated) with the full result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePreferences {
    /// The atomic diffs to apply.
    pub updates: Vec<PreferenceUpdate>,
}

impl crate::common::bus::BusMessage for UpdatePreferences {}

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
    fn set_compaction_model_some_applies_to_preferences() {
        // Given default preferences.
        let mut prefs = UserPreferences::default();

        // When applying SetCompactionModel with a model.
        PreferenceUpdate::SetCompactionModel(Some("anthropic/claude-sonnet-4-20250514".to_owned()))
            .apply(&mut prefs);

        // Then the compaction model is set.
        assert_eq!(
            prefs.compaction.model.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
    }

    #[rstest::rstest]
    fn set_compaction_model_none_clears_existing() {
        // Given preferences with an existing compaction model.
        let mut prefs = UserPreferences::default();
        prefs.compaction.model = Some("anthropic/claude-sonnet-4-20250514".to_owned());

        // When applying SetCompactionModel(None).
        PreferenceUpdate::SetCompactionModel(None).apply(&mut prefs);

        // Then the compaction model is cleared.
        assert!(prefs.compaction.model.is_none());
    }

    #[rstest::rstest]
    fn set_accumulation_threshold_applies_to_preferences() {
        // Given default preferences (threshold defaults to 10_000).
        let mut prefs = UserPreferences::default();

        // When applying SetAccumulationThreshold.
        PreferenceUpdate::SetAccumulationThreshold(2500).apply(&mut prefs);

        // Then the threshold is updated.
        assert_eq!(prefs.auto_prune.accumulation_threshold_tokens, 2500);
    }

    #[rstest::rstest]
    fn add_project_appends_to_projects_list() {
        // Given empty project list.
        let mut prefs = UserPreferences::default();

        // When adding a project directory.
        PreferenceUpdate::AddProject(PathBuf::from("/home/me/code/alpha")).apply(&mut prefs);

        // Then the project list contains exactly that path.
        assert_eq!(prefs.projects.len(), 1);
        assert_eq!(prefs.projects[0].path, PathBuf::from("/home/me/code/alpha"));
    }

    #[rstest::rstest]
    fn add_project_dedupes_existing_path() {
        // Given preferences with one project.
        let mut prefs = UserPreferences {
            projects: vec![ProjectConfig {
                path: PathBuf::from("/home/me/code/alpha"),
                command_policy: Vec::new(),
            }],
            ..UserPreferences::default()
        };

        // When adding the same path again.
        PreferenceUpdate::AddProject(PathBuf::from("/home/me/code/alpha")).apply(&mut prefs);

        // Then the list still has exactly one entry (no duplicate).
        assert_eq!(prefs.projects.len(), 1);
    }

    #[rstest::rstest]
    fn remove_project_deletes_matching_path() {
        // Given preferences with two projects.
        let mut prefs = UserPreferences {
            projects: vec![
                ProjectConfig {
                    path: PathBuf::from("/home/me/code/alpha"),
                    command_policy: Vec::new(),
                },
                ProjectConfig {
                    path: PathBuf::from("/home/me/code/beta"),
                    command_policy: Vec::new(),
                },
            ],
            ..UserPreferences::default()
        };

        // When removing the first path.
        PreferenceUpdate::RemoveProject(PathBuf::from("/home/me/code/alpha")).apply(&mut prefs);

        // Then only the second project remains.
        assert_eq!(prefs.projects.len(), 1);
        assert_eq!(prefs.projects[0].path, PathBuf::from("/home/me/code/beta"));
    }

    #[rstest::rstest]
    fn remove_project_noop_when_path_absent() {
        // Given preferences with one project.
        let mut prefs = UserPreferences {
            projects: vec![ProjectConfig {
                path: PathBuf::from("/home/me/code/alpha"),
                command_policy: Vec::new(),
            }],
            ..UserPreferences::default()
        };

        // When removing a path that is not present.
        PreferenceUpdate::RemoveProject(PathBuf::from("/home/me/code/missing")).apply(&mut prefs);

        // Then the existing project is retained unchanged.
        assert_eq!(prefs.projects.len(), 1);
    }
}
