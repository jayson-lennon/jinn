// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Tests for the MCP server inspector.
//!
//! Every test drives the slice's real `activate()` over its real `Slices`
//! registry, so a test cannot pass against wiring that production never runs.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use std::collections::BTreeSet;

use jinn_mcp_msg::{McpPickerState, McpPreviewMode, mcp_picker_scope, mcp_picker_slot};
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{EditIntent, RouteOutcome, ScopeSignal};
use jinn_slices::{KeyRoutes, SliceHost, Slices};

/// The inspector wired exactly as composition wires it.
struct Wired {
    slices: Slices,
    routes: KeyRoutes,
    state: std::cell::RefCell<jinn_kernel::AppState>,
    /// Replaced by `with_servers` so a test can configure the MCP catalog.
    config: std::cell::RefCell<jinn_config::ConfigLayer>,
}

impl Wired {
    /// Builds the slice with the inspector registered.
    async fn new() -> Self {
        let slices = Slices::new();
        // Production boot seeds every slice cell before any slice activates;
        // `activate_picker` resolves the inspector's cell by slot key, so
        // the harness seeds the same registry the app would.
        jinn_cell_catalog::register_all_cells(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let routes = KeyRoutes::new();
        let services = jinn_kernel::Services::new_fake().await;
        {
            let mut host = SliceHost::new(
                &slices,
                &mut viewport,
                &overlay_views,
                &routes,
                &services.trouper_system,
            );
            crate::activate_picker(&mut host);
        }
        // Mirror `jinn_tui::app::builder`: the catalog already seeded the
        // scope-focus slot on *this* registry, so this is a plain attach.
        let state = jinn_kernel::AppState::default();
        state.frontend.attach_slices(slices.clone());
        Self {
            slices,
            routes,
            state: std::cell::RefCell::new(state),
            config: std::cell::RefCell::new(jinn_config::testutil::config_layer("")),
        }
    }

    /// The inspector's cell.
    fn cell(&self) -> TypedCell<McpPickerState> {
        self.slices
            .reader(&mcp_picker_slot())
            .expect("the catalog registers the inspector cell the activation wires")
    }

    /// Configures `servers` in the app state and enables `enabled` of them on
    /// the session, then opens the inspector. The session's enabled set is the
    /// source of truth the rows mirror, so a test that wants an enabled row
    /// has to say so here.
    fn with_servers(&self, servers: &[(&str, bool)]) -> &Self {
        // The catalog lives in the configuration layer's `[mcp]` section, so a
        // test that wants rows there has to seed the layer.
        let document = servers
            .iter()
            .map(|(name, _)| {
                format!("[mcp.{name}]\ncommand = \"npx\"\nargs = [\"@scope/{name}\"]\n")
            })
            .collect::<Vec<_>>()
            .concat();
        self.config
            .replace(jinn_config::testutil::config_layer(&document));
        {
            let mut state = self.state.borrow_mut();
            let mut enabled = BTreeSet::new();
            for (name, on) in servers {
                if *on {
                    enabled.insert((*name).to_owned());
                }
            }
            state.active_session_mut().set_enabled_mcp_servers(enabled);
        }
        let _ = self.open();
        self
    }

    /// Opens the inspector through its real `open` route action.
    fn open(&self) -> jinn_slices::RouteResult {
        self.fire("open-mcp-picker")
    }

    /// Dispatches one of the inspector's actions by its route action name.
    fn fire(&self, action: &str) -> jinn_slices::RouteResult {
        let mut state = self.state.borrow_mut();
        self.routes
            .action_for(
                &jinn_slices::DynamicIntent::new(mcp_picker_scope(), action, action),
                jinn_slices::ActionCtx {
                    state: &mut *state,
                    slices: &self.slices,
                    config: &self.config.borrow(),
                    key_bytes: Vec::new(),
                },
            )
            .unwrap_or_else(|| panic!("the inspector attaches an action named {action}"))
    }

    /// Feeds an edit intent to the inspector's registered input hook.
    fn edit(&self, intent: &EditIntent) {
        let hook = self
            .routes
            .input_hook(&mcp_picker_scope())
            .expect("the inspector registers an input hook for its filter");
        hook(intent).expect("the filter hook always consumes the edit");
    }

    /// Types `text` into the filter, one character at a time.
    fn type_text(&self, text: &str) {
        for ch in text.chars() {
            self.edit(&EditIntent::InsertChar(ch));
        }
    }

    /// The row names the inspector holds, in display order.
    fn names(&self) -> Vec<String> {
        let cell = self.cell();
        let guard = cell.read();
        (0..guard.selection.filtered_count())
            .filter_map(|i| guard.selection.filtered_item(i))
            .map(|item| item.entry().name.clone())
            .collect()
    }

    /// The highlighted row's name.
    fn highlighted(&self) -> Option<String> {
        let cell = self.cell();
        let guard = cell.read();
        guard
            .selection
            .selected_item()
            .map(|item| item.entry().name.clone())
    }

    /// The names of every enabled row, in order.
    fn enabled(&self) -> Vec<String> {
        self.cell()
            .read()
            .selection
            .items()
            .iter()
            .filter(|item| item.entry().enabled)
            .map(|item| item.entry().name.clone())
            .collect()
    }

    /// The filter text.
    fn filter(&self) -> String {
        self.cell().read().selection.filter().to_owned()
    }

    /// Highlights row `index`.
    fn move_to(&self, index: usize) {
        self.cell()
            .update(|state| state.selection.set_selection(index));
    }

    /// The action a key resolves to in the inspector's own scope, if any.
    fn resolve_key(&self, key: &str) -> Option<&'static str> {
        let row = self
            .routes
            .rows()
            .into_iter()
            .find(|row| row.scope == mcp_picker_scope() && row.key == key)?;
        // A `StaticIntent` outcome would mean the row was dropped at keymap
        // generation: the static_intent table knows only six route ids, and an
        // unknown one is silently skipped. An inspector row must be an Action.
        match row.outcome {
            RouteOutcome::Action { action, .. } => Some(action),
            RouteOutcome::StaticIntent(_) => None,
        }
    }

    /// Draws one frame of the inspector and returns the buffer's symbols.
    fn draw(&self) -> String {
        self.draw_at(ratatui::layout::Rect::new(0, 0, 120, 34))
    }

    /// Draws one frame of the inspector at `area` and returns the buffer's
    /// symbols.
    fn draw_at(&self, area: ratatui::layout::Rect) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let facts = jinn_slices::RenderFacts::new(jinn_theme::default_theme(), &self.slices);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("a test terminal allocates");
        terminal
            .draw(|frame| {
                let popup = crate::mcp_picker_render::mcp_picker_overlay_rect(&area)
                    .expect("geometry fn yields a popup rect");
                crate::mcp_picker_render::render_mcp_picker(frame, popup, &facts);
            })
            .expect("the inspector renders one frame");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Builds the preview widget exactly as the render fn does — same state,
    /// same footers, same colors — renders it into a fresh test terminal at
    /// `popup`, and returns the `PickerLayout` it reports. The expectation
    /// side of the measured-equals-cell assertions: what the widget itself
    /// solves, never hand arithmetic.
    fn preview_layout_for(
        &self,
        popup: ratatui::layout::Rect,
    ) -> jinn_selection_widget::PickerLayout {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let theme = jinn_theme::default_theme();
        let report = std::cell::Cell::new(None);
        let mut terminal = Terminal::new(TestBackend::new(popup.width, popup.height))
            .expect("a test terminal allocates");
        let cell = self.cell();
        let guard = cell.read();
        let palette = crate::mcp_picker_render::mcp_picker_palette(&theme);
        let keybind = {
            let binds = crate::mcp_picker_render::mcp_picker_binds();
            jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette)
        };
        let enabled = {
            let state = &guard;
            crate::mcp_picker_actions::enabled_count(state)
        };
        let total = guard.selection.items().len();
        let footers = vec![
            crate::mcp_picker_render::mcp_picker_status(
                enabled,
                total,
                guard.context_tokens,
                &theme,
            ),
            ratatui::text::Line::from(keybind.0),
        ];
        terminal
            .draw(|frame| {
                let layout = {
                    let state = &guard;
                    jinn_selection_widget::PreviewSelectionWidget::new(&state.selection)
                        .title(ratatui::text::Line::from(" MCP Servers "))
                        .title_style(ratatui::style::Style::default().fg(theme.popup_title))
                        .footers(footers)
                        .colors(palette.selection_colors())
                        .preview_scroll(state.preview_scroll)
                        .render(frame, popup)
                };
                report.set(Some(layout));
            })
            .expect("the inspector renders one frame");
        report.into_inner().expect("render reports a PickerLayout")
    }
}

#[rstest::rstest]
#[tokio::test]
async fn open_seeds_a_row_per_configured_server() {
    // Given a session with two configured servers.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When reading the inspector's rows.
    let names = wired.names();

    // Then both are present.
    assert_eq!(names, vec!["a".to_owned(), "b".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn open_marks_a_server_enabled_when_the_session_enables_it() {
    // Given a session with both servers enabled.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", true)]);

    // When reading the rows' enabled flags.
    let enabled = wired.enabled();

    // Then both rows are enabled.
    assert_eq!(enabled, vec!["a".to_owned(), "b".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn open_pushes_the_inspector_scope() {
    // Given a session with a configured server.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[]);

    // When opening the inspector.
    let result = wired.open();

    // Then the result pushes the inspector's own scope.
    assert!(
        matches!(
            result.scope_signal,
            Some(ScopeSignal::Push(ref scope)) if scope == &mcp_picker_scope()
        ),
        "open must push the inspector scope; got {:?}",
        result.scope_signal,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn open_snapshots_the_session_enabled_set_for_escape() {
    // Given a session that enables "a".
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);

    // When toggling "a" off and then reading the snapshot.
    wired.fire("toggle-mcp-picker");
    let snapshot = wired.cell().read().snapshot.clone();

    // Then the pre-open set is still recoverable.
    assert_eq!(snapshot, Some(BTreeSet::from(["a".to_owned()])));
}

#[rstest::rstest]
#[tokio::test]
async fn tab_toggles_the_highlighted_server() {
    // Given an inspector with "a" highlighted and enabled.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When pressing Tab.
    wired.fire("toggle-mcp-picker");

    // Then "a" is no longer enabled.
    assert_eq!(wired.enabled(), Vec::<String>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn tab_advances_the_highlight_after_toggling() {
    // Given a two-row inspector with the first row highlighted.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When pressing Tab.
    wired.fire("toggle-mcp-picker");

    // Then the highlight has moved to the next row.
    assert_eq!(wired.highlighted(), Some("b".to_owned()));
}

#[rstest::rstest]
#[tokio::test]
async fn tab_does_not_advance_past_the_last_row() {
    // Given a two-row inspector already on the last row.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    wired.fire("move-mcp-picker-down");

    // When pressing Tab.
    wired.fire("toggle-mcp-picker");

    // Then the highlight stays on the last row.
    assert_eq!(wired.highlighted(), Some("b".to_owned()));
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_t_flips_the_preview_pane_to_tools() {
    // Given a highlighted server showing the logs pane.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);

    // When pressing Ctrl+T.
    wired.fire("toggle-preview-mcp-picker");

    // Then the pane shows the tools instead.
    let mode = wired
        .cell()
        .read()
        .selection
        .selected_item()
        .map(|item| item.entry().preview_mode);
    assert_eq!(mode, Some(McpPreviewMode::Tools));
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_t_flips_the_preview_pane_back_to_logs() {
    // Given a highlighted server already showing the tools pane.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);
    wired.fire("toggle-preview-mcp-picker");

    // When pressing Ctrl+T again.
    wired.fire("toggle-preview-mcp-picker");

    // Then the pane shows the logs again.
    let mode = wired
        .cell()
        .read()
        .selection
        .selected_item()
        .map(|item| item.entry().preview_mode);
    assert_eq!(mode, Some(McpPreviewMode::Logs));
}

#[rstest::rstest]
#[tokio::test]
async fn esc_restores_the_pre_open_enabled_set() {
    // Given an inspector that snapshotted "a" enabled, with "b" toggled on
    // since, and the coordinator having meanwhile written "b" to the session
    // (it does this whenever a server comes up). That external write is what
    // makes the restore observable: toggling alone never touches the session,
    // so without a prior write the Esc restore would be indistinguishable
    // from doing nothing.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    wired.move_to(1);
    wired.fire("toggle-mcp-picker");
    wired
        .state
        .borrow_mut()
        .active_session_mut()
        .set_enabled_mcp_servers(BTreeSet::from(["a".to_owned(), "b".to_owned()]));

    // When pressing Esc.
    let result = wired.fire("close-mcp-picker");

    // Then the session's enabled set is back to what it was on open, so the
    // coordinator's "b" is undone.
    let enabled = wired
        .state
        .borrow()
        .active_session()
        .enabled_mcp_servers()
        .clone();
    assert_eq!(enabled, BTreeSet::from(["a".to_owned()]));
    // And the scope is popped.
    assert!(
        matches!(result.scope_signal, Some(ScopeSignal::PopIf(ref scope)) if scope == &mcp_picker_scope()),
        "esc must pop the inspector scope; got {:?}",
        result.scope_signal,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn enter_writes_the_enabled_set_to_the_session() {
    // Given an inspector with "a" enabled and "b" disabled, then "b" toggled
    // on in the picker. The toggle is what makes this a real guard: the
    // session still holds only "a", so a confirm that failed to write would
    // leave the session unchanged and the assertion below would still pass.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    wired.move_to(1);
    let _ = wired.fire("toggle-mcp-picker");

    // When confirming.
    let _ = wired.fire("confirm-mcp-picker");

    // Then the session carries both servers, which only the confirm could
    // have written.
    let enabled = wired
        .state
        .borrow()
        .active_session()
        .enabled_mcp_servers()
        .clone();
    assert_eq!(enabled, BTreeSet::from(["a".to_owned(), "b".to_owned()]));
}

#[rstest::rstest]
#[tokio::test]
async fn enter_tells_the_coordinator_which_servers_changed() {
    // Given an inspector with "a" enabled and "b" disabled.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When confirming.
    let result = wired.fire("confirm-mcp-picker");

    // Then the coordinator receives the enablement change.
    assert!(
        result.message_names.contains(&"McpEnablementChanged"),
        "enter must notify the coordinator; got {:?}",
        result.message_names,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn esc_after_a_confirm_cannot_undo_the_commit() {
    // Given an inspector that has already confirmed.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    wired.fire("toggle-mcp-picker");
    let _ = wired.fire("confirm-mcp-picker");

    // When pressing Esc.
    let _ = wired.fire("close-mcp-picker");

    // Then the committed set stands, because the confirm was authoritative.
    let enabled = wired
        .state
        .borrow()
        .active_session()
        .enabled_mcp_servers()
        .clone();
    assert_eq!(enabled, BTreeSet::new());
}

#[rstest::rstest]
#[tokio::test]
async fn down_moves_the_highlight_one_row() {
    // Given a two-row inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When moving down.
    wired.fire("move-mcp-picker-down");

    // Then the highlight is on the second row.
    assert_eq!(wired.highlighted(), Some("b".to_owned()));
}

#[rstest::rstest]
#[tokio::test]
async fn down_stops_at_the_last_row() {
    // Given a two-row inspector already on the last row.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    wired.fire("move-mcp-picker-down");

    // When moving down again.
    wired.fire("move-mcp-picker-down");

    // Then the highlight does not run off the end.
    assert_eq!(wired.highlighted(), Some("b".to_owned()));
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_narrows_the_visible_rows() {
    // Given three servers.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("alpha", true), ("beta", true), ("gamma", true)]);

    // When typing "bet".
    wired.type_text("bet");

    // Then only the matching row is visible.
    assert_eq!(wired.names(), vec!["beta".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn backspace_restores_the_rows_the_filter_hid() {
    // Given three servers, filtered down to one.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("alpha", true), ("beta", true), ("gamma", true)]);
    wired.type_text("bet");

    // When clearing the rest of the filter.
    while !wired.filter().is_empty() {
        wired.edit(&EditIntent::DeleteBackward);
    }

    // Then every row is visible again.
    assert_eq!(wired.filter(), "");
    assert_eq!(wired.names().len(), 3);
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_c_clears_the_filter_without_closing() {
    // Given a filtered inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("alpha", true), ("beta", true)]);
    wired.type_text("bet");

    // When pressing Ctrl+C.
    let result = wired.fire("clear-filter-or-leave-mcp-picker");

    // Then the filter is empty and the inspector stays open.
    assert_eq!(wired.filter(), "");
    assert_eq!(result.scope_signal, None);
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_c_closes_the_inspector_when_the_filter_is_already_empty() {
    // Given an unfiltered inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("alpha", true), ("beta", true)]);

    // When pressing Ctrl+C.
    let result = wired.fire("clear-filter-or-leave-mcp-picker");

    // Then the inspector scope is popped.
    assert!(
        matches!(result.scope_signal, Some(ScopeSignal::PopIf(ref scope)) if scope == &mcp_picker_scope()),
        "ctrl-c on an empty filter must pop; got {:?}",
        result.scope_signal,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_r_asks_the_coordinator_to_restart_the_highlighted_server() {
    // Given an inspector with "a" highlighted.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When pressing Ctrl+R.
    let result = wired.fire("restart-mcp-picker");

    // Then a restart is asked for.
    assert!(
        result.message_names.contains(&"RestartMcpServer"),
        "ctrl-r must ask for a restart; got {:?}",
        result.message_names,
    );
    // And the user is told why the status will cycle.
    assert!(
        result.message_names.contains(&"PushChatEntry"),
        "ctrl-r must explain the restart; got {:?}",
        result.message_names,
    );
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_r_does_not_close_the_inspector() {
    // Given an open inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);

    // When pressing Ctrl+R.
    let result = wired.fire("restart-mcp-picker");

    // Then the scope is untouched, so the user can watch the status cycle.
    assert_eq!(result.scope_signal, None);
}

#[rstest::rstest]
#[case::tab("<tab>")]
#[case::ctrl_r("<c-r>")]
#[case::ctrl_t("<c-t>")]
#[case::enter("<enter>")]
#[case::esc("<esc>")]
#[tokio::test]
async fn every_advertised_key_resolves_to_a_slice_action(#[case] key: &str) {
    // Given the wired inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);

    // When resolving a key the footer advertises.
    let action = wired.resolve_key(key);

    // Then it resolves to an action the slice implements.
    assert!(
        action.is_some(),
        "footer key {key} resolves to no slice action"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn typing_a_character_filters_the_inspector() {
    // Given an open inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("alpha", true), ("beta", true)]);

    // When typing a printable character.
    wired.type_text("b");

    // Then the filter holds it, proving the catch-all is bound.
    assert_eq!(wired.filter(), "b");
}

#[rstest::rstest]
#[tokio::test]
async fn a_narrow_frame_publishes_the_stacked_viewport_into_the_cell() {
    // Given an activated inspector and a frame whose popup is too narrow to
    // split (100-wide frame → 80-wide popup, below the split threshold).
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    let area = ratatui::layout::Rect::new(0, 0, 100, 40);

    // When drawing a frame, then building the expectation the way the widget
    // itself solves the same frame — same construction as the render fn,
    // rendered into a fresh terminal at the same popup rect.
    let _ = wired.draw_at(area);
    let popup = crate::mcp_picker_render::mcp_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let expected = wired.preview_layout_for(popup);

    // Then the cell holds the widget's report for this frame: the stacked
    // list's fixed row count, exactly as the widget's own constant spells it.
    let measured = wired.cell().read().results_viewport;
    assert_eq!(measured, usize::from(expected.inner.height));
    assert_eq!(
        expected.inner.height,
        jinn_selection_widget::HORIZONTAL_LIST_ROWS,
        "a narrow frame must carry the stacked list's height, which the \
         widget itself reports"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_degenerate_popup_publishes_the_widget_report_unclamped() {
    // Given an activated inspector whose cell holds a sentinel viewport, and
    // a popup too short for any chrome.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true)]);
    let area = ratatui::layout::Rect::new(0, 0, 20, 1);
    wired
        .cell()
        .update(|state: &mut McpPickerState| state.results_viewport = 99);
    let popup = crate::mcp_picker_render::mcp_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");

    // When drawing a frame at that popup, then building the expectation the
    // way the widget itself solves the same frame.
    let _ = wired.draw_at(area);
    let expected = wired.preview_layout_for(popup);

    // Then the cell was overwritten with the widget's honest report — zero
    // rows, not a clamped 1 and not the sentinel. Navigation windows against
    // zero safely, so the clamp the old arithmetic needed is gone.
    let measured = wired.cell().read().results_viewport;
    assert_eq!(measured, usize::from(expected.inner.height));
    assert_eq!(measured, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn a_render_publishes_the_measured_viewport_into_the_cell() {
    // Given an activated inspector.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    let area = ratatui::layout::Rect::new(0, 0, 120, 34);

    // When drawing a frame, then building the expectation the way the widget
    // itself solves the same frame — same construction as the render fn,
    // rendered into a fresh terminal at the same popup rect.
    let _ = wired.draw();
    let popup = crate::mcp_picker_render::mcp_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let expected = wired.preview_layout_for(popup);

    // Then the cell holds the widget's own report for this frame, not the
    // unmeasured fallback of zero.
    let measured = wired.cell().read().results_viewport;
    assert_eq!(measured, usize::from(expected.inner.height));
    assert_ne!(measured, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn a_wide_render_publishes_the_side_by_side_viewport_into_the_cell() {
    // Given an activated inspector and a frame wide enough that the widget
    // splits the list and preview side by side (popup width ≥ 101).
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);
    let area = ratatui::layout::Rect::new(0, 0, 200, 50);

    // When drawing a frame, then building the expectation the way the widget
    // itself solves the same frame — same construction as the render fn,
    // rendered into a fresh terminal at the same popup rect.
    let _ = wired.draw_at(area);
    let popup = crate::mcp_picker_render::mcp_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let expected = wired.preview_layout_for(popup);

    // Then the cell holds the widget's own report for this frame: the list
    // pane's full height, not a fixed stacked-list row count.
    let measured = wired.cell().read().results_viewport;
    assert_eq!(measured, usize::from(expected.inner.height));
    assert_ne!(
        measured,
        jinn_selection_widget::HORIZONTAL_LIST_ROWS as usize,
        "a side-by-side frame must not carry the stacked list's fixed height"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_render_draws_the_server_names() {
    // Given an inspector with one server.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("excalimate", true)]);

    // When drawing a frame.
    let rendered = wired.draw();

    // Then the buffer shows the server name.
    assert!(rendered.contains("excalimate"), "buffer: {rendered}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_render_shows_the_enabled_count_in_the_status_line() {
    // Given an inspector with one of two servers enabled.
    let wired = Wired::new().await;
    let wired = wired.with_servers(&[("a", true), ("b", false)]);

    // When drawing a frame.
    let rendered = wired.draw();

    // Then the status line reads "1/2 enabled".
    assert!(rendered.contains("1/2 enabled"), "buffer: {rendered}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_inspector_scope_is_namespaced_by_its_slice() {
    // Given the inspector's scope.
    let scope = mcp_picker_scope();

    // When naming it.
    let name = scope.to_string();

    // Then it is namespaced, not a static per-picker scope variant.
    assert!(name.contains("mcp"), "scope name was {name:?}");
}
