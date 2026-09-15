//! The MCP server picker's spec — behavior authored once in the builder.
//!
//! The inspector lists every server declared in `jinn.toml` under
//! `[[mcp_server]]` and lets the user toggle which ones are enabled for the
//! active session. Enablement is an opt-in set persisted in `SessionCore`;
//! only the toggled-on servers spawn an `McpActor`. TAB toggles the
//! highlighted server (advancing to the next row), CTRL+R signals the
//! coordinator to kill and respawn the selected server's actor (the
//! inspector stays open to watch the status cycle), and CTRL+T flips the
//! preview pane between live logs (status badge + stderr) and the server's
//! advertised tools. A snapshot of the pre-edit enabled set is taken on open
//! so ESC can revert; Enter commits the set and emits
//! `McpEnablementChanged` — the bus message the coordinator diffs against
//! its spawned-actor map to spawn/kill `McpActor`s.

use std::collections::BTreeSet;

use jinn_picker::ActionCtx;
use jinn_picker::PickerId;
use jinn_picker::PickerOutcome;
use jinn_picker::PickerSpec;
use jinn_picker::PickerWidget;
use jinn_picker::PreviewCtx;
use jinn_picker::PreviewSpec;
use jinn_picker::RowCtx;
use jinn_picker::StatusCtx;
use jinn_selection_widget::highlight::highlight_text_with_bg;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::PushChatEntry;
use crate::common::app_state::AppState;
use crate::feat::mcp::picker_entry::McpPreviewMode;
use crate::feat::mcp::picker_entry::McpServerEntry;
use crate::feat::mcp_coordinator_actor::protocol::McpEnablementChanged;
use crate::feat::mcp_coordinator_actor::protocol::RestartMcpServer;
use crate::feat::picker::style::dim_style;
use crate::feat::picker::style::split_match_indices;
use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::ChatEntry;

/// Builds the MCP server picker's spec.
#[must_use]
pub fn mcp_server_spec() -> PickerSpec<McpServerEntry> {
    PickerSpec::new(PickerId::new(crate::feat::picker::registry::MCP_SERVER_ID))
        .title(" MCP Servers ")
        // The legacy inspector has no preview-scroll mechanism; the pane
        // stays put across cursor moves.
        .widget(PickerWidget::Preview(PreviewSpec {
            reset_scroll_on_selection_change: false,
        }))
        .row(mcp_server_row)
        .search(|entry| format!("{} {}", entry.name, entry.description))
        .preview(render_mcp_preview)
        .bind("<tab>", "toggle", mcp_toggle)
        .bind("<c-r>", "restart", mcp_restart)
        .bind("<c-t>", "logs/tools", mcp_toggle_preview)
        .on_open(open_mcp)
        .on_confirm(confirm_mcp)
        .on_close(restore_mcp)
        .status(mcp_server_status)
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

/// Renders one picker row: the ✓/✗ enablement marker, the server name, and
/// an em-dash launch description. Filter matches highlight within the name
/// and the description separately — byte offsets from the search text
/// `"{name} {description}"` split at the separator.
fn mcp_server_row(entry: &McpServerEntry, ctx: &RowCtx<'_>) -> Line<'static> {
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

/// Renders the preview pane for the selected server: live logs (a status
/// badge line followed by the wrapped stderr tail) or the advertised tools
/// (`name — description`), per the entry's `preview_mode`.
fn render_mcp_preview(entry: &McpServerEntry, ctx: &PreviewCtx<'_>) -> Vec<Line<'static>> {
    match entry.preview_mode {
        McpPreviewMode::Logs => logs_preview(entry, ctx.width),
        McpPreviewMode::Tools => tools_preview(entry),
    }
}

/// Logs pane: a status badge line followed by the stderr tail,
/// soft-wrapped so long lines don't overflow the preview width.
fn logs_preview(entry: &McpServerEntry, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(status_badge_line(entry));
    if entry.stderr_tail.trim().is_empty() {
        lines.push(
            Line::from("(no stderr yet)".to_owned())
                .style(Style::default().fg(entry.theme.muted_text)),
        );
    } else {
        for raw in entry.stderr_tail.lines() {
            lines.extend(wrap_line(raw, width, entry.theme.primary_text));
        }
    }
    lines
}

/// Tools pane: one line per advertised tool (`name — description`).
fn tools_preview(entry: &McpServerEntry) -> Vec<Line<'static>> {
    if entry.tools.is_empty() {
        return vec![
            Line::from("(no tools advertised)".to_owned())
                .style(Style::default().fg(entry.theme.muted_text)),
        ];
    }
    entry
        .tools
        .iter()
        .map(|(name, desc)| {
            Line::from(vec![
                Span::styled(name.clone(), Style::default().fg(entry.theme.primary_text)),
                Span::styled(
                    format!(" \u{2014} {desc}"),
                    Style::default().fg(entry.theme.muted_text),
                ),
            ])
        })
        .collect()
}

/// One styled line: `Status: running` colored by the live state.
fn status_badge_line(entry: &McpServerEntry) -> Line<'static> {
    let (label, color) = match entry.status {
        None => ("disabled", entry.theme.muted_text),
        Some(crate::feat::mcp_actor::protocol::McpConnectionStatus::Starting) => {
            ("starting", ratatui::style::Color::Yellow)
        }
        Some(crate::feat::mcp_actor::protocol::McpConnectionStatus::Running) => {
            ("running", ratatui::style::Color::Green)
        }
        Some(crate::feat::mcp_actor::protocol::McpConnectionStatus::Dead) => {
            ("dead", ratatui::style::Color::Red)
        }
    };
    Line::from(vec![
        Span::styled(
            "Status: ".to_owned(),
            Style::default().fg(entry.theme.muted_text),
        ),
        Span::styled(label.to_owned(), Style::default().fg(color)),
    ])
}

/// Greedily wraps `raw` to `width` columns, returning one styled line per
/// chunk. Guards against a zero width by treating it as 1 so we never loop
/// forever on an empty/negative-pane edge case.
fn wrap_line(raw: &str, width: usize, color: ratatui::style::Color) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    let cap = width.max(1);
    let style = Style::default().fg(color);
    let mut out = Vec::new();
    let mut buf = String::new();
    for grapheme in raw.graphemes(true) {
        if buf.graphemes(true).count() >= cap {
            out.push(Line::from(buf.clone()).style(style));
            buf.clear();
        }
        buf.push_str(grapheme);
    }
    if !buf.is_empty() || out.is_empty() {
        out.push(Line::from(buf).style(style));
    }
    out
}

/// The status line: the live enabled count, e.g. `3/7 enabled`.
#[expect(clippy::unnecessary_wraps, reason = "hook signature is Option<Line>")]
fn mcp_server_status(ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
    let state = state_ref_of(ctx);
    let items = state.frontend.mcp_server_picker().items();
    let enabled = items.iter().filter(|item| item.entry().enabled).count();
    let total = items.len();
    Some(Line::from(Span::styled(
        format!("{enabled}/{total} enabled"),
        Style::default().fg(state.frontend.theme.muted_text),
    )))
}

// ── Bind actions ─────────────────────────────────────────────────────────

/// TAB on the MCP server picker: flip the highlighted server's enabled
/// state and advance the cursor (checklist style) by the measured viewport.
fn mcp_toggle(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let viewport = crate::feat::picker::geometry::active_viewport(state);
    state
        .frontend
        .mcp_server_picker_mut()
        .with_selected_mut(|item| {
            let entry = item.entry_mut();
            entry.enabled = !entry.enabled;
        });
    state.frontend.mcp_server_picker_mut().move_down(viewport);
    PickerOutcome::empty()
}

/// CTRL+R on the MCP server picker: signal the coordinator to kill and
/// respawn the selected server's `McpActor`, pushing a transient chat entry
/// to explain the pause.
///
/// Does not close the picker; the inspector stays open so the user can
/// watch the status cycle. Only acts on the currently selected entry.
fn mcp_restart(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let Some(server) = state
        .frontend
        .mcp_server_picker()
        .selected_item()
        .map(|item| item.entry().name.clone())
    else {
        return PickerOutcome::empty();
    };
    let session_id = state.active_session().session_id().clone();

    PickerOutcome::empty()
        .with_message(RestartMcpServer {
            session_id: session_id.clone(),
            server,
        })
        .with_message(PushChatEntry {
            session_id,
            entry: ChatEntry::transient("Restarting MCP server"),
        })
}

/// CTRL+T on the MCP server picker: flip the selected entry's preview pane
/// between logs and tools in place; the next render shows the other pane.
fn mcp_toggle_preview(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state
        .frontend
        .mcp_server_picker_mut()
        .with_selected_mut(|item| {
            let entry = item.entry_mut();
            entry.preview_mode = match entry.preview_mode {
                McpPreviewMode::Logs => McpPreviewMode::Tools,
                McpPreviewMode::Tools => McpPreviewMode::Logs,
            };
        });
    PickerOutcome::empty()
}

// ── Lifecycle ────────────────────────────────────────────────────────────

/// Opening the MCP server picker: fresh filter + selection, snapshot the
/// session's enabled set for the ESC revert, and load the configured
/// servers.
fn open_mcp(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    state.frontend.mcp_server_picker_mut().reset();
    // Snapshot current enabled set so ESC can restore it.
    *state.frontend.mcp_server_picker_snapshot_mut() =
        Some(state.active_session().enabled_mcp_servers().clone());
    load_mcp_server_entries(state);
    PickerOutcome::empty()
}

/// Enter on the MCP server picker: collect the enabled server names from
/// all entries and write them to the active session's enabled set.
///
/// The session set is the source of truth for which `McpActor`s should be
/// running, so the outcome carries `McpEnablementChanged` — the coordinator
/// diffs it against its spawned-actor map, spawning newly-enabled servers
/// and killing newly-disabled ones.
fn confirm_mcp(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    let session_id = state.active_session().session_id().clone();
    let enabled: BTreeSet<String> = state
        .frontend
        .mcp_server_picker()
        .items()
        .iter()
        .filter(|item| item.entry().enabled)
        .map(|item| item.entry().name.clone())
        .collect();

    state
        .active_session_mut()
        .set_enabled_mcp_servers(enabled.clone());
    *state.frontend.mcp_server_picker_snapshot_mut() = None;
    PickerOutcome::new_message(McpEnablementChanged {
        session_id,
        enabled,
    })
    .close()
}

/// ESC on the MCP server picker (the revert path — never the confirm path):
/// restore the snapshotted pre-open enabled set. Defensive: confirms clear
/// the snapshot, so this is normally a no-op. Signals `close` so the
/// dispatch layer pops the picker scope.
fn restore_mcp(ctx: &mut ActionCtx<'_>) -> PickerOutcome {
    let state = state_of(ctx);
    if let Some(snapshot) = state.frontend.mcp_server_picker_snapshot_mut().take() {
        state.active_session_mut().set_enabled_mcp_servers(snapshot);
    }
    PickerOutcome::empty().close()
}

/// Loads MCP servers into the picker from `jinn.toml`'s `[[mcp_server]]`
/// tables.
///
/// Entries are marked enabled according to the active session's
/// `enabled_mcp_servers` set and sorted case-insensitively by name. Live
/// status/stderr/tools start empty; the TUI's per-frame refresh pre-pass
/// fills them for the selected entry. Opening the picker never touches the
/// filesystem.
fn load_mcp_server_entries(state: &mut AppState) {
    let (enabled, theme) = {
        let active_session = state.active_session();
        let enabled = active_session.enabled_mcp_servers().clone();
        let theme = state.frontend.theme.clone();
        (enabled, theme)
    };
    let mut entries: Vec<McpServerEntry> = state
        .frontend
        .preferences
        .mcp_server
        .iter()
        .map(|(name, server)| {
            McpServerEntry::new(
                name.clone(),
                server.description_for_picker(),
                enabled.contains(name),
                theme.clone(),
            )
        })
        .collect();

    entries.sort_by_key(|e| e.name.to_lowercase());

    let wrapped = {
        let registry = crate::feat::picker::registry::build_picker_registry();
        registry
            .make_items(crate::feat::picker::registry::MCP_SERVER_ID, entries)
            .unwrap_or_default()
    };
    state.frontend.mcp_server_picker_mut().set_items(wrapped);
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
    use crate::feat::mcp_actor::protocol::McpConnectionStatus;
    use crate::feat::picker::PickerKind;
    use crate::feat::picker::host_impl::AppStatePickerHost;
    use crate::feat::picker::registry::MCP_SERVER_ID;
    use crate::feat::session::chat_session::ChatSessionState;
    use crate::feat::theme::default_theme;
    use jinn_picker::SpecHandle;

    /// A configured MCP server: command + args become the picker description.
    fn server_config(command: &str, args: &[&str]) -> crate::feat::mcp::McpServerConfig {
        crate::feat::mcp::McpServerConfig {
            command: Some(command.to_owned()),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            transport: crate::feat::mcp::TransportKind::Stdio,
            url: None,
            headers: std::collections::BTreeMap::new(),
            auto_enable: false,
        }
    }

    /// State with an active session, the given servers configured in
    /// preferences, and an empty picker scope pushed (entries not loaded).
    fn state_with_servers(servers: &[(&str, bool)]) -> AppState {
        let mut state = AppState::default();
        state.session.insert(ChatSessionState::new());
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let enabled: std::collections::BTreeSet<String> = servers
            .iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(name, _)| (*name).to_owned())
            .collect();
        for (name, _) in servers {
            state
                .frontend
                .preferences
                .mcp_server
                .insert((*name).to_owned(), server_config("npx", &[name]));
        }
        state.active_session_mut().set_enabled_mcp_servers(enabled);
        state
    }

    /// Opens the picker through the real open path (handles scope push +
    /// spec open hook), mirroring what the intent handler does.
    fn open(state: &mut AppState) {
        let registry = crate::feat::picker::registry::build_picker_registry();
        crate::feat::picker::intent::handle_open_picker(state, PickerKind::McpServer, &registry);
    }

    /// Runs a spec hook against `state` with a fresh dispatch context.
    fn run(
        state: &mut AppState,
        f: impl FnOnce(&mut ActionCtx<'_>) -> PickerOutcome,
    ) -> PickerOutcome {
        let mut host = AppStatePickerHost::new(state);
        let mut ctx = ActionCtx::new(PickerId::new(MCP_SERVER_ID), &mut host);
        f(&mut ctx)
    }

    /// The spec under test, from a fresh registry.
    fn spec() -> SpecHandle {
        crate::feat::picker::registry::build_picker_registry()
            .get(MCP_SERVER_ID)
            .expect("mcp-server spec is registered")
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn open_loads_sorted_entries_and_snapshots_enabled_set() {
        // Given state with two configured servers and one pre-enabled,
        // inserted in non-sorted order.
        let mut state = state_with_servers(&[("zeta", false), ("alpha", true)]);

        // When opening the picker.
        open(&mut state);

        // Then entries load case-insensitively sorted by name.
        let names: Vec<&str> = state
            .frontend
            .mcp_server_picker()
            .items()
            .iter()
            .map(|item| item.entry().name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        // And the pre-enabled server loads enabled, the other disabled.
        assert!(
            state.frontend.mcp_server_picker().items()[0]
                .entry()
                .enabled
        );
        assert!(
            !state.frontend.mcp_server_picker().items()[1]
                .entry()
                .enabled
        );
        // And the revert snapshot holds the pre-open enabled set.
        assert_eq!(
            state.frontend.mcp_server_picker_snapshot().clone(),
            Some(std::collections::BTreeSet::from(["alpha".to_owned()])),
        );
    }

    #[rstest::rstest]
    #[test]
    fn tab_toggle_flips_enabled_and_advances_the_cursor() {
        // Given an open picker with the first entry selected and enabled.
        let mut state = state_with_servers(&[("alpha", true), ("zeta", true)]);
        open(&mut state);
        assert_eq!(state.frontend.mcp_server_picker().selection(), 0);
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When pressing TAB.
        let _ =
            crate::feat::picker::action::run_action(&mut state, &registry, MCP_SERVER_ID, "<tab>");

        // Then the selected entry flipped and the cursor advanced.
        assert!(
            !state.frontend.mcp_server_picker().items()[0]
                .entry()
                .enabled,
            "first entry must be toggled off"
        );
        assert_eq!(state.frontend.mcp_server_picker().selection(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn toggle_on_empty_picker_is_a_no_op() {
        // Given an open picker with no configured servers.
        let mut state = state_with_servers(&[]);
        open(&mut state);
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When pressing TAB.
        let _ =
            crate::feat::picker::action::run_action(&mut state, &registry, MCP_SERVER_ID, "<tab>");

        // Then nothing panicked and nothing is selected.
        assert!(state.frontend.mcp_server_picker().selected_item().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn restart_selected_emits_restart_command_for_selected_server() {
        // Given an open picker with "alpha" selected.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);

        // When restarting the selected server.
        let outcome = run(&mut state, mcp_restart);

        // Then a RestartMcpServer message is emitted for the active session.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("RestartMcpServer")),
            "restart must emit RestartMcpServer: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn restart_also_pushes_a_transient_chat_entry() {
        // Given an open picker with a selection.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);

        // When restarting.
        let outcome = run(&mut state, mcp_restart);

        // Then a PushChatEntry message accompanies the restart signal.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("PushChatEntry")),
            "restart must push a chat entry: {:?}",
            outcome.message_names
        );
    }

    #[rstest::rstest]
    #[test]
    fn restart_with_no_selection_emits_nothing() {
        // Given an open picker with no entries.
        let mut state = state_with_servers(&[]);
        open(&mut state);

        // When restarting.
        let outcome = run(&mut state, mcp_restart);

        // Then nothing is emitted.
        assert!(outcome.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn restart_keeps_picker_open() {
        // Given an open MCP inspector.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);

        // When restarting.
        let outcome = run(&mut state, mcp_restart);

        // Then the picker scope is still on the stack.
        assert!(!outcome.close, "restart must not close the inspector");
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::McpServer
            }
        ));
    }

    #[rstest::rstest]
    #[test]
    fn toggle_preview_flips_logs_to_tools() {
        // Given an open picker whose selected entry defaults to Logs mode.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);
        assert_eq!(
            state.frontend.mcp_server_picker().items()[0]
                .entry()
                .preview_mode,
            McpPreviewMode::Logs
        );

        // When toggling preview.
        let _ = run(&mut state, mcp_toggle_preview);

        // Then the selected entry is now in Tools mode.
        assert_eq!(
            state.frontend.mcp_server_picker().items()[0]
                .entry()
                .preview_mode,
            McpPreviewMode::Tools
        );
    }

    #[rstest::rstest]
    #[test]
    fn toggle_preview_flips_tools_back_to_logs() {
        // Given an open picker whose selected entry is already in Tools mode.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);
        state
            .frontend
            .mcp_server_picker_mut()
            .with_selected_mut(|item| item.entry_mut().preview_mode = McpPreviewMode::Tools);

        // When toggling preview.
        let _ = run(&mut state, mcp_toggle_preview);

        // Then the selected entry is back in Logs mode.
        assert_eq!(
            state.frontend.mcp_server_picker().items()[0]
                .entry()
                .preview_mode,
            McpPreviewMode::Logs
        );
    }

    #[rstest::rstest]
    #[test]
    fn toggle_preview_emits_no_messages() {
        // Given an open picker.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);

        // When toggling preview.
        let outcome = run(&mut state, mcp_toggle_preview);

        // Then no messages are emitted (pure state flip).
        assert!(outcome.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn confirm_writes_enabled_set_emits_and_closes() {
        // Given an open picker with the first entry toggled off.
        let mut state = state_with_servers(&[("alpha", true), ("zeta", true)]);
        open(&mut state);
        let _ = run(&mut state, mcp_toggle);

        // When confirming.
        let outcome = run(&mut state, confirm_mcp);

        // Then the session's enabled set holds exactly the remaining servers.
        assert_eq!(
            state.active_session().enabled_mcp_servers(),
            &std::collections::BTreeSet::from(["zeta".to_owned()]),
        );
        // And McpEnablementChanged is emitted so the coordinator respawns actors.
        assert!(
            outcome
                .message_names
                .iter()
                .any(|n| n.contains("McpEnablementChanged")),
            "confirm must emit McpEnablementChanged: {:?}",
            outcome.message_names
        );
        // And the snapshot is cleared and the picker closes.
        assert!(state.frontend.mcp_server_picker_snapshot().is_none());
        assert!(outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn escape_restores_the_snapshotted_enabled_set() {
        // Given an open picker with one toggle applied on top of the
        // pre-open snapshot.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);
        let _ = run(&mut state, mcp_toggle);
        assert!(
            !state.frontend.mcp_server_picker().items()[0]
                .entry()
                .enabled
        );

        // When ESC closes the picker through the dispatch path.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let result = crate::feat::picker::action::try_close_active(&mut state, &registry);

        // Then the hook ran and the pre-open enabled set is restored.
        assert!(result.is_some());
        assert!(
            state
                .active_session()
                .enabled_mcp_servers()
                .contains("alpha")
        );
        // And the snapshot is consumed and the scope popped.
        assert!(state.frontend.mcp_server_picker_snapshot().is_none());
        assert!(state.frontend.scope_stack.picker_kind().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn mcp_spec_declares_no_selection_change() {
        // Given the domain registry.
        let registry = crate::feat::picker::registry::build_picker_registry();

        // When checking the mcp-server spec's hooks.
        let spec = registry.get(MCP_SERVER_ID).expect("spec registered");

        // Then it declares no selection-change hook (the preview is
        // refreshed per frame by the TUI pre-pass, not on cursor moves).
        assert!(!spec.has_selection_change());
    }

    // ── Rendering ─────────────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn status_renders_the_live_enabled_count() {
        // Given an open picker with one of two servers enabled.
        let mut state = state_with_servers(&[("alpha", true), ("zeta", false)]);
        open(&mut state);

        // When rendering the status line.
        let line = {
            let host = crate::feat::picker::host_impl::AppStateRenderHost::new(&state);
            let ctx = StatusCtx::new(PickerId::new(MCP_SERVER_ID), &host);
            spec()
                .status_line(&ctx)
                .expect("mcp spec declares a status")
        };

        // Then it reads 1/2 enabled.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "1/2 enabled");
    }

    #[rstest::rstest]
    #[test]
    fn row_renders_marker_name_and_description() {
        // Given an enabled entry rendered unselected.
        let entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx @excalimate/mcp-server".to_owned(),
            true,
            default_theme(),
        );

        // When rendering its row.
        let line = mcp_server_row(&entry, &RowCtx::flat(false, &[]));

        // Then the check marker, the name, and the em-dash description appear.
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            text.contains('\u{2713}'),
            "enabled rows show ✓; got {text:?}"
        );
        assert!(!text.contains('\u{2717}'));
        assert!(text.contains("excalimate") && text.contains('\u{2014}'));
        assert!(text.contains("npx @excalimate/mcp-server"));
    }

    #[rstest::rstest]
    #[test]
    fn row_renders_cross_marker_when_disabled() {
        // Given a disabled entry.
        let entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx ...".to_owned(),
            false,
            default_theme(),
        );

        // When rendering its row.
        let line = mcp_server_row(&entry, &RowCtx::flat(false, &[]));

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
        // Given an entry and a filter matching "sc" in the name and the
        // description (offsets into "alpha npx desc": name len 5, desc at 6).
        let entry = McpServerEntry::new(
            "alpha".to_owned(),
            "npx desc".to_owned(),
            true,
            default_theme(),
        );
        let match_ranges = [1..3usize, 8..10];

        // When rendering its row with the match ranges.
        let line = mcp_server_row(&entry, &RowCtx::flat(false, &match_ranges));

        // Then both portions render (the split did not panic and content
        // is preserved).
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("alpha") && text.contains("npx desc"));
    }

    #[rstest::rstest]
    #[test]
    fn preview_hook_renders_logs_pane_with_status_badge_and_tail() {
        // Given an enabled entry with a running status and a multi-line
        // stderr tail, in Logs mode.
        let mut entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx ...".to_owned(),
            true,
            default_theme(),
        );
        entry.status = Some(McpConnectionStatus::Running);
        entry.stderr_tail = "first line\nsecond line".to_owned();
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };

        // When rendering the preview.
        let lines = render_mcp_preview(&entry, &ctx);

        // Then the first line is the status badge and the tail lines follow.
        assert_eq!(
            lines.first().expect("at least badge").to_string(),
            "Status: running"
        );
        let rendered: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
        assert!(rendered[1..].iter().any(|l| l.contains("first line")));
        assert!(rendered[1..].iter().any(|l| l.contains("second line")));
    }

    #[rstest::rstest]
    #[test]
    fn logs_pane_empty_tail_shows_placeholder() {
        // Given an entry with no stderr captured, in Logs mode.
        let entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx ...".to_owned(),
            true,
            default_theme(),
        );
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };

        // When rendering the preview.
        let lines = render_mcp_preview(&entry, &ctx);

        // Then the placeholder line is shown after the badge.
        assert_eq!(lines.len(), 2);
        assert!(lines[1].to_string().contains("no stderr yet"));
    }

    #[rstest::rstest]
    #[test]
    fn logs_pane_wraps_long_stderr_lines_to_the_pane_width() {
        // Given an entry whose single stderr line exceeds the pane width.
        let mut entry = McpServerEntry::new(
            "srv".to_owned(),
            "npx ...".to_owned(),
            true,
            default_theme(),
        );
        entry.stderr_tail = "abcdefghij".to_owned(); // 10 columns
        let ctx = PreviewCtx {
            width: 4,
            cache: None,
        };

        // When rendering the preview.
        let lines = render_mcp_preview(&entry, &ctx);

        // Then the tail wraps into ceil(10 / 4) = 3 chunks after the badge.
        assert_eq!(lines.len(), 4);
        let rendered: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(rendered[1], "abcd");
        assert_eq!(rendered[2], "efgh");
        assert_eq!(rendered[3], "ij");
    }

    #[rstest::rstest]
    #[test]
    fn preview_hook_renders_tools_pane_one_line_per_tool() {
        // Given an entry with two advertised tools, in Tools mode.
        let mut entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx ...".to_owned(),
            true,
            default_theme(),
        );
        entry.preview_mode = McpPreviewMode::Tools;
        entry.tools = vec![
            ("create_scene".to_owned(), "Create a scene".to_owned()),
            ("auto_animate".to_owned(), "Auto-animate".to_owned()),
        ];
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };

        // When rendering the preview.
        let lines = render_mcp_preview(&entry, &ctx);

        // Then there is one line per tool, each naming the tool.
        assert_eq!(lines.len(), 2);
        let rendered: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("create_scene"));
        assert!(rendered.contains("auto_animate"));
    }

    #[rstest::rstest]
    #[test]
    fn tools_pane_empty_shows_placeholder() {
        // Given an entry with no advertised tools, in Tools mode.
        let mut entry = McpServerEntry::new(
            "excalimate".to_owned(),
            "npx ...".to_owned(),
            true,
            default_theme(),
        );
        entry.preview_mode = McpPreviewMode::Tools;
        let ctx = PreviewCtx {
            width: 80,
            cache: None,
        };

        // When rendering the preview.
        let lines = render_mcp_preview(&entry, &ctx);

        // Then a single placeholder line is shown.
        assert_eq!(lines.len(), 1);
        assert!(lines[0].to_string().contains("no tools advertised"));
    }

    // ── Storage contract ─────────────────────────────────────────────

    #[rstest::rstest]
    #[test]
    fn host_lends_the_wrapped_mcp_storage() {
        // Given state whose MCP picker holds wrapped items.
        let mut state = state_with_servers(&[("alpha", true)]);
        open(&mut state);

        // When lending the storage for the mcp-server id.
        let lend = {
            let mut host = AppStatePickerHost::new(&mut state);
            jinn_picker::PickerHost::selection_state(&mut host, PickerId::new(MCP_SERVER_ID))
                .expect("mcp-server is mapped")
                .downcast_ref::<jinn_selection_widget::SelectionState<
                    jinn_picker::PickerEntry<McpServerEntry>,
                >>()
                .is_some()
        };

        // Then it downcasts to the wrapped selection storage.
        assert!(
            lend,
            "mcp-server lend must downcast to SelectionState<PickerEntry<McpServerEntry>>"
        );
    }
}
