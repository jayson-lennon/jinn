//! The skill picker's spec — behavior authored once in the builder.
//!
//! Owns everything the legacy per-kind handler arms used to scatter: the
//! TAB toggle (with its loaded-skill no-op), CTRL+L load (pinned tool
//! call/result pair, durable auto-enable), CTRL+R refresh (full discovery
//! rescan), preview scrolling, the ESC snapshot revert, and the cached
//! markdown preview. The id and widget kind here are the registry's source
//! of truth.

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::PickerWidget;
use jinn_picker::PreviewCtx;
use jinn_picker::PreviewKey;
use jinn_picker::PreviewSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::common::app_state::AppState;
use crate::feat::context::protocol::command::ScanContextFiles;
use crate::feat::provider::protocol::command::RescanPromptTemplates;
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;
use crate::feat::session::tool_result_status::ToolResultStatus;
use crate::feat::skills::ScanSkills;
use crate::feat::skills::SkillSource;
use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::ChatEntry;
use crate::protocol::ChatEntryId;
use crate::protocol::PinPosition;

/// The kernel entry this picker's items wrap in storage.
pub use crate::feat::skills::SkillEntry;

/// Rows visible in the preview pane per page (the legacy constant).
const PREVIEW_PAGE_SIZE: usize = 10;

/// Builds the skill picker's spec.
#[must_use]
pub fn skill_spec() -> PickerSpec<SkillEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::SKILL_ID))
        .title(" Skills ")
        .widget(PickerWidget::Preview(PreviewSpec {
            reset_scroll_on_selection_change: true,
        }))
        .row(skill_row)
        .search(|entry| format!("{} {}", entry.name, entry.description))
        .preview(render_skill_preview)
        .preview_key(|entry| Some(PreviewKey(body_hash_key(&entry.body))))
        .status(skill_status)
        .bind("<tab>", "toggle", skill_toggle)
        .bind("<c-l>", "load", skill_load)
        .bind_navigation("<c-u>", "page up", skill_scroll_up)
        .bind_navigation("<c-d>", "page down", skill_scroll_down)
        .bind("<c-r>", "refresh", skill_refresh)
        .on_open(open_skill)
        .on_confirm(confirm_skill)
        .on_close(close_skill)
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

/// Renders one picker row: the enabled marker, the skill name (highlighted
/// on filter matches), and the project badge for project-scoped skills.
fn skill_row(entry: &SkillEntry, ctx: &RowCtx<'_>) -> Line<'static> {
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

    if ctx.match_ranges.is_empty() {
        let name_span = Span::styled(entry.name.clone(), style);
        let mut spans = vec![marker_span, name_span];
        if let Some(badge) = project_badge_span(entry) {
            spans.push(badge);
        }
        return Line::from(spans);
    }

    // Match indices are byte offsets into search_text = "{name} {description}".
    // Only highlight the name portion in the row (description is in the preview pane).
    let name_indices = split_match_indices(ctx.match_ranges, entry.name.len());

    let name_spans = jinn_selection_widget::highlight::highlight_text_with_bg(
        &entry.name,
        style,
        &name_indices,
        entry.theme.picker_highlight_bg,
    );

    let mut spans = vec![marker_span];
    spans.extend(name_spans);
    if let Some(badge) = project_badge_span(entry) {
        spans.push(badge);
    }
    Line::from(spans)
}

/// Badge span indicating project-scoped provenance, if applicable.
///
/// Appended to the row after the skill name. Global skills render no badge.
fn project_badge_span(entry: &SkillEntry) -> Option<Span<'static>> {
    match &entry.source {
        SkillSource::Project { .. } => Some(Span::styled(
            " (project)".to_owned(),
            Style::default().fg(entry.theme.muted_text),
        )),
        SkillSource::Global => None,
    }
}

/// Renders the skill's markdown body for the preview pane.
fn render_skill_preview(entry: &SkillEntry, ctx: &PreviewCtx<'_>) -> Vec<Line<'static>> {
    if entry.body.is_empty() {
        return Vec::new();
    }
    crate::feat::ui::chat_log::markdown::render_markdown(
        &entry.body,
        ctx.width as u16,
        &entry.theme,
    )
}

/// Splits match indices from `search_text = "{name} {description}"` into
/// name-portion ranges, clamped to the name's byte length. Description
/// indices are dropped — the row highlights the name only.
fn split_match_indices(
    indices: &[std::ops::Range<usize>],
    name_len: usize,
) -> Vec<std::ops::Range<usize>> {
    indices
        .iter()
        .filter(|range| range.start < name_len)
        .map(|range| range.start..range.end.min(name_len))
        .collect()
}

/// Stable cache key for a skill body: the decimal content hash.
///
/// Keyed on body content (not name) so that editing a SKILL.md or a project
/// skill shadowing a global of the same name produces a distinct cache entry
/// — the render cache never serves the wrong markdown.
fn body_hash_key(body: &str) -> String {
    crate::feat::skills::skill_entry::body_hash_key(body)
}

/// The status line: how many discovered skills are enabled for the session.
// The hook signature is Option so `bottom_rows()` stays truthful even if the
// status ever becomes conditional; geometry reserves the row either way.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn skill_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let picker = state.frontend.skill_picker();
    let enabled = picker
        .items()
        .iter()
        .filter(|item| item.entry().enabled)
        .count();
    let total = picker.items().len();
    Some(Line::from(Span::styled(
        format!("{enabled}/{total} enabled"),
        Style::default().fg(state.frontend.theme.muted_text),
    )))
}

// ── Bind actions ─────────────────────────────────────────────────────────

/// TAB on the skill picker: toggle the selected entry's `enabled` state,
/// then advance the cursor so several adjacent skills can be toggled in a
/// run. A skill already loaded into context cannot be disabled here —
/// disabling would give a false sense of "unloaded" (the body stays pinned
/// in history until it is unpinned and pruned) — so TAB is a full no-op for
/// a loaded skill: the entry stays enabled and the cursor stays put.
fn skill_toggle(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);

    let selected_loaded = state
        .frontend
        .skill_picker()
        .selected_item()
        .map(|item| item.entry().name.clone())
        .is_some_and(|name| state.active_session().loaded_skills().contains(&name));
    if selected_loaded {
        return PickerOutcome::empty();
    }

    state
        .frontend
        .skill_picker_mut()
        .with_selected_mut(|item| item.entry_mut().enabled = !item.entry().enabled);
    let viewport = crate::feat::picker::geometry::active_viewport(state);
    state.frontend.skill_picker_mut().move_down(viewport);
    PickerOutcome::empty()
}

/// CTRL+L on the skill picker: load the selected skill into context as a
/// pinned tool call/result pair.
///
/// Already-loaded skills get a transient notice instead of a duplicate
/// pair. A disabled skill is auto-enabled, durably: the name is removed
/// from both the cancel-revert snapshot and the session's live disabled
/// set, so neither Enter nor ESC can re-disable a skill the user loaded.
/// The picker stays open.
fn skill_load(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);

    let Some(item) = state.frontend.skill_picker().selected_item() else {
        return PickerOutcome::empty();
    };
    let (name, enabled, body) = {
        let entry = item.entry();
        (entry.name.clone(), entry.enabled, entry.body.clone())
    };

    // Resolve the skill's file_path from the session's discovered set rather
    // than re-deriving from the global dir — this is what makes project-local
    // skills loadable and matches the `skill` tool's `resolve_skill_path`.
    let Some(skill_path) = state
        .active_session()
        .discovered_skills()
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.file_path.clone())
    else {
        return PickerOutcome::empty();
    };

    // Idempotency: a pinned ToolResult for this skill already exists in history.
    if state.active_session().loaded_skills().contains(&name) {
        state
            .active_session_mut()
            .push_entry(ChatEntry::transient(format!(
                "Skill '{name}' is already loaded"
            )));
        return PickerOutcome::empty();
    }

    // Auto-enable a disabled skill so the load is not immediately contradicted
    // by a staged/committed disable. Make the enable durable against both the
    // commit (`Enter`) and revert (`ESC`) paths.
    if !enabled {
        state
            .frontend
            .skill_picker_mut()
            .with_selected_mut(|item| item.entry_mut().enabled = true);
        if let Some(snap) = state.frontend.skill_picker_snapshot_mut() {
            snap.remove(&name);
        }
        let mut disabled = state.active_session().disabled_skills().clone();
        disabled.remove(&name);
        state.active_session_mut().set_disabled_skills(disabled);
    }

    // Push the paired entries with a shared synthetic id. The body comes from
    // the in-memory SkillEntry (already frontmatter-stripped), so there is no
    // file I/O.
    let tool_call_id = ChatEntryId::new().to_string();
    let location = skill_path.to_string_lossy().to_string();
    let xml = format!("<skill name=\"{name}\" location=\"{location}\">\n{body}\n</skill>");
    let arguments = serde_json::json!({ "name": name }).to_string();

    state.active_session_mut().push_entry(ChatEntry::tool_call(
        tool_call_id.clone(),
        "skill",
        arguments,
    ));
    let mut result = ChatEntry::tool_result(tool_call_id, "skill", xml, ToolResultStatus::Success);
    result.pin_position = Some(PinPosition::Relative);
    state.active_session_mut().push_entry(result);

    let session_id = state.active_session().session_id().clone();
    PickerOutcome::empty().with_message(MarkSessionInteracted { session_id })
}

/// CTRL+UP on the skill picker: scroll the preview pane up one page.
fn skill_scroll_up(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let id = ctx.picker_id();
    let state = state_of(ctx);
    let scroll = state
        .frontend
        .pickers
        .pickers_scrolls
        .get(id)
        .saturating_sub(PREVIEW_PAGE_SIZE);
    state.frontend.pickers.pickers_scrolls.set(id, scroll);
    PickerOutcome::empty()
}

/// CTRL+DOWN on the skill picker: scroll the preview pane down one page.
fn skill_scroll_down(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let id = ctx.picker_id();
    let state = state_of(ctx);
    let scroll = state
        .frontend
        .pickers
        .pickers_scrolls
        .get(id)
        .saturating_add(PREVIEW_PAGE_SIZE);
    state.frontend.pickers.pickers_scrolls.set(id, scroll);
    PickerOutcome::empty()
}

/// CTRL+R on the skill picker: rescan every discovery source. All three
/// scan commands are emitted together so discovery settles cleanly, and a
/// transient note explains the pause.
fn skill_refresh(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);

    state
        .active_session_mut()
        .push_entry(ChatEntry::transient("Refreshing project resources..."));

    let session_id = state.active_session().session_id().clone();
    PickerOutcome::empty()
        .with_message(ScanSkills {
            session_id: session_id.clone(),
        })
        .with_message(RescanPromptTemplates {
            session_id: session_id.clone(),
        })
        .with_message(ScanContextFiles { session_id })
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the skill picker: fresh filter + selection, snapshot the
/// session's disabled set for the ESC revert, and load entries from the
/// session's discovered skills.
fn open_skill(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.skill_picker_mut().reset();
    *state.frontend.skill_picker_snapshot_mut() =
        Some(state.active_session().disabled_skills().clone());
    load_skill_picker_entries(state);
    PickerOutcome::empty()
}

/// Enter on the skill picker: commit the toggled set as the session's
/// disabled skills and close. The snapshot is cleared without restoring —
/// the commit is authoritative.
fn confirm_skill(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let disabled: std::collections::HashSet<String> = state
        .frontend
        .skill_picker()
        .items()
        .iter()
        .filter(|item| !item.entry().enabled)
        .map(|item| item.entry().name.clone())
        .collect();

    state.active_session_mut().set_disabled_skills(disabled);
    *state.frontend.skill_picker_snapshot_mut() = None;
    PickerOutcome::empty().close()
}

/// ESC on the skill picker (the revert path — never the confirm path):
/// restore the snapshotted disabled set. Signals `close` so the dispatch
/// layer pops the picker scope — without it ESC would revert the snapshot
/// but strand the user inside the picker.
fn close_skill(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if let Some(snapshot) = state.frontend.skill_picker_snapshot_mut().take() {
        state.active_session_mut().set_disabled_skills(snapshot);
    }
    PickerOutcome::empty().close()
}

/// Repopulates the skill picker from the active session's discovered skills,
/// preserving the disabled set. Delegates to the shared reload helper so the
/// picker-open path and the rescan actor path wrap items identically.
fn load_skill_picker_entries(state: &mut AppState) {
    let disabled = state.active_session().disabled_skills().clone();
    let theme = state.frontend.theme.clone();
    let discovered = state.active_session().discovered_skills().to_vec();
    crate::feat::skills::reload::reload_skill_picker_entries(
        &mut state.frontend,
        &discovered,
        &disabled,
        &theme,
    );
}

/// A second spec under the same skill id exercising bind dispatch in
/// integration tests: `<tab>` pushes a transient entry; `<esc>` closes.
#[cfg(test)]
impl SkillEntry {
    #[must_use]
    pub fn spec_for_tests() -> PickerSpec<Self> {
        PickerSpec::new(PickerId::new(crate::feat::picker::registry::SKILL_ID))
            .title(" Skills (test) ")
            .bind("<tab>", "test", |ctx: &mut ActionCtx<'_>| {
                let state = ctx
                    .state_any()
                    .downcast_mut::<crate::common::app_state::AppState>()
                    .expect("domain host lends AppState");
                state
                    .active_session_mut()
                    .push_entry(crate::protocol::ChatEntry::transient("test bind ran"));
                PickerOutcome::empty()
            })
            .bind("<esc>", "close", |_ctx: &mut ActionCtx<'_>| {
                PickerOutcome::empty().close()
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]
    use super::*;
    use crate::common::app_state::FocusScope;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::registry::SKILL_ID;
    use crate::feat::session::ChatSessionState;
    use crate::protocol::ChatEntryKind;
    use crate::protocol::PickerKind;
    use jinn_picker::PickerEntry;
    use jinn_selection_widget::SelectionState;

    /// A discovered skill with a small markdown body.
    fn skill(name: &str, description: &str, body: &str) -> crate::feat::skills::Skill {
        crate::feat::skills::Skill {
            name: name.to_owned(),
            description: description.to_owned(),
            body: body.to_owned(),
            file_path: std::path::PathBuf::from(format!("/tmp/{name}/SKILL.md")),
            base_dir: std::path::PathBuf::from(format!("/tmp/{name}")),
            source: SkillSource::Global,
        }
    }

    /// State with an active session, two discovered skills, and the skill
    /// picker scope open (entries not yet loaded).
    fn state_with_skills() -> AppState {
        let mut state = AppState::default();
        state.session.insert(ChatSessionState::new());
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_discovered_skills(vec![
            skill("phased-task-loop", "Phased execution", "# phase body"),
            skill("web-coder", "Web coding", "# web body"),
        ]);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });
        state
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run<'a>(
        state: &'a mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(PickerId::new(SKILL_ID), &mut host);
        f(&mut ctx)
    }

    /// The spec under test, from a fresh registry.
    fn spec() -> jinn_picker::SpecHandle {
        crate::feat::picker::registry::build_picker_registry()
            .get(SKILL_ID)
            .expect("skill spec is registered")
    }

    // ── Rendering ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn row_renders_marker_name_and_selected_background() {
        // Given an enabled and a disabled entry.
        let theme = crate::feat::theme::default_theme();
        let mut enabled = SkillEntry {
            name: String::from("a"),
            description: String::from("desc"),
            body: String::new(),
            enabled: true,
            source: SkillSource::Global,
            theme: theme.clone(),
        };
        enabled.enabled = true;
        let disabled = SkillEntry {
            name: String::from("b"),
            description: String::from("desc"),
            body: String::new(),
            enabled: false,
            source: SkillSource::Global,
            theme,
        };

        // When rendering unselected rows.
        let enabled_row = skill_row(&enabled, &RowCtx::flat(false, &[]));
        let disabled_row = skill_row(&disabled, &RowCtx::flat(false, &[]));

        // Then the marker reflects the enabled state.
        assert!(enabled_row.to_string().starts_with('\u{2713}'));
        assert!(disabled_row.to_string().starts_with('\u{2717}'));
        assert!(enabled_row.to_string().contains('a'));

        // And a selected row carries the selection background.
        let selected = skill_row(&enabled, &RowCtx::flat(true, &[]));
        assert_eq!(
            selected.spans[1].style.bg,
            Some(crate::feat::theme::default_theme().picker_selected_bg),
        );
    }

    #[rstest::rstest]
    #[test]
    fn row_highlights_only_the_name_on_filter_matches() {
        // Given an entry with match ranges spanning name and description.
        let entry = SkillEntry {
            name: String::from("web"),
            description: String::from("coder"),
            body: String::new(),
            enabled: true,
            source: SkillSource::Global,
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering with a match range covering "b c" (bytes 2..5,
        // crossing the name/description boundary).
        let row = skill_row(&entry, &RowCtx::flat(false, &[2..5]));

        // Then the row still names the skill (highlighting clamped, not
        // crashing, on the boundary-crossing range).
        let text: String = row.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("web"), "row text must keep the name: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn project_scoped_skills_render_a_badge() {
        // Given an entry discovered from a project directory.
        let entry = SkillEntry {
            name: String::from("local"),
            description: String::from("desc"),
            body: String::new(),
            enabled: true,
            source: SkillSource::Project {
                dir: std::path::PathBuf::from("/tmp/proj"),
            },
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering its row.
        let row = skill_row(&entry, &RowCtx::flat(false, &[]));

        // Then the badge is appended.
        let text: String = row.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("(project)"), "badge missing: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn preview_renders_the_markdown_body() {
        // Given an entry with a markdown body.
        let entry = SkillEntry {
            name: String::from("doc"),
            description: String::from("d"),
            body: String::from("# Hello World"),
            enabled: true,
            source: SkillSource::Global,
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering the preview.
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };
        let lines = render_skill_preview(&entry, &ctx);

        // Then the body text is rendered.
        let rendered: String = lines.iter().map(std::string::ToString::to_string).collect();
        assert!(rendered.contains("Hello World"));
    }

    #[rstest::rstest]
    #[test]
    fn preview_of_an_empty_body_is_empty() {
        // Given an entry with no body.
        let entry = SkillEntry {
            name: String::from("empty"),
            description: String::from("d"),
            body: String::new(),
            enabled: true,
            source: SkillSource::Global,
            theme: crate::feat::theme::default_theme(),
        };

        // When rendering the preview.
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };

        // Then no lines are produced.
        assert!(render_skill_preview(&entry, &ctx).is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn status_line_reports_enabled_over_total() {
        // Given a loaded skill picker with one of two skills disabled.
        let mut state = state_with_skills();
        let outcome = run(&mut state, open_skill);
        assert!(!outcome.close);
        state.frontend.skill_picker_mut().move_down(1);
        let _ = run(&mut state, skill_toggle);

        // When reading the status line.
        let _registry = crate::feat::picker::registry::build_picker_registry();
        let handle = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
        let ctx = StatusCtx::new(PickerId::new(SKILL_ID), &handle);
        let line = spec()
            .status_line(&ctx)
            .expect("skill spec declares a status");

        // Then it reads 1/2.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "1/2 enabled");
    }

    // ── TAB toggle ────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn toggle_flips_enabled_and_advances_the_cursor() {
        // Given an open skill picker (first entry selected, enabled).
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        assert!(state.frontend.skill_picker().items()[0].entry().enabled);

        // When toggling.
        let _ = run(&mut state, skill_toggle);

        // Then the entry is disabled and the cursor moved down.
        assert!(!state.frontend.skill_picker().items()[0].entry().enabled);
        assert_eq!(state.frontend.skill_picker().selection(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn toggle_is_a_full_no_op_on_a_loaded_skill() {
        // Given an open picker whose selected skill is loaded (pinned in
        // history) and cursor parked on it.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        {
            let name = state.frontend.skill_picker().items()[0]
                .entry()
                .name
                .clone();
            let call_id = ChatEntryId::new().to_string();
            state.active_session_mut().push_entry(ChatEntry::tool_call(
                call_id.clone(),
                "skill",
                serde_json::json!({ "name": name }).to_string(),
            ));
            let mut result = ChatEntry::tool_result(
                call_id,
                "skill",
                format!("<skill name=\"{name}\" location=\"/t\">b</skill>"),
                ToolResultStatus::Success,
            );
            result.pin_position = Some(PinPosition::Relative);
            state.active_session_mut().push_entry(result);
        }

        // When toggling.
        let _ = run(&mut state, skill_toggle);

        // Then the entry stays enabled and the cursor does not move.
        assert!(state.frontend.skill_picker().items()[0].entry().enabled);
        assert_eq!(state.frontend.skill_picker().selection(), 0);
    }

    // ── CTRL+L load ───────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn load_pushes_a_pinned_tool_pair_and_marks_the_session() {
        // Given an open skill picker with the first entry selected.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);

        // When loading the selected skill.
        let outcome = run(&mut state, skill_load);

        // Then a paired tool_call/tool_result lands in history, pinned.
        let history = state.active_session().history();
        assert!(
            matches!(
                history.last(),
                Some(entry) if entry.pin_position == Some(PinPosition::Relative)
                    && matches!(entry.kind, ChatEntryKind::ToolResult { .. })
            ),
            "load must push a pinned tool result"
        );
        // And the picker stays open with MarkSessionInteracted emitted.
        assert!(!outcome.close, "load keeps the picker open");
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("MarkSessionInteracted")),
            "load must mark the session: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn load_auto_enables_durably_against_enter_and_esc() {
        // Given an open picker with "web-coder" disabled and selected.
        let mut state = state_with_skills();
        state
            .active_session_mut()
            .set_disabled_skills(std::collections::HashSet::from(["web-coder".to_owned()]));
        let _ = run(&mut state, open_skill);
        state.frontend.skill_picker_mut().move_down(1);

        // When loading the disabled skill.
        let _ = run(&mut state, skill_load);

        // Then the live disabled set no longer holds the name.
        assert!(
            !state
                .active_session()
                .disabled_skills()
                .contains("web-coder"),
            "load must enable the skill in the live set"
        );
        // And the revert snapshot dropped it too, so ESC cannot re-disable.
        let snapshot = state
            .frontend
            .skill_picker_snapshot()
            .clone()
            .expect("snapshot kept for ESC");
        assert!(
            !snapshot.contains("web-coder"),
            "auto-enabled skill must not be reverted by ESC"
        );
    }

    #[rstest::rstest]
    #[test]
    fn load_of_an_already_loaded_skill_pushes_a_transient_and_stays_open() {
        // Given an open picker whose selected skill is already loaded.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        let name = state.frontend.skill_picker().items()[0]
            .entry()
            .name
            .clone();
        let call_id = ChatEntryId::new().to_string();
        state.active_session_mut().push_entry(ChatEntry::tool_call(
            call_id.clone(),
            "skill",
            serde_json::json!({ "name": name }).to_string(),
        ));
        let mut result = ChatEntry::tool_result(
            call_id,
            "skill",
            format!("<skill name=\"{name}\" location=\"/t\">b</skill>"),
            ToolResultStatus::Success,
        );
        result.pin_position = Some(PinPosition::Relative);
        state.active_session_mut().push_entry(result);
        let history_len_before = state.active_session().history().len();

        // When loading it again.
        let outcome = run(&mut state, skill_load);

        // Then only a transient notice was appended (no second pair).
        let history = state.active_session().history();
        assert_eq!(history.len(), history_len_before + 1);
        assert!(
            matches!(
                history.last(),
                Some(entry) if matches!(entry.kind, ChatEntryKind::Transient(_))
            ),
            "the duplicate load should append a transient notice"
        );
        // And the picker stays open.
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn load_with_no_selection_is_a_no_op() {
        // Given an open skill picker with no entries.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        state.frontend.skill_picker_mut().set_items(Vec::new());

        // When loading.
        let outcome = run(&mut state, skill_load);

        // Then nothing was pushed and no messages emitted.
        assert!(state.active_session().history().is_empty());
        assert!(outcome.messages.is_empty());
    }

    // ── Preview scrolling ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn scroll_down_then_up_returns_to_zero_by_page_size() {
        // Given an open skill picker.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);

        // When scrolling down twice and up once.
        let _ = run(&mut state, skill_scroll_down);
        let _ = run(&mut state, skill_scroll_down);
        let _ = run(&mut state, skill_scroll_up);

        // Then the scroll advanced one net page (2 × 10 − 10).
        assert_eq!(
            state
                .frontend
                .pickers
                .pickers_scrolls
                .get(PickerId::new(SKILL_ID)),
            PREVIEW_PAGE_SIZE,
        );
    }

    #[rstest::rstest]
    #[test]
    fn scroll_up_saturates_at_zero() {
        // Given an open skill picker.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);

        // When scrolling up from zero.
        let _ = run(&mut state, skill_scroll_up);

        // Then the scroll stays at zero.
        assert_eq!(
            state
                .frontend
                .pickers
                .pickers_scrolls
                .get(PickerId::new(SKILL_ID)),
            0,
        );
    }

    // ── CTRL+R refresh ────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn refresh_emits_all_three_scan_commands() {
        // Given an open skill picker.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);

        // When refreshing.
        let outcome = run(&mut state, skill_refresh);

        // Then the three discovery scans are requested.
        for name in ["ScanSkills", "RescanPromptTemplates", "ScanContextFiles"] {
            assert!(
                outcome.message_names.iter().any(|n| n.contains(name)),
                "refresh must emit {name}: {:?}",
                outcome.message_names
            );
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_resets_state_snapshots_and_loads_entries() {
        // Given state with a dirty picker (items + filter from a prior open)
        // and "web-coder" disabled.
        let mut state = state_with_skills();
        {
            let registry = crate::feat::picker::registry::build_picker_registry();
            let items = registry
                .make_items::<SkillEntry>(
                    SKILL_ID,
                    vec![SkillEntry {
                        name: String::from("stale"),
                        description: String::new(),
                        body: String::new(),
                        enabled: true,
                        source: SkillSource::Global,
                        theme: crate::feat::theme::default_theme(),
                    }],
                )
                .expect("skill spec registered");
            state.frontend.skill_picker_mut().set_items(items);
            state.frontend.skill_picker_mut().insert_char('x');
        }
        state
            .active_session_mut()
            .set_disabled_skills(std::collections::HashSet::from(["web-coder".to_owned()]));

        // When opening.
        let outcome = run(&mut state, open_skill);

        // Then the picker was reloaded from discovery (2 fresh entries,
        // filter cleared) with the disabled state respected.
        assert!(!outcome.close);
        let items = state.frontend.skill_picker().items();
        assert_eq!(items.len(), 2);
        assert!(items[0].entry().enabled);
        assert!(
            !items[1].entry().enabled,
            "disabled skill must load disabled"
        );
        // And the revert snapshot holds the pre-open disabled set.
        assert_eq!(
            state.frontend.skill_picker_snapshot().clone(),
            Some(std::collections::HashSet::from(["web-coder".to_owned()])),
        );
    }

    #[rstest::rstest]
    #[test]
    fn confirm_commits_the_disabled_set_and_closes() {
        // Given an open picker with "web-coder" toggled off.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        state.frontend.skill_picker_mut().move_down(1);
        let _ = run(&mut state, skill_toggle);

        // When confirming.
        let outcome = run(&mut state, confirm_skill);

        // Then the session's disabled set contains exactly "web-coder".
        assert_eq!(
            state.active_session().disabled_skills(),
            &std::collections::HashSet::from(["web-coder".to_owned()]),
        );
        // And the snapshot is cleared without restoring, and the picker closes.
        assert!(state.frontend.skill_picker_snapshot().is_none());
        assert!(outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn close_restores_the_snapshotted_disabled_set() {
        // Given an open picker whose user toggled a new disable on top of
        // the pre-open snapshot.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);
        let _ = run(&mut state, skill_toggle); // disable "phased-task-loop"
        assert!(!state.frontend.skill_picker().items()[0].entry().enabled,);

        // When closing via ESC.
        let outcome = run(&mut state, close_skill);

        // Then the session's disabled set is back to the snapshot (empty).
        assert!(state.active_session().disabled_skills().is_empty());
        // And the hook signals close so the dispatch layer pops the scope.
        assert!(outcome.close, "the close hook must pop the picker scope");
    }

    #[rstest::rstest]
    #[test]
    fn escape_through_the_intent_handler_closes_the_skill_picker() {
        use crate::common::slices::Slices;
        use crate::common::slices::key_routes::KeyRoutes;
        use crate::feat::intent::handler::IntentHandler;
        use crate::protocol::Intent;

        // Given an open skill picker (real registry, real handler) with a
        // toggled disable staged on top of the snapshot.
        let mut state = state_with_skills();
        let pickers = crate::feat::picker::registry::build_picker_registry();
        let _ = run(&mut state, open_skill);
        let _ = run(&mut state, skill_toggle);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Skill,
        });

        // When handling the ESC intent through the IntentHandler.
        let _ = IntentHandler::handle(
            &Intent::EnterNormalMode,
            &mut state,
            &Slices::new(),
            &KeyRoutes::new(),
            &pickers,
        );

        // Then the picker scope is gone — ESC actually leaves the picker.
        assert!(
            state.frontend.scope_stack.picker_kind().is_none(),
            "ESC must close the skill picker"
        );
        // And the snapshot revert still applied.
        assert!(state.active_session().disabled_skills().is_empty());
    }

    // ── Storage contract ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn host_lends_the_wrapped_skill_storage() {
        // Given state whose skill picker holds wrapped items.
        let mut state = state_with_skills();
        let _ = run(&mut state, open_skill);

        // When lending the storage for the skill id.
        let lend = {
            let mut host = AppStatePickerHost::new(&mut state);
            jinn_picker::PickerHost::selection_state(&mut host, PickerId::new(SKILL_ID))
                .expect("skill is mapped")
                .downcast_ref::<SelectionState<PickerEntry<SkillEntry>>>()
                .is_some()
        };

        // Then it downcasts to the wrapped selection storage.
        assert!(
            lend,
            "skill lend must downcast to SelectionState<PickerEntry<SkillEntry>>"
        );
    }
}
