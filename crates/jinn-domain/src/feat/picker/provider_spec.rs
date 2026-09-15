//! The model/provider picker's spec — behavior authored once in the builder.
//!
//! Open signals the provider actor to load entries from the registry (the
//! load needs `Services`, so it stays an async round-trip), TAB toggles the
//! highlighted entry's alloy check in place, CTRL+A flips single/alloy mode,
//! and CTRL+R refreshes the model cache through the actor. Enter resolves the
//! confirmed selection — single model, or the checked-set union the highlight
//! as an alloy — and switches the session.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::ChatEntry;
use crate::common::app_state::AppState;
use crate::feat::picker::style::selected_style;
use crate::feat::picker::style::split_match_indices;
use crate::feat::preferences_actor::protocol::app_state_command::AppStateUpdate;
use crate::feat::preferences_actor::protocol::app_state_command::UpdateAppState;
use crate::feat::provider::ProviderState;
use crate::feat::provider::picker_entry::ProviderPickerEntry;
use crate::feat::provider::protocol::command::LoadProviderPickerEntries;
use crate::feat::provider::protocol::command::ProviderSwitch;
use crate::feat::provider::protocol::command::RefreshModels;
use crate::feat::session::model_selection::AlloyStrategy;
use crate::feat::session::model_selection::ModelSelection;

/// Builds the model picker's spec.
#[must_use]
pub fn provider_spec() -> PickerSpec<ProviderPickerEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::PROVIDER_ID))
        .title(" Model ")
        .row(provider_row)
        .search(|entry| format!("{} {}", entry.model, entry.provider_name))
        .on_open(open_provider)
        .bind("<tab>", "toggle", toggle_selected)
        .bind("<c-a>", "alloy", toggle_alloy)
        .bind("<c-r>", "refresh", refresh_models)
        .on_confirm(confirm_provider)
        .status(provider_status)
}

/// The domain state behind an [`ActionCtx`]. The kernel's host lens always
/// lends `AppState`; this downcast is the spec's single sanctioned escape.
fn state_of<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state_any()
        .downcast_mut::<AppState>()
        .expect("domain host lends AppState")
}

/// The read-only domain state behind a [`StatusCtx`].
fn state_ref_of<'a>(ctx: &'a StatusCtx<'_>) -> &'a AppState {
    ctx.state_any_ref()
        .downcast_ref::<AppState>()
        .expect("domain host lends AppState")
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Renders one picker row: the ✓ check marker (alloy members), a status
/// prefix (✗ unavailable / → alias / * remote), the model name, and the
/// provider name in parens. Filter matches highlight the model and provider
/// portions separately — byte offsets from the search text
/// `"{model} {provider_name}"` split at the separator.
fn provider_row(entry: &ProviderPickerEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let selection_marker = if entry.selected {
        // ✓
        Span::styled(
            "\u{2713} ".to_owned(),
            Style::default().fg(entry.theme.picker_active_marker),
        )
    } else {
        Span::styled("  ".to_owned(), Style::default())
    };

    let status_prefix = if !entry.is_available {
        "\u{2717} " // ✗
    } else if entry.is_alias {
        "\u{2192} " // →
    } else if entry.is_remote {
        "* "
    } else {
        "  "
    };

    let label_style = if entry.is_available {
        selected_style(ctx.is_selected, &entry.theme)
    } else {
        Style::default().fg(entry.theme.muted_text)
    };

    let highlight_bg = entry.theme.picker_highlight_bg;

    // search_text = "{model} {provider_name}"
    // Split match indices into model-portion and provider-portion.
    let (model_indices, provider_indices) =
        split_match_indices(ctx.match_ranges, entry.model.len());

    let mut spans = Vec::new();

    // Prefix (status + alias arrow if applicable).
    if entry.is_alias {
        spans.push(Span::styled(
            format!("{}{} → ", status_prefix, entry.name),
            label_style,
        ));
    } else {
        spans.push(Span::styled(status_prefix.to_owned(), label_style));
    }

    // Model text with highlights.
    spans.extend(highlight_text_with_bg(
        &entry.model,
        label_style,
        &model_indices,
        highlight_bg,
    ));

    // Suffix: " (" + highlighted provider_name + ")"
    spans.push(Span::styled(" (".to_owned(), label_style));
    spans.extend(highlight_text_with_bg(
        &entry.provider_name,
        label_style,
        &provider_indices,
        highlight_bg,
    ));
    spans.push(Span::styled(")".to_owned(), label_style));

    Line::from(
        std::iter::once(selection_marker)
            .chain(spans)
            .collect::<Vec<_>>(),
    )
}

/// The status line: the model cache's age plus the live alloy-mode state
/// (`N selected` while alloy is on). The generated keybind row advertises
/// the keys; this line carries the dynamic state.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn provider_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let gray = Style::default().fg(state.frontend.theme.muted_text);
    let orange = Style::default().fg(state.frontend.theme.accent_action);

    let mut spans = Vec::new();
    if let Some(cache) = state.provider.model_cache.as_ref()
        && let Some(ts) = cache.last_updated_at
    {
        let elapsed = jiff::Timestamp::now() - ts;
        let secs = elapsed
            .total(jiff::Unit::Second)
            .unwrap_or(0.0)
            .max(0.0)
            .round() as u64;
        let human = humantime::format_duration(std::time::Duration::from_secs(secs));
        spans.push(Span::styled(format!("updated {human} ago"), gray));
    }

    let selected_count = state
        .provider
        .provider_picker
        .items()
        .iter()
        .filter(|e| e.entry().selected)
        .count();
    if state.provider.is_alloy_mode() {
        spans.push(Span::styled(
            format!("alloy \u{00b7} {selected_count} selected"),
            orange,
        ));
    } else {
        spans.push(Span::styled("single model".to_owned(), gray));
    }

    Some(Line::from(spans))
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the model picker: fresh filter + selection, then signal the
/// provider actor to load entries from the registry (it needs `Services`).
fn open_provider(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.provider.provider_picker.reset();
    // Derive alloy mode from the active session's model selection: an
    // existing Alloy opens in alloy mode (with members pre-checked by the
    // loader), anything else opens in single mode.
    state.provider.set_alloy_mode(matches!(
        state.active_session().profile().model,
        ModelSelection::Alloy { .. }
    ));
    PickerOutcome::empty().with_message(LoadProviderPickerEntries)
}

/// TAB on the model picker: flip the highlighted entry's alloy check in
/// place (no cursor move — the checked float to the top), alloy mode only.
fn toggle_selected(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if !state.provider.is_alloy_mode() {
        return PickerOutcome::empty();
    }
    state.provider.provider_picker.with_selected_mut(|item| {
        let entry = item.entry_mut();
        entry.selected = !entry.selected;
    });
    resort_provider_picker(&mut state.provider.provider_picker);
    PickerOutcome::empty()
}

/// CTRL+A on the model picker: flip single/alloy mode. Entering alloy
/// pre-checks the session's current models; leaving clears every check.
/// Either way the checked entries float back to the top.
fn toggle_alloy(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let now_alloy = state.provider.toggle_alloy_mode();

    if now_alloy {
        // Entered alloy mode: pre-check the current session model's entries,
        // so editing an existing alloy only requires swapping members.
        let model_selection = state.active_session().profile().model.clone();
        let mut items = state.provider.provider_picker.items().to_vec();
        for item in &mut items {
            crate::feat::provider::loader::pre_check_active_models(
                std::slice::from_mut(item.entry_mut()),
                &model_selection,
            );
        }
        state.provider.provider_picker.set_items(items);
    } else {
        // Left alloy mode: clear every check.
        let mut items = state.provider.provider_picker.items().to_vec();
        for item in &mut items {
            item.entry_mut().selected = false;
        }
        state.provider.provider_picker.set_items(items);
    }

    resort_provider_picker(&mut state.provider.provider_picker);
    PickerOutcome::empty()
}

/// CTRL+R on the model picker: refresh the model cache through the provider
/// actor, pushing a transient chat entry to explain the pause. Gated on a
/// provider being configured at all.
fn refresh_models(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if crate::feat::session::validator::validate_refresh_models(state).is_err() {
        return PickerOutcome::empty();
    }
    state
        .active_session_mut()
        .push_entry(ChatEntry::transient("Refreshing models..."));
    PickerOutcome::empty().with_message(RefreshModels)
}

/// Enter on the model picker: gate on the highlighted entry's availability,
/// resolve Single vs Alloy from the checked set plus the highlight, then
/// switch the session and seed the global last-model default.
fn confirm_provider(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    // The highlighted entry is the foundation of both modes, and its
    // availability gates the entire confirm.
    let Some(highlight) = state_of(ctx)
        .provider
        .provider_picker
        .selected_item()
        .cloned()
    else {
        return PickerOutcome::empty();
    };
    if !highlight.entry().is_available {
        return PickerOutcome::empty();
    }
    let highlight_id = highlight.entry().provider_id.clone();

    let state = state_of(ctx);
    let model_selection = resolve_provider_selection(&state.provider, highlight_id);
    let last_model = Some(model_selection.clone());
    let session_id = state.session.active_session_id().clone();

    PickerOutcome::empty()
        .with_message(ProviderSwitch {
            session_id,
            provider_id: model_selection,
        })
        .with_message(UpdateAppState {
            updates: vec![AppStateUpdate::SetLastModel(last_model)],
        })
        .close()
}

/// Resolves the provider confirmation decision for the given highlighted entry.
///
/// Single mode: the highlighted entry becomes `ModelSelection::Single`.
/// Alloy mode: the checked set union the highlight (deduped); one model -> `Single`,
/// two or more -> `Alloy`.
fn resolve_provider_selection(provider: &ProviderState, highlighted: String) -> ModelSelection {
    if !provider.is_alloy_mode() {
        return ModelSelection::Single(highlighted);
    }
    let mut models: Vec<String> = provider
        .provider_picker
        .items()
        .iter()
        .filter(|e| e.entry().selected && e.entry().is_available)
        .map(|e| e.entry().provider_id.clone())
        .collect();

    // Force-include the highlighted entry (ENTER adds it before committing).
    if !models.contains(&highlighted) {
        models.push(highlighted);
    }

    if models.len() <= 1 {
        ModelSelection::Single(models.into_iter().next().unwrap_or_default())
    } else {
        ModelSelection::Alloy {
            models,
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        }
    }
}

/// Re-sorts provider picker entries: checked entries float to the top
/// (stable within each group).
fn resort_provider_picker(
    picker: &mut jinn_selection_widget::SelectionState<
        jinn_picker::PickerEntry<ProviderPickerEntry>,
    >,
) {
    let mut items: Vec<jinn_picker::PickerEntry<ProviderPickerEntry>> = picker.items().to_vec();
    items.sort_by_key(|item| !item.entry().selected);
    picker.set_items(items);
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
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::intent::handle_open_picker;
    use crate::feat::session::ChatSessionState;
    use crate::feat::theme::default_theme;
    use crate::protocol::IntentResult;

    /// A raw provider entry builder for tests.
    fn entry(
        provider_id: &str,
        model: &str,
        available: bool,
        selected: bool,
    ) -> ProviderPickerEntry {
        ProviderPickerEntry {
            provider_id: provider_id.to_owned(),
            name: "prov".to_owned(),
            provider_name: "prov".to_owned(),
            backend: "openrouter".to_owned(),
            model: model.to_owned(),
            search_text: format!("{model} prov"),
            is_alias: false,
            alias_target: None,
            is_available: available,
            is_remote: false,
            is_active: false,
            selected,
            theme: default_theme(),
        }
    }

    /// AppState with an active session and `n` available provider entries
    /// wrapped through the spec's hooks (cursor on the first entry).
    fn state_with_provider_picker(n: usize) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let entries: Vec<ProviderPickerEntry> = (0..n)
            .map(|i| {
                entry(
                    &format!("prov/model-{i}"),
                    &format!("model-{i}"),
                    true,
                    false,
                )
            })
            .collect();
        state.provider.provider_picker.set_items(wrap(entries));
        state // selection starts on the first entry
    }

    /// Wraps raw entries through the registered spec's hooks.
    fn wrap(
        entries: Vec<ProviderPickerEntry>,
    ) -> Vec<jinn_picker::PickerEntry<ProviderPickerEntry>> {
        crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::PROVIDER_ID, entries)
            .expect("provider spec is registered")
    }

    /// Opens the picker through the real open path.
    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        handle_open_picker(state, PickerKind::Provider, &registry);
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(
            PickerId::new(crate::feat::picker::registry::PROVIDER_ID),
            &mut host,
        );
        f(&mut ctx)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_resets_and_emits_load_message() {
        // Given a session on a single model.
        let mut state = state_with_provider_picker(2);
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
        state.provider.set_alloy_mode(true); // stale — open must derive single

        // When opening the provider picker.
        let result: IntentResult = {
            let registry = crate::feat::picker::registry::build_picker_registry();
            handle_open_picker(&mut state, PickerKind::Provider, &registry)
        };

        // Then the open emits the actor load message.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("LoadProviderPickerEntries")),
            "open should emit LoadProviderPickerEntries: {:?}",
            result.message_names
        );
        // And the mode derives from the session (single), not the stale flag.
        assert!(
            !state.provider.is_alloy_mode(),
            "picker should open in single mode for a single-model session"
        );
    }

    #[rstest::rstest]
    #[test]
    fn open_sets_alloy_mode_for_alloy_session() {
        // Given a session on an alloy of two models.
        let mut state = state_with_provider_picker(2);
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "openrouter/gpt-4".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        // When opening the provider picker.
        open(&mut state);

        // Then alloy mode is on.
        assert!(
            state.provider.is_alloy_mode(),
            "picker should open in alloy mode for an alloy session"
        );
    }

    // ── TAB toggle ────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn tab_toggles_check_in_place_in_alloy_mode() {
        // Given a picker with two available entries, cursor on the first, alloy on.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(true);
        assert_eq!(state.provider.provider_picker.selection(), 0);

        // When pressing TAB.
        let _ = run(&mut state, toggle_selected);

        // Then the first entry is checked and the cursor did not move.
        assert!(
            state.provider.provider_picker.items()[0].entry().selected,
            "first entry should be checked after TAB"
        );
        assert_eq!(
            state.provider.provider_picker.selection(),
            0,
            "cursor should not advance after toggle"
        );
    }

    #[rstest::rstest]
    #[test]
    fn tab_is_a_noop_in_single_mode() {
        // Given single mode.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(false);

        // When pressing TAB.
        let _ = run(&mut state, toggle_selected);

        // Then nothing became checked.
        let any_selected = state
            .provider
            .provider_picker
            .items()
            .iter()
            .any(|item| item.entry().selected);
        assert!(!any_selected, "single-mode TAB must not check anything");
    }

    #[rstest::rstest]
    #[test]
    fn tab_toggles_off_an_already_checked_entry() {
        // Given alloy mode with the highlighted entry already checked.
        let mut state = state_with_provider_picker(1);
        state.provider.set_alloy_mode(true);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().selected = true);

        // When pressing TAB twice-worth of state (one toggle).
        let _ = run(&mut state, toggle_selected);

        // Then the entry is unchecked.
        assert!(
            !state.provider.provider_picker.items()[0].entry().selected,
            "entry should be deselected after toggling off"
        );
    }

    // ── CTRL+A alloy mode ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn ctrl_a_pre_checks_current_models_on_enter() {
        // Given single mode with the session on an alloy of two models.
        let mut state = state_with_provider_picker(2);
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["prov/model-0".to_owned(), "prov/model-1".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });
        state.provider.set_alloy_mode(false);

        // When pressing CTRL+A.
        let _ = run(&mut state, toggle_alloy);

        // Then alloy mode is on and both members are pre-checked.
        assert!(state.provider.is_alloy_mode(), "mode should flip to alloy");
        let checked = state
            .provider
            .provider_picker
            .items()
            .iter()
            .filter(|item| item.entry().selected)
            .count();
        assert_eq!(checked, 2, "both alloy members should be pre-checked");
    }

    #[rstest::rstest]
    #[test]
    fn ctrl_a_clears_checks_on_exit() {
        // Given alloy mode with both entries checked.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(true);
        state.provider.provider_picker.set_items(wrap(vec![
            entry("prov/model-0", "model-0", true, true),
            entry("prov/model-1", "model-1", true, true),
        ]));

        // When pressing CTRL+A.
        let _ = run(&mut state, toggle_alloy);

        // Then mode is single and no entries remain checked.
        assert!(
            !state.provider.is_alloy_mode(),
            "mode should flip to single"
        );
        let any_checked = state
            .provider
            .provider_picker
            .items()
            .iter()
            .any(|item| item.entry().selected);
        assert!(
            !any_checked,
            "all checks should be cleared on leaving alloy mode"
        );
    }

    // ── CTRL+R refresh ────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn ctrl_r_pushes_transient_and_emits_refresh() {
        // Given a picker whose session has a provider configured.
        let mut state = state_with_provider_picker(2);
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("prov/model-0".to_owned()));

        // When pressing CTRL+R.
        let outcome = run(&mut state, refresh_models);

        // Then a transient chat entry is pushed and RefreshModels is emitted.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("RefreshModels")),
            "refresh should emit RefreshModels: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn ctrl_r_is_a_noop_without_a_provider() {
        // Given a session on no provider.
        let mut state = state_with_provider_picker(2);
        state
            .active_session_mut()
            .set_model(ModelSelection::default()); // the no-provider sentinel

        // When pressing CTRL+R.
        let outcome = run(&mut state, refresh_models);

        // Then nothing is emitted.
        assert!(
            outcome.message_names.is_empty(),
            "refresh must be gated on a configured provider"
        );
    }

    // ── Confirm ───────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn confirm_single_mode_emits_provider_switch() {
        // Given single mode with a stale check on model-0 and model-1 highlighted.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(false);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().selected = true);
        state.provider.provider_picker.move_down(1); // highlight model-1

        // When confirming through the spec.
        let outcome = run(&mut state, confirm_provider);

        // Then ProviderSwitch is emitted.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("ProviderSwitch")),
            "confirm should emit ProviderSwitch: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn single_mode_resolution_ignores_checks() {
        // Given single mode with a stale check on model-0 and model-1 highlighted.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(false);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().selected = true);
        state.provider.provider_picker.move_down(1); // highlight model-1

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-1".to_owned());

        // Then it is Single of the highlighted entry, not the checked one.
        assert_eq!(selection, ModelSelection::Single("prov/model-1".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn alloy_resolution_unions_checks_with_highlight() {
        // Given alloy mode with model-0 checked and model-2 highlighted.
        let mut state = state_with_provider_picker(3);
        state.provider.set_alloy_mode(true);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().selected = true); // check model-0 (cursor at 0)
        state.provider.provider_picker.move_down(1);
        state.provider.provider_picker.move_down(1); // highlight model-2

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-2".to_owned());

        // Then it is an Alloy containing both model-0 and model-2.
        match selection {
            ModelSelection::Alloy { models, .. } => {
                assert_eq!(models.len(), 2);
                assert!(models.contains(&"prov/model-0".to_owned()));
                assert!(models.contains(&"prov/model-2".to_owned()));
            }
            other => panic!("expected Alloy, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn alloy_resolution_dedups_checked_highlight() {
        // Given alloy mode with the highlighted entry (model-0) already checked.
        let mut state = state_with_provider_picker(1);
        state.provider.set_alloy_mode(true);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().selected = true);

        // When resolving (highlight is already checked).
        let selection = resolve_provider_selection(&state.provider, "prov/model-0".to_owned());

        // Then it collapses to Single (one model, no duplication).
        assert_eq!(
            selection,
            ModelSelection::Single("prov/model-0".to_owned()),
            "already-checked highlight must not duplicate; 1-model set collapses to Single"
        );
    }

    #[rstest::rstest]
    #[test]
    fn alloy_resolution_one_model_collapses_to_single() {
        // Given alloy mode with nothing checked and model-1 highlighted.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(true);

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-1".to_owned());

        // Then a Single selection is returned (1-model alloy collapses).
        assert_eq!(selection, ModelSelection::Single("prov/model-1".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn confirm_rejects_unavailable_highlight() {
        // Given the highlighted entry is unavailable.
        let mut state = state_with_provider_picker(1);
        state
            .provider
            .provider_picker
            .with_selected_mut(|item| item.entry_mut().is_available = false);

        // When confirming through the spec.
        let outcome = run(&mut state, confirm_provider);

        // Then nothing is emitted.
        assert!(
            outcome.message_names.is_empty(),
            "unavailable highlight must be rejected"
        );
    }

    #[rstest::rstest]
    #[test]
    fn confirm_emits_last_model_and_closes() {
        // Given single mode with the first entry highlighted.
        let mut state = state_with_provider_picker(1);

        // When confirming through the spec.
        let outcome = run(&mut state, confirm_provider);

        // Then the outcome closes the picker and carries the app-state update.
        assert!(outcome.close, "confirm should close the picker");
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("UpdateAppState")),
            "confirm should seed the global last model: {:?}",
            outcome.message_names
        );
    }
}
