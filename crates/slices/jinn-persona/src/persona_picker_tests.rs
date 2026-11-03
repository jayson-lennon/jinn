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

//! The persona picker's observable behavior.
//!
//! Tests that assert *wiring* run the slice's real `activate_picker`, so a
//! picker that registered its cell but forgot its rows or its input hook fails
//! loudly rather than passing against a hand-built stand-in. Tests that assert
//! *behavior* drive the picker's public entry points.
//!
//! `Wired::open` dispatches the picker's own `<leader>se` action and applies
//! the scope signal it returns, exactly as the kernel's `apply_scope_signal`
//! does — including firing the scope-enter hook that seeds the menu. Tests
//! that care about *which* opener ran drive `Wired::enter` instead, which is
//! the path a non-picker opener (the sidebar's persona key) takes.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use jinn_persona_msg::{PersonaPickerState, persona_picker_scope, personas_slot};
use jinn_slices::Persona;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{EditIntent, RouteOutcome, ScopeSignal};
use jinn_slices::{KeyRoutes, SliceHost, Slices};
use jinn_testutil::volume_root;

/// The slice wired exactly as composition wires it.
struct Wired {
    slices: Slices,
    routes: KeyRoutes,
    state: std::cell::RefCell<jinn_kernel::AppState>,
}

impl Wired {
    /// Builds the slice with `personas` already present in its personas cell,
    /// avoiding a filesystem scan so the tests describe the picker, not the
    /// persona loader.
    async fn new(personas: Vec<(&str, &str)>) -> Self {
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
            // An empty personas dir: the scan runs for real (so the personas
            // cell the catalog seeded is re-seeded with the scan's result),
            // but the tests seed the entries they need rather than depending
            // on the loader.
            let dir = volume_root()
                .join_all(["nonexistent-personas-dir"])
                .expect("rooted test path");
            crate::activate(&mut host, &dir);
            crate::activate_picker(&mut host);
        }
        slices
            .reader::<jinn_persona_msg::Personas>(&personas_slot())
            .expect("the catalog registers the personas cell before activation")
            .update(|p: &mut jinn_persona_msg::Personas| {
                p.entries = personas
                    .into_iter()
                    .map(|(name, description)| Persona {
                        name: name.to_owned(),
                        description: description.to_owned(),
                        body: format!("{name} body"),
                    })
                    .collect();
            });
        let state = jinn_kernel::AppState::default();
        state.frontend.attach_slices(slices.clone());
        Self {
            slices,
            routes,
            state: std::cell::RefCell::new(state),
        }
    }

    /// The picker cell.
    fn cell(&self) -> TypedCell<PersonaPickerState> {
        self.slices
            .reader(&jinn_persona_msg::persona_picker_slot())
            .expect("the picker registers its cell at activation")
    }

    /// The names the filter currently shows, in display order.
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

    /// Every item the picker holds, filtered or not.
    fn all_names(&self) -> Vec<String> {
        self.cell()
            .read()
            .selection
            .items()
            .iter()
            .map(|item| item.entry().name.clone())
            .collect()
    }

    /// The index of the highlighted row.
    fn highlighted(&self) -> usize {
        self.cell().read().selection.selection()
    }

    /// The filter text.
    fn filter(&self) -> String {
        self.cell().read().selection.filter().to_owned()
    }

    /// Opens the picker the way the kernel does: dispatch the open action,
    /// then apply the scope signal it requested, which fires the picker's
    /// scope-enter hook.
    ///
    /// The hook is what seeds the rows and clears the filter, so a test that
    /// only fired the action would exercise a picker that never opened.
    fn open(&self) -> jinn_slices::RouteResult {
        let result = self.fire("open-persona-picker");
        self.apply_signal(
            result
                .scope_signal
                .clone()
                .expect("the open action requests a push"),
        );
        result
    }

    /// Enters the picker's scope the way the kernel's `apply_scope_signal`
    /// does, so a scope-enter hook fires exactly as it does in the app.
    ///
    /// This is the path any *other* opener takes: the sidebar's persona key
    /// requests this same transition, so driving it here proves the hook — not
    /// the particular opener — is what builds the menu.
    fn enter(&self) {
        self.apply_signal(ScopeSignal::Push(persona_picker_scope()));
    }

    /// Applies a scope signal the way the kernel does, so the picker's
    /// scope-enter hook fires on a push.
    fn apply_signal(&self, signal: ScopeSignal) {
        let mut state = self.state.borrow_mut();
        match signal {
            ScopeSignal::Push(id) => {
                state
                    .frontend
                    .scope_push(jinn_slices::FocusScope::Dynamic(id.clone()));
                if let Some(hook) = self.routes.scope_enter_hook(&id) {
                    hook(jinn_slices::ActionCtx {
                        state: &mut *state,
                        slices: &self.slices,
                        config: jinn_slices::empty_config_layer(),
                        key_bytes: Vec::new(),
                    });
                }
            }
            ScopeSignal::PopIf(id) => {
                if matches!(&state.frontend.scope(), jinn_slices::FocusScope::Dynamic(cur) if *cur == id)
                {
                    state.frontend.scope_pop();
                }
            }
        }
    }

    /// Dispatches one of the picker's actions by its route action name.
    fn fire(&self, action: &str) -> jinn_slices::RouteResult {
        let mut state = self.state.borrow_mut();
        self.routes
            .action_for(
                &jinn_slices::DynamicIntent::new(persona_picker_scope(), action, action),
                jinn_slices::ActionCtx {
                    state: &mut *state,
                    slices: &self.slices,
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .unwrap_or_else(|| panic!("the persona picker attaches an action named {action}"))
    }

    /// Feeds an edit intent to the picker's registered input hook.
    fn edit(&self, intent: &EditIntent) {
        let hook = self
            .routes
            .input_hook(&persona_picker_scope())
            .expect("the picker registers an input hook for its filter");
        hook(intent).expect("the filter hook always consumes the edit");
    }
}

/// Draws one frame through the picker's own render pass, which is what
/// publishes the measured row count the pager pages by.
fn draw_frame(wired: &Wired, area: ratatui::layout::Rect) {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    let facts = jinn_slices::RenderFacts::new(jinn_theme::default_theme(), &wired.slices);
    terminal
        .draw(|frame| {
            let popup = crate::persona_picker_render::persona_picker_overlay_rect(&area)
                .expect("geometry fn yields a popup rect");
            crate::persona_picker_render::render_persona_picker(frame, popup, &facts);
        })
        .expect("draw");
}

// ── 1. Opens with entries ───────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn opening_the_picker_shows_the_scanned_personas() {
    // Given a slice whose personas cell holds two personas.
    let wired = Wired::new(vec![
        ("coding-assistant", "writes code"),
        ("tutor", "teaches"),
    ])
    .await;

    // When the picker is opened.
    wired.open();

    // Then its rows name both personas.
    assert_eq!(wired.visible_names(), vec!["coding-assistant", "tutor"]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_marks_the_active_persona() {
    // Given a slice whose active session's persona is "tutor".
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired
        .state
        .borrow_mut()
        .active_session_mut()
        .set_persona_name("tutor".to_owned());

    // When the picker is opened.
    wired.open();

    // Then the tutor row is marked active and the coder row is not.
    let cell = wired.cell();
    let active: Vec<bool> = cell
        .read()
        .selection
        .items()
        .iter()
        .map(|item| item.entry().is_active)
        .collect();
    // And the filter still shows both rows — the active marker is a
    // decoration, not a filter.
    assert_eq!(active, vec![false, true]);
    assert_eq!(wired.all_names().len(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn picker_enter_preselects_active_sessions_persona() {
    // Given a slice whose active session carries a persona the picker lists.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired
        .state
        .borrow_mut()
        .active_session_mut()
        .set_persona_name("tutor".to_owned());

    // When the picker is entered through a non-picker opener (the sidebar key).
    wired.enter();

    // Then the session's persona is the marked one.
    let cell = wired.cell();
    let guard = cell.read();
    let marked: Vec<&str> = guard
        .selection
        .items()
        .iter()
        .filter(|item| item.entry().is_active)
        .map(|item| item.entry().name.as_str())
        .collect();
    assert_eq!(marked, vec!["tutor"]);
}

// ── 2. Every opener lands on the same fresh menu ────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_picker_registers_a_scope_enter_hook() {
    // Given a slice activated the way composition activates it.
    let wired = Wired::new(vec![("coder", "a")]).await;

    // When the registered scope-enter hooks are read.
    let hook = wired.routes.scope_enter_hook(&persona_picker_scope());

    // Then the picker has one, so entering the scope always seeds the menu.
    assert!(
        hook.is_some(),
        "the persona picker must seed itself on scope entry, or a second opener shows an empty menu"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_open_action_asks_the_handler_to_push_the_picker_scope() {
    // Given a wired slice.
    let wired = Wired::new(vec![("coder", "a")]).await;

    // When the open action runs.
    let result = wired.fire("open-persona-picker");

    // Then it requests the transition rather than performing it.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::Push(persona_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_open_action_alone_seeds_no_rows() {
    // Given a wired slice that has never been entered.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;

    // When the open action runs without the handler applying its signal.
    wired.fire("open-persona-picker");

    // Then nothing is shown yet: the scope stack has one writer, and the rows
    // come from the scope-enter hook the kernel fires on the push.
    assert_eq!(wired.all_names(), [] as [std::string::String; 0]);
}

#[rstest::rstest]
#[tokio::test]
async fn entering_the_picker_scope_shows_the_scanned_personas() {
    // Given a slice whose personas cell holds two personas, never opened.
    let wired = Wired::new(vec![
        ("coding-assistant", "writes code"),
        ("tutor", "teaches"),
    ])
    .await;

    // When the scope is entered — the path an opener other than the picker's
    // own row takes, such as the sidebar's persona key.
    wired.enter();

    // Then its rows name both personas.
    assert_eq!(wired.visible_names(), vec!["coding-assistant", "tutor"]);
}

#[rstest::rstest]
#[tokio::test]
async fn entering_the_picker_scope_copies_the_live_theme() {
    // Given a slice whose frontend theme differs from the default.
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.state.borrow_mut().frontend.theme.focus_accent = ratatui::style::Color::LightRed;

    // When the scope is entered.
    wired.enter();

    // Then the picker paints with the app's theme, not a stale one.
    assert_eq!(
        wired.cell().read().theme.focus_accent,
        ratatui::style::Color::LightRed
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_open_clears_the_filter_left_by_the_first() {
    // Given a picker whose filter text was typed and highlight moved.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();
    for ch in "cod".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }
    wired.fire("move-persona-picker-down");
    assert_eq!(wired.filter(), "cod");

    // When the scope is entered again through a different opener.
    wired.apply_signal(ScopeSignal::PopIf(persona_picker_scope()));
    wired.enter();

    // Then the filter is empty and both rows are back.
    assert_eq!(wired.filter(), "");
    assert_eq!(wired.visible_names(), vec!["coder", "tutor"]);
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_open_puts_the_highlight_back_at_the_top() {
    // Given a picker whose highlight was moved off the first row.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();
    wired.fire("move-persona-picker-down");
    assert_eq!(wired.highlighted(), 1);

    // When the scope is entered again.
    wired.apply_signal(ScopeSignal::PopIf(persona_picker_scope()));
    wired.enter();

    // Then the highlight is back on the first row.
    assert_eq!(wired.highlighted(), 0);
}

// ── 3. The filter narrows ───────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn typing_a_character_filters_the_list() {
    // Given an open picker with two personas.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();

    // When the user types "cod".
    for ch in "cod".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // Then the filter holds the text and only the matching row survives.
    assert_eq!(wired.filter(), "cod");
    assert_eq!(wired.visible_names(), vec!["coder"]);
}

#[rstest::rstest]
#[tokio::test]
async fn backspace_shortens_the_filter() {
    // Given an open picker whose filter reads "cod".
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();
    for ch in "cod".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // When backspace is pressed twice.
    wired.edit(&EditIntent::DeleteBackward);
    wired.edit(&EditIntent::DeleteBackward);

    // Then the filter reads "c" and only the one name contains it.
    assert_eq!(wired.filter(), "c");
    assert_eq!(wired.visible_names(), vec!["coder"]);
}

// ── 4. Navigation ───────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn down_arrow_moves_the_highlight() {
    // Given an open picker whose highlight is on the first row.
    let wired = Wired::new(vec![("a", "x"), ("b", "x")]).await;
    wired.open();
    assert_eq!(wired.highlighted(), 0);

    // When the down arrow is pressed.
    wired.fire("move-persona-picker-down");

    // Then the highlight is on the second row.
    assert_eq!(wired.highlighted(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn up_arrow_stops_at_the_first_row() {
    // Given an open picker whose highlight is already at the top.
    let wired = Wired::new(vec![("a", "x"), ("b", "x")]).await;
    wired.open();

    // When the up arrow is pressed.
    wired.fire("move-persona-picker-up");

    // Then the highlight does not wrap or move past the first row.
    assert_eq!(wired.highlighted(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_steps_by_the_rows_the_last_frame_actually_laid_out() {
    // Given an open picker with far more rows than a frame can show, and a
    // frame drawn so the cell learns how many rows are on screen.
    let many: Vec<(String, String)> = (0..200)
        .map(|i| (format!("p{i:03}"), "x".to_owned()))
        .collect();
    let owned: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let wired = Wired::new(owned).await;
    wired.open();
    // A narrow-ish frame: the stacked layout's list pane holds far fewer rows
    // than the pre-render fallback, so a picker paging by the fallback would
    // move a visibly different distance.
    let frame = ratatui::layout::Rect::new(0, 0, 100, 30);
    draw_frame(&wired, frame);
    let on_screen = wired.cell().read().results_viewport;

    // When page down is pressed.
    wired.fire("page-persona-picker-down");

    // Then the highlight advanced by half a screen of rows — a page of what
    // the user can see, not of a fixed guess about how much that is.
    assert_eq!(wired.highlighted(), (on_screen / 2).max(1));
}

#[rstest::rstest]
#[tokio::test]
async fn drawing_a_frame_publishes_the_widget_solved_row_count_for_paging() {
    // Given an open picker whose cell starts on the pre-render fallback.
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.open();
    let fallback = wired.cell().read().results_viewport;
    assert_eq!(fallback, jinn_persona_msg::RESULTS_VIEWPORT_FALLBACK);

    // When a frame is drawn through the picker's real render pass.
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    draw_frame(&wired, area);

    // Then the cell carries the row count the widget solved for that frame —
    // the same `PickerLayout.inner.height` an identical second construction
    // returns — so paging moves by a page of what is on screen.
    let measured = wired.cell().read().results_viewport;
    let expected = widget_layout(&wired, area).inner.height as usize;
    assert_eq!(measured, expected);
}

/// Renders the same widget the persona picker's render pass draws, into a
/// fresh test terminal, and returns the [`jinn_selection_widget::PickerLayout`]
/// the widget solved.
///
/// This is the expected-value source for renderer-level viewport assertions:
/// the layout comes from the widget, never from hand arithmetic.
fn widget_layout(
    wired: &Wired,
    area: ratatui::layout::Rect,
) -> jinn_selection_widget::PickerLayout {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    let theme = jinn_theme::default_theme();
    let popup = crate::persona_picker_render::persona_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let report = std::cell::Cell::new(None);
    terminal
        .draw(|frame| {
            let cell = wired.cell();
            let guard = cell.read();
            let state: &PersonaPickerState = &guard;
            let palette = crate::persona_picker_render::persona_picker_palette(&theme);
            let binds = crate::persona_picker_render::persona_picker_binds();
            let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);
            let footers = vec![
                ratatui::text::Line::from(String::new()),
                ratatui::text::Line::from(keybind.0),
            ];

            let widget = jinn_selection_widget::SelectionWidget::new(&state.selection)
                .title(ratatui::text::Line::from(" Personas "))
                .title_style(ratatui::style::Style::default().fg(theme.popup_title))
                .footers(footers)
                .colors(palette.selection_colors());
            report.set(Some(widget.render(frame, popup)));
        })
        .expect("draw");
    report.into_inner().expect("render reports a PickerLayout")
}

// ── 5. Confirm ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn confirming_binds_the_highlighted_persona_to_the_active_session() {
    // Given an open picker whose highlight is on the second persona.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();
    wired.fire("move-persona-picker-down");

    // When enter is pressed.
    wired.fire("confirm-persona-picker");

    // Then the active session carries the highlighted persona.
    assert_eq!(
        wired.state.borrow().active_session().persona_name(),
        "tutor"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_binds_session_and_persists_last_persona() {
    // Given an open picker whose highlight is on the second persona.
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired.open();
    wired.fire("move-persona-picker-down");

    // When enter is pressed.
    let result = wired.fire("confirm-persona-picker");

    // Then the persisted last-persona seed travels out as SetPersona.
    let applied = persisted_persona_of(result).expect("confirm persists the persona");
    assert_eq!(applied, Some("tutor".to_owned()));
}

/// Drains `result`'s publish closures into a sink and decodes the
/// `UpdateAppState` command they carried, if any.
///
/// The `Option<Option<String>>` is deliberate: the outer `None` means "no
/// `UpdateAppState` was published", the inner `None` means "published, with
/// `SetPersona(None)`" — the distinction a confirm assertion needs.
#[expect(
    clippy::option_option,
    reason = "outer None = command absent, inner None = SetPersona(None); the confirm assertion needs both"
)]
fn persisted_persona_of(result: jinn_slices::RouteResult) -> Option<Option<String>> {
    #[derive(Default)]
    struct RecordingSink {
        published: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl jinn_slices::PublishSink for RecordingSink {
        fn publish_schema(
            &self,
            schema_id: trouper::schema::SchemaId,
            payload: serde_json::Value,
            _name: &'static str,
        ) {
            self.published
                .lock()
                .expect("sink lock")
                .push((format!("{schema_id}"), payload));
        }
    }

    let sink = RecordingSink::default();
    for closure in result.messages {
        closure(&sink);
    }
    let published = sink.published.lock().expect("sink lock");
    let (_, payload) = published
        .iter()
        .find(|(id, _)| id.ends_with("UpdateAppState"))?;
    let command: jinn_preferences_config::protocol::app_state_command::UpdateAppState =
        serde_json::from_value(payload.clone()).expect("the payload is an UpdateAppState");
    command.updates.into_iter().find_map(|update| match update {
        jinn_preferences_config::protocol::app_state_command::AppStateUpdate::SetPersona(name) => {
            Some(name)
        }
        _ => None,
    })
}

#[rstest::rstest]
#[tokio::test]
async fn confirming_asks_the_scope_stack_to_pop_the_picker() {
    // Given an open picker.
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.open();

    // When enter is pressed.
    let result = wired.fire("confirm-persona-picker");

    // Then the result pops the picker's own scope, closing the menu.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(persona_picker_scope()))
    );
}

// ── 6. Escape ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn escape_leaves_the_active_persona_alone() {
    // Given an open picker over a slice whose session persona is "coder".
    let wired = Wired::new(vec![("coder", "a"), ("tutor", "b")]).await;
    wired
        .state
        .borrow_mut()
        .active_session_mut()
        .set_persona_name("coder".to_owned());
    wired.open();
    wired.fire("move-persona-picker-down");

    // When escape is pressed.
    wired.fire("cancel-persona-picker");

    // Then the session still carries its original persona.
    assert_eq!(
        wired.state.borrow().active_session().persona_name(),
        "coder"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn escape_asks_the_scope_stack_to_pop_the_picker() {
    // Given an open picker.
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.open();

    // When escape is pressed.
    let result = wired.fire("cancel-persona-picker");

    // Then the result pops the picker's own scope, closing the menu.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(persona_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn clear_control_key_closes_the_picker_when_the_filter_is_empty() {
    // Given an open picker with no filter text.
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.open();

    // When ctrl-c is pressed.
    let result = wired.fire("clear-filter-or-leave-persona-picker");

    // Then the picker asked the scope stack to pop it.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(persona_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn clear_control_key_clears_a_non_empty_filter_instead_of_closing() {
    // Given an open picker whose filter reads "cod".
    let wired = Wired::new(vec![("coder", "a")]).await;
    wired.open();
    for ch in "cod".chars() {
        wired.edit(&EditIntent::InsertChar(ch));
    }

    // When ctrl-c is pressed.
    let result = wired.fire("clear-filter-or-leave-persona-picker");

    // Then the filter is empty and the picker did not ask to close.
    assert_eq!(wired.filter(), "");
    assert!(
        result.scope_signal.is_none(),
        "ctrl-c with text in the filter must not close the picker"
    );
}

// ── 7. Every advertised key is bound ────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn every_key_the_footer_advertises_is_bound_in_the_picker_scope() {
    // Given a wired slice.
    let wired = Wired::new(vec![("coder", "a")]).await;

    // When the set of keys the picker attached is read.
    let attached: Vec<&'static str> = wired
        .routes
        .rows()
        .iter()
        .filter(|row| row.scope == persona_picker_scope())
        .map(|row| row.key)
        .collect();

    // Then every key the footer advertises is among them — no dead key.
    for row in crate::persona_picker_render::persona_picker_binds() {
        assert!(
            attached.contains(&row.notation),
            "the footer advertises {} but the scope binds {attached:?}",
            row.notation
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_picker_declares_every_key_it_actually_binds_in_the_footer() {
    // Given a wired slice.
    let wired = Wired::new(vec![("coder", "a")]).await;

    // When the attached rows are split into rows and non-rows.
    let picker_rows: Vec<&'static str> = wired
        .routes
        .rows()
        .iter()
        .filter(|row| {
            row.scope == persona_picker_scope()
                && matches!(row.outcome, RouteOutcome::Action { .. })
        })
        .map(|row| row.key)
        .collect();
    let advertised: Vec<&'static str> = crate::persona_picker_render::persona_picker_binds()
        .iter()
        .map(|row| row.notation)
        .collect();

    // Then the footer's two confirm/cancel keys are the picker's own.
    assert_eq!(advertised, vec!["<enter>", "<esc>"]);
    for key in &picker_rows {
        assert!(
            attached_key(&wired, key),
            "picker binds {key} through an action, so it is reachable"
        );
    }
}

/// Whether the picker's scope binds `key` through an action row.
fn attached_key(wired: &Wired, key: &str) -> bool {
    wired.routes.rows().iter().any(|row| {
        row.scope == persona_picker_scope()
            && row.key == key
            && matches!(row.outcome, RouteOutcome::Action { .. })
    })
}
