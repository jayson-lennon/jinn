//! Spec-driven picker actions — the registry-first dispatch layer.
//!
//! These functions sit between the intent handler and the erased specs:
//! they resolve the active picker's spec through the [`PickerRegistry`],
//! build the [`ActionCtx`] over the domain [`PickerHost`] impl, run the
//! hook, and fold the [`PickerOutcome`] into an [`IntentResult`] (messages
//! drained; `close` pops the picker scope).
//!
//! Every function is a no-op returning an empty result when the active
//! picker has no spec (unmigrated kinds fall through to legacy arms) or
//! the id/action doesn't resolve — stale intents are ignored, never panics.

use jinn_picker::ActionCtx;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerRegistry;

use crate::common::app_state::AppState;
use crate::feat::picker::host_impl::AppStatePickerHost;
use crate::protocol::intent::IntentResult;

/// Folds a picker outcome into an intent result; `close` clears the
/// overlay scopes back to Normal — the same landing spot trunk's ESC from
/// a picker produced (`clear_overlays`), so a picker opened from Input
/// (e.g. the model picker) doesn't strand the user in a stale Input scope.
fn fold(state: &mut AppState, outcome: PickerOutcome) -> IntentResult {
    let close = outcome.close;
    let mut result = IntentResult::empty();
    result.messages = outcome.messages;
    result.message_names = outcome.message_names;
    if close {
        state.frontend.scope_clear_overlays();
    }
    result
}

/// Runs the active picker's spec hook for `which`, when declared.
///
/// Per-hook fallback: a lifecycle step is spec-driven only when the
/// spec *declares that hook* — anything the spec hasn't taken over yet
/// still flows to the legacy per-kind handlers. This is what keeps the
/// repository green between registering a spec stub and migrating its
/// hooks; when all pickers have migrated, the fallbacks disappear.
pub(crate) fn run_active_hook(
    state: &mut AppState,
    registry: &PickerRegistry,
    hook: Hook,
) -> IntentResult {
    let Some(kind) = state.frontend.picker_kind() else {
        return IntentResult::empty();
    };
    let Some(id) = crate::feat::picker::registry::spec_id_for_kind(&kind) else {
        return IntentResult::empty();
    };
    let Some(spec) = registry.get(id) else {
        return IntentResult::empty();
    };
    let declared = match hook {
        Hook::Open => spec.has_open(),
        Hook::Confirm => spec.has_confirm(),
        Hook::Close => spec.has_close(),
    };
    if !declared {
        return IntentResult::empty();
    }
    let picker_id = jinn_picker::PickerId::new(spec.id().as_str());
    let outcome = {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(picker_id, &mut host);
        match hook {
            Hook::Open => spec.run_open(&mut ctx),
            Hook::Confirm => spec.run_confirm(&mut ctx),
            Hook::Close => spec.run_close(&mut ctx),
        }
    };
    fold(state, outcome)
}

/// Runs the active picker's selection-change hook when its spec declares
/// one — the live preview fired after the cursor moved or the page turned.
/// A no-op when no spec is active or the spec has no selection-change
/// behavior (unmigrated/hookless pickers are untouched).
pub fn run_selection_change(state: &mut AppState, registry: &PickerRegistry) {
    let Some(kind) = state.frontend.picker_kind() else {
        return;
    };
    let Some(id) = crate::feat::picker::registry::spec_id_for_kind(&kind) else {
        return;
    };
    let Some(spec) = registry.get(id) else {
        return;
    };
    if !spec.has_selection_change() {
        return;
    }
    // The cursor position lives in the picker's selection storage, whose
    // concrete type only the typed spec knows — resolve it through the
    // erased seam, then run the hook.
    let picker_id = jinn_picker::PickerId::new(spec.id().as_str());
    let index = {
        let host = crate::feat::picker::host_impl::AppStateRenderHost::new(state);
        spec.selected_index(&host)
    };
    let mut host = AppStatePickerHost::new(state);
    let mut ctx = ActionCtx::new(picker_id, &mut host);
    spec.run_selection_change(index, &mut ctx);
}

/// The lifecycle hook to run for the active picker.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Hook {
    /// The open hook.
    Open,
    /// The confirm hook (Enter).
    Confirm,
    /// The close hook (ESC revert path).
    Close,
}

/// Runs the active picker's close hook when it has a spec. Returns
/// `Some(result)` when the hook ran (the caller stops — legacy restores
/// must not double-apply), `None` when no spec is active.
pub fn try_close_active(state: &mut AppState, registry: &PickerRegistry) -> Option<IntentResult> {
    let kind = state.frontend.picker_kind()?;
    let id = crate::feat::picker::registry::spec_id_for_kind(&kind)?;
    let spec = registry.get(id)?;
    if !spec.has_close() {
        return None;
    }
    Some(run_active_hook(state, registry, Hook::Close))
}

/// Runs the `picker` spec's `action`-named bind.
pub fn run_action(
    state: &mut AppState,
    registry: &PickerRegistry,
    picker: &str,
    action: &str,
) -> IntentResult {
    // Guard: the intent's picker must be the one actually open — a stale
    // keypress from a previous scope is ignored.
    let Some(kind) = state.frontend.picker_kind() else {
        return IntentResult::empty();
    };
    let Some(active_id) = crate::feat::picker::registry::spec_id_for_kind(&kind) else {
        return IntentResult::empty();
    };
    if active_id != picker {
        return IntentResult::empty();
    }
    let Some(spec) = registry.get(picker) else {
        return IntentResult::empty();
    };
    let picker_id = jinn_picker::PickerId::new(spec.id().as_str());
    let outcome = {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(picker_id, &mut host);
        spec.run_action(action, &mut ctx)
    };
    fold(state, outcome)
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
    use crate::feat::picker::registry::SKILL_ID;
    use crate::feat::ui::picker_states::PickerExt;
    use crate::protocol::ChatEntryKind;

    fn state_with_skill_picker() -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Picker {
            kind: crate::feat::picker::PickerKind::Skill,
        });
        state
    }

    /// A test spec under the skill id with one `<tab>` bind that pushes a
    /// transient entry, and one `<esc>` bind that closes the picker.
    fn registry_with_test_skill_spec() -> jinn_picker::PickerRegistry {
        let mut registry = crate::feat::picker::registry::build_picker_registry();
        registry.register(crate::feat::picker::skill_spec::SkillEntry::spec_for_tests());
        registry
    }

    #[rstest::rstest]
    #[test]
    fn picker_action_unknown_id_is_a_no_op() {
        // Given an open skill picker and the domain registry.
        let mut state = state_with_skill_picker();
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When running an action naming a picker id that doesn't exist.
        let result = run_action(&mut state, &registry, "nope", "<tab>");

        // Then nothing is emitted.
        assert!(result.messages.is_empty());
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn picker_action_with_wrong_active_picker_is_ignored() {
        // Given an open skill picker.
        let mut state = state_with_skill_picker();
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When running an action addressed to a different picker.
        let result = run_action(&mut state, &registry, "persona", "<tab>");

        // Then nothing is emitted (stale intents are dropped).
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn picker_action_resolves_the_row_and_runs_it() {
        // Given an open skill picker whose test spec declares a `<tab>` row
        // that pushes a transient entry.
        let mut state = state_with_skill_picker();
        let registry = registry_with_test_skill_spec();

        // When running the `<tab>` action.
        let _ = run_action(&mut state, &registry, SKILL_ID, "<tab>");

        // Then the action ran (transient entry pushed).
        let history = state.active_session().history();
        assert!(
            matches!(
                history.last(),
                Some(entry) if matches!(entry.kind, ChatEntryKind::Transient(_))
            ),
            "the <tab> bind action should have run"
        );
    }

    #[rstest::rstest]
    #[test]
    fn picker_action_close_outcome_pops_the_scope() {
        // Given an open skill picker.
        let mut state = state_with_skill_picker();
        let registry = registry_with_test_skill_spec();

        // When running an action whose outcome closes the picker.
        let _ = run_action(&mut state, &registry, SKILL_ID, "<esc>");

        // Then the picker scope is popped.
        assert!(
            state.frontend.picker_kind().is_none(),
            "close outcome must pop the picker scope"
        );
    }

    #[rstest::rstest]
    #[test]
    fn theme_close_via_the_esc_path_restores_the_snapshotted_theme() {
        // Given an open theme picker that previewed a different theme after
        // opening (open snapshotted the pre-open theme).
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut state = AppState::default_with_scope_focus();
        let original_accent = state.frontend.theme.focus_accent;
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        state.plugins.set_themes(
            "theme-loader",
            vec![("other".to_owned(), None, other.clone())],
        );
        crate::feat::picker::intent::handle_open_picker(
            &mut state,
            crate::feat::picker::PickerKind::Theme,
            &registry,
        );
        state.frontend.theme = other;

        // When ESC closes the picker through the IntentHandler path.
        let result = try_close_active(&mut state, &registry);

        // Then the hook ran (legacy restores must not double-apply).
        assert!(result.is_some());
        // And the pre-open theme is restored.
        assert_eq!(state.frontend.theme.focus_accent, original_accent);
        // And the snapshot is consumed and the scope popped.
        assert!(state.frontend.theme_preview_original().is_none());
        assert!(state.frontend.picker_kind().is_none());
    }
}
