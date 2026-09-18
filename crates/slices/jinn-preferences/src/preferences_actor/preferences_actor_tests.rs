#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use super::*;
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::state::State;
use jinn_preferences_config::protocol::command::PreferenceUpdate;

async fn create_actor() -> (PreferencesActor, State) {
    let services = Services::new_fake().await;
    let state = State::new(AppState::default_with_scope_focus());
    let actor = PreferencesActor {
        services: services.clone(),
        state: state.clone(),
        cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
    };
    (actor, state)
}

#[rstest::rstest]
#[tokio::test]
async fn set_compaction_model_overwrites_previous() {
    // Given a preferences actor.
    let (mut actor, _state) = create_actor().await;

    // When applying the first update.
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![PreferenceUpdate::SetCompactionModel(Some(
            "ollama/llama3".into(),
        ))],
    });
    // When applying a second update with a different model.
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![PreferenceUpdate::SetCompactionModel(Some(
            "openrouter/gpt-4".into(),
        ))],
    });

    // Then only the latest model is persisted.
    let prefs = actor.services.user_preferences_storage.read();
    assert_eq!(
        prefs.compaction.model.as_deref(),
        Some("openrouter/gpt-4"),
        "expected persisted compaction.model=openrouter/gpt-4"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn update_persists_applied_preferences() {
    // Given a preferences actor.
    let (mut actor, _state) = create_actor().await;

    // When applying an update.
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![PreferenceUpdate::SetCompactionModel(Some(
            "ollama/llama3".into(),
        ))],
    });

    // Then the full preferences are persisted with the applied model.
    let prefs = actor.services.user_preferences_storage.read();
    assert_eq!(
        prefs.compaction.model.as_deref(),
        Some("ollama/llama3"),
        "expected persisted compaction.model=ollama/llama3"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn empty_diffs_does_not_change_storage() {
    // Given a preferences actor with a model already set.
    let (mut actor, _state) = create_actor().await;
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![PreferenceUpdate::SetCompactionModel(Some(
            "ollama/llama3".into(),
        ))],
    });

    // When applying an update with empty diffs.
    actor.handle_update_preferences(&UpdatePreferences { updates: vec![] });

    // Then the existing preferences are preserved.
    let prefs = actor.services.user_preferences_storage.read();
    assert_eq!(
        prefs.compaction.model.as_deref(),
        Some("ollama/llama3"),
        "expected model to be preserved after empty update"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persist_writes_frontend_preferences() {
    // Given a preferences actor.
    let (mut actor, state) = create_actor().await;

    // When applying an update.
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![PreferenceUpdate::SetCompactionModel(Some(
            "ollama/llama3".into(),
        ))],
    });

    // Then frontend.preferences matches the persisted preferences.
    let guard = state.read();
    assert_eq!(
        guard.frontend.preferences.compaction.model.as_deref(),
        Some("ollama/llama3"),
        "frontend.preferences must be written inline after persist"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn persist_reloads_open_project_picker_items() {
    use jinn_domain::common::focus::FocusScope;
    use jinn_domain::feat::picker::PickerKind;
    use jinn_domain::feat::picker::project_spec::load_project_entries as load_project_picker_entries;
    use jinn_domain::feat::ui::picker_states::PickerExt;

    // Given a state with the project picker open and zero entries.
    let (mut actor, state) = create_actor().await;
    {
        let mut guard = state.write_test_no_cap();
        load_project_picker_entries(&mut guard.frontend);
        guard.frontend.scope_push(FocusScope::Picker {
            kind: PickerKind::Project,
        });
        assert_eq!(
            guard.frontend.project_picker().items().len(),
            0,
            "picker starts empty with default preferences"
        );
    }

    // When preferences update adds two projects.
    actor.handle_update_preferences(&UpdatePreferences {
        updates: vec![
            PreferenceUpdate::AddProject(std::path::PathBuf::from("/tmp/alpha")),
            PreferenceUpdate::AddProject(std::path::PathBuf::from("/tmp/beta")),
        ],
    });

    // Then the open project picker's items are reloaded from the new prefs.
    let guard = state.read();
    assert_eq!(
        guard.frontend.project_picker().items().len(),
        2,
        "open project picker should reload items after preferences update"
    );
}
