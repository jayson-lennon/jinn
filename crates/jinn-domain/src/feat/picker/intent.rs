//! Picker intent handlers - navigation, filtering, confirmation, and scope toggling.
//!
//! Handles all picker intents: open, insert char, backspace, confirm, move up/down,
//! cursor movement, and keymap scope filter toggle. The `handle_picker_confirm`
//! function returns `(IntentResult, Option<Intent>)` to allow the caller
//! (`jinn-intent`) to re-dispatch keymap intents without creating a circular
//! dependency.

use crate::common::app_state::AppState;
use crate::common::app_state::FocusScope;
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::feat::session::model_selection::ModelSelection;

use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::{Intent, IntentResult, PickerKind};

use super::geometry::active_viewport;
use super::validator;

/// Opens a picker of the given kind. Sets mode to Picker and optionally
/// requests picker entries from the actor system.
pub fn handle_open_picker(
    state: &mut AppState,
    kind: PickerKind,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    if validator::validate_open_picker(state, &kind).is_err() {
        return IntentResult::empty();
    }

    // Endpoint picker is only reachable for a Single (non-alloy) model. The
    // backend gate (OpenRouter vs direct) runs later in the discovery actor,
    // which owns `Services`; here we only reject the model-shape mismatch.
    if matches!(kind, PickerKind::Endpoint)
        && matches!(
            state.active_session().profile().model,
            ModelSelection::Alloy { .. }
        )
    {
        return IntentResult::empty();
    }

    state.frontend.scope_stack.push(FocusScope::Picker { kind });

    // Spec-driven pickers own their open-time preparation entirely —
    // but only when the registry actually holds their spec; otherwise the
    // legacy open path still runs.
    if crate::feat::picker::registry::spec_id_for_kind(&kind)
        .is_some_and(|id| pickers.get(id).is_some())
    {
        return crate::feat::picker::action::run_active_hook(
            state,
            pickers,
            crate::feat::picker::action::Hook::Open,
        );
    }

    reset_picker_for_open(state, kind);

    match kind {
        PickerKind::Provider
        | PickerKind::Persona
        | PickerKind::Skill
        | PickerKind::Theme
        | PickerKind::Tool
        | PickerKind::McpServer
        | PickerKind::Session
        | PickerKind::SessionLifecycle
        | PickerKind::ReasoningEffort
        | PickerKind::TaskList
        | PickerKind::Project
        | PickerKind::Plugin
        | PickerKind::Endpoint => IntentResult::empty(),
    }
}

/// Resets and repopulates the target picker's state before it is shown.
///
/// Each arm owns its picker's open-time preparation: reset, snapshot for
/// ESC revert (where applicable), and synchronous entry loading.
fn reset_picker_for_open(state: &mut AppState, kind: PickerKind) {
    match kind {
        PickerKind::Persona
        | PickerKind::Skill
        | PickerKind::Theme
        | PickerKind::Tool
        | PickerKind::McpServer
        | PickerKind::SessionLifecycle
        | PickerKind::ReasoningEffort
        | PickerKind::Plugin
        | PickerKind::TaskList
        | PickerKind::Session
        | PickerKind::Provider => {
            // Spec-driven when the registry holds their spec; nothing to
            // prepare in the legacy path.
        }
        PickerKind::Project => {
            // Defensive: a stale stash from an abandoned previous flow must
            // never leak into a new project-picker session.
            state.frontend.pending_creation = None;
            state.frontend.project_picker_mut().reset();
            load_project_picker_entries(&mut state.frontend);
        }
        PickerKind::Endpoint => {
            // Spec-driven when the registry holds their spec; nothing to
            // prepare in the legacy path.
        }
    }
}

/// Populates the skill picker entries from discovered skills.
///
/// Populates the project picker entries from `UserPreferences.projects`.
///
/// Entries are pre-computed display strings (tilde-compressed) so the picker
/// never has to call `shorten_path` per-render.
pub(crate) fn load_project_picker_entries(
    frontend: &mut crate::feat::ui::frontend_state::FrontendState,
) {
    use crate::feat::project::picker_entry::{ProjectEntry, project_entries};

    let theme = frontend.theme.clone();
    let entries: Vec<ProjectEntry> = project_entries(&frontend.preferences.projects, &theme);
    frontend.project_picker_mut().set_items(entries);
}

/// Resets the preview scroll offset when the active picker's spec opts in
/// (`PreviewSpec::reset_scroll_on_selection_change`).
fn reset_preview_scroll(state: &mut AppState, registry: &jinn_picker::PickerRegistry) {
    let Some(kind) = state.frontend.scope_stack.picker_kind().copied() else {
        return;
    };
    let Some(spec) =
        crate::feat::picker::registry::spec_id_for_kind(&kind).and_then(|id| registry.get(id))
    else {
        return;
    };
    if spec.resets_scroll_on_selection_change() {
        state
            .frontend
            .pickers
            .pickers_scrolls
            .reset(jinn_picker::PickerId::new(spec.id().as_str()));
    }
}

pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    validator::validate_picker_insert_char(state, ch);
    if let Some(picker) = state.active_picker_ops() {
        picker.insert_char(ch);
    }
    IntentResult::empty()
}

/// Handles `PasteText` in picker scope - bulk inserts pasted text into the filter.
///
/// Newlines are stripped by the picker's `insert_text` method since the filter
/// is a single-line input.
pub fn handle_picker_paste(state: &mut AppState, text: &str) -> IntentResult {
    if let Some(picker) = state.active_picker_ops() {
        picker.insert_text(text);
    }
    IntentResult::empty()
}

/// Removes the last character from the active picker's filter.
pub fn handle_backspace(state: &mut AppState) -> IntentResult {
    validator::validate_picker_backspace(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.backspace();
    }
    IntentResult::empty()
}

/// Confirms the active picker selection.
///
/// Returns `(IntentResult, Option<Intent>)`. For Provider and
/// Session pickers, the second element is `None`. For Keymap picker, returns
/// `(IntentResult::empty(), Some(selected_intent))` so the caller can re-dispatch.
pub fn handle_picker_confirm(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> (IntentResult, Option<Intent>) {
    if validator::validate_picker_confirm(state).is_err() {
        return (IntentResult::empty(), None);
    }

    // Spec-driven pickers own their confirm behavior entirely — but only
    // when the registry actually holds their spec; otherwise the legacy
    // confirm path still runs.
    if state
        .frontend
        .scope_stack
        .picker_kind()
        .and_then(crate::feat::picker::registry::spec_id_for_kind)
        .is_some_and(|id| pickers.get(id).is_some())
    {
        return (
            crate::feat::picker::action::run_active_hook(
                state,
                pickers,
                crate::feat::picker::action::Hook::Confirm,
            ),
            None,
        );
    }

    match state.frontend.scope_stack.picker_kind().copied() {
        Some(PickerKind::Project) => (confirm_project(state), None),

        // Persona, Skill, Theme, Tool, McpServer, and SessionLifecycle are
        // fully spec-driven; the registry guard above runs their confirm
        // hook. Reaching the match means the registry is empty (test seams) —
        // nothing to do.
        Some(
            PickerKind::McpServer
            | PickerKind::Persona
            | PickerKind::Provider
            | PickerKind::ReasoningEffort
            | PickerKind::Session
            | PickerKind::SessionLifecycle
            | PickerKind::TaskList
            | PickerKind::Plugin
            | PickerKind::Skill
            | PickerKind::Theme
            | PickerKind::Tool
            | PickerKind::Endpoint,
        )
        | None => (IntentResult::empty(), None),
    }
}

/// Moves the selection up in the active picker.
pub fn handle_move_up(state: &mut AppState, pickers: &jinn_picker::PickerRegistry) -> IntentResult {
    validator::validate_picker_move_up(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_up(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Moves the selection down in the active picker.
pub fn handle_move_down(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    validator::validate_picker_move_down(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_down(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Pages the selection up by half the visible window in the active picker.
pub fn handle_page_up(state: &mut AppState, pickers: &jinn_picker::PickerRegistry) -> IntentResult {
    validator::validate_picker_page_up(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.page_up(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Pages the selection down by half the visible window in the active picker.
pub fn handle_page_down(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    validator::validate_picker_page_down(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.page_down(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Moves the filter cursor left in the active picker.
pub fn handle_move_cursor_left(state: &mut AppState) -> IntentResult {
    validator::validate_picker_move_cursor_left(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_cursor_left();
    }
    IntentResult::empty()
}

/// Moves the filter cursor right in the active picker.
pub fn handle_move_cursor_right(state: &mut AppState) -> IntentResult {
    validator::validate_picker_move_cursor_right(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_cursor_right();
    }
    IntentResult::empty()
}

/// Confirms the highlighted project: stashes its dir as the pending session
/// creation (project stamp + starting CWD), pops the picker, and delegates to
/// a plain new session.
///
/// `Enter` path: the new session inherits nothing from the active session's
/// CWD - it is created at the chosen project dir via the stash.
fn confirm_project(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item() else {
        return IntentResult::empty();
    };

    // Stash the chosen dir and pop the picker before delegating, so the new
    // session is created in Normal scope at the right CWD. The project stamp
    // and the starting CWD start equal here but are semantically distinct:
    // lifecycle scripts may re-cwd the session, the project never moves.
    state.frontend.pending_creation =
        Some(crate::feat::ui::frontend_state::PendingSessionCreation {
            project_dir: entry.path.clone(),
            starting_cwd: entry.path.clone(),
        });
    state.frontend.scope_stack.pop();

    crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(state, "", &[], None)
}

/// Confirms the highlighted project and chains into the lifecycle picker.
///
/// `<c-enter>` path: same stash + pop as `confirm_project`, but then opens
/// the session lifecycle picker so the user picks a recipe (and optional args).
/// The stash survives the lifecycle -> arg-input chain because every
/// confirmation path in that chain consumes `pending_creation`.
pub fn handle_project_lifecycle_confirm(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item() else {
        return IntentResult::empty();
    };

    state.frontend.pending_creation =
        Some(crate::feat::ui::frontend_state::PendingSessionCreation {
            project_dir: entry.path.clone(),
            starting_cwd: entry.path.clone(),
        });
    state.frontend.scope_stack.pop();

    // Re-enter the lifecycle picker. `handle_open_picker` pushes a fresh
    // `Picker { SessionLifecycle }` scope.
    handle_open_picker(
        state,
        PickerKind::SessionLifecycle,
        &jinn_picker::PickerRegistry::new(),
    )
}

/// Removes the highlighted project from the curated list (`d`).
///
/// Applies the diff optimistically to `frontend.preferences.projects`,
/// reloads the picker items so the list updates immediately, and emits
/// `UpdatePreferences` so the `PreferencesActor` persists the change.
/// The `PreferencesActor` will also write `frontend.preferences` inline
/// after persisting, reconciling any divergence.
pub fn handle_project_remove_highlighted(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item().cloned() else {
        return IntentResult::empty();
    };

    state
        .frontend
        .preferences
        .projects
        .retain(|p| p.path != entry.path);
    load_project_picker_entries(&mut state.frontend);

    IntentResult::new_message(UpdatePreferences {
        updates: vec![PreferenceUpdate::RemoveProject(entry.path)],
    })
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
    use super::*;

    /// Wraps persona entries through the persona spec's hooks for storage.
    fn wrap_persona_entries(
        entries: Vec<crate::feat::persona::PersonaEntry>,
    ) -> Vec<jinn_picker::PickerEntry<crate::feat::persona::PersonaEntry>> {
        crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::PERSONA_ID, entries)
            .expect("persona spec is registered")
    }

    /// Wraps provider entries through the provider spec's hooks for storage.
    fn wrap_provider_entries(
        entries: Vec<crate::protocol::ProviderPickerEntry>,
    ) -> Vec<jinn_picker::PickerEntry<crate::protocol::ProviderPickerEntry>> {
        crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::PROVIDER_ID, entries)
            .expect("provider spec is registered")
    }

    fn empty_pickers() -> jinn_picker::PickerRegistry {
        jinn_picker::PickerRegistry::new()
    }
    use crate::feat::session::ChatSessionState;
    use crate::feat::session::model_selection::AlloyStrategy;
    use crate::feat::todo_list::TaskStatus;
    use crate::feat::todo_list::picker_entry::RowStatus;
    use jinn_selection_widget::TreeItem;
    #[rstest::rstest]
    fn confirm_persona_sets_correct_persona() {
        // If the match were inverted, the wrong persona would be set.
        use crate::feat::persona::PersonaEntry;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        // Add two personas to context.
        state.context.set_personas(vec![
            crate::feat::persona::Persona {
                name: "coder".to_owned(),
                description: String::new(),
                body: "You are a coder.".to_owned(),
            },
            crate::feat::persona::Persona {
                name: "writer".to_owned(),
                description: String::new(),
                body: "You are a writer.".to_owned(),
            },
        ]);

        // Set picker entries with "writer" as the selected item.
        let entries = vec![
            PersonaEntry {
                name: "coder".to_owned(),
                description: String::new(),
                is_active: false,
                theme: crate::feat::theme::default_theme(),
            },
            PersonaEntry {
                name: "writer".to_owned(),
                description: String::new(),
                is_active: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state
            .frontend
            .persona_picker_mut()
            .set_items(wrap_persona_entries(entries));
        state.frontend.persona_picker_mut().move_down(1); // coder
        state.frontend.persona_picker_mut().move_down(1); // writer

        // Persona confirm runs through its spec (registry dispatch).
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Persona,
            });
        let (result, _redispatch) = handle_picker_confirm(
            &mut state,
            &crate::feat::picker::registry::build_picker_registry(),
        );

        // Then the active persona is "writer", not "coder".
        assert_eq!(
            state.context.active_persona().map(|p| p.name.as_str()),
            Some("writer"),
            "confirm_persona should set the correct persona"
        );
        assert!(!result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn open_endpoint_picker_is_noop_for_alloy_model() {
        // Given a session on an alloy of two models.
        use crate::feat::session::model_selection::ModelSelection;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        // When opening the endpoint picker.
        handle_open_picker(&mut state, PickerKind::Endpoint, &empty_pickers());

        // Then no picker scope is pushed (the gate rejected it).
        assert!(
            !state.frontend.scope_stack.is_picker(),
            "endpoint picker must not open for an alloy model"
        );
    }

    #[rstest::rstest]
    fn confirm_persona_emits_mark_session_interacted() {
        // added to confirm_persona.
        // If the persist message were never emitted, a pick-then-quit would lose
        // the persona change.
        use crate::feat::persona::PersonaEntry;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .context
            .set_personas(vec![crate::feat::persona::Persona {
                name: "coder".to_owned(),
                description: String::new(),
                body: "You are a coder.".to_owned(),
            }]);
        let entry = PersonaEntry {
            name: "coder".to_owned(),
            description: String::new(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .persona_picker_mut()
            .set_items(wrap_persona_entries(vec![entry]));
        state.frontend.persona_picker_mut().move_down(1);

        // When confirming through the persona spec (registry dispatch).
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Persona,
            });
        let (result, _redispatch) = handle_picker_confirm(
            &mut state,
            &crate::feat::picker::registry::build_picker_registry(),
        );

        // Then a MarkSessionInteracted message is emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("MarkSessionInteracted")),
            "confirm_persona should emit MarkSessionInteracted to persist"
        );
    }

    fn setup_state_with_task_list() -> (AppState, crate::feat::todo_list::TaskId) {
        use crate::feat::todo_list::{PhaseInput, TaskStatus};

        let mut state = AppState::default();
        let mut origin = ChatSessionState::new();

        origin.task_list_mut().set_from_inputs(&[
            // Phase 1 with 2 tasks (one Pending, one Completed).
            PhaseInput {
                description: "Research".to_owned(),
                tasks: vec![
                    ("Read codebase".to_owned(), TaskStatus::Pending),
                    ("Write notes".to_owned(), TaskStatus::Completed),
                ],
            },
            // Phase 2 with a Pending task, a Cancelled task, and a Postponed
            // source whose ID is surfaced to tests (a Pending copy with the
            // same description sits beside it).
            PhaseInput {
                description: "Build".to_owned(),
                tasks: vec![
                    ("Implement feature".to_owned(), TaskStatus::Pending),
                    ("Investigate alt".to_owned(), TaskStatus::Cancelled),
                    ("Refactor later".to_owned(), TaskStatus::Postponed),
                    ("Refactor later".to_owned(), TaskStatus::Pending),
                ],
            },
        ]);

        let postponed_id = {
            let list = origin.task_list();
            let build = &list.phases()[1];
            build
                .tasks
                .iter()
                .find(|t| t.status == TaskStatus::Postponed)
                .map(|t| t.id.clone())
                .expect("postponed source present")
        };

        let origin_id = origin.session_id().clone();
        state.session.insert(origin);
        assert!(
            state.session.set_active(origin_id),
            "origin session must be present for set_active"
        );
        (state, postponed_id)
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_skips_postponed() {
        // Given a session with one postponed task among other tasks.
        // postpone_task creates a new Pending copy with the same description, so we
        // must verify the *source* (Postponed) entry is excluded by ID, not by label.
        let (mut state, postponed_id) = setup_state_with_task_list();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then no entry has the postponed task's ID.
        let excluded_id = format!("task:{postponed_id}");
        let items = state.frontend.task_list_picker().items();
        assert!(
            items.iter().all(|e| e.id() != excluded_id),
            "postponed source task should not appear in picker (id={excluded_id})"
        );
        // Sanity: the new Pending copy with the same description IS present.
        assert!(
            items.iter().any(|e| e.display_label() == "Refactor later"),
            "Pending copy of postponed task should be visible"
        );
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_produces_correct_tree_shape() {
        // Given a session with two phases and mixed-status tasks.
        let (mut state, _postponed_id) = setup_state_with_task_list();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then there are exactly 2 phase roots.
        let items = state.frontend.task_list_picker().items();
        let roots: Vec<_> = items.iter().filter(|e| e.parent_id().is_none()).collect();
        assert_eq!(roots.len(), 2, "should have 2 phase roots");
        assert_eq!(roots[0].display_label(), "Research");
        assert_eq!(roots[1].display_label(), "Build");

        // And each task's parent_id matches its phase's id.
        let phase_ids: Vec<&str> = roots.iter().map(|e| e.id()).collect();
        for item in items.iter().filter(|e| e.parent_id().is_some()) {
            assert!(
                phase_ids.contains(&item.parent_id().expect("task parent")),
                "task {:?} should reference a known phase id",
                item.display_label()
            );
        }

        // And the counts match: Phase 1 -> 2 tasks; Phase 2 -> 3 tasks (Pending,
        // Cancelled, and the Pending copy created by postpone_task).
        let research_children: Vec<_> = items
            .iter()
            .filter(|e| e.parent_id() == Some(phase_ids[0]))
            .collect();
        let build_children: Vec<_> = items
            .iter()
            .filter(|e| e.parent_id() == Some(phase_ids[1]))
            .collect();
        assert_eq!(research_children.len(), 2);
        assert_eq!(build_children.len(), 3);
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_carries_status_through() {
        // Given a session with completed and cancelled tasks.
        let (mut state, _postponed_id) = setup_state_with_task_list();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then task rows carry their status in row_status.

        let items = state.frontend.task_list_picker().items();
        let statuses: Vec<_> = items
            .iter()
            .filter_map(|e| match e.entry().row_status() {
                RowStatus::Task(s) => Some((e.display_label(), s)),
                RowStatus::Phase => None,
            })
            .collect();

        let by_label: std::collections::HashMap<&str, TaskStatus> =
            statuses.iter().map(|(l, s)| (*l, *s)).collect();
        assert_eq!(
            by_label.get("Write notes").copied(),
            Some(TaskStatus::Completed),
            "'Write notes' should be Completed"
        );
        assert_eq!(
            by_label.get("Investigate alt").copied(),
            Some(TaskStatus::Cancelled),
            "'Investigate alt' should be Cancelled"
        );
        assert_eq!(
            by_label.get("Read codebase").copied(),
            Some(TaskStatus::Pending),
            "'Read codebase' should be Pending"
        );
        // The Pending copy of the postponed task should also carry its status.
        assert_eq!(
            by_label.get("Refactor later").copied(),
            Some(TaskStatus::Pending),
            "'Refactor later' (Pending copy) should be Pending"
        );
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_empty_task_list_no_panic() {
        // Given a default session with an empty task list.
        let mut state = AppState::default();

        // When opening the task-list picker through the real open path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(&mut state, PickerKind::TaskList, &registry);

        // Then the picker is empty and nothing panicked.
        assert!(state.frontend.task_list_picker().items().is_empty());
    }

    #[rstest::rstest]
    fn handle_picker_confirm_task_list_is_noop_and_keeps_scope() {
        // Given state with the TaskList picker scope on the stack.
        let (mut state, _postponed_id) = setup_state_with_task_list();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::TaskList,
        });
        let len_before = state.frontend.scope_stack.len();

        // When confirming.
        let (result, follow_up) = handle_picker_confirm(&mut state, &empty_pickers());

        // Then no commands, no follow-up, and the scope stack is unchanged.
        assert!(result.message_names.is_empty(), "no commands");
        assert!(follow_up.is_none(), "no follow-up");
        assert_eq!(
            state.frontend.scope_stack.len(),
            len_before,
            "scope stack must remain unchanged on no-op confirm"
        );
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::TaskList
            }
        ));
    }

    #[rstest::rstest]
    fn esc_from_task_list_picker_restores_sidebar_task_list_scope() {
        // Given a scope stack like: [Normal, SidebarTaskList, Picker(TaskList)].
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::SidebarTaskList);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::TaskList,
        });

        // When Esc is pressed.
        let _ = crate::feat::chat_input::intent::handle_enter_normal_mode(&mut state);

        // Then we should return to SidebarTaskList, not Normal.
        assert!(
            matches!(
                state.frontend.scope_stack.current(),
                FocusScope::SidebarTaskList
            ),
            "Esc from TaskList picker should restore SidebarTaskList scope, got: {:?}",
            state.frontend.scope_stack.current()
        );
    }

    /// Builds a state with the provider picker open (Provider scope), `n` available
    /// single-model entries `model-0..model-n`, and the first entry highlighted.
    fn state_with_provider_picker(n: usize) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        let entries: Vec<crate::protocol::ProviderPickerEntry> = (0..n)
            .map(|i| crate::protocol::ProviderPickerEntry {
                provider_id: format!("prov/model-{i}"),
                name: "prov".to_owned(),
                provider_name: "prov".to_owned(),
                backend: "openrouter".to_owned(),
                model: format!("model-{i}"),
                search_text: format!("model-{i}"),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            })
            .collect();
        state
            .provider
            .provider_picker
            .set_items(wrap_provider_entries(entries));
        state.provider.provider_picker.move_down(1); // highlight first entry
        state
    }

    /// Builds an AppState with a project picker open, the active session's CWD
    /// set to a distinct value, and `n` curated project entries loaded.
    fn state_with_project_picker(paths: &[&str]) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/active-session-cwd"));
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Project,
        });
        let projects: Vec<crate::feat::project::ProjectConfig> = paths
            .iter()
            .map(|p| crate::feat::project::ProjectConfig {
                path: std::path::PathBuf::from(p),
                command_policy: Vec::new(),
            })
            .collect();
        state.frontend.preferences.projects = projects;
        load_project_picker_entries(&mut state.frontend);
        // index 0 is selected by default after set_items + reset.
        state
    }

    #[rstest::rstest]
    fn confirm_project_creates_new_session_at_chosen_dir() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_project_picker(&["/tmp/project-a", "/tmp/project-b"]);

        // When confirming the highlighted project (Enter).
        let result = confirm_project(&mut state);

        // Then a new session was created (a message was emitted to drive it).
        assert!(!result.message_names.is_empty());
        // And the new active session's CWD is the chosen project dir, not the
        // previously active session's CWD.
        assert_eq!(
            state.active_session().cwd(),
            std::path::Path::new("/tmp/project-a"),
        );
        // And the stash was consumed.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn confirm_project_leaves_previous_session_cwd_unchanged() {
        // Given a project picker with an existing active session.
        let mut state = state_with_project_picker(&["/tmp/project-a"]);
        let prev_id = state.session.active_session_id().clone();

        // When confirming the highlighted project.
        let _result = confirm_project(&mut state);

        // Then the previous session (now backgrounded) keeps its original CWD.
        let prev = state
            .session
            .get(&prev_id)
            .expect("previous session still exists");
        assert_eq!(prev.cwd(), std::path::Path::new("/tmp/active-session-cwd"));
    }

    #[rstest::rstest]
    fn confirm_project_with_lifecycle_sets_override_then_opens_lifecycle_picker() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_project_picker(&["/tmp/project-a"]);

        // When confirming with lifecycle (<c-enter>).
        let _result = handle_project_lifecycle_confirm(&mut state);

        // Then the project scope was popped and the lifecycle picker opened.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::SessionLifecycle
            }
        ));
        // And the chosen dir is stashed in a pending creation, awaiting the
        // lifecycle/args confirm chain.
        let pending = state
            .frontend
            .pending_creation
            .as_ref()
            .expect("pending creation stashed");
        assert_eq!(pending.project_dir, std::path::Path::new("/tmp/project-a"));
        assert_eq!(pending.starting_cwd, std::path::Path::new("/tmp/project-a"));
    }

    #[rstest::rstest]
    fn project_remove_highlighted_deletes_highlighted_entry() {
        // Given a project picker with two entries and the first highlighted.
        let mut state = state_with_project_picker(&["/tmp/project-a", "/tmp/project-b"]);

        // When removing the highlighted entry (d).
        let result = handle_project_remove_highlighted(&mut state);

        // Then the highlighted entry is removed from preferences.projects.
        let paths: Vec<_> = state
            .frontend
            .preferences
            .projects
            .iter()
            .map(|p| p.path.clone())
            .collect();
        assert_eq!(paths, vec![std::path::PathBuf::from("/tmp/project-b")]);
        // And an UpdatePreferences(RemoveProject) message was emitted.
        assert!(!result.message_names.is_empty());
        // And the picker now shows one entry.
        assert_eq!(state.frontend.project_picker().items().len(), 1);
    }

    #[rstest::rstest]
    fn handle_move_down_uses_measured_viewport() {
        // Given a provider picker with 20 entries and a measured viewport of 5,
        // selection already on the last visible row (index 4).
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(5);
        state.provider.provider_picker.move_up(5); // back to selection 0
        for _ in 0..4 {
            state.provider.provider_picker.move_down(5);
        }
        assert_eq!(state.provider.provider_picker.selection(), 4);
        assert_eq!(state.provider.provider_picker.scroll_offset(), 0);

        // When moving down once more.
        handle_move_down(&mut state, &empty_pickers());

        // Then selection advances to 5 and scroll_offset advances by one
        // (measured viewport of 5, not the old hardcoded 100).
        assert_eq!(state.provider.provider_picker.selection(), 5);
        assert_eq!(state.provider.provider_picker.scroll_offset(), 1);
    }

    #[rstest::rstest]
    fn handle_move_down_uses_fallback_when_viewport_unmeasured() {
        // Given a provider picker with 30 entries and viewport left at 0
        // (before the first render writes a measurement).
        let mut state = state_with_provider_picker(30);
        assert_eq!(state.frontend.picker_results_viewport(), 0);

        // When moving down once.
        handle_move_down(&mut state, &empty_pickers());

        // Then selection advances by one without panic, using the fallback.
        assert_eq!(state.provider.provider_picker.selection(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn move_down_previews_the_theme_when_the_registry_holds_the_spec() {
        // Given an open theme picker whose second entry is a distinct theme,
        // with the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = AppState::default();
        state.plugins.set_themes(
            "theme-loader",
            vec![("other".to_owned(), None, other.clone())],
        );
        crate::feat::picker::intent::handle_open_picker(&mut state, PickerKind::Theme, &registry);

        // When moving the selection down one entry.
        handle_move_down(&mut state, &registry);

        // Then the highlighted theme is applied live (spec selection-change
        // hook ran through the move handler).
        assert_eq!(
            state.frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
    }

    #[rstest::rstest]
    #[test]
    fn move_down_skips_selection_change_when_the_spec_has_no_hook() {
        // Given an open provider picker (its kind maps to no spec) with two
        // entries and the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut state = state_with_provider_picker(2);
        let theme_before = state.frontend.theme.clone();

        // When moving the selection down.
        handle_move_down(&mut state, &registry);

        // Then the app theme is untouched (no selection-change dispatch).
        assert_eq!(state.frontend.theme.focus_accent, theme_before.focus_accent);
    }

    #[rstest::rstest]
    #[test]
    fn page_down_previews_the_theme_when_the_registry_holds_the_spec() {
        // Given an open theme picker whose second entry is a distinct theme,
        // with the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = AppState::default();
        state.plugins.set_themes(
            "theme-loader",
            vec![("other".to_owned(), None, other.clone())],
        );
        crate::feat::picker::intent::handle_open_picker(&mut state, PickerKind::Theme, &registry);

        // When paging down (selection jumps to the last entry).
        handle_page_down(&mut state, &registry);

        // Then the highlighted theme is applied live.
        assert_eq!(
            state.frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
    }

    #[rstest::rstest]
    fn handle_page_down_advances_selection_by_half_viewport() {
        // Given a provider picker with 20 entries, selection at 0, viewport 10.
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(10);
        state.provider.provider_picker.move_up(5); // selection back to 0

        // When handling PickerPageDown (half of 10 = 5).
        handle_page_down(&mut state, &empty_pickers());

        // Then selection advances by 5.
        assert_eq!(state.provider.provider_picker.selection(), 5);
    }

    #[rstest::rstest]
    fn handle_page_up_decrements_selection_by_half_viewport() {
        // Given a provider picker with 20 entries, selection at 10, viewport 10.
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(10);
        // Advance selection to 10.
        for _ in 0..9 {
            state.provider.provider_picker.move_down(10);
        }

        // When handling PickerPageUp (half of 10 = 5).
        handle_page_up(&mut state, &empty_pickers());

        // Then selection decrements by 5.
        assert_eq!(state.provider.provider_picker.selection(), 5);
    }
}
