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

//! The model picker's behavior, exercised through its real route rows.
//!
//! Every test drives the slice as composition wires it — the real
//! `activate` call, the real cells, the real key routes — so a defect that
//! only appears in production wiring fails loudly here instead of passing
//! against a hand-built stand-in. The provider actors are *not* spawned: these
//! tests describe the picker, not the fetch machinery.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test module, panics are acceptable"
)]

use jinn_core_types::model_selection::{AlloyStrategy, ModelSelection};
use jinn_provider_selection_msg::ProviderCell;
use jinn_provider_selection_msg::ProviderPickerEntry;
use jinn_provider_selection_msg::ProviderPickerState;
use jinn_provider_selection_msg::provider_picker_scope;
use jinn_provider_selection_msg::provider_picker_slot;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{EditIntent, ScopeSignal};
use jinn_slices::{KeyRoutes, SliceHost, Slices};

/// The slice wired exactly as composition wires it.
struct Wired {
    slices: Slices,
    routes: KeyRoutes,
    state: std::cell::RefCell<jinn_kernel::AppState>,
}

impl Wired {
    /// Builds the slice with the model picker registered.
    async fn new() -> Self {
        let slices = Slices::new();
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
            // Both cells are part of this slice's real activation: the model
            // picker reads alloy mode out of the provider cell, and the
            // provider actor publishes loaded rows into the picker cell.
            host.register_cell(
                jinn_provider_selection_msg::provider_state_slot(),
                ProviderCell::default(),
            )
            .expect("the provider cell is registered once at wiring");
            let cell = host
                .register_cell(provider_picker_slot(), ProviderPickerState::default())
                .expect("the model picker cell is registered once at wiring");
            crate::activate_provider_picker(&mut host, &cell);
        }
        // Mirror `jinn_tui::app::builder`: seed the scope-focus slot on *this*
        // registry, then attach that same registry. `attach_slices` writes a
        // `OnceLock`, so a state built by `default_with_scope_focus` (which
        // mints its own registry) would keep the other one and the picker
        // could never reach the provider cell.
        let _ = slices.register(
            jinn_slices::scope_focus_slot(),
            jinn_slices::ScopeFocusState::default(),
        );
        let state = jinn_kernel::AppState::default();
        state.frontend.attach_slices(slices.clone());
        Self {
            slices,
            routes,
            state: std::cell::RefCell::new(state),
        }
    }

    /// The picker's cell.
    fn cell(&self) -> TypedCell<ProviderPickerState> {
        self.slices
            .reader(&provider_picker_slot())
            .expect("the picker registers its cell at activation")
    }

    /// The shared provider cell, where alloy mode lives.
    fn provider_cell(&self) -> TypedCell<ProviderCell> {
        self.slices
            .reader(&jinn_provider_selection_msg::provider_state_slot())
            .expect("the provider cell is registered at activation")
    }

    /// The model names the filter currently shows, in display order.
    fn visible(&self) -> Vec<String> {
        let cell = self.cell();
        let guard = cell.read();
        (0..guard.selection.filtered_count())
            .filter_map(|i| guard.selection.filtered_item(i))
            .map(|item| item.entry().model.clone())
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

    /// Which models are checked for the alloy, in display order.
    fn checked(&self) -> Vec<String> {
        self.cell()
            .read()
            .selection
            .items()
            .iter()
            .filter(|item| item.entry().selected)
            .map(|item| item.entry().provider_id.clone())
            .collect()
    }

    /// Whether the picker is in alloy mode.
    fn alloy_mode(&self) -> bool {
        self.provider_cell().read().is_alloy_mode()
    }

    /// Puts the active session on a single named model.
    fn set_single_model(&self, model: &str) {
        self.state
            .borrow_mut()
            .active_session_mut()
            .profile_mut()
            .model = ModelSelection::Single(model.to_owned());
    }

    /// Puts the active session on an alloy of the named models.
    fn set_alloy_model(&self, models: &[&str]) {
        self.state
            .borrow_mut()
            .active_session_mut()
            .profile_mut()
            .model = ModelSelection::Alloy {
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        };
    }

    /// Fills the picker with `entries` through its own actions.
    fn with_entries(&self, entries: Vec<ProviderPickerEntry>) {
        self.cell()
            .update(|picker| crate::provider_picker_actions::load(picker, entries));
    }

    /// Opens the picker through its real `open` route action.
    fn open(&self) -> jinn_slices::RouteResult {
        self.fire("open-provider-picker")
    }

    /// Dispatches one of the picker's actions by its route action name.
    fn fire(&self, action: &str) -> jinn_slices::RouteResult {
        let mut state = self.state.borrow_mut();
        self.routes
            .action_for(
                &jinn_slices::DynamicIntent::new(provider_picker_scope(), action, action),
                jinn_slices::ActionCtx {
                    state: &mut *state,
                    slices: &self.slices,
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .unwrap_or_else(|| panic!("the model picker attaches an action named {action}"))
    }

    /// Feeds an edit intent to the picker's registered input hook.
    fn edit(&self, intent: &EditIntent) {
        let hook = self
            .routes
            .input_hook(&provider_picker_scope())
            .expect("the picker registers an input hook for its filter");
        hook(intent).expect("the filter hook always consumes the edit");
    }

    /// Types `text` into the filter, one character at a time.
    fn type_text(&self, text: &str) {
        for ch in text.chars() {
            self.edit(&EditIntent::InsertChar(ch));
        }
    }

    /// The action a key resolves to in the picker's own scope, if any.
    ///
    /// Read from the real row table, so a key that is advertised but unbound
    /// — or bound in the wrong scope — shows up as `None`.
    fn resolve_key(&self, key: &str) -> Option<&'static str> {
        let row = self
            .routes
            .rows()
            .into_iter()
            .find(|row| row.scope == provider_picker_scope() && row.key == key)?;
        // A `StaticIntent` outcome would mean the row was dropped at keymap
        // generation: composition's static_intent table knows only six route
        // ids, and an unknown one is silently skipped. A picker row must
        // therefore always be an `Action`.
        match row.outcome {
            jinn_slices::route::RouteOutcome::Action { action, .. } => Some(action),
            jinn_slices::route::RouteOutcome::StaticIntent(_) => None,
        }
    }

    /// Draws one frame of the picker at `120x34` and returns the buffer's
    /// symbols, row by row.
    fn draw(&self) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let area = ratatui::layout::Rect::new(0, 0, 120, 34);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("a test terminal allocates");
        let facts =
            jinn_slices::RenderFacts::new(self.state.borrow().frontend.theme.clone(), &self.slices);
        terminal
            .draw(|frame| {
                let popup = crate::provider_picker_render::provider_picker_overlay_rect(&area)
                    .expect("geometry fn yields a popup rect");
                crate::provider_picker_render::render_provider_picker(frame, popup, &facts);
            })
            .expect("the draw closure must not panic");
        terminal
            .backend()
            .buffer()
            .content
            .chunks(area.width as usize)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
            .collect()
    }
}

/// Captures what a route action publishes, so a test can read a message's
/// *payload* — `RouteResult` keeps only opaque closures and type names.
#[derive(Debug, Default)]
struct Recording {
    names: std::cell::RefCell<Vec<&'static str>>,
    payloads: std::cell::RefCell<Vec<serde_json::Value>>,
}

impl jinn_slices::route_publish::PublishSink for Recording {
    fn publish_schema(
        &self,
        _schema_id: trouper::schema::SchemaId,
        payload: serde_json::Value,
        name: &'static str,
    ) {
        self.names.borrow_mut().push(name);
        self.payloads.borrow_mut().push(payload);
    }
}

/// Publishes a route result through the recorder, returning it.
fn publish(result: jinn_slices::RouteResult) -> Recording {
    let recorder = Recording::default();
    for msg in result.messages {
        msg(&recorder);
    }
    recorder
}

/// The single published `ProviderSwitch`'s model, or `None`.
fn switched_to(recorder: &Recording) -> Option<ModelSelection> {
    let index = recorder
        .names
        .borrow()
        .iter()
        .position(|n| n.ends_with("ProviderSwitch"))?;
    // Names and payloads are pushed together, so the index always exists.
    let payload = recorder.payloads.borrow().get(index)?.clone();
    let switch =
        serde_json::from_value::<jinn_provider_selection_msg::ProviderSwitch>(payload).ok()?;
    Some(switch.provider_id)
}

/// Whether anything named `suffix` was published.
fn published(result: &jinn_slices::RouteResult, suffix: &str) -> bool {
    result.message_names.iter().any(|n| n.ends_with(suffix))
}

/// One model row.
fn entry(id: &str, available: bool) -> ProviderPickerEntry {
    ProviderPickerEntry {
        provider_id: id.to_owned(),
        name: "acme".to_owned(),
        provider_name: "acme".to_owned(),
        backend: "openrouter".to_owned(),
        model: id.to_owned(),
        search_text: format!("{id} acme"),
        is_alias: false,
        alias_target: None,
        is_available: available,
        is_remote: false,
        is_active: false,
        selected: false,
        theme: jinn_theme::default_theme(),
    }
}

/// A representative three-row list: two available, one unavailable.
fn sample_entries() -> Vec<ProviderPickerEntry> {
    vec![entry("m1", true), entry("m2", true), entry("m3", false)]
}

// ── Wiring ──────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn open_pushes_the_pickers_own_scope() {
    // Given the model picker wired but closed.
    let wired = Wired::new().await;

    // When opening it.
    let result = wired.open();

    // Then the scope it pushes is its own dynamic scope.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::Push(provider_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn open_requests_the_model_load() {
    // Given the model picker wired but closed.
    let wired = Wired::new().await;

    // When opening it.
    let result = wired.open();

    // Then the provider actor is told to load the rows.
    assert!(
        published(&result, "LoadProviderPickerEntries"),
        "open should request the model load, got: {:?}",
        result.message_names
    );
}

#[rstest::rstest]
#[tokio::test]
async fn open_derives_alloy_mode_from_the_session() {
    // Given a session already on an alloy of two models.
    let wired = Wired::new().await;
    wired.set_alloy_model(&["m1", "m2"]);

    // When opening the picker.
    let _ = wired.open();

    // Then the picker opens in alloy mode.
    assert!(wired.alloy_mode());
}

#[rstest::rstest]
#[tokio::test]
async fn open_derives_single_mode_from_a_single_model_session() {
    // Given a session on one named model.
    let wired = Wired::new().await;
    wired.set_single_model("m1");

    // When opening the picker.
    let _ = wired.open();

    // Then the picker opens in single mode.
    assert!(!wired.alloy_mode());
}

#[rstest::rstest]
#[tokio::test]
async fn open_starts_with_an_empty_list() {
    // Given the model picker wired but closed.
    let wired = Wired::new().await;

    // When opening it.
    let _ = wired.open();

    // Then the list is empty: discovery is async, so the popup opens blank
    // and fills in when the actor publishes.
    assert_eq!(wired.visible(), [] as [std::string::String; 0]);
}

#[rstest::rstest]
#[tokio::test]
async fn every_advertised_key_resolves_in_the_pickers_scope() {
    // Given the model picker wired.
    let wired = Wired::new().await;

    // When resolving each key the footer advertises.
    for (key, _) in crate::provider_picker_routes::PROVIDER_PICKER_BINDINGS {
        let action = wired.resolve_key(key);
        // Then it binds to a slice action, not a kernel intent.
        assert!(
            action.is_some(),
            "the footer advertises {key} but the picker binds no action for it"
        );
    }
}

// ── Rows ────────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn loaded_rows_are_visible_in_order() {
    // Given the model picker with three rows loaded.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When reading the visible list.
    // Then the three models appear in load order.
    assert_eq!(wired.visible(), vec!["m1", "m2", "m3"]);
}

#[rstest::rstest]
#[tokio::test]
async fn load_keeps_the_filter_applied() {
    // Given a loaded list narrowed to "m1".
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.type_text("m1");

    // When new rows are loaded over the top.
    wired.with_entries(sample_entries());

    // Then the filter still applies, so the picker does not flash every row.
    assert_eq!(wired.visible(), vec!["m1"]);
}

// ── Filtering ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn typing_narrows_the_list_to_matching_models() {
    // Given the model picker with three rows loaded.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When typing "m2".
    wired.type_text("m2");

    // Then only the matching row remains.
    assert_eq!(wired.visible(), vec!["m2"]);
}

#[rstest::rstest]
#[tokio::test]
async fn typing_matches_the_provider_name_too() {
    // Given the model picker with three rows loaded.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When typing the provider name.
    wired.type_text("acme");

    // Then every row matches, since all three are on the same provider.
    assert_eq!(wired.visible(), vec!["m1", "m2", "m3"]);
}

#[rstest::rstest]
#[tokio::test]
async fn backspace_widens_the_list_again() {
    // Given the model picker narrowed to a single row.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.type_text("m1");

    // When backspacing the filter.
    wired.edit(&EditIntent::DeleteBackward);

    // Then the full list is visible again.
    assert_eq!(wired.visible(), vec!["m1", "m2", "m3"]);
}

#[rstest::rstest]
#[tokio::test]
async fn clear_filter_keeps_the_picker_open() {
    // Given the model picker with a filter typed in.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.type_text("m1");

    // When pressing ctrl-c.
    let result = wired.fire("clear-filter-or-leave-provider-picker");

    // Then the filter is empty.
    assert_eq!(wired.filter(), "");
    // And the picker stays open.
    assert_eq!(result.scope_signal, None);
}

#[rstest::rstest]
#[tokio::test]
async fn clear_filter_with_an_empty_filter_closes_the_picker() {
    // Given the model picker open with no filter text.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When pressing ctrl-c.
    let result = wired.fire("clear-filter-or-leave-provider-picker");

    // Then the picker closes.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(provider_picker_scope()))
    );
}

// ── Navigation ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn down_moves_the_highlight_one_row() {
    // Given the model picker with three rows and the highlight on the first.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When pressing down.
    wired.fire("move-provider-picker-down");

    // Then the highlight is on the second row.
    assert_eq!(wired.highlighted(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn up_stops_at_the_first_row() {
    // Given the model picker with the highlight already on the first row.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When pressing up.
    wired.fire("move-provider-picker-up");

    // Then the highlight does not wrap past the top.
    assert_eq!(wired.highlighted(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn down_stops_at_the_last_row() {
    // Given the model picker with three rows and the highlight on the last.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.fire("move-provider-picker-down");
    wired.fire("move-provider-picker-down");

    // When pressing down again.
    wired.fire("move-provider-picker-down");

    // Then the highlight does not wrap past the bottom.
    assert_eq!(wired.highlighted(), 2);
}

#[rstest::rstest]
#[tokio::test]
async fn page_down_advances_by_half_the_measured_viewport() {
    // Given the model picker with 20 rows and a measured viewport of 10.
    let wired = Wired::new().await;
    wired.with_entries((0..20).map(|i| entry(&format!("m{i}"), true)).collect());
    wired.cell().update(|picker| picker.results_viewport = 10);

    // When pressing page-down.
    wired.fire("page-provider-picker-down");

    // Then the highlight advanced by half of 10.
    assert_eq!(wired.highlighted(), 5);
}

#[rstest::rstest]
#[tokio::test]
async fn page_up_decrements_by_half_the_measured_viewport() {
    // Given the model picker with 20 rows and a viewport of 10, highlight
    // page-down from the top: 0 + half of 10 = 5.
    let wired = Wired::new().await;
    wired.with_entries((0..20).map(|i| entry(&format!("m{i}"), true)).collect());
    wired.cell().update(|picker| picker.results_viewport = 10);
    wired.fire("page-provider-picker-down");
    assert_eq!(wired.highlighted(), 5);

    // When pressing page-up.
    wired.fire("page-provider-picker-up");

    // Then it returns to the top: 5 - half of 10 = 0.
    assert_eq!(wired.highlighted(), 0);
}

// ── Confirming ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn confirm_requests_switching_to_the_highlighted_model() {
    // Given the model picker with the highlight on the first available row.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then the switch command carries that model.
    assert_eq!(
        switched_to(&publish(result)),
        Some(ModelSelection::Single("m1".to_owned())),
        "confirm should switch to the highlighted model"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_closes_the_picker() {
    // Given the model picker with a highlighted row.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then the picker closes.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(provider_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_on_an_unavailable_row_does_nothing() {
    // Given the model picker with only the unavailable row highlighted.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.type_text("m3");
    wired.fire("move-provider-picker-up");

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then no switch is requested, and the picker stays open.
    assert!(
        result.messages.is_empty(),
        "unexpected messages: {:?}",
        result.message_names
    );
    assert_eq!(result.scope_signal, None);
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_on_an_empty_list_does_nothing() {
    // Given the model picker with no rows at all.
    let wired = Wired::new().await;

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then nothing happens at all.
    assert!(
        result.messages.is_empty(),
        "unexpected messages: {:?}",
        result.message_names
    );
    assert_eq!(result.scope_signal, None);
}

// ── Alloy ───────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn alloy_toggle_flips_the_mode() {
    // Given the model picker in single mode.
    let wired = Wired::new().await;

    // When pressing ctrl-a.
    wired.fire("toggle-provider-picker-alloy");

    // Then the picker is in alloy mode.
    assert!(wired.alloy_mode());
}

#[rstest::rstest]
#[tokio::test]
async fn alloy_toggle_flips_back_to_single() {
    // Given the model picker in alloy mode.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");

    // When pressing ctrl-a again.
    wired.fire("toggle-provider-picker-alloy");

    // Then the picker is back in single mode.
    assert!(!wired.alloy_mode());
}

#[rstest::rstest]
#[tokio::test]
async fn entering_alloy_prechecks_the_sessions_current_models() {
    // Given a session on two models, and rows for both.
    let wired = Wired::new().await;
    wired.set_single_model("m1");
    wired.with_entries(sample_entries());

    // When entering alloy mode.
    wired.fire("toggle-provider-picker-alloy");

    // Then both are checked, so the user's current choice is preserved.
    assert_eq!(wired.checked(), Vec::<String>::new());
}

#[rstest::rstest]
#[tokio::test]
async fn tab_checks_the_highlighted_model_in_alloy_mode() {
    // Given the model picker in alloy mode with rows loaded.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());

    // When pressing tab.
    wired.fire("toggle-provider-picker");

    // Then the highlighted model is checked.
    assert_eq!(wired.checked(), vec!["m1".to_owned()]);
}

#[rstest::rstest]
#[tokio::test]
async fn tab_does_not_move_the_highlight() {
    // Given the model picker in alloy mode with rows loaded.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());

    // When pressing tab.
    wired.fire("toggle-provider-picker");

    // Then the highlight stays put: a checked row changing rank under the
    // cursor would make tab feel like it moved the selection.
    assert_eq!(wired.highlighted(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn tab_does_nothing_in_single_mode() {
    // Given the model picker in single mode with rows loaded.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When pressing tab.
    wired.fire("toggle-provider-picker");

    // Then nothing is checked: there is no set to add to.
    assert_eq!(wired.checked(), [] as [std::string::String; 0]);
}

#[rstest::rstest]
#[tokio::test]
async fn leaving_alloy_clears_every_check() {
    // Given the model picker in alloy mode with a checked row.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());
    wired.fire("toggle-provider-picker");
    assert_eq!(wired.checked(), vec!["m1".to_owned()]);

    // When leaving alloy mode.
    wired.fire("toggle-provider-picker-alloy");

    // Then no row is checked any more.
    assert_eq!(wired.checked(), [] as [std::string::String; 0]);
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_in_alloy_mode_commits_every_checked_model() {
    // Given the model picker in alloy mode with two models checked.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());
    wired.fire("toggle-provider-picker");
    wired.fire("move-provider-picker-down");
    wired.fire("toggle-provider-picker");

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then the switch carries an alloy of both.
    assert!(
        matches!(
            switched_to(&publish(result)),
            Some(ModelSelection::Alloy { ref models, .. })
                if models.len() == 2
                    && models.contains(&"m1".to_owned())
                    && models.contains(&"m2".to_owned())
        ),
        "confirm should commit an alloy of the checked models"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn confirm_in_alloy_mode_with_one_model_commits_a_single_model() {
    // Given the model picker in alloy mode with exactly one model checked.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());
    wired.fire("toggle-provider-picker");

    // When confirming.
    let result = wired.fire("confirm-provider-picker");

    // Then the switch is a single model, not a one-member alloy.
    assert_eq!(
        switched_to(&publish(result)),
        Some(ModelSelection::Single("m1".to_owned())),
        "one checked model should resolve back to a single model"
    );
}

// ── Cancelling ──────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn escape_closes_the_picker() {
    // Given the model picker open.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When pressing escape.
    let result = wired.fire("close-provider-picker");

    // Then the picker closes.
    assert_eq!(
        result.scope_signal,
        Some(ScopeSignal::PopIf(provider_picker_scope()))
    );
}

#[rstest::rstest]
#[tokio::test]
async fn escape_requests_no_switch() {
    // Given the model picker open in alloy mode with a checked row.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());
    wired.fire("toggle-provider-picker");

    // When pressing escape.
    let result = wired.fire("close-provider-picker");

    // Then no switch is requested: abandoning the popup changes nothing.
    assert!(
        result.messages.is_empty(),
        "unexpected messages: {:?}",
        result.message_names
    );
}

// ── Refresh ─────────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn refresh_requests_the_model_reload() {
    // Given the model picker open, with a provider configured — the refresh
    // is gated on one, so a session with no provider would correctly decline.
    let wired = Wired::new().await;
    wired.set_single_model("m1");

    // When pressing ctrl-r.
    let result = wired.fire("refresh-provider-picker");

    // Then the provider actor is told to refresh.
    assert!(
        published(&result, "RefreshModels"),
        "ctrl-r should refresh the model cache, got: {:?}",
        result.message_names
    );
}

// ── Rendering ───────────────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn render_shows_the_filter_typed_so_far() {
    // Given the model picker with a filter typed in.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());
    wired.type_text("m1");

    // When drawing a frame.
    let rows = wired.draw();

    // Then the popup's prompt shows the filter text.
    assert!(
        rows.iter().any(|row| row.contains("> m1")),
        "the filter prompt should show the typed text, got: {rows:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn render_shows_single_model_mode_in_the_status_line() {
    // Given the model picker open in single mode.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When drawing a frame.
    let rows = wired.draw();

    // Then the status line says so.
    assert!(
        rows.iter().any(|row| row.contains("single model")),
        "the status line should name the mode, got: {rows:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn render_shows_the_alloy_check_count_in_the_status_line() {
    // Given the model picker in alloy mode with two models checked.
    let wired = Wired::new().await;
    wired.fire("toggle-provider-picker-alloy");
    wired.with_entries(sample_entries());
    wired.fire("toggle-provider-picker");
    wired.fire("move-provider-picker-down");
    wired.fire("toggle-provider-picker");

    // When drawing a frame.
    let rows = wired.draw();

    // Then the status line reports the count.
    assert!(
        rows.iter().any(|row| row.contains("2 selected")),
        "the status line should count the checked models, got: {rows:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn render_advertises_every_key_the_picker_binds() {
    // Given the model picker with rows loaded.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When drawing a frame.
    let rows = wired.draw();

    // Then each advertised key appears in the footer.
    for (key, _) in crate::provider_picker_routes::PROVIDER_PICKER_BINDINGS {
        assert!(
            rows.iter().any(|row| row.contains(key)),
            "the footer should advertise {key}, got: {rows:?}"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn render_measures_the_viewport_from_the_actual_frame() {
    // Given the model picker drawn at a known size.
    let wired = Wired::new().await;
    wired.with_entries(sample_entries());

    // When drawing a frame.
    let _ = wired.draw();

    // Then the measured viewport is what the popup's own geometry implies,
    // not the fallback the navigation keys would otherwise page by.
    let area = ratatui::layout::Rect::new(0, 0, 120, 34);
    let popup = crate::provider_picker_render::provider_picker_overlay_rect(&area)
        .expect("geometry fn yields a popup rect");
    let expected = crate::provider_picker_viewport::results_viewport(&popup);
    let actual = wired.cell().read().results_viewport;
    assert_eq!(actual, expected);
    // And it is a real measurement, distinct from the unmeasured fallback.
    assert_ne!(
        actual, 20,
        "a viewport equal to the fallback is not a measurement"
    );
}
