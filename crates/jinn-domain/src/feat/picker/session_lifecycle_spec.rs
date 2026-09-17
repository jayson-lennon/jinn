//! The session-lifecycle picker's spec — behavior authored once in the builder.
//!
//! Open loads the implicit "blank" lifecycle plus every configured lifecycle
//! from `jinn.toml`, flagging those whose setup command takes `$`-parameters.
//! Enter either starts the session immediately (no-args lifecycles) or hands
//! off to the arg-input popup so the user can fill the parameters first. The
//! confirm hooks own all scope transitions — neither sets the close flag,
//! because the no-args path manages scopes itself (clear overlays, push the
//! input scope) and the has-args path replaces the picker scope with the
//! arg-input scope in place.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::common::app_state::ArgInputState;
use crate::common::app_state::FocusScope;
use crate::common::line_input::LineInput;
use crate::feat::picker::style::active_marker;
use crate::feat::picker::style::dim_style;
use crate::feat::picker::style::selected_style;
use crate::feat::session_lifecycle::builtin::LifecycleCommand;
use crate::feat::session_lifecycle::command_template::CommandTemplate;
use crate::feat::session_lifecycle::picker_entry::SessionLifecycleEntry;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the session-lifecycle picker's spec.
#[must_use]
pub fn session_lifecycle_spec() -> PickerSpec<SessionLifecycleEntry> {
    PickerSpec::new(PickerId::new(
        crate::feat::picker::registry::SESSION_LIFECYCLE_ID,
    ))
    .title(" Session Lifecycle ")
    .row(lifecycle_row)
    .search(lifecycle_search_text)
    .on_open(open_lifecycle)
    .on_confirm(confirm_lifecycle)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

/// The search text for one lifecycle row: name plus description. An absent
/// description contributes nothing but the separator space.
fn lifecycle_search_text(entry: &SessionLifecycleEntry) -> String {
    match &entry.description {
        Some(desc) => format!("{} {desc}", entry.name),
        None => entry.name.clone(),
    }
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Renders one picker row: the cursor marker, the lifecycle name, a ` *`
/// marker when the setup command needs user-supplied args, and the
/// description after an em-dash separator.
pub fn lifecycle_row(entry: &SessionLifecycleEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let base_style = selected_style(ctx.is_selected, &entry.theme);
    let desc_style = dim_style(ctx.is_selected, &entry.theme);

    let mut spans = vec![active_marker(ctx.is_selected, &entry.theme)];

    if ctx.match_ranges.is_empty() {
        spans.push(Span::styled(entry.name.clone(), base_style));
    } else {
        spans.extend(highlight_text_with_bg(
            &entry.name,
            base_style,
            ctx.match_ranges,
            entry.theme.picker_highlight_bg,
        ));
    }

    if entry.has_args {
        spans.push(Span::styled(" *".to_owned(), desc_style));
    }

    if let Some(desc) = &entry.description {
        spans.push(Span::styled(format!(" \u{2014} {desc}"), desc_style));
    }

    Line::from(spans)
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the lifecycle picker: fresh filter + selection, then load the
/// implicit blank lifecycle plus every configured one. Opening never touches
/// the filesystem and emits no messages.
fn open_lifecycle(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.session_lifecycle_picker_mut().reset();
    load_lifecycle_entries(state);
    PickerOutcome::empty()
}

/// Enter on the lifecycle picker: start the session, or hand off to the
/// arg-input popup when the lifecycle's setup command needs parameters.
///
/// Neither path sets the close flag. The no-args path delegates to
/// [`handle_session_lifecycle_setup`], which itself clears overlays and
/// pushes the input scope — a `close` would wipe that push afterwards. The
/// has-args path swaps the picker scope for the arg-input scope directly.
fn confirm_lifecycle(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let Some(selected) = state_of(ctx)
        .frontend
        .session_lifecycle_picker()
        .selected_item()
        .map(|item| {
            let entry = item.entry();
            (entry.name.clone(), entry.has_args)
        })
    else {
        return PickerOutcome::empty();
    };
    let (lifecycle_name, has_args) = selected;
    let state = state_of(ctx);

    state.frontend.scope_pop();

    if has_args {
        // Save context and open the arg input popup.
        let template_display = state
            .frontend
            .preferences
            .session_lifecycles
            .iter()
            .find(|l| l.name == lifecycle_name)
            .and_then(|l| l.setup.as_ref())
            .and_then(|cmd| match cmd {
                LifecycleCommand::Shell(s) => Some(s.as_str()),
                LifecycleCommand::Builtin(_) => None,
            })
            .map(|cmd| CommandTemplate::parse(cmd).display())
            .unwrap_or_default();

        state.frontend.arg_input = ArgInputState {
            lifecycle_name,
            template_display,
            text: LineInput::new(),
        };
        state.frontend.scope_push(FocusScope::ArgInput);
        return PickerOutcome::empty();
    }

    // No args - proceed directly. The setup function owns the scope
    // transition (clear overlays, push input), so this outcome carries no
    // close signal.
    let result = crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(
        state,
        &lifecycle_name,
        &[],
        None,
    );
    PickerOutcome::from_route_result(result)
}

/// Loads lifecycle entries into the picker: the implicit blank lifecycle
/// first, then every configured lifecycle with its `has_args` flag detected
/// from the setup command's template parameters.
fn load_lifecycle_entries(state: &mut AppState) {
    let mut entries = Vec::new();

    let theme = state.frontend.theme.clone();

    // Always include the implicit blank lifecycle.
    entries.push(SessionLifecycleEntry {
        name: "blank".to_owned(),
        description: Some("New empty session".to_owned()),
        has_args: false,
        theme: theme.clone(),
    });

    // Add lifecycles from preferences.
    for lifecycle in &state.frontend.preferences.session_lifecycles {
        let has_args = lifecycle
            .setup
            .as_ref()
            .and_then(|cmd| match cmd {
                LifecycleCommand::Shell(s) => Some(s.as_str()),
                LifecycleCommand::Builtin(_) => None,
            })
            .is_some_and(|cmd| CommandTemplate::parse(cmd).has_params());
        entries.push(SessionLifecycleEntry {
            name: lifecycle.name.clone(),
            description: lifecycle.description.clone(),
            has_args,
            theme: theme.clone(),
        });
    }

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::SESSION_LIFECYCLE_ID, entries)
            .unwrap_or_default()
    };
    state
        .frontend
        .session_lifecycle_picker_mut()
        .set_items(wrapped);
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
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::registry::SESSION_LIFECYCLE_ID;
    use crate::feat::preferences_actor::user_preferences::SessionLifecycle;
    use crate::feat::session_lifecycle::builtin::LifecycleCommand;

    /// State with an active origin session and the given configured
    /// lifecycles (name, description, setup-with-args).
    fn state_with_lifecycles(lifecycles: &[(&str, Option<&str>, Option<&str>)]) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.preferences.session_lifecycles = lifecycles
            .iter()
            .map(|(name, description, setup)| SessionLifecycle {
                name: (*name).to_owned(),
                description: description.map(std::borrow::ToOwned::to_owned),
                setup: setup.map(|s| LifecycleCommand::Shell(s.to_owned())),
                teardown: None,
            })
            .collect();
        state
    }

    /// Opens the picker through the real open path (scope push + spec open
    /// hook), mirroring what the intent handler does.
    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        crate::feat::picker::intent::handle_open_picker(
            state,
            PickerKind::SessionLifecycle,
            &registry,
        );
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(PickerId::new(SESSION_LIFECYCLE_ID), &mut host);
        f(&mut ctx)
    }

    // ── Open ─────────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_lists_blank_and_configured_lifecycles_with_args_flags() {
        // Given state with a parameterless lifecycle and a `$1` lifecycle.
        let mut state = state_with_lifecycles(&[
            ("plain", Some("No parameters"), None),
            ("templated", None, Some("cd /a/$1")),
        ]);

        // When opening the picker.
        open(&mut state);

        // Then blank comes first, followed by the configured lifecycles.
        let items = state.frontend.session_lifecycle_picker().items();
        let names: Vec<&str> = items.iter().map(|i| i.entry().name.as_str()).collect();
        assert_eq!(names, vec!["blank", "plain", "templated"]);
        // And only the `$1` lifecycle is flagged has_args (blank never is).
        assert!(!items[0].entry().has_args);
        assert!(!items[1].entry().has_args);
        assert!(items[2].entry().has_args);
    }

    // ── Confirm, no args ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn confirm_on_empty_picker_is_a_no_op() {
        // Given an open picker whose entries were never populated (empty
        // registry test seam: the scope is active but storage is empty).
        let mut state = state_with_lifecycles(&[]);

        // When running the confirm hook.
        let outcome = run(&mut state, confirm_lifecycle);

        // Then nothing happened: no session was created and no messages.
        assert_eq!(
            state.session.session_count(),
            1,
            "only the default origin exists"
        );
        assert!(outcome.messages.is_empty());
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn confirm_without_args_starts_the_session() {
        // Given an open picker with the blank lifecycle selected.
        let mut state = state_with_lifecycles(&[]);
        open(&mut state);

        // When confirming the selection.
        let outcome = run(&mut state, confirm_lifecycle);

        // Then a second session was created and is active.
        assert_eq!(
            state.session.session_count(),
            2,
            "a new session was created"
        );
        // And the lifecycle name was stamped on the new session (the
        // "blank" pseudo-entry is stamped verbatim, as legacy did).
        assert_eq!(state.active_session().lifecycle_name(), Some("blank"));
        // And the setup messages were emitted for the new session.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.ends_with("SessionCreated")),
            "SessionCreated must be emitted: {:?}",
            outcome.message_names
        );
        // And the picker closed by clearing overlays (input scope active).
        assert_eq!(
            state.frontend.scope(),
            FocusScope::Input,
            "setup transitions to the input scope"
        );
    }

    #[rstest::rstest]
    #[test]
    fn confirm_scripted_lifecycle_without_params_stamps_and_runs_setup() {
        // Given an open picker with a no-args scripted lifecycle selected.
        let mut state =
            state_with_lifecycles(&[("research", Some("Research setup"), Some("echo ready"))]);
        open(&mut state);
        state.frontend.session_lifecycle_picker_mut().move_down(1);

        // When confirming.
        let outcome = run(&mut state, confirm_lifecycle);

        // Then the new session carries the lifecycle name.
        assert_eq!(state.active_session().lifecycle_name(), Some("research"),);
        // And the setup run message was emitted.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.ends_with("RunSessionSetup")),
            "RunSessionSetup must be emitted: {:?}",
            outcome.message_names
        );
    }

    // ── Confirm, with args ───────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn confirm_with_args_opens_the_arg_input_popup() {
        // Given an open picker with a `$1` lifecycle selected.
        let mut state = state_with_lifecycles(&[
            ("project-a", None, Some("cd /a/$1")),
            ("project-b", None, Some("cd /b/$1")),
        ]);
        open(&mut state);
        // blank -> project-a -> project-b (move_down is single-step).
        state.frontend.session_lifecycle_picker_mut().move_down(1);
        state.frontend.session_lifecycle_picker_mut().move_down(1);

        // When confirming.
        let outcome = run(&mut state, confirm_lifecycle);

        // Then the arg-input state holds the right lifecycle and template
        // (the dialectic regression: find() must match the selected one).
        assert_eq!(state.frontend.arg_input.lifecycle_name, "project-b");
        assert!(
            state.frontend.arg_input.template_display.contains("/b/"),
            "template_display should come from project-b's setup, got: {}",
            state.frontend.arg_input.template_display,
        );
        // And the picker scope was replaced by the arg-input scope.
        assert_eq!(state.frontend.scope(), FocusScope::ArgInput);
        // And no close signal (the hook manages scopes itself).
        assert!(!outcome.close);
        assert!(outcome.messages.is_empty());
    }
}
