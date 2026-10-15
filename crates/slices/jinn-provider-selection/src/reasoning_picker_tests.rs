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

//! The reasoning picker's observable behavior.
//!
//! Tests that assert *wiring* run the slice's real `activate_picker`, so a
//! picker that registered its cell but forgot its rows or its input hook fails
//! loudly rather than passing against a hand-built stand-in. Tests that assert
//! *behavior* drive the picker's public entry points.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use jinn_provider_selection_msg::{
    ReasoningEffort, ReasoningPickerState, reasoning_picker_scope, resolve_effort,
};
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{EditIntent, RouteOutcome, ScopeSignal};
use jinn_slices::{KeyRoutes, SliceHost, Slices};

/// The slice wired exactly as composition wires it.
struct Wired {
    slices: Slices,
    routes: KeyRoutes,
    state: std::cell::RefCell<jinn_kernel::AppState>,
}

impl Wired {
    /// Builds the slice with the reasoning picker registered. The provider
    /// actors are not spawned: the picker needs no actor, and the tests
    /// describe the picker, not the provider machinery.
    async fn new() -> Self {
        let slices = Slices::new();
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
        let state = jinn_kernel::AppState::default_with_scope_focus();
        state.frontend.attach_slices(slices.clone());
        Self {
            slices,
            routes,
            state: std::cell::RefCell::new(state),
        }
    }

    /// The picker cell.
    fn cell(&self) -> TypedCell<ReasoningPickerState> {
        self.slices
            .reader(&jinn_provider_selection_msg::reasoning::reasoning_picker_slot())
            .expect("the picker registers its cell at activation")
    }

    /// The effort names the filter currently shows, in display order.
    ///
    /// Reads the *filtered* view, not the underlying item list: a picker whose
    /// filter text is written but never applied still has every item present.
    fn visible_names(&self) -> Vec<String> {
        let state = self.cell();
        let guard = state.read();
        (0..guard.selection.filtered_count())
            .filter_map(|i| guard.selection.filtered_item(i))
            .map(|item| item.entry().name.clone())
            .collect()
    }

    /// The index of the highlighted row.
    fn highlighted(&self) -> usize {
        self.cell().read().selection.selection()
    }

    /// The name of the highlighted row.
    fn highlighted_name(&self) -> Option<String> {
        self.cell()
            .read()
            .selection
            .selected_item()
            .map(|item| item.entry().name.clone())
    }

    /// The filter text.
    fn filter(&self) -> String {
        self.cell().read().selection.filter().to_owned()
    }

    /// Gives the active session its own reasoning effort.
    fn set_session_effort(&self, effort: ReasoningEffort) {
        self.state
            .borrow_mut()
            .active_session_mut()
            .profile_mut()
            .reasoning_effort = Some(effort);
    }

    /// The active session's own resolved reasoning effort.
    fn session_effort(&self) -> Option<ReasoningEffort> {
        resolve_effort(
            self.state
                .borrow()
                .active_session()
                .profile()
                .reasoning_effort,
        )
    }

    /// Opens the picker through its real `open` route action.
    fn open(&self) -> jinn_slices::RouteResult {
        self.fire("open-reasoning-picker")
    }

    /// Dispatches one of the picker's actions by its route action name.
    fn fire(&self, action: &str) -> jinn_slices::RouteResult {
        let mut state = self.state.borrow_mut();
        self.routes
            .action_for(
                &jinn_slices::DynamicIntent::new(reasoning_picker_scope(), action, action),
                jinn_slices::ActionCtx {
                    state: &mut *state,
                    slices: &self.slices,
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .unwrap_or_else(|| panic!("the reasoning picker attaches an action named {action}"))
    }

    /// Feeds an edit intent to the picker's registered input hook.
    fn edit(&self, intent: &EditIntent) {
        let hook = self
            .routes
            .input_hook(&reasoning_picker_scope())
            .expect("the picker registers an input hook for its filter");
        hook(intent).expect("the filter hook always consumes the edit");
    }

    /// Draws one frame of the picker at `100x30` and returns the buffer's
    /// symbols, row by row.
    fn draw(&self) -> Vec<String> {
        draw_frame(self, ratatui::layout::Rect::new(0, 0, 100, 30))
    }
}

/// Draws one frame of the picker's popup into a fresh test terminal and
/// returns the buffer's symbols, row by row.
fn draw_frame(wired: &Wired, area: ratatui::layout::Rect) -> Vec<String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    let facts =
        jinn_slices::RenderFacts::new(wired.state.borrow().frontend.theme.clone(), &wired.slices);
    terminal
        .draw(|frame| {
            let popup = crate::reasoning_picker_render::reasoning_picker_overlay_rect(&area)
                .expect("geometry fn yields a popup rect");
            crate::reasoning_picker_render::render_reasoning_picker(frame, popup, &facts);
        })
        .expect("draw");
    terminal
        .backend()
        .buffer()
        .content
        .chunks(area.width as usize)
        .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
        .collect()
}

// ── 1. Opens with entries ───────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_lists_every_effort_variant() {
    // Given a wired slice.
    let wired = Wired::new().await;

    // When the picker is opened.
    wired.open();

    // Then its rows name all seven efforts in declaration order.
    assert_eq!(
        wired.visible_names(),
        vec!["max", "xhigh", "high", "medium", "low", "minimal", "none"]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_marks_the_sessions_own_effort_active() {
    // Given a session whose own effort is Low.
    let wired = Wired::new().await;
    wired.set_session_effort(ReasoningEffort::Low);

    // When the picker is opened.
    wired.open();

    // Then exactly the Low row carries the active marker.
    let cell = wired.cell();
    let active: Vec<String> = cell
        .read()
        .selection
        .items()
        .iter()
        .filter(|item| item.entry().is_active)
        .map(|item| item.entry().name.clone())
        .collect();
    assert_eq!(active, vec!["low"]);
}

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_marks_nothing_active_when_the_session_has_no_effort() {
    // Given a session with no effort of its own and a global default set —
    // the global is not this picker's business, because effort is
    // session-owned.
    let wired = Wired::new().await;
    wired.state.borrow_mut().frontend.app_state.reasoning_effort = Some(ReasoningEffort::High);

    // When the picker is opened.
    wired.open();

    // Then no row is marked active.
    let cell = wired.cell();
    let active = cell
        .read()
        .selection
        .items()
        .iter()
        .filter(|item| item.entry().is_active)
        .count();
    assert_eq!(active, 0);
}

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_pushes_its_own_scope() {
    // Given a wired slice.
    let wired = Wired::new().await;
    let before = wired.state.borrow().frontend.scope_len();

    // When the picker is opened.
    wired.open();

    // Then the picker's dynamic scope is pushed and is now on top of the
    // focus stack — the render pass and the rows hang off that scope.
    let state = wired.state.borrow();
    assert_eq!(state.frontend.scope_len(), before + 1);
    assert_eq!(
        state.frontend.scope(),
        jinn_kernel::FocusScope::Dynamic(reasoning_picker_scope())
    );
}

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_is_not_gated_on_the_models_reasoning_support() {
    // Given a session whose model carries no reasoning metadata at all.
    let wired = Wired::new().await;

    // When the picker is opened.
    wired.open();

    // Then it still opens and offers the full set of efforts.
    //
    // Pinned deliberately: the picker is ungated by design. Effort is sent
    // verbatim and a provider that rejects an unsupported value errors at
    // request time, so a model-capability gate here would have to consult
    // discovery data the picker never reads. Any future gate must change this
    // test deliberately rather than by accident.
    assert_eq!(
        wired.visible_names(),
        vec!["max", "xhigh", "high", "medium", "low", "minimal", "none"]
    );
}

// ── 2. The filter narrows ───────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn typing_a_character_filters_the_list() {
    // Given an open picker holding all seven efforts.
    let wired = Wired::new().await;
    wired.open();

    // When the user types "none".
    for ch in "none".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // Then the filter holds the text and only the matching row survives.
    assert_eq!(wired.filter(), "none");
    assert_eq!(wired.visible_names(), vec!["none"]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_also_matches_an_efforts_description() {
    // Given an open picker.
    let wired = Wired::new().await;
    wired.open();

    // When the user types "skip".
    for ch in "skip".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // Then the row whose description reads "Skip reasoning" is the one
    // that survives — no effort *name* contains "skip".
    assert_eq!(wired.visible_names(), vec!["none"]);
}

#[rstest::rstest]
#[tokio::test]
async fn backspace_shortens_the_filter() {
    // Given an open picker whose filter reads "xhigh".
    let wired = Wired::new().await;
    wired.open();
    for ch in "xhigh".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }
    assert_eq!(wired.visible_names(), vec!["xhigh"]);

    // When backspace is pressed.
    wired.edit(&EditIntent::DeleteBackward);

    // Then the filter reads "xhig" and still only the xhigh row matches.
    assert_eq!(wired.filter(), "xhig");
    assert_eq!(wired.visible_names(), vec!["xhigh"]);
}

// ── 3. Navigation ───────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn down_arrow_moves_the_highlight() {
    // Given an open picker whose highlight is on the first row.
    let wired = Wired::new().await;
    wired.open();
    assert_eq!(wired.highlighted(), 0);

    // When the down arrow is pressed.
    wired.fire("move-reasoning-picker-down");

    // Then the highlight is on the second row.
    assert_eq!(wired.highlighted(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn up_arrow_stops_at_the_first_row() {
    // Given an open picker whose highlight is already at the top.
    let wired = Wired::new().await;
    wired.open();

    // When the up arrow is pressed.
    wired.fire("move-reasoning-picker-up");

    // Then the highlight does not wrap or move past the first row.
    assert_eq!(wired.highlighted(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_steps_by_the_rows_the_last_frame_actually_laid_out() {
    // Given an open picker, and a frame drawn so the cell learns how many
    // rows are on screen.
    let wired = Wired::new().await;
    wired.open();
    let frame = ratatui::layout::Rect::new(0, 0, 100, 30);
    draw_frame(&wired, frame);
    let popup = crate::reasoning_picker_render::reasoning_picker_overlay_rect(&frame)
        .expect("geometry fn yields a popup rect");
    let on_screen = crate::reasoning_picker_viewport::results_viewport(popup);

    // When page down is pressed.
    wired.fire("page-reasoning-picker-down");

    // Then the highlight advanced by half a screen of rows — a page of what
    // the user can see, not of a fixed guess about how much that is. The
    // assertion reads the *cell's* measurement rather than recomputing it, so
    // a renderer that stopped publishing it is caught here too.
    let measured = wired.cell().read().results_viewport;
    assert_ne!(
        measured,
        jinn_provider_selection_msg::reasoning::RESULTS_VIEWPORT_FALLBACK,
        "the frame must measure fewer rows than the pre-render fallback, or this test cannot detect a broken measurement"
    );
    assert_eq!(measured, on_screen);
    assert_eq!(wired.highlighted(), (measured / 2).max(1));
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_moves_off_the_first_row() {
    // Given an open picker whose highlight is on the first row.
    let wired = Wired::new().await;
    wired.open();
    assert_eq!(wired.highlighted(), 0);

    // When page down is pressed.
    wired.fire("page-reasoning-picker-down");

    // Then the highlight left the first row.
    assert!(
        wired.highlighted() > 0,
        "page down must advance the highlight"
    );
}

// ── 4. Confirm ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn confirming_writes_the_effort_to_the_sessions_own_setting() {
    // Given an open picker whose highlight is on "xhigh".
    let wired = Wired::new().await;
    wired.open();
    wired.fire("move-reasoning-picker-down");
    assert_eq!(wired.highlighted_name().as_deref(), Some("xhigh"));

    // When enter is pressed.
    wired.fire("confirm-reasoning-picker");

    // Then the session's own reasoning setting is Xhigh — the value the turn
    // dispatcher reads when it builds the next request.
    assert_eq!(wired.session_effort(), Some(ReasoningEffort::Xhigh),);
}

#[rstest::rstest]
#[tokio::test]
async fn confirming_does_not_leak_into_another_session() {
    // Given a second session in the map whose own effort is High.
    let wired = Wired::new().await;
    let active_id = wired.state.borrow().session.active_session_id().clone();
    let other_id = {
        let mut session = jinn_session_state::ChatSessionState::new();
        session.profile_mut().reasoning_effort = Some(ReasoningEffort::High);
        let id = session.session_id().clone();
        wired.state.borrow_mut().session.insert(session);
        id
    };
    wired.open();
    wired.fire("move-reasoning-picker-down");

    // When enter is pressed in the active session.
    wired.fire("confirm-reasoning-picker");

    // Then the other session still resolves its own High.
    let state = wired.state.borrow();
    assert_eq!(
        resolve_effort(
            state
                .session
                .get(&active_id)
                .expect("active")
                .profile()
                .reasoning_effort
        ),
        Some(ReasoningEffort::Xhigh)
    );
    assert_eq!(
        resolve_effort(
            state
                .session
                .get(&other_id)
                .expect("other")
                .profile()
                .reasoning_effort
        ),
        Some(ReasoningEffort::High),
        "the other session's own effort must be unaffected by this session's choice"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirming_asks_the_persistence_messages_to_run() {
    // Given an open picker.
    let wired = Wired::new().await;
    wired.open();

    // When enter is pressed.
    let result = wired.fire("confirm-reasoning-picker");

    // Then both persistence messages travel out: the session is written now,
    // and the global default is seeded for sessions created later.
    for expected in ["MarkSessionInteracted", "UpdateAppState"] {
        assert!(
            result.message_names.iter().any(|n| n.contains(expected)),
            "confirm must emit {expected}; got {:?}",
            result.message_names
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn confirming_asks_the_scope_stack_to_pop_the_picker() {
    // Given an open picker.
    let wired = Wired::new().await;
    wired.open();

    // When enter is pressed.
    let result = wired.fire("confirm-reasoning-picker");

    // Then the result pops the picker's own scope, closing the menu.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(reasoning_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirming_with_no_selection_writes_nothing() {
    // Given a picker whose rows were never populated.
    let wired = Wired::new().await;

    // When enter is pressed.
    let result = wired.fire("confirm-reasoning-picker");

    // Then nothing was written, emitted, or closed.
    assert_eq!(wired.session_effort(), None);
    assert_eq!(result.message_names, [] as [&str; 0]);
    assert!(result.scope_signal.is_none());
}

// ── 5. Escape ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn escape_leaves_the_sessions_effort_alone() {
    // Given an open picker whose highlight is on a different effort.
    let wired = Wired::new().await;
    wired.set_session_effort(ReasoningEffort::Low);
    wired.open();
    wired.fire("move-reasoning-picker-down");

    // When escape is pressed.
    wired.fire("cancel-reasoning-picker");

    // Then the session still resolves Low.
    assert_eq!(wired.session_effort(), Some(ReasoningEffort::Low));
}

#[rstest::rstest]
#[tokio::test]
async fn escape_asks_the_scope_stack_to_pop_the_picker() {
    // Given an open picker.
    let wired = Wired::new().await;
    wired.open();

    // When escape is pressed.
    let result = wired.fire("cancel-reasoning-picker");

    // Then the result pops the picker's own scope, closing the menu.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(reasoning_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn escape_persists_nothing() {
    // Given an open picker whose highlight is on a different effort.
    let wired = Wired::new().await;
    wired.open();
    wired.fire("move-reasoning-picker-down");

    // When escape is pressed.
    let result = wired.fire("cancel-reasoning-picker");

    // Then no message travels out — the move was a look, not a choice.
    assert!(
        result.message_names.is_empty(),
        "escape must not persist an effort; got {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[tokio::test]
async fn clear_control_key_closes_the_picker_when_the_filter_is_empty() {
    // Given an open picker with no filter text.
    let wired = Wired::new().await;
    wired.open();

    // When ctrl-c is pressed.
    let result = wired.fire("clear-filter-or-leave-reasoning-picker");

    // Then the picker asked the scope stack to pop it.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(reasoning_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn clear_control_key_clears_a_non_empty_filter_instead_of_closing() {
    // Given an open picker whose filter reads "hi".
    let wired = Wired::new().await;
    wired.open();
    for ch in "hi".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // When ctrl-c is pressed.
    let result = wired.fire("clear-filter-or-leave-reasoning-picker");

    // Then the filter is empty and the picker did not ask to close.
    assert_eq!(wired.filter(), "");
    assert!(
        result.scope_signal.is_none(),
        "ctrl-c with text in the filter must not close the picker"
    );
}

// ── Rendering ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_status_line_names_the_sessions_active_effort() {
    // Given a session whose own effort is Low, with the picker open.
    let wired = Wired::new().await;
    wired.set_session_effort(ReasoningEffort::Low);
    wired.open();

    // When the popup is drawn.
    let frame = wired.draw();

    // Then the status line reports it.
    let rendered: String = frame.concat();
    assert!(
        rendered.contains("Active: low"),
        "the status line names the session's effort; got {rendered:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_status_line_says_none_when_the_session_has_no_effort() {
    // Given a session with no effort of its own, with the picker open.
    let wired = Wired::new().await;
    wired.open();

    // When the popup is drawn.
    let frame = wired.draw();

    // Then the status line reports "Active: none".
    let rendered: String = frame.concat();
    assert!(
        rendered.contains("Active: none"),
        "the status line names the session's effort; got {rendered:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_active_efforts_row_is_marked_in_the_drawn_picker() {
    // Given a session whose own effort is High, with the picker open.
    let wired = Wired::new().await;
    wired.set_session_effort(ReasoningEffort::High);
    wired.open();

    // When the popup is drawn.
    let frame = wired.draw();

    // Then the active row is drawn with the bold arrow marker.
    let rendered: String = frame.concat();
    assert!(
        rendered.contains("> high"),
        "the active effort's row carries the marker; got {rendered:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn drawing_a_frame_publishes_the_measured_row_count_for_paging() {
    // Given an open picker whose cell starts on the pre-render fallback.
    let wired = Wired::new().await;
    wired.open();
    let fallback = wired.cell().read().results_viewport;
    assert_eq!(
        fallback,
        jinn_provider_selection_msg::reasoning::RESULTS_VIEWPORT_FALLBACK
    );

    // When a frame is drawn.
    wired.draw();

    // Then the cell carries what the frame actually laid out, so paging
    // moves by a page of what is on screen rather than a fixed guess.
    let measured = wired.cell().read().results_viewport;
    let expected = crate::reasoning_picker_viewport::results_viewport(ratatui::layout::Rect::new(
        0, 0, 100, 30,
    ));
    assert_eq!(measured, expected);
    assert_ne!(
        measured, fallback,
        "a narrow popup lays out far fewer rows than the fallback, so the \
         measurement must replace it rather than sit beside it"
    );
}

// ── 6. Every advertised key is bound ────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn every_key_the_footer_advertises_is_bound_in_the_picker_scope() {
    // Given a wired slice.
    let wired = Wired::new().await;

    // When the set of keys the picker attached is read.
    let attached: Vec<&'static str> = wired
        .routes
        .rows()
        .iter()
        .filter(|row| row.scope == reasoning_picker_scope())
        .map(|row| row.key)
        .collect();

    // Then every key the footer advertises is among them — no dead key.
    for row in crate::reasoning_picker_render::reasoning_picker_binds() {
        assert!(
            attached.contains(&row.notation),
            "the footer advertises {} but the scope binds {attached:?}",
            row.notation
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_binds_every_base_key_through_an_action() {
    // Given a wired slice.
    let wired = Wired::new().await;

    // When the picker's own rows are read.
    let bound: Vec<&'static str> = wired
        .routes
        .rows()
        .iter()
        .filter(|row| {
            row.scope == reasoning_picker_scope()
                && matches!(row.outcome, RouteOutcome::Action { .. })
        })
        .map(|row| row.key)
        .collect();

    // Then every key the picker used to inherit from the kernel's shared
    // picker base is present, and reachable through an action rather than a
    // static intent — a `StaticIntent` row naming a picker route id is
    // silently dropped at keymap generation.
    for key in [
        "<enter>", "<esc>", "<up>", "<down>", "<pgup>", "<pgdn>", "<c-n>", "<c-c>",
    ] {
        assert!(
            bound.contains(&key),
            "the picker must bind {key}; got {bound:?}"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_declares_exactly_its_confirm_and_cancel_keys_in_the_footer() {
    // Given a wired slice with the picker open.
    let wired = Wired::new().await;
    wired.open();

    // When the popup is drawn.
    let frame = wired.draw();

    // Then the footer names both the confirm and cancel keys, and nothing
    // else: no padding advertising a key the picker does not bind.
    let footer = frame
        .iter()
        .find(|row| row.contains("Enter"))
        .unwrap_or_else(|| panic!("the popup must draw a keybind footer; got {frame:?}"));
    assert!(
        footer.contains("Esc") || footer.contains("esc"),
        "the footer must name the cancel key; got {footer:?}"
    );
    let advertised: Vec<&'static str> = crate::reasoning_picker_render::reasoning_picker_binds()
        .iter()
        .map(|row| row.notation)
        .collect();
    assert_eq!(advertised, vec!["<enter>", "<esc>"]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_hook_is_registered_so_typing_reaches_the_picker() {
    // Given a wired slice.
    let wired = Wired::new().await;

    // When the scope's input hook is looked up.
    let hook = wired.routes.input_hook(&reasoning_picker_scope());

    // Then it exists: without it the catch-all character key is never
    // synthesized and the filter silently stops accepting typed characters.
    assert!(
        hook.is_some(),
        "the picker must register an input hook for its filter"
    );
}

// ── 7. One owner ────────────────────────────────────────────────────────

#[rstest::rstest]
fn the_picker_state_lives_only_in_its_slice_cell() {
    // Given the shared frontend state, which must hold no reasoning picker
    // state: the slice cell is the only home. A second copy there would let the
    // menu show one store while a different one is written.
    let state_source = include_str!("../../../jinn-app-state/src/frontend_state.rs");

    // When scanning it for the picker's vocabulary.
    // Then no reasoning picker state is held in the shared state layer.
    assert!(
        !state_source.contains("reasoning_effort_picker"),
        "the shared state layer must not hold reasoning picker state; the slice cell is the only home"
    );
}

#[rstest::rstest]
fn the_kernel_names_no_reasoning_picker_at_all() {
    // Given the central app crate and the TUI layer, which must not know this
    // picker exists: no scope variant, no picker kind, no spec id. That is what
    // makes adding a picker a folder-local change.
    for (label, source) in [
        (
            "shared frontend state",
            include_str!("../../../jinn-app-state/src/frontend_state.rs"),
        ),
        (
            "jinn-kernel intent handler",
            include_str!("../../../jinn-kernel/src/feat/intent/handler.rs"),
        ),
        (
            "jinn-tui scope table",
            include_str!("../../../jinn-tui/src/scope.rs"),
        ),
    ] {
        let picker_named = [
            "PickerReasoning",
            "Picker(reasoning-effort)",
            "REASONING_EFFORT_ID",
            "reasoning_effort_spec",
        ];
        // When scanning each source for the picker's own vocabulary.
        let hits: Vec<&str> = picker_named
            .iter()
            .filter(|needle| source.contains(*needle))
            .copied()
            .collect();

        // Then none of them name the reasoning picker.
        assert!(
            hits.is_empty(),
            "{label} still names the reasoning picker ({hits:?}); the slice must own it entirely"
        );
    }
}
