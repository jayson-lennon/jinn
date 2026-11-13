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

//! The task-list browser's behavior, exercised through the real wiring.
//!
//! Every test here goes through `activate()` rather than building its own
//! registry: an earlier picker shipped with tests that wired a *different*
//! registry than production, so they passed with the production registration
//! deleted. These would not have caught that.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_kernel::AppState;
use jinn_selection_widget::TreeItem;
use jinn_slices::DynamicIntent;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{ActionCtx, ScopeSignal};
use jinn_tools_msg::TaskListPickerState;
use jinn_tools_msg::todo_list::{TaskList, TaskStatus};
use ratatui::layout::Rect;

/// The picker's observable scalars, read under the cell guard.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pick {
    /// The filter text typed so far.
    filter: String,
    /// Index of the highlighted visible row.
    selection: usize,
    /// How many rows the render pass measured.
    results_viewport: usize,
}

/// A registry wired exactly as the app wires it.
struct Harness {
    slices: jinn_slices::Slices,
    routes: jinn_slices::KeyRoutes,
    state: std::cell::RefCell<AppState>,
}

impl Harness {
    /// Builds the slice through its real `activate()`, over a `Slices` the
    /// test owns.
    ///
    /// The two-part construction is deliberate and is the whole point of this
    /// harness: the cells are registered on *this* registry and the app state
    /// then attaches *this* registry. Building state any other way attaches a
    /// second one, and a test reading the cell through a different handle than
    /// the action wrote it would pass against an untouched cell — a real bug
    /// that shipped in an earlier picker.
    async fn new() -> Self {
        let slices = jinn_slices::Slices::new();
        // Production boot seeds every slice cell before any slice activates;
        // `activate_picker` resolves both pickers' cells by slot key, so the
        // harness seeds the same registry the app would.
        jinn_cell_catalog::register_all_cells(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let routes = jinn_slices::KeyRoutes::new();
        let services = jinn_kernel::Services::new_fake().await;
        {
            let mut host = jinn_slices::SliceHost::new(
                &slices,
                &mut viewport,
                &overlay_views,
                &routes,
                &services.trouper_system,
            );
            // `activate_picker` registers both pickers this slice owns, so
            // calling it is what production does — the task-list browser is
            // not separately wired.
            crate::activate_picker(&mut host);
        }
        // `AppState::default()` (not `default_with_scope_focus`) so the
        // `attach_slices` below is the first and only attachment: the facade
        // handle is a `OnceLock`, and the harness already minted the cells on
        // *this* registry.
        let mut state = AppState::default();
        let origin = jinn_session_state::ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.attach_slices(slices.clone());
        state.session.attach_slices(slices.clone());
        Self {
            slices,
            routes,
            state: std::cell::RefCell::new(state),
        }
    }

    /// Opens the browser through the same action the sidebar's `s` key runs.
    fn open(&self) -> IntentResult {
        crate::task_list_picker_routes::task_list_opener_action().run(ActionCtx {
            state: &mut *self.state.borrow_mut(),
            slices: &self.slices,
            config: jinn_slices::empty_config_layer(),
            key_bytes: Vec::new(),
        })
    }

    /// Dispatches a slice action by name, the way the kernel would.
    fn dispatch(&self, action: &str) -> IntentResult {
        let mut state = self.state.borrow_mut();
        let intent = DynamicIntent::new(jinn_tools_msg::task_list_picker_scope(), action, action);
        self.routes
            .action_for(
                &intent,
                ActionCtx {
                    state: &mut *state,
                    slices: &self.slices,
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .unwrap_or_else(|| panic!("`{action}` must be a bound route row in the picker scope"))
    }

    /// Feeds an edit intent to the picker's registered input hook, the way
    /// the composed keymap does for a printable character.
    fn edit(&self, intent: &jinn_slices::EditIntent) {
        let hook = self
            .routes
            .input_hook(&jinn_tools_msg::task_list_picker_scope())
            .expect("the browser registers an input hook for its filter");
        hook(intent).expect("the filter hook always consumes the edit");
    }

    /// The picker's cell — the one the slice registered.
    fn cell(&self) -> TypedCell<TaskListPickerState> {
        self.slices
            .reader(&jinn_tools_msg::task_list_picker_slot())
            .expect("the tools slice registers the task-list picker cell at activation")
    }

    /// The filter text, the highlight index, and the measured row count —
    /// the three scalars every test here asserts on.
    ///
    /// Projected rather than cloned: `TreePickerState` holds a filter, a
    /// scroll window, and a per-entry match map, and copying that per
    /// assertion would make the tests assert on a snapshot instead of on the
    /// live state.
    fn read(&self) -> Pick {
        let cell = self.cell();
        let guard = cell.read();
        Pick {
            filter: guard.tree.filter().to_owned(),
            selection: guard.tree.selection(),
            results_viewport: guard.results_viewport,
        }
    }

    /// Every label the picker currently shows, in visible order.
    ///
    /// Reads the *filtered* view, not the underlying list: a picker whose
    /// filter text is written but never applied still has every item present.
    /// Replaces the active session's task list.
    fn set_task_list(&self, list: TaskList) {
        let mut state = self.state.borrow_mut();
        let active = state.session.active_session_id().clone();
        *state
            .session
            .get_mut(&active)
            .expect("the harness seeds one active session")
            .task_list_mut() = list;
    }

    /// Every label the picker currently shows, in visible order.
    fn labels(&self) -> Vec<String> {
        let cell = self.cell();
        let guard = cell.read();
        (0..guard.tree.filtered_count())
            .filter_map(|i| {
                guard
                    .tree
                    .filtered_item(i)
                    .map(|e| e.entry().display_label().to_owned())
            })
            .collect()
    }
}

/// A two-phase list: "Build" with two tasks, "Test" with one.
fn sample_list() -> TaskList {
    let mut list = TaskList::default();
    // One call, not two: `set_from_inputs` replaces the whole list, so a
    // second call would silently drop the first phase.
    list.set_from_inputs(&[
        jinn_tools_msg::PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![
                ("write the parser".to_owned(), TaskStatus::Completed),
                ("wire the route".to_owned(), TaskStatus::Pending),
            ],
        },
        jinn_tools_msg::PhaseInput {
            description: "Test".to_owned(),
            tasks: vec![("add coverage".to_owned(), TaskStatus::Pending)],
        },
    ]);
    list
}

// ── Opening ─────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn open_shows_phase_as_a_root_row() {
    // Given a session with a one-phase task list.
    let h = Harness::new().await;
    h.set_task_list({
        let mut list = TaskList::default();
        list.add_phase("Build");
        list
    });

    // When opening the browser.
    h.open();

    // Then the phase appears as a row.
    assert_eq!(h.labels(), vec!["Build".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn open_shows_tasks_indented_under_their_phase() {
    // Given a session with a phase holding two tasks.
    let h = Harness::new().await;
    h.set_task_list({
        let mut list = TaskList::default();
        list.set_from_inputs(&[jinn_tools_msg::PhaseInput {
            description: "Build".to_owned(),
            tasks: vec![
                ("write the parser".to_owned(), TaskStatus::Completed),
                ("wire the route".to_owned(), TaskStatus::Pending),
            ],
        }]);
        list
    });

    // When opening the browser.
    h.open();

    // Then the phase is followed by its two tasks.
    assert_eq!(
        h.labels(),
        vec![
            "Build".to_owned(),
            "write the parser".to_owned(),
            "wire the route".to_owned()
        ]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn open_pushes_the_picker_scope() {
    // Given a wired harness.
    let h = Harness::new().await;

    // When opening the browser.
    let result = h.open();

    // Then the picker's own scope is pushed.
    assert!(matches!(
        result.scope_signal,
        Some(ScopeSignal::Push(ref scope)) if *scope == jinn_tools_msg::task_list_picker_scope()
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn open_over_an_empty_task_list_still_opens() {
    // Given a session whose task list is empty.
    let h = Harness::new().await;

    // When opening the browser.
    h.open();

    // Then it opens, showing no rows — "nothing to do" is a thing to look at.
    assert_eq!(h.labels(), [] as [std::string::String; 0]);
}

#[rstest::rstest]
#[tokio::test]
async fn postponed_tasks_are_hidden_from_the_browser() {
    // Given a phase whose only task is postponed.
    let h = Harness::new().await;
    h.set_task_list(sample_list_with_postponed());

    // When opening the browser.
    h.open();

    // Then the postponed task is not shown, but its phase is.
    assert_eq!(h.labels(), vec!["Later".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn opening_twice_starts_from_a_clean_filter() {
    // Given a harness whose filter has text in it.
    let h = Harness::new().await;
    h.set_task_list(sample_list());
    h.open();
    h.edit(&jinn_slices::EditIntent::InsertChar('B'));
    assert_eq!(h.read().filter, "B", "the filter took the typed character");

    // When opening the browser again.
    h.open();

    // Then the filter is empty and every row is back.
    assert_eq!(h.read().filter, "");
    assert_eq!(
        h.labels().len(),
        5,
        "two phases plus the three tasks under them"
    );
}

/// A one-phase list whose single task is postponed.
fn sample_list_with_postponed() -> TaskList {
    let mut list = TaskList::default();
    list.set_from_inputs(&[jinn_tools_msg::PhaseInput {
        description: "Later".to_owned(),
        tasks: vec![("maybe".to_owned(), TaskStatus::Postponed)],
    }]);
    list
}

// ── Navigation ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn down_moves_the_highlight_to_the_next_row() {
    // Given an open browser.
    let h = open_with(sample_list()).await;

    // When pressing down.
    h.dispatch("move-task-list-picker-down");

    // Then the highlight is on the second row.
    assert_eq!(h.read().selection, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn up_moves_the_highlight_back() {
    // Given an open browser highlighted on the second row.
    let h = open_with(sample_list()).await;
    h.dispatch("move-task-list-picker-down");

    // When pressing up.
    h.dispatch("move-task-list-picker-up");

    // Then the highlight is back on the first row.
    assert_eq!(h.read().selection, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn down_stops_at_the_last_row() {
    // Given an open browser with a single row.
    let h = open_with(single_phase_list()).await;

    // When pressing down twice.
    h.dispatch("move-task-list-picker-down");
    h.dispatch("move-task-list-picker-down");

    // Then the highlight stays on the only row.
    assert_eq!(h.read().selection, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_uses_the_measured_viewport() {
    // Given an open browser whose render pass measured a 3-row window.
    let h = open_with(many_phase_list(20)).await;
    h.cell()
        .update(|state: &mut TaskListPickerState| state.results_viewport = 3);

    // When pressing page down.
    h.dispatch("page-task-list-picker-down");

    // Then the highlight moved by half the measured window — (3/2).max(1) —
    // proving the step comes from the published row count.
    assert_eq!(h.read().selection, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn page_up_uses_the_measured_viewport() {
    // Given an open browser highlighted four rows down.
    let h = open_with(many_phase_list(20)).await;
    h.cell()
        .update(|state: &mut TaskListPickerState| state.results_viewport = 8);
    for _ in 0..4 {
        h.dispatch("move-task-list-picker-down");
    }

    // When pressing page up.
    h.dispatch("page-task-list-picker-up");

    // Then the highlight moved back by half the measured window — 8/2 == 4.
    assert_eq!(h.read().selection, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_clamps_at_the_last_row() {
    // Given an open browser near the end of a 3-phase list.
    let h = open_with(many_phase_list(3)).await;

    // When pressing page down repeatedly.
    for _ in 0..5 {
        h.dispatch("page-task-list-picker-down");
    }

    // Then the highlight rests on the last row rather than running past it.
    assert_eq!(h.read().selection, 2);
}

// ── Filtering ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn typing_narrows_the_rows() {
    // Given an open browser.
    let h = open_with(sample_list()).await;

    // When typing a letter that matches one task.
    h.edit(&jinn_slices::EditIntent::Paste("parser".to_owned()));

    // Then only the matching task and its phase remain. Tree filtering keeps
    // ancestors on screen so the match stays in context; dropping the phase
    // would leave an orphan row with no visible parent.
    assert_eq!(
        h.labels(),
        vec!["Build".to_owned(), "write the parser".to_owned()]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn backspace_widens_the_filter() {
    // Given an open browser filtered to one task.
    let h = open_with(sample_list()).await;
    h.edit(&jinn_slices::EditIntent::Paste("pars".to_owned()));

    // When backspacing.
    h.edit(&jinn_slices::EditIntent::DeleteBackward);

    // Then more rows match again.
    assert!(h.labels().len() > 1);
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_c_clears_the_whole_filter() {
    // Given an open browser with a multi-character filter.
    let h = open_with(sample_list()).await;
    h.edit(&jinn_slices::EditIntent::Paste("parser".to_owned()));

    // When pressing Ctrl-C.
    h.dispatch("clear-filter-or-leave-task-list-picker");

    // Then the filter is empty, not one character shorter.
    assert_eq!(h.read().filter, "");
    assert_eq!(
        h.labels().len(),
        5,
        "two phases plus the three tasks under them"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn ctrl_c_with_an_empty_filter_closes_the_browser() {
    // Given an open browser with no filter text.
    let h = open_with(sample_list()).await;

    // When pressing Ctrl-C.
    let result = h.dispatch("clear-filter-or-leave-task-list-picker");

    // Then the picker scope is popped.
    assert!(matches!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(ref scope)) if *scope == jinn_tools_msg::task_list_picker_scope()
    ));
}

// ── Closing ─────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn escape_pops_only_the_picker_scope() {
    // Given the browser opened on top of the sidebar's task-list section.
    let h = Harness::new().await;
    h.state
        .borrow_mut()
        .frontend
        .scope_push(jinn_slices::FocusScope::Dynamic(
            jinn_slices::SliceScopeId::navigation("sidebar", "task-list"),
        ));
    h.open();

    // When pressing escape.
    let result = h.dispatch("quit-task-list-picker");

    // Then exactly one scope pops, and it is this picker's.
    // Popping the whole stack would take the sidebar section with it and
    // strand the user in Normal, which is why this picker differs.
    assert!(matches!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(ref scope)) if *scope == jinn_tools_msg::task_list_picker_scope()
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn escape_closes_the_browser() {
    // Given an open browser.
    let h = open_with(sample_list()).await;

    // When pressing escape.
    let result = h.dispatch("quit-task-list-picker");

    // Then the picker scope pops.
    assert!(matches!(result.scope_signal, Some(ScopeSignal::PopIf(_))));
}

#[rstest::rstest]
#[tokio::test]
async fn enter_does_nothing() {
    // Given an open browser.
    let h = open_with(sample_list()).await;
    let before = h.labels();

    // When pressing enter.
    let result = h.dispatch("confirm-task-list-picker");

    // Then nothing changes: this is a read-only browser.
    assert_eq!(h.labels(), before);
    assert!(
        result.scope_signal.is_none(),
        "enter must not close or navigate"
    );
    assert!(result.message_names.is_empty(), "enter must not act");
}

// ── Contract ────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn every_footer_key_is_a_bound_row() {
    // Given the slice wired through its real activation.
    let h = Harness::new().await;

    // When matching the advertised bindings against the attached rows.
    let attached: Vec<(String, String)> = h
        .routes
        .rows()
        .iter()
        .filter(|row| row.scope == jinn_tools_msg::task_list_picker_scope())
        .map(|row| (row.key.to_owned(), row.route_id.as_str().to_owned()))
        .collect();
    let bindings = crate::task_list_picker_routes::TASK_LIST_PICKER_BINDINGS;

    // Then every advertised key has a row behind it.
    for (notation, _) in bindings {
        assert!(
            attached.iter().any(|(key, _)| key == notation),
            "footer advertises `{notation}` but no route row binds it: {attached:?}"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_has_no_central_intent_names() {
    // Given the slice wired through its real activation.
    let h = Harness::new().await;

    // When reading the picker's route ids.
    let ids: Vec<String> = h
        .routes
        .rows()
        .iter()
        .filter(|row| row.scope == jinn_tools_msg::task_list_picker_scope())
        .map(|row| row.route_id.as_str().to_owned())
        .collect();

    // Then none of them is a kernel intent, and each names the slice.
    for id in &ids {
        assert_ne!(id, "");
    }
}

// ── Rendering ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn render_draws_the_tree_popup() {
    // Given an open browser with rows.
    let h = open_with(sample_list()).await;
    let area = Rect::new(0, 0, 80, 24);

    // When rendering.
    let rows = draw_frame(&h, area);

    // Then the popup border and the phase name are on screen.
    let screen = rows.join("\n");
    assert!(screen.contains("Task List"), "popup not drawn:\n{screen}");
    assert!(screen.contains("Build"), "phase row missing:\n{screen}");
}

#[rstest::rstest]
#[tokio::test]
async fn render_ignores_an_unregistered_cell() {
    // Given a harness whose cell was never populated.
    let h = Harness::new().await;
    let area = Rect::new(0, 0, 80, 24);

    // When rendering.
    let rows = draw_frame(&h, area);

    // Then a popup border is still drawn (the cell is registered, just empty),
    // but no phase row is — an empty browser is not a broken one.
    let screen = rows.join("\n");
    assert!(
        !screen.contains("Build"),
        "stale rows in an empty browser:\n{screen}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn render_measures_the_result_viewport_into_the_cell() {
    // Given an open browser.
    let h = open_with(sample_list()).await;
    let area = Rect::new(0, 0, 80, 40);

    // When rendering at this height.
    let _ = draw_frame(&h, area);

    // Then the cell holds the row count this frame actually showed, which is
    // what makes paging follow the window rather than a fixed constant. The
    // expectation is the widget's own report for the same frame — built by an
    // identical construction, never hand arithmetic.
    let popup = crate::task_list_picker_render::task_list_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let expected = tree_layout_for(&h, popup);
    assert_eq!(
        h.read().results_viewport,
        usize::from(expected.inner.height),
        "paging must use the row count the widget reported, not a constant"
    );
    assert_ne!(
        usize::from(expected.inner.height),
        jinn_tools_msg::TASK_LIST_PICKER_RESULTS_VIEWPORT_FALLBACK,
        "the test is only meaningful if this frame measures something other \
         than the fallback — a taller popup must not be indistinguishable \
         from an unmeasured one"
    );
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Opens the browser over a given list and returns the harness.
async fn open_with(list: TaskList) -> Harness {
    let h = Harness::new().await;
    h.set_task_list(list);
    h.open();
    h
}

/// A list with a single phase and no tasks.
fn single_phase_list() -> TaskList {
    let mut list = TaskList::default();
    list.add_phase("Only");
    list
}

/// A list with `count` phases, for paging tests.
fn many_phase_list(count: usize) -> TaskList {
    let mut list = TaskList::default();
    for i in 0..count {
        list.add_phase(&format!("Phase {i}"));
    }
    list
}

/// Draws one frame of the browser into a fresh test terminal and returns the
/// buffer's rows, so a test can tell "drew an empty popup" from "drew nothing".
fn draw_frame(h: &Harness, area: Rect) -> Vec<String> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal");
    let facts = jinn_slices::RenderFacts::new(h.state.borrow().frontend.theme.clone(), &h.slices);
    terminal
        .draw(|frame| {
            let popup = crate::task_list_picker_render::task_list_picker_overlay_rect(&area)
                .expect("geometry fn yields a popup rect");
            crate::task_list_picker_render::render_task_list_picker(frame, popup, &facts);
        })
        .expect("draw");
    terminal
        .backend()
        .buffer()
        .content
        .chunks(area.width as usize)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect()
}

/// Builds the tree widget exactly as the render fn does — same state, same
/// footers, same colors — renders it into a fresh test terminal at `popup`,
/// and returns the `PickerLayout` it reports. The expectation side of the
/// measured-equals-cell assertion: what the widget itself solves, never
/// hand arithmetic.
fn tree_layout_for(h: &Harness, popup: Rect) -> jinn_selection_widget::PickerLayout {
    let report = std::cell::Cell::new(None);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
        popup.width,
        popup.height,
    ))
    .expect("terminal");
    let cell = h.cell();
    let guard = cell.read();
    let theme = &h.state.borrow().frontend.theme;
    let palette = crate::task_list_picker_render::task_list_picker_palette(theme);
    let keybind = {
        let binds = crate::task_list_picker_render::task_list_picker_binds();
        jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette)
    };
    let (roots, children) = {
        let state = &guard;
        let mut roots = 0;
        let mut children = 0;
        for item in state.tree.items() {
            if item.entry().parent_id().is_some() {
                children += 1;
            } else {
                roots += 1;
            }
        }
        (roots, children)
    };
    let status = ratatui::text::Line::from(ratatui::text::Span::styled(
        format!("{roots} phases, {children} tasks"),
        ratatui::style::Style::default().fg(theme.muted_text),
    ));
    let footers = vec![status, ratatui::text::Line::from(keybind.0)];
    terminal
        .draw(|frame| {
            let layout = {
                let state = &guard;
                jinn_selection_widget::TreePickerWidget::new(&state.tree)
                    .title(ratatui::text::Line::from(" Task List "))
                    .title_style(ratatui::style::Style::default().fg(theme.popup_title))
                    .footers(footers)
                    .colors(palette.selection_colors())
                    .tree_prefix_color(theme.muted_text)
                    .render(frame, popup)
            };
            report.set(Some(layout));
        })
        .expect("draw");
    report.into_inner().expect("render reports a PickerLayout")
}
