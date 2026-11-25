//! Session management intent handlers - session creation, model refresh, and
//! the project-resources refresh.

use crate::RefreshModels;
use crate::RefreshProjectResources;
use crate::common::app_state::AppState;
use crate::protocol::{ChatEntry, IntentResult};

use crate::session_lifecycle::validator;

/// Refreshes the model list from the active provider.
pub fn handle_refresh_models(state: &mut AppState) -> IntentResult {
    if validator::validate_refresh_models(state).is_err() {
        return IntentResult::empty();
    }

    state
        .active_session_mut()
        .push_entry(ChatEntry::transient("Refreshing models..."));

    IntentResult::new_message(RefreshModels)
}

/// Refreshes every project resource for the active session.
///
/// The command rides the session-init supervisor's keyed discovery path, so
/// the manual refresh produces the same `*Loaded` events and settled summary
/// as a watcher-detected edit. The note is transient: it is feedback about a
/// UI action, not history the model should read back.
pub fn handle_refresh_project_resources(state: &mut AppState) -> IntentResult {
    state
        .active_session_mut()
        .push_entry(ChatEntry::transient("Refreshing project resources..."));

    let session_id = state.active_session().session_id().clone();
    let cwd = state.active_session().cwd().clone();

    IntentResult::new_message(RefreshProjectResources { session_id, cwd })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use crate::common::app_state::AppState;
    use crate::protocol::{ChatEntry, ChatEntryKind, IntentResult};

    use jinn_core_types::model_selection::ModelSelection;

    use super::*;

    /// A publish sink that keeps every payload published through it, so a
    /// result's typed closures can be decoded and inspected.
    #[derive(Default)]
    struct RecordingSink {
        published: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl jinn_slices::route_publish::PublishSink for RecordingSink {
        fn publish_schema(
            &self,
            schema_id: trouper::schema::SchemaId,
            payload: serde_json::Value,
            name: &'static str,
        ) {
            self.published
                .lock()
                .expect("sink lock")
                .push((format!("{schema_id}"), payload));
            let _ = name;
        }
    }

    /// Drains a result's closures into a sink and decodes every published
    /// payload of type `M`.
    fn published_of<M>(result: IntentResult) -> Vec<M>
    where
        M: serde::de::DeserializeOwned,
    {
        let sink = RecordingSink::default();
        for closure in result.messages {
            closure(&sink);
        }
        sink.published
            .lock()
            .expect("sink lock")
            .iter()
            .filter(|(id, _)| {
                id.ends_with(std::any::type_name::<M>().rsplit("::").next().unwrap_or(""))
            })
            .filter_map(|(_, payload)| serde_json::from_value(payload.clone()).ok())
            .collect()
    }

    #[rstest::rstest]
    fn session_new_creates_fresh_session() {
        // Given a state with an existing session.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("old"));

        // When handling SessionNew.
        let _result = crate::session_lifecycle::intent::handle_session_new(
            &mut state,
            crate::common::render_ctx::empty_config_layer(),
        );

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        assert!(state.active_session().history().is_empty());
        assert!(!state.frontend.is_picker());
        // And the old session is preserved in the sessions map.
        assert!(state.session.contains(&old_id));
    }

    #[rstest::rstest]
    fn session_new_closes_picker_and_creates_session() {
        // Given a state with an active picker.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(jinn_slices::FocusScope::Dynamic(
            jinn_project_msg::project_picker_scope(),
        ));
        let old_id = state.session.active_session_id().clone();

        // When handling SessionNew.
        let _result = crate::session_lifecycle::intent::handle_session_new(
            &mut state,
            crate::common::render_ctx::empty_config_layer(),
        );

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the picker is closed.
        assert!(!state.frontend.is_picker());
    }

    #[rstest::rstest]
    fn refresh_models_posts_transient_message() {
        // Given a state with a provider.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama".to_owned()));

        // When handling RefreshModels.
        let _result = handle_refresh_models(&mut state);

        // Then a transient message was posted.
        let last = state
            .active_session()
            .history()
            .last()
            .expect("should have entry");
        assert!(matches!(last.kind, ChatEntryKind::Transient(_)));
    }

    #[rstest::rstest]
    fn refresh_models_returns_refresh_command() {
        // Given a state with a provider.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama".to_owned()));
        let _initial_len = state.active_session().history().len();

        // When handling RefreshModels.
        let result = handle_refresh_models(&mut state);

        // Then a message is returned.
        assert!(!result.messages.is_empty());
    }

    #[rstest::rstest]
    fn refresh_models_noop_with_no_provider() {
        // Given a state with no provider.
        let mut state = AppState::default_with_scope_focus();

        // When handling RefreshModels.
        let result = handle_refresh_models(&mut state);

        // Then no commands.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn refresh_project_resources_posts_transient_message() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();
        let initial_len = state.active_session().history().len();

        // When handling RefreshProjectResources.
        let _result = handle_refresh_project_resources(&mut state);

        // Then exactly one transient note was posted — feedback about the
        // action, never a persisted system entry the model would read back.
        let history = state.active_session().history();
        assert_eq!(history.len(), initial_len + 1);
        let last = history.last().expect("the refresh note");
        assert!(matches!(last.kind, ChatEntryKind::Transient(_)));
    }

    #[rstest::rstest]
    fn refresh_project_resources_returns_refresh_command() {
        // Given a default state.
        let mut state = AppState::default_with_scope_focus();

        // When handling RefreshProjectResources.
        let result = handle_refresh_project_resources(&mut state);

        // Then exactly one message is returned, and it is the refresh
        // command — the prompt-only rescan command is no longer published.
        assert_eq!(result.message_names, ["RefreshProjectResources"]);
    }

    #[rstest::rstest]
    fn refresh_project_resources_addresses_the_active_session() {
        // Given a state whose active session sits at a distinct cwd.
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().set_cwd(dir.path.clone());
        let active = state.active_session().session_id().clone();

        // When handling RefreshProjectResources.
        let result = handle_refresh_project_resources(&mut state);

        // Then exactly one refresh command was published, and it names the
        // active session at that session's cwd — the discovery worker scans
        // the directory the command carries, whatever state holds later.
        let commands = published_of::<RefreshProjectResources>(result);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].session_id, active);
        assert_eq!(commands[0].cwd, dir.path);
    }

    #[rstest::rstest]
    fn session_new_inherits_active_session_cwd() {
        // Given a state whose active session has a distinct CWD (not the app
        // launch dir).
        let dir = jinn_testutil::tempdir();
        let mut state = AppState::default_with_scope_focus();
        let inherited_cwd = dir
            .path
            .join("inherited-project")
            .expect("temp root joins a bare name");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        assert_ne!(state.active_session().cwd(), state.session.default_cwd());

        // When handling SessionNew.
        let _result = crate::session_lifecycle::intent::handle_session_new(
            &mut state,
            crate::common::render_ctx::empty_config_layer(),
        );

        // Then the new session inherited the active session's CWD.
        assert_eq!(state.active_session().cwd(), &inherited_cwd);
    }
}
