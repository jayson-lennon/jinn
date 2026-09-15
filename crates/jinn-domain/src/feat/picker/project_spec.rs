//! The project picker's spec — behavior authored once in the builder.
//!
//! Lists curated project directories and turns a selection into a new
//! session: plain confirm starts the blank lifecycle at the chosen dir,
//! `<c-enter>` chains into the session-lifecycle picker, `<c-n>` opens the
//! add-directory input, and `<c-d>` removes the highlighted project (the
//! picker stays open). Enter carries no close signal — the lifecycle setup
//! owns the scope transition.

use jinn_picker::ActionCtx;
use jinn_picker::PickerEntry;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use ratatui::text::Line;

use crate::common::app_state::AppState;
use crate::common::focus::FocusScope;
use crate::feat::picker::PickerKind;
use crate::feat::preferences_actor::protocol::command::PreferenceUpdate;
use crate::feat::preferences_actor::protocol::command::UpdatePreferences;
use crate::feat::project::picker_entry::ProjectEntry;
use crate::feat::project::picker_entry::render_project_row;
use crate::feat::ui::frontend_state::PendingSessionCreation;
use crate::feat::ui::picker_states::PickerExt;

/// The kernel entry this picker's items wrap in storage.
pub use crate::feat::project::picker_entry::ProjectEntry as SpecEntry;

/// Renders one project row — the same tilde-compressed line trunk drew via
/// `ProjectEntry: PickerItem`, now routed through the spec.
fn project_row(entry: &ProjectEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    render_project_row(
        &entry.display,
        ctx.is_selected,
        ctx.match_ranges,
        &entry.theme,
    )
}

/// Downcasts the host's `Any` state to `AppState`.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState
where
{
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

/// Loads project entries into the picker: one row per curated directory,
/// display strings precomputed (tilde-compressed) and sorted by display.
pub(crate) fn load_project_entries(frontend: &mut crate::feat::ui::frontend_state::FrontendState) {
    let theme = frontend.theme.clone();
    let entries: Vec<ProjectEntry> =
        crate::feat::project::picker_entry::project_entries(&frontend.preferences.projects, &theme);
    let wrapped = crate::feat::picker::registry::build_picker_registry()
        .make_items(crate::feat::picker::registry::PROJECT_ID, entries)
        .unwrap_or_default();
    frontend.project_picker_mut().set_items(wrapped);
}

/// Builds the project picker's spec.
#[must_use]
pub fn project_spec() -> PickerSpec<ProjectEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::PROJECT_ID))
        .title(" Projects ")
        .row(project_row)
        .search(|entry| entry.display.clone())
        .on_open(|ctx| {
            // Fresh filter + selection each open; entries come straight from
            // the curated preferences (synchronous — no actor round-trip).
            if let Some(picker) =
                ctx.selection::<jinn_selection_widget::SelectionState<PickerEntry<ProjectEntry>>>()
            {
                picker.reset();
            }
            let state = state_of(ctx);
            load_project_entries(&mut state.frontend);
            PickerOutcome::empty()
        })
        .bind("<c-enter>", "new+lifecycle", |ctx| {
            // Stash the chosen dir and pop the picker, then chain into the
            // session-lifecycle picker via the REAL registry — its open hook
            // (not a legacy loader) now fills the entries.
            let (path, starting_cwd) = {
                let state = state_of(ctx);
                let Some(entry) = state.frontend.project_picker().selected_item() else {
                    return PickerOutcome::empty();
                };
                let path = entry.entry().path.clone();
                (path.clone(), path)
            };
            let state = state_of(ctx);
            state.frontend.pending_creation = Some(PendingSessionCreation {
                project_dir: path,
                starting_cwd,
            });
            state.frontend.scope_stack.pop();
            state.frontend.scope_stack.push(FocusScope::Picker {
                kind: PickerKind::SessionLifecycle,
            });
            let registry = crate::feat::picker::registry::build_picker_registry();
            let result = crate::feat::picker::action::run_active_hook(
                state,
                &registry,
                crate::feat::picker::action::Hook::Open,
            );
            PickerOutcome::from_route_result(result)
        })
        .bind("<c-n>", "add dir", |ctx| {
            // Hand off to the add-directory input (the handler owns the
            // scope push). No close signal — we are handing off, not done.
            let state = state_of(ctx);
            let result =
                crate::feat::project_add_input::intent::handle_project_add_input_enter(state);
            PickerOutcome::from_route_result(result)
        })
        .bind("<c-d>", "remove", |ctx| {
            // Remove the highlighted project from the curated list and
            // refresh in place — the picker stays open.
            let state = state_of(ctx);
            let Some(entry) = state.frontend.project_picker().selected_item().cloned() else {
                return PickerOutcome::empty();
            };
            let path = entry.entry().path.clone();
            state
                .frontend
                .preferences
                .projects
                .retain(|p| p.path != path);
            load_project_entries(&mut state.frontend);
            PickerOutcome::new_message(UpdatePreferences {
                updates: vec![PreferenceUpdate::RemoveProject(path)],
            })
        })
        .on_confirm(|ctx| {
            // Stash the chosen dir, pop, and run the blank lifecycle setup.
            // The setup owns the scope transition (it lands the new session
            // in Normal scope), so this outcome carries no close signal.
            let state = state_of(ctx);
            let Some(entry) = state.frontend.project_picker().selected_item() else {
                return PickerOutcome::empty();
            };
            let path = entry.entry().path.clone();
            state.frontend.pending_creation = Some(PendingSessionCreation {
                project_dir: path.clone(),
                starting_cwd: path,
            });
            state.frontend.scope_stack.pop();
            let result = crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(
                state,
                "",
                &[],
                None,
            );
            PickerOutcome::from_route_result(result)
        })
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::common::app_state::FocusScope;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::registry::PROJECT_ID;
    use crate::feat::picker::registry::build_picker_registry;
    use crate::feat::session::ChatSessionState;
    use crate::feat::ui::picker_states::PickerExt;
    use jinn_picker::ActionCtx;
    use jinn_picker::PickerId;

    /// State with an active origin session (cwd distinct from the project
    /// dirs), the project picker open, and the given curated projects.
    fn state_with_projects(paths: &[&str]) -> AppState {
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
        state.frontend.preferences.projects = paths
            .iter()
            .map(|p| crate::feat::project::ProjectConfig {
                path: std::path::PathBuf::from(p),
                command_policy: Vec::new(),
            })
            .collect();
        load_project_entries(&mut state.frontend);
        // index 0 is selected by default after set_items + reset.
        state
    }

    /// Runs a spec action against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(PickerId::new(PROJECT_ID), &mut host);
        f(&mut ctx)
    }

    #[rstest::rstest]
    fn open_loads_curated_entries_with_tilde_displays() {
        // Given an app with two curated projects and the project picker open.
        let mut state = state_with_projects(&["/tmp/project-a", "/tmp/project-b"]);
        let registry = build_picker_registry();

        // When opening the picker through the real open path.
        let result = crate::feat::picker::intent::handle_open_picker(
            &mut state,
            PickerKind::Project,
            &registry,
        );

        // Then the open hook ran clean (synchronous load, no messages).
        assert!(result.message_names.is_empty());
        // And the picker holds both curated dirs, display-sorted.
        let items = state.frontend.project_picker().items().to_vec();
        assert_eq!(items.len(), 2);
        let mut displays: Vec<&str> = items.iter().map(|i| i.entry().display.as_str()).collect();
        displays.sort_unstable();
        assert_eq!(displays, vec!["/tmp/project-a", "/tmp/project-b"]);
    }

    #[rstest::rstest]
    fn confirm_creates_new_session_at_chosen_dir() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_projects(&["/tmp/project-a", "/tmp/project-b"]);
        let registry = build_picker_registry();

        // When confirming the highlighted project (Enter).
        let result = crate::feat::picker::intent::handle_picker_confirm(&mut state, &registry);

        // Then a new session was created (a message was emitted to drive it).
        assert!(!result.0.message_names.is_empty());
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
    fn confirm_leaves_previous_session_cwd_unchanged() {
        // Given a project picker with an existing active session.
        let mut state = state_with_projects(&["/tmp/project-a"]);
        let prev_id = state.session.active_session_id().clone();
        let registry = build_picker_registry();

        // When confirming the highlighted project.
        let _result = crate::feat::picker::intent::handle_picker_confirm(&mut state, &registry);

        // Then the previous session (now backgrounded) keeps its original CWD.
        let prev = state
            .session
            .get(&prev_id)
            .expect("previous session still exists");
        assert_eq!(prev.cwd(), std::path::Path::new("/tmp/active-session-cwd"));
    }

    #[rstest::rstest]
    fn ctrl_enter_chains_into_lifecycle_picker_with_entries() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_projects(&["/tmp/project-a"]);
        let registry = build_picker_registry();

        // When pressing <c-enter> (new + lifecycle).
        let _result = crate::feat::picker::action::run_action(
            &mut state,
            &build_picker_registry(),
            PROJECT_ID,
            "<c-enter>",
        );

        // Then the project scope was popped and the lifecycle picker opened.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::SessionLifecycle
            }
        ));
        // And the lifecycle picker holds entries (the real registry ran the
        // lifecycle spec's open hook — the empty-picker regression is fixed).
        assert!(
            !state.frontend.session_lifecycle_picker().items().is_empty(),
            "lifecycle picker must be populated by the chained open"
        );
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
    fn ctrl_n_opens_the_add_dir_input() {
        // Given a project picker with an active session.
        let mut state = state_with_projects(&["/tmp/project-a"]);

        // When pressing <c-n> (add dir).
        let _result = crate::feat::picker::action::run_action(
            &mut state,
            &build_picker_registry(),
            PROJECT_ID,
            "<c-n>",
        );

        // Then the add-input scope was pushed (seeded from the session cwd).
        assert_eq!(
            state.frontend.scope_stack.current(),
            &FocusScope::ProjectAddInput
        );
    }

    #[rstest::rstest]
    fn ctrl_d_removes_highlighted_and_stays_open() {
        // Given a project picker with two entries and the first highlighted.
        let mut state = state_with_projects(&["/tmp/project-a", "/tmp/project-b"]);

        // When removing the highlighted entry (<c-d>).
        let result = crate::feat::picker::action::run_action(
            &mut state,
            &build_picker_registry(),
            PROJECT_ID,
            "<c-d>",
        );

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
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("UpdatePreferences"))
        );
        // And the picker stayed open, now showing one entry.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::Project
            }
        ));
        assert_eq!(state.frontend.project_picker().items().len(), 1);
    }

    #[rstest::rstest]
    fn ctrl_d_on_an_empty_picker_is_a_noop() {
        // Given a project picker with no curated projects.
        let mut state = state_with_projects(&[]);

        // When removing the highlighted entry (<c-d>) — there is none.
        let result = crate::feat::picker::action::run_action(
            &mut state,
            &build_picker_registry(),
            PROJECT_ID,
            "<c-d>",
        );

        // Then nothing happened: no messages, prefs untouched.
        assert!(result.message_names.is_empty());
        assert!(state.frontend.preferences.projects.is_empty());
    }
}
