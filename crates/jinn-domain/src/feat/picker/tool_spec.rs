//! The tool picker's spec — behavior authored once in the builder.
//!
//! TAB toggles the highlighted tool's enabled state and advances the cursor
//! (checklist style), Enter writes the collected disabled set back to the
//! session profile, and ESC restores the snapshotted pre-open set. Open
//! resets the picker, snapshots the session's `disabled_tools`, and loads
//! entries from the session's tool definitions — only tools available for
//! the session's provider, seeded from the disabled set, sorted
//! case-insensitively by name.

use std::collections::HashSet;

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use jinn_selection_widget::highlight::highlight_text_with_bg;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::picker::style::dim_style;
use crate::feat::picker::style::split_match_indices;
use crate::feat::tools_actor::tool_entry::ToolEntry;
use crate::feat::ui::picker_states::PickerExt;

/// Builds the tool picker's spec.
#[must_use]
pub fn tool_spec() -> PickerSpec<ToolEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::TOOL_ID))
        .title(" Tools ")
        .row(tool_row)
        .search(|entry| format!("{} {}", entry.name, entry.description))
        .on_open(open_tool)
        .bind("<tab>", "toggle", tool_toggle)
        .on_confirm(confirm_tool)
        .on_close(restore_tool)
        .status(tool_status)
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

/// Renders one picker row: the ✓/✗ enablement marker, the tool name, and an
/// em-dash description. Filter matches highlight within the name and the
/// description separately — byte offsets from the search text
/// `"{name} {description}"` split at the separator.
fn tool_row(entry: &ToolEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let style = if ctx.is_selected {
        Style::default()
            .fg(entry.theme.primary_text)
            .bg(entry.theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let (marker, marker_color) = if entry.enabled {
        ("\u{2713} ", entry.theme.focus_accent) // ✓
    } else {
        ("\u{2717} ", entry.theme.error_text) // ✗
    };
    let marker_span = Span::styled(marker.to_owned(), Style::default().fg(marker_color));

    // Match ranges are byte offsets into "{name} {description}"; the space
    // separator sits at byte offset `name_len`.
    let (name_indices, desc_indices) = split_match_indices(ctx.match_ranges, entry.name.len());

    let name_spans = highlight_text_with_bg(
        &entry.name,
        style,
        &name_indices,
        entry.theme.picker_highlight_bg,
    );

    // Separator and description in dim style.
    let desc_style = dim_style(ctx.is_selected, &entry.theme);
    let sep_span = Span::styled(" \u{2014} ".to_owned(), desc_style);
    let desc_spans = highlight_text_with_bg(
        &entry.description,
        desc_style,
        &desc_indices,
        entry.theme.picker_highlight_bg,
    );

    let mut spans = vec![marker_span];
    spans.extend(name_spans);
    spans.push(sep_span);
    spans.extend(desc_spans);
    Line::from(spans)
}

/// The status line: the live enabled count, e.g. `3/7 enabled`.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn tool_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let items = state.frontend.tool_picker().items();
    let enabled = items.iter().filter(|item| item.entry().enabled).count();
    let total = items.len();
    Some(Line::from(Span::styled(
        format!("{enabled}/{total} enabled"),
        Style::default().fg(state.frontend.theme.muted_text),
    )))
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the tool picker: fresh filter + selection, snapshot the session's
/// disabled set for the ESC revert, and load the provider-available tools.
fn open_tool(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.tool_picker_mut().reset();
    // Snapshot current disabled tools so ESC can restore them.
    *state.frontend.tool_picker_snapshot_mut() =
        Some(state.active_session().disabled_tools().clone());
    load_tool_entries(state);
    PickerOutcome::empty()
}

/// TAB on the tool picker: flip the highlighted tool's enabled state and
/// advance the cursor (checklist style) by the measured viewport.
fn tool_toggle(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let viewport = crate::feat::picker::geometry::active_viewport(state);
    state.frontend.tool_picker_mut().with_selected_mut(|item| {
        let entry = item.entry_mut();
        entry.enabled = !entry.enabled;
    });
    state.frontend.tool_picker_mut().move_down(viewport);
    PickerOutcome::empty()
}

/// Enter on the tool picker: collect the disabled tool names from all
/// entries and write them to the session profile. No bus message — the
/// profile is the in-memory session's source of truth (legacy parity).
fn confirm_tool(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let disabled: HashSet<String> = state
        .frontend
        .tool_picker()
        .items()
        .iter()
        .filter(|item| !item.entry().enabled)
        .map(|item| item.entry().name.clone())
        .collect();

    state.active_session_mut().set_disabled_tools(disabled);
    *state.frontend.tool_picker_snapshot_mut() = None;
    PickerOutcome::empty().close()
}

/// ESC on the tool picker (the revert path — never the confirm path):
/// restore the snapshotted pre-open disabled set. Defensive: confirms clear
/// the snapshot, so this is normally a no-op. Signals `close` so the
/// dispatch layer pops the picker scope.
fn restore_tool(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if let Some(snapshot) = state.frontend.tool_picker_snapshot_mut().take() {
        state.active_session_mut().set_disabled_tools(snapshot);
    }
    PickerOutcome::empty().close()
}

/// Loads tools into the tool picker from the session's tool definitions.
///
/// Only tools available for the session's provider are offered; entries are
/// seeded from the session's disabled set and sorted case-insensitively by
/// name. Opening the picker never touches the filesystem.
fn load_tool_entries(state: &mut AppState) {
    let (disabled, provider_name, theme) = {
        let active_session = state.active_session();
        let disabled = active_session.disabled_tools().clone();
        let provider_name = active_session.model_selection().provider_name().to_owned();
        let theme = state.frontend.theme.clone();
        (disabled, provider_name, theme)
    };
    let active_id = state.session.active_session_id().clone();
    let mut entries: Vec<ToolEntry> = {
        let Some(registry) = state.tool_registry() else {
            return;
        };
        registry
            .read()
            .tools_for_session(&active_id)
            .into_iter()
            .filter(|def| def.available_for_provider(&provider_name))
            .map(|def| {
                let name = def.name.clone();
                let description = def.description.clone();
                ToolEntry {
                    name,
                    description,
                    enabled: !disabled.contains(&def.name),
                    theme: theme.clone(),
                }
            })
            .collect()
    };

    entries.sort_by_key(|e| e.name.to_lowercase());

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::TOOL_ID, entries)
            .unwrap_or_default()
    };
    state.frontend.tool_picker_mut().set_items(wrapped);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::single_range_in_vec_init,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::feat::picker::registry::TOOL_ID;

    /// State with an active session and the given tool definitions
    /// registered in the context.
    fn state_with_tools(defs: &[(&str, &str, Option<jinn_provider::ServerToolType>)]) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let origin = crate::feat::session::chat_session::ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let registry = state
            .tool_registry()
            .expect("tools registry seeded by default_with_scope_focus");
        registry.update(|r| {
            for (name, description, server_tool_type) in defs {
                r.global.insert(
                    (*name).to_owned(),
                    crate::protocol::ToolDefinition {
                        name: (*name).to_owned(),
                        description: (*description).to_owned(),
                        parameters: serde_json::json!({}),
                        prompt_snippet: None,
                        prompt_guidelines: vec![],
                        server_tool_type: server_tool_type.clone(),
                    },
                );
            }
        });
        state
    }

    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        crate::feat::picker::intent::handle_open_picker(
            state,
            crate::feat::picker::PickerKind::Tool,
            &registry,
        );
    }

    fn tool_state() -> AppState {
        state_with_tools(&[
            ("edit", "Edit files", None),
            ("Bash", "Run shell", None),
            ("read", "Read files", None),
        ])
    }

    #[rstest::rstest]
    #[test]
    fn open_loads_provider_available_tools_sorted_and_snapshots() {
        // Given a session with three tools and one pre-disabled.
        let mut state = tool_state();
        state
            .active_session_mut()
            .set_disabled_tools(["read"].iter().map(|s| (*s).to_owned()).collect());

        // When opening the tool picker.
        open(&mut state);

        // Then the disabled set is snapshotted for the ESC revert.
        let snapshotted = state
            .frontend
            .tool_picker_snapshot()
            .clone()
            .unwrap_or_default();
        assert_eq!(
            snapshotted,
            ["read"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<std::collections::HashSet<_>>()
        );
        // And entries are sorted case-insensitively by name.
        let names: Vec<&str> = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert_eq!(names, vec!["Bash", "edit", "read"]);
        // And the pre-disabled tool renders disabled.
        assert!(
            state
                .frontend
                .tool_picker()
                .items()
                .iter()
                .find(|item| item.entry().name == "read")
                .is_some_and(|item| !item.entry().enabled)
        );
    }

    #[rstest::rstest]
    #[test]
    fn tab_toggle_flips_enabled_and_advances_the_cursor() {
        // Given an open tool picker with the first entry selected.
        let mut state = tool_state();
        open(&mut state);
        assert_eq!(state.frontend.tool_picker().selection(), 0);
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When pressing TAB.
        let _ = crate::feat::picker::action::run_action(&mut state, &registry, TOOL_ID, "<tab>");

        // Then the selected entry flipped and the cursor advanced.
        assert!(
            !state.frontend.tool_picker().items()[0].entry().enabled,
            "first entry must be toggled off"
        );
        assert_eq!(state.frontend.tool_picker().selection(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn toggle_on_empty_picker_is_a_no_op() {
        // Given an open tool picker with no entries (empty tool context).
        let mut state = state_with_tools(&[]);
        open(&mut state);
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When pressing TAB.
        let _ = crate::feat::picker::action::run_action(&mut state, &registry, TOOL_ID, "<tab>");

        // Then nothing panicked and nothing is selected.
        assert!(state.frontend.tool_picker().selected_item().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn confirm_writes_disabled_set_clears_snapshot_and_closes() {
        // Given an open tool picker with the first entry toggled off.
        let mut state = tool_state();
        open(&mut state);
        let registry = crate::feat::picker::registry::build_picker_registry();
        let _ = crate::feat::picker::action::run_action(&mut state, &registry, TOOL_ID, "<tab>");

        // When confirming (the spec-driven confirm path, folded like the
        // dispatch layer does).
        {
            let picker_id = PickerId::new(TOOL_ID);
            let outcome = {
                let mut host = crate::feat::picker::host_impl::AppStatePickerHost::new(&mut state);
                let mut ctx = ActionCtx::new(picker_id, &mut host);
                let spec = registry.get(TOOL_ID).expect("tool spec registered");
                spec.run_confirm(&mut ctx)
            };
            // Then the confirm closes the picker and emits nothing (legacy
            // parity: the disabled set goes straight to the session
            // profile, no bus message).
            assert!(outcome.close, "confirm must close the picker");
            assert!(outcome.message_names.is_empty());
        }
        // The dispatch layer's fold clears overlay scopes on `close` —
        // mirror that so the closed-picker assertion holds.
        state.frontend.scope_clear_overlays();
        // And the disabled set was written to the session profile.
        let profile = state.active_session().profile();
        assert!(
            profile.disabled_tools.contains("Bash"),
            "toggled-off tool must land in the profile"
        );
        assert_eq!(profile.disabled_tools.len(), 1);
        // And the snapshot was cleared.
        assert!(state.frontend.tool_picker_snapshot().is_none());
        // And the picker closed.
        assert!(state.frontend.picker_kind().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn escape_restores_the_snapshotted_disabled_set() {
        // Given an open tool picker with a pre-disabled set and one toggle
        // applied.
        let mut state = tool_state();
        state
            .active_session_mut()
            .set_disabled_tools(["read"].iter().map(|s| (*s).to_owned()).collect());
        open(&mut state);
        let registry = crate::feat::picker::registry::build_picker_registry();
        let _ = crate::feat::picker::action::run_action(&mut state, &registry, TOOL_ID, "<tab>");

        // When ESC closes the picker.
        let result = crate::feat::picker::action::try_close_active(&mut state, &registry);

        // Then the hook ran and the pre-open disabled set is restored.
        assert!(result.is_some());
        let profile = state.active_session().profile();
        assert!(profile.disabled_tools.contains("read"));
        assert!(!profile.disabled_tools.contains("edit"));
        // And the snapshot is consumed and the scope popped.
        assert!(state.frontend.tool_picker_snapshot().is_none());
        assert!(state.frontend.picker_kind().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn open_seeds_disabled_state_from_the_session_profile() {
        // Given a session whose profile has "read" disabled.
        let mut state = tool_state();
        state
            .active_session_mut()
            .set_disabled_tools(["read"].iter().map(|s| (*s).to_owned()).collect());

        // When opening the tool picker.
        open(&mut state);

        // Then "read" renders disabled and the others enabled.
        let enabled_of = |name: &str| {
            state
                .frontend
                .tool_picker()
                .items()
                .iter()
                .find(|item| item.entry().name == name)
                .map(|item| item.entry().enabled)
        };
        assert_eq!(enabled_of("read"), Some(false));
        assert_eq!(enabled_of("edit"), Some(true));
    }

    #[rstest::rstest]
    #[test]
    fn open_hides_web_search_for_non_openrouter_model() {
        // Given a state on a non-openrouter model with a web_search tool.
        let mut state = state_with_tools(&[(
            "openrouter:web_search",
            "Search the web",
            Some(jinn_provider::ServerToolType::OpenrouterWebSearch),
        )]);
        state.active_session_mut().set_model(
            crate::feat::session::model_selection::ModelSelection::Single("zai/glm-4.6".to_owned()),
        );

        // When opening the tool picker.
        open(&mut state);

        // Then the web_search tool is NOT offered.
        let names: Vec<&str> = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert!(!names.contains(&"openrouter:web_search"));
    }

    #[rstest::rstest]
    #[test]
    fn open_shows_web_search_for_openrouter_model() {
        // Given a state on an openrouter model with a web_search tool.
        let mut state = state_with_tools(&[(
            "openrouter:web_search",
            "Search the web",
            Some(jinn_provider::ServerToolType::OpenrouterWebSearch),
        )]);
        state.active_session_mut().set_model(
            crate::feat::session::model_selection::ModelSelection::Single(
                "openrouter/openai/gpt-oss-120b".to_owned(),
            ),
        );

        // When opening the tool picker.
        open(&mut state);

        // Then the web_search tool IS offered.
        let names: Vec<&str> = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert!(names.contains(&"openrouter:web_search"));
    }

    #[rstest::rstest]
    #[test]
    fn open_marks_task_disabled_in_subagent_session() {
        // Given a subagent (child) session whose spawn stamp disables task.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = crate::protocol::SessionId::new();
        let child =
            crate::feat::session::chat_session::ChatSessionState::new_child(&parent_id, true);
        state.session.insert(child);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .profile_mut()
            .disabled_tools
            .insert(crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned());
        let registry = state
            .tool_registry()
            .expect("tools registry seeded by default_with_scope_focus");
        registry.update(|r| {
            r.global.insert(
                crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned(),
                crate::protocol::ToolDefinition {
                    name: crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned(),
                    description: "Delegate a sub-task to a subagent".to_owned(),
                    parameters: serde_json::json!({}),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    server_tool_type: None,
                },
            );
        });

        // When opening the tool picker.
        open(&mut state);

        // Then the task tool renders disabled.
        assert!(
            state
                .frontend
                .tool_picker()
                .items()
                .iter()
                .find(|item| item.entry().name == crate::feat::tools_actor::task::TASK_TOOL_NAME)
                .is_some_and(|item| !item.entry().enabled)
        );
    }

    #[rstest::rstest]
    #[test]
    fn status_renders_the_live_enabled_count() {
        // Given an open tool picker with one of three tools disabled.
        let mut state = tool_state();
        state
            .active_session_mut()
            .set_disabled_tools(["read"].iter().map(|s| (*s).to_owned()).collect());
        open(&mut state);

        // When rendering the status line.
        let rendered = {
            let host = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
            let ctx = StatusCtx::new(PickerId::new(TOOL_ID), &host);
            tool_status(&ctx)
        };

        // Then it reads 2/3 enabled.
        let line = rendered.expect("status declared");
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "2/3 enabled");
    }

    #[rstest::rstest]
    #[test]
    fn row_renders_marker_name_and_description() {
        // Given an enabled tool entry rendered unselected.
        let entry = ToolEntry {
            name: "bash".to_owned(),
            description: "Run shell".to_owned(),
            enabled: true,
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering its row.
        let line = tool_row(&entry, &RowCtx::flat(false, &[]));

        // Then the check marker, the name, and the em-dash description all
        // appear.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            text.contains('\u{2713}'),
            "enabled rows show ✓; got {text:?}"
        );
        assert!(!text.contains('\u{2717}'));
        assert!(text.contains("bash") && text.contains('\u{2014}') && text.contains("Run shell"));
    }

    #[rstest::rstest]
    #[test]
    fn row_renders_cross_marker_when_disabled() {
        // Given a disabled tool entry.
        let entry = ToolEntry {
            name: "bash".to_owned(),
            description: "Run shell".to_owned(),
            enabled: false,
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering its row.
        let line = tool_row(&entry, &RowCtx::flat(false, &[]));

        // Then the cross marker replaces the check.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            text.contains('\u{2717}'),
            "disabled rows show ✗; got {text:?}"
        );
        assert!(!text.contains('\u{2713}'));
    }

    #[rstest::rstest]
    #[test]
    fn row_highlights_matches_across_name_and_description() {
        // Given a tool entry and a filter matching "sh" in the name and the
        // description (offsets into "bash run shell": name len 4, desc at 5).
        let entry = ToolEntry {
            name: "bash".to_owned(),
            description: "run shell".to_owned(),
            enabled: true,
            theme: crate::feat::theme::default_theme(),
        };
        let match_ranges = [2..4usize, 7..9];

        // When rendering its row with the match ranges.
        let line = tool_row(&entry, &RowCtx::flat(false, &match_ranges));

        // Then both portions render (split did not panic and content is
        // preserved).
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("bash") && text.contains("run shell"));
    }

    #[rstest::rstest]
    #[test]
    fn tool_spec_declares_no_selection_change() {
        // Given the domain registry.
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When checking the tool spec's hooks.
        let spec = registry.get(TOOL_ID).expect("tool spec registered");

        // Then it declares no selection-change hook (moving the cursor must
        // not dispatch one).
        assert!(!spec.has_selection_change());
    }
}
