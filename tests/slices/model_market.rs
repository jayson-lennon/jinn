//! Model-market slice composition tests.
//!
//! These drive the slice exactly as composition wires it — the real
//! catalog-style registration, the real `activate`, the real route rows
//! and input hook — so a wiring defect fails here instead of at a user's
//! keypress. The actors are NOT spawned (they belong to T2/T3): every
//! test seeds the data cell by hand where the table needs rows.
#![allow(
    clippy::items_after_statements,
    clippy::string_slice,
    reason = "integration test: helper structs read best beside their use; slices at fixture-verified offsets"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use jinn_kernel::IntentHandler;
use jinn_kernel::KernelIntent;
use jinn_model_market::market_ui_scope;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::SortColumn;
use jinn_slices::DynamicIntent;
use jinn_slices::KeyRoutes;
use jinn_slices::PublishSink;
use jinn_slices::RouteResult;
use jinn_slices::Slices;
use jinn_slices::cell::TypedCell;
use jinn_slices::empty_config_layer;
use jinn_slices::route::BindSite;
use ratatui::buffer::Cell;

/// The slice wired exactly as composition wires it: catalog-seeded
/// cells, real activation, real rows and hook.
struct Wired {
    slices: Slices,
    routes: KeyRoutes,
    state: jinn_kernel::AppState,
}

impl Wired {
    /// Builds the slice with the model-market popup registered.
    fn new() -> Self {
        let slices = Slices::new();
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let routes = KeyRoutes::new();
        // The actors are T2/T3 territory; an empty system is fine —
        // no test here publishes to an actor.
        let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let mut host =
            jinn_slices::SliceHost::new(&slices, &mut viewport, &overlay_views, &routes, &system);
        // The cell catalog's role: production registers every slot before
        // any slice activates, so the harness does the same — including
        // the scope-focus slot the state's scope stack writes through.
        host.register_cell(
            jinn_model_market::market_data_slot(),
            MarketDataState::default(),
        )
        .expect("a fresh host never has the data slot registered");
        host.register_cell(
            jinn_model_market::market_ui_slot(),
            MarketUiState::default(),
        )
        .expect("a fresh host never has the UI slot registered");
        host.register_cell(
            jinn_slices::scope_focus_slot(),
            jinn_slices::ScopeFocusState::default(),
        )
        .expect("a fresh host never has the scope-focus slot registered");
        jinn_model_market::activate(&mut host);
        let state = {
            let state = jinn_kernel::AppState::default();
            state.frontend.attach_slices(slices.clone());
            state
        };
        Self {
            slices,
            routes,
            state,
        }
    }

    /// The UI cell handle.
    fn ui(&self) -> TypedCell<MarketUiState> {
        self.slices
            .reader(&jinn_model_market::market_ui_slot())
            .expect("the registration in `new` minted the UI cell")
    }

    /// The data cell handle.
    fn data(&self) -> TypedCell<MarketDataState> {
        self.slices
            .reader(&jinn_model_market::market_data_slot())
            .expect("the registration in `new` minted the data cell")
    }

    /// Dispatches a dynamic intent through the real route table — the
    /// handler path production dispatch uses.
    fn handle(&mut self, intent: DynamicIntent) -> RouteResult {
        IntentHandler::handle(
            &KernelIntent::Dynamic(intent),
            &mut self.state,
            &self.slices,
            &self.routes,
            empty_config_layer(),
        )
    }

    /// Presses a bound popup key: mints the dynamic intent the keymap
    /// would mint for the row's action, then dispatches it. The popup's
    /// scope must be focused first (the opener pushes it, or the test
    /// focuses it directly) — the hook consult resolves by focus.
    fn press(&mut self, action: &'static str, display: &'static str) -> RouteResult {
        self.handle(DynamicIntent::new(market_ui_scope(), action, display))
    }
}

/// One priced listing row for a test table.
fn row(
    id: &str,
    author: &str,
    slug: &str,
    prompt: f64,
    completion: f64,
    cache: Option<f64>,
) -> MarketRow {
    MarketRow {
        id: id.to_owned(),
        author: author.to_owned(),
        slug: slug.to_owned(),
        variant: None,
        prompt_price: prompt,
        completion_price: completion,
        cache_read_price: cache,
        has_overrides: false,
        context_length: Some(128_000),
        eci: None,
        eci_ci: None,
        vals: None,
        is_free: false,
    }
}

/// A two-row table: cheap-then-expensive in prompt price.
fn seeded(wired: &mut Wired) {
    wired.data().update(|d| {
        d.rows = vec![
            row(
                "openrouter/a/small",
                "a",
                "small",
                0.000_001,
                0.000_002,
                Some(0.000_000_1),
            ),
            row("openrouter/b/big", "b", "big", 0.000_003, 0.000_006, None),
        ];
    });
}

/// Decodes the first published message of type `M` out of a route
/// result, draining the closures with a payload-collecting sink — the
/// Decodes every published message out of a route result as
/// `(type-suffix, payload)` pairs, draining the closures with a
/// payload-collecting sink — one pass reads every message, so tests
/// asserting several payloads drain once and filter per type. Publish
/// closures are `FnOnce`, so the result is consumed.
fn drain_payloads(result: &mut RouteResult) -> Vec<(&'static str, serde_json::Value)> {
    let mut found: Vec<(&'static str, serde_json::Value)> = Vec::new();
    for closure in std::mem::take(&mut result.messages) {
        let cell: std::cell::RefCell<Option<(&'static str, serde_json::Value)>> =
            std::cell::RefCell::new(None);
        struct Collect<'a> {
            found: &'a std::cell::RefCell<Option<(&'static str, serde_json::Value)>>,
        }
        impl PublishSink for Collect<'_> {
            fn publish_schema(
                &self,
                _schema_id: trouper::schema::SchemaId,
                payload: serde_json::Value,
                name: &'static str,
            ) {
                let short = name.rsplit("::").next().unwrap_or(name);
                *self.found.borrow_mut() = Some((short, payload));
            }
        }
        closure(&Collect { found: &cell });
        if let Some(pair) = cell.into_inner() {
            found.push(pair);
        }
    }
    found
}

/// Decodes the first published message of type `M` out of a route
/// result — the typed twin of the `message_names` assertions.
fn first_message<M>(result: &mut RouteResult) -> Option<M>
where
    M: jinn_slices::BusMessage
        + trouper::schema::Schema
        + serde::Serialize
        + serde::de::DeserializeOwned
        + Clone
        + Send
        + Sync
        + trouper::envelope::PayloadValue,
{
    let want = std::any::type_name::<M>().rsplit("::").next().unwrap_or("");
    let payloads = drain_payloads(result);
    payloads
        .iter()
        .find(|(name, _)| *name == want)
        .and_then(|(_, payload)| serde_json::from_value::<M>(payload.clone()).ok())
}

/// The editing intent the composition keymap's hook catch-all mints for
/// a printable character in the popup's scope.
fn edit_intent(action: &'static str, ch: char) -> DynamicIntent {
    DynamicIntent::with_bytes(
        market_ui_scope(),
        action,
        action,
        ch.to_string().into_bytes(),
    )
}

/// The dynamic intent a bracketed paste travels as — the shape the TUI
/// mints and the kernel's paste branch routes by focus.
fn paste_intent(text: &str) -> DynamicIntent {
    DynamicIntent::with_bytes(
        market_ui_scope(),
        jinn_chat_input_msg::PASTE_TEXT_ACTION,
        "paste text",
        text.as_bytes().to_vec(),
    )
}

// ── Wiring ──────────────────────────────────────────────────────────────

#[rstest::rstest]
fn cells_resolve_through_real_activation() {
    // Given the slice wired over a registry seeded the way the catalog
    // seeds it.
    let wired = Wired::new();

    // When resolving both cells.
    let ui = wired.ui();
    let data = wired.data();

    // Then both hold usable defaults.
    assert_eq!(ui.read().sort_col, SortColumn::Proj);
    assert_eq!(
        data.read().rows,
        [] as [jinn_model_market_msg::MarketRow; 0]
    );
}

#[rstest::rstest]
fn every_advertised_key_binds_in_the_popup_scope() {
    // Given the wired slice's route rows, its registered hook scope,
    // and the footer's advertised bindings.
    let wired = Wired::new();
    let bound: Vec<&str> = wired
        .routes
        .rows()
        .iter()
        .filter(|r| r.feature == "model-market")
        .map(|r| r.key)
        .collect();
    let hook_owned = wired
        .routes
        .input_hook_scopes()
        .contains(&market_ui_scope());

    // When every advertised key is checked.
    for (key, _) in jinn_model_market::routes::MARKET_BINDINGS {
        // Non-character keys must be route rows.
        if !is_char_key(key) {
            // Then a route row binds the key.
            assert!(
                bound.contains(key),
                "footer advertises {key} but no route row binds it"
            );
            continue;
        }
        // Character keys must be hook cases: a char key cannot be a
        // row in an input-hook scope, or route dispatch would steal it
        // from filter typing.
        // And the popup scope owns the input hook that claims them.
        assert!(
            hook_owned,
            "footer advertises {key} but the popup scope has no input hook"
        );
    }
}

/// Whether a footer key is a single printable character (no `<...>`
/// wrapper, no modifier).
fn is_char_key(key: &str) -> bool {
    key.len() == 1 && key.chars().next().is_some_and(|c| !c.is_control())
}

#[rstest::rstest]
fn every_market_char_key_dispatches_through_the_composition_keymap() {
    // Given the composed slice's keymap state at the popup's dynamic
    // scope — the resolution path production dispatch uses.
    let wired = Wired::new();
    let mut keymap = jinn_tui::keymap::init();
    jinn_tui::keymap_gen::bind_route_rows(&wired.routes, &mut keymap);
    let mut state =
        ratatui_which_key::WhichKeyState::new(keymap, jinn_tui::Scope::Dynamic(market_ui_scope()));

    // When every advertised character key is pressed.
    for (key, _) in jinn_model_market::routes::MARKET_BINDINGS {
        if !is_char_key(key) {
            continue;
        }
        let ch = key.chars().next().unwrap_or_default();
        let intent = state.handle_key(key_event_char(ch));

        // Then it resolves — to the insert-char edit intent the hook
        // consult consumes, never an unbound drop.
        let Some(jinn_kernel::KernelIntent::Dynamic(dynamic)) = intent else {
            panic!("footer char key {key} resolved to no intent in the popup scope");
        };
        assert_eq!(
            dynamic.action, "insert-char",
            "char key {key} must dispatch as an insert-char edit intent"
        );
        assert_eq!(
            dynamic.slice,
            market_ui_scope(),
            "the insert-char intent must target the popup's scope"
        );
    }
}

#[rstest::rstest]
fn every_market_non_char_key_resolves_to_its_row() {
    // Given the same composed keymap state at the popup's scope.
    let wired = Wired::new();
    let mut keymap = jinn_tui::keymap::init();
    jinn_tui::keymap_gen::bind_route_rows(&wired.routes, &mut keymap);
    let mut state =
        ratatui_which_key::WhichKeyState::new(keymap, jinn_tui::Scope::Dynamic(market_ui_scope()));

    // When every advertised non-character key is pressed.
    for (key, _) in jinn_model_market::routes::MARKET_BINDINGS {
        if is_char_key(key) {
            continue;
        }
        let intent = state.handle_key(key_event_notation(key));

        // Then it resolves to the dynamic intent its route row names.
        let Some(jinn_kernel::KernelIntent::Dynamic(dynamic)) = intent else {
            panic!("footer key {key} resolved to no intent in the popup scope");
        };
        assert_eq!(
            dynamic.slice,
            market_ui_scope(),
            "key {key} must dispatch within the popup's scope"
        );
    }
}

/// A bare character key event.
fn key_event_char(c: char) -> jinn_kernel::KeyEvent {
    jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Char(c),
        modifiers: jinn_kernel::Modifiers::none(),
    }
}

/// Parses a footer notation (`<enter>`, `<pgdn>`) into a key event.
#[expect(
    clippy::expect_used,
    reason = "a footer notation that fails to parse is a table bug this test names"
)]
fn key_event_notation(notation: &str) -> jinn_kernel::KeyEvent {
    let events = ratatui_which_key::parse_key_sequence::<jinn_kernel::KeyEvent>(
        notation,
        &key_event_char('\\'),
    );
    events
        .into_iter()
        .next()
        .expect("a single-key notation yields one event")
}

#[rstest::rstest]
fn j_and_k_move_the_highlight() {
    // Given a seeded table, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When `j` arrives as the insert-char intent the keymap's catch-all
    // mints (route rows cannot own char keys in a hook scope).
    wired.handle(edit_intent("insert-char", 'j'));

    // Then the highlight moved down.
    assert_eq!(wired.ui().read().selection, 1);
    // And `k` moved it back up.
    wired.handle(edit_intent("insert-char", 'k'));
    assert_eq!(wired.ui().read().selection, 0);
    // And `k` at the top clamps.
    wired.handle(edit_intent("insert-char", 'k'));
    assert_eq!(wired.ui().read().selection, 0);
}

#[rstest::rstest]
fn h_l_s_while_the_filter_is_focused_type_instead_of_firing() {
    // Given an open popup with the filter focused, PROJ ascending.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| s.filter_focused = true);

    // When "hls" is typed.
    for ch in ['h', 'l', 's'] {
        wired.handle(edit_intent("insert-char", ch));
    }

    // Then every letter landed in the filter text.
    assert_eq!(wired.ui().read().filter, "hls");
    // And no sort action fired: still PROJ, ascending.
    assert_eq!(wired.ui().read().sort_col, SortColumn::Proj);
    assert!(!wired.ui().read().sort_desc);
    // And the filter is still focused.
    assert!(wired.ui().read().filter_focused);
}

#[rstest::rstest]
fn h_l_s_fire_when_the_table_is_focused() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `l`, `h`, and `s` each arrive.
    wired.handle(edit_intent("insert-char", 'l'));
    wired.handle(edit_intent("insert-char", 'h'));
    wired.handle(edit_intent("insert-char", 's'));

    // Then the sort is still PROJ (l then h round-trips) but the
    // direction flipped.
    assert_eq!(wired.ui().read().sort_col, SortColumn::Proj);
    assert!(wired.ui().read().sort_desc);
    // And none of the letters typed into the filter.
    assert_eq!(wired.ui().read().filter, "");
}

#[rstest::rstest]
fn m_does_nothing_when_the_table_is_focused() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    let before = wired.ui().read().clone();

    // When `m` arrives.
    wired.handle(edit_intent("insert-char", 'm'));

    // Then nothing changed: the key has no action in the popup.
    assert_eq!(wired.ui().read().filter, before.filter);
    assert_eq!(wired.ui().read().selection, before.selection);
    assert_eq!(wired.ui().read().sort_col, before.sort_col);
    assert!(!wired.ui().read().filter_focused);
}

#[rstest::rstest]
fn the_opener_binds_in_the_normal_scope() {
    // Given the wired slice's rows.
    let wired = Wired::new();
    let rows = wired.routes.rows();

    // When the opener row is looked up.
    let opener = rows
        .iter()
        .find(|r| r.route_id.as_str() == "model-market:open")
        .expect("the slice attaches its opener row");

    // Then it binds in the Normal static scope, not its own — a key
    // that opens a scope cannot live inside it.
    assert_eq!(opener.key, "<leader>so");
    assert_eq!(opener.site, BindSite::StaticScopes(&["Normal"]));
}

// ── Open + ESC semantics ────────────────────────────────────────────────

#[rstest::rstest]
fn open_resets_ui_state_and_pushes_the_scope() {
    // Given UI state carried over from a previous open.
    let mut wired = Wired::new();
    wired.ui().update(|s| {
        s.filter = "sonnet".to_owned();
        s.filter_focused = true;
        s.sort_col = SortColumn::Eci;
        s.sort_desc = true;
    });

    // When the opener fires.
    let result = wired.press("open-model-market", "search openrouter models");

    // Then the UI cell is back to the opener defaults: table focused,
    // filter empty, PROJ ascending.
    let ui = wired.ui();
    assert_eq!(ui.read().filter, "");
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().sort_col, SortColumn::Proj);
    assert!(!ui.read().sort_desc);
    // And the handler applied the push (the signal is consumed by
    // `apply_scope_signal`, so the stack is the observable) and the
    // fetch was requested.
    assert_eq!(
        wired.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(market_ui_scope())
    );
    assert!(result.message_names.contains(&"FetchModels"));
}

#[rstest::rstest]
fn esc_in_table_mode_pops_the_scope() {
    // Given an open popup with the table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When ESC fires with the filter unfocused.
    wired.press("close-model-market", "close");

    // Then the scope popped (the signal is consumed by the handler).
    assert_eq!(wired.state.frontend.scope(), jinn_slices::FocusScope::Input);
}

#[rstest::rstest]
fn esc_in_filter_mode_clears_the_filter_and_returns_focus_to_the_table() {
    // Given an open popup with the filter focused and text typed.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| {
        s.filter_focused = true;
        s.filter = "son".to_owned();
        s.selection = 1;
    });

    // When ESC fires.
    wired.press("close-model-market", "close");

    // Then the filter emptied, the selection reset, and focus handed
    // back — the popup stays open.
    let ui = wired.ui();
    assert_eq!(ui.read().filter, "");
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().selection, 0);
    assert_eq!(
        wired.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(market_ui_scope())
    );
}

#[rstest::rstest]
fn esc_clearing_a_focused_filter_publishes_nothing() {
    // Given an open popup with the filter focused, "small" narrowing
    // the seeded table to one row.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| {
        s.filter_focused = true;
        s.filter = "small".to_owned();
    });

    // When ESC fires.
    let result = wired.press("close-model-market", "close");

    // Then the filter cleared, focus handed back, the selection reset
    // to the widened set's top — and nothing published.
    let ui = wired.ui();
    assert_eq!(ui.read().filter, "");
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().selection, 0);
    assert!(result.messages.is_empty());
}

// ── Filter focus (the hook) ─────────────────────────────────────────────

#[rstest::rstest]
fn f_focuses_the_filter_from_the_table() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `f` arrives as the editing intent the keymap's catch-all
    // mints.
    wired.handle(edit_intent("insert-char", 'f'));

    // Then the filter is focused and the character was not inserted.
    let ui = wired.ui();
    assert!(ui.read().filter_focused);
    assert_eq!(ui.read().filter, "");
}

#[rstest::rstest]
fn i_also_focuses_the_filter_from_the_table() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `i` arrives.
    wired.handle(edit_intent("insert-char", 'i'));

    // Then the filter is focused.
    assert!(wired.ui().read().filter_focused);
}

#[rstest::rstest]
fn typing_while_filter_focused_inserts_into_the_filter() {
    // Given an open popup with the filter focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| s.filter_focused = true);

    // When a character arrives.
    wired.handle(edit_intent("insert-char", 's'));

    // Then it landed in the filter text, and the selection reset to the
    // top of the narrowed list.
    let ui = wired.ui();
    assert_eq!(ui.read().filter, "s");
    assert_eq!(ui.read().selection, 0);
}

#[rstest::rstest]
fn f_types_into_a_focused_filter_instead_of_refocusing() {
    // Given an open popup with the filter focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| s.filter_focused = true);

    // When `f` arrives.
    wired.handle(edit_intent("insert-char", 'f'));

    // Then the character was inserted — the focus flip did not re-fire.
    assert_eq!(wired.ui().read().filter, "f");
}

#[rstest::rstest]
fn unclaimed_characters_fall_through_the_hook() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When an unclaimed character arrives (`z`).
    wired.handle(edit_intent("insert-char", 'z'));

    // Then nothing changed: the hook did not claim it.
    let ui = wired.ui();
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().filter, "");
}

#[rstest::rstest]
fn backspace_edits_a_focused_filter() {
    // Given a focused filter holding "son".
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| {
        s.filter_focused = true;
        s.filter = "son".to_owned();
    });

    // When delete-backward arrives.
    wired.handle(edit_intent("delete-backward", '\u{8}'));

    // Then the filter shrank by the one character.
    assert_eq!(wired.ui().read().filter, "so");
}

#[rstest::rstest]
fn paste_into_a_focused_filter_appends_the_text() {
    // Given an open popup with the filter focused and holding "so".
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| {
        s.filter_focused = true;
        s.filter = "so".to_owned();
    });

    // When a bracketed paste arrives — the intent the kernel's paste
    // branch routes to the focused scope's input hook.
    wired.handle(paste_intent("nnet"));

    // Then the pasted text landed in the filter and the selection reset.
    let ui = wired.ui();
    assert_eq!(ui.read().filter, "sonnet");
    assert_eq!(ui.read().selection, 0);
}

#[rstest::rstest]
fn paste_while_the_table_is_focused_is_declined() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When a bracketed paste arrives.
    wired.handle(paste_intent("sonnet"));

    // Then the hook declined it: the filter is untouched and stays
    // unfocused — the text belongs to the next surface, not the table.
    let ui = wired.ui();
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().filter, "");
}

// ── ENTER: drill-in + detail confirm ────────────────────────────────────

#[rstest::rstest]
fn enter_from_table_mode_opens_detail_and_publishes_one_fetch() {
    // Given a seeded table with the first row highlighted, no detail
    // loaded.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When ENTER fires.
    let mut result = wired.press("confirm-model-market", "switch/pin");

    // Then the UI cell is pinned to the highlighted row, the scope is
    // still pushed (the popup stays open), and the only published
    // message is the row's endpoint fetch.
    let ui = wired.ui();
    assert_eq!(
        ui.read().detail_model,
        Some("openrouter/a/small".to_owned()),
        "the mode pins the highlighted row"
    );
    assert_eq!(
        wired.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(market_ui_scope()),
        "the popup stays open"
    );
    assert_eq!(result.message_names, vec!["FetchEndpoints"]);
    let fetches = first_message::<jinn_model_market_msg::FetchEndpoints>(&mut result)
        .expect("the drill-in fetches the pinned model");
    assert_eq!(fetches.model_id, "openrouter/a/small");
    assert_eq!(fetches.seq, wired.ui().read().fetch_seq);
}

#[rstest::rstest]
fn enter_into_a_cached_detail_publishes_nothing() {
    // Given a seeded table whose highlighted row's detail is already in
    // the data cell.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.data().update(|d| {
        d.detail = Some(jinn_model_market_msg::MarketDetail {
            model_id: "openrouter/a/small".to_owned(),
            endpoints: vec![],
        });
    });

    // When ENTER fires.
    let result = wired.press("confirm-model-market", "switch/pin");

    // Then the mode flips with zero messages — the cached detail
    // renders instantly.
    assert_eq!(
        wired.ui().read().detail_model,
        Some("openrouter/a/small".to_owned())
    );
    assert!(result.messages.is_empty());
}

/// A detail-mode fixture: detail for `openrouter/a/small` with two
/// endpoints — one tagged, one with an empty tag — distinct prices, a
/// missing cache price, and absent percentiles on the tagged one.
fn detail_fixture(wired: &mut Wired) {
    wired.data().update(|d| {
        d.detail = Some(jinn_model_market_msg::MarketDetail {
            model_id: "openrouter/a/small".to_owned(),
            endpoints: vec![
                jinn_model_market_msg::MarketEndpointRow {
                    tag: "azure/us-east".to_owned(),
                    provider_name: "Azure".to_owned(),
                    prompt_price: 0.000_002,
                    completion_price: 0.000_004,
                    cache_read_price: None,
                    latency_p50_ms: None,
                    throughput_p50: None,
                    uptime_1d: Some(0.99),
                    quantization: None,
                    overrides: Vec::new(),
                },
                jinn_model_market_msg::MarketEndpointRow {
                    tag: String::new(),
                    provider_name: "Lambda".to_owned(),
                    prompt_price: 0.000_000_5,
                    completion_price: 0.000_001,
                    cache_read_price: Some(0.000_000_05),
                    latency_p50_ms: Some(300.0),
                    throughput_p50: Some(90.0),
                    uptime_1d: Some(0.98),
                    quantization: None,
                    overrides: Vec::new(),
                },
            ],
        });
    });
}

/// Puts the wired popup into detail mode with the fixture's detail
/// landed, and the table-side state the round trip preserves seeded
/// alongside.
fn drill_in(wired: &mut Wired) {
    wired.press("open-model-market", "search openrouter models");
    seeded(wired);
    detail_fixture(wired);
    // Enter detail with the detail already cached (publishes nothing).
    wired.ui().update(|s| {
        s.filter = "son".to_owned();
        s.sort_col = SortColumn::In;
        s.sort_desc = true;
        s.selection = 1;
        s.enter_detail("openrouter/a/small".to_owned());
    });
}

#[rstest::rstest]
fn enter_on_the_default_row_publishes_the_model_only_triple() {
    // Given detail mode with the Default sentinel highlighted.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    assert_eq!(wired.ui().read().detail_selection, 0);

    // When ENTER fires.
    let mut result = wired.press("confirm-model-market", "switch/pin");

    // Then the quad publishes: the pinned model's ProviderSwitch, the
    // SetLastModel update, the pin-clearing SetEndpointDefault (tag
    // None), and the handler popped the scope. The write-back, save,
    // and registry round-trip behind that command are
    // `openrouter_actor.rs`'s existing `SetEndpointDefault` tests; the
    // picker-side reflection of the pin is
    // `endpoint_loader.rs::build_endpoint_entries_prepends_sentinel_and_marks_pinned_active`.
    assert_eq!(
        result.message_names,
        vec!["ProviderSwitch", "UpdateAppState", "SetEndpointDefault"]
    );
    // Both payload asserts decode from one drain — the closures are
    // `FnOnce`, so a single pass reads every message, filtered per type.
    let drained = drain_payloads(&mut result);
    let switch_payload = drained
        .iter()
        .find(|(name, _)| *name == "ProviderSwitch")
        .map(|(_, payload)| payload.clone())
        .expect("ProviderSwitch publishes");
    let switch_decodes: jinn_provider_selection_msg::ProviderSwitch =
        serde_json::from_value(switch_payload).expect("ProviderSwitch decodes");
    assert_eq!(
        switch_decodes.provider_id,
        jinn_core_types::model_selection::ModelSelection::Single("openrouter/a/small".to_owned())
    );
    let pin_payload = drained
        .iter()
        .find(|(name, _)| *name == "SetEndpointDefault")
        .map(|(_, payload)| payload.clone())
        .expect("SetEndpointDefault publishes");
    let pinned: jinn_provider_selection_msg::SetEndpointDefault =
        serde_json::from_value(pin_payload).expect("SetEndpointDefault decodes");
    assert_eq!(pinned.model, "openrouter/a/small");
    assert_eq!(pinned.tag, None, "the Default row clears any pin");
    assert_eq!(wired.state.frontend.scope(), jinn_slices::FocusScope::Input);
}

#[rstest::rstest]
fn enter_on_a_provider_row_pins_that_endpoint() {
    // Given detail mode with the highlight on the second displayed
    // provider row.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    wired.ui().update(|s| s.detail_selection = 2);

    // When ENTER fires.
    let mut result = wired.press("confirm-model-market", "switch/pin");

    // Then the same triple publishes but the pin carries the
    // endpoint's routing tag (empty string is still Some).
    assert_eq!(
        result.message_names,
        vec!["ProviderSwitch", "UpdateAppState", "SetEndpointDefault"]
    );
    let pins = drain_payloads(&mut result);
    let pin_payload = pins
        .iter()
        .find(|(name, _)| *name == "SetEndpointDefault")
        .map(|(_, payload)| payload.clone())
        .expect("SetEndpointDefault publishes");
    let pinned: jinn_provider_selection_msg::SetEndpointDefault =
        serde_json::from_value(pin_payload).expect("SetEndpointDefault decodes");
    assert_eq!(pinned.model, "openrouter/a/small");
    assert_eq!(
        pinned.tag,
        Some("azure/us-east".to_owned()),
        "the pin follows the RENDERED row: the fixture's fetch order is [Azure, \
         Lambda] but the PROJ-ascending view renders [Default, Lambda, Azure], \
         so cursor index 2 is Azure"
    );
    assert_eq!(wired.state.frontend.scope(), jinn_slices::FocusScope::Input);
}

#[rstest::rstest]
fn confirm_detail_pins_the_row_the_cursor_names_not_the_fetch_order() {
    // Given detail mode over the detail_fixture — fetch order [Azure,
    // Lambda], rendered PROJ-ascending order [Default, Lambda, Azure] —
    // with the cursor on index 2. Fetch order names Lambda there; the
    // rendered projection names Azure.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    wired.ui().update(|s| s.detail_selection = 2);

    // When ENTER fires.
    let mut result = wired.press("confirm-model-market", "switch/pin");

    // Then the pin carries Azure's tag — the row the cursor sat on.
    let drained = drain_payloads(&mut result);
    let pin_payload = drained
        .iter()
        .find(|(name, _)| *name == "SetEndpointDefault")
        .map(|(_, payload)| payload.clone())
        .expect("SetEndpointDefault publishes");
    let pinned: jinn_provider_selection_msg::SetEndpointDefault =
        serde_json::from_value(pin_payload).expect("SetEndpointDefault decodes");
    assert_eq!(
        pinned.tag,
        Some("azure/us-east".to_owned()),
        "the pin names the rendered row, not the fetch-order row"
    );
}

#[rstest::rstest]
fn confirm_detail_at_the_fetch_order_twin_pins_the_untagged_row() {
    // Given the same fixture with the cursor on index 1 — Lambda in the
    // rendered order, Azure in fetch order.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    wired.ui().update(|s| s.detail_selection = 1);

    // When ENTER fires.
    let mut result = wired.press("confirm-model-market", "switch/pin");

    // Then the pin carries Lambda's EMPTY tag — proving the projection
    // (not the fixture layout) decides which endpoint pins.
    let drained = drain_payloads(&mut result);
    let pin_payload = drained
        .iter()
        .find(|(name, _)| *name == "SetEndpointDefault")
        .map(|(_, payload)| payload.clone())
        .expect("SetEndpointDefault publishes");
    let pinned: jinn_provider_selection_msg::SetEndpointDefault =
        serde_json::from_value(pin_payload).expect("SetEndpointDefault decodes");
    assert_eq!(
        pinned.tag,
        Some(String::new()),
        "index 1 in the rendered order is Lambda (untagged)"
    );
}

#[rstest::rstest]
fn esc_in_detail_mode_returns_to_table_with_state_preserved() {
    // Given detail mode entered over table state (filter `son`, IN
    // desc, selection 1), with the detail highlight moved off 0.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    wired.ui().update(|s| s.detail_selection = 1);

    // When ESC fires.
    let result = wired.press("close-model-market", "close");

    // Then the pin is gone, everything table-side is exactly as it was,
    // nothing published, and the popup stays open.
    let ui = wired.ui();
    assert_eq!(ui.read().detail_model, None, "the pin cleared");
    assert_eq!(ui.read().filter, "son");
    assert_eq!(ui.read().sort_col, SortColumn::In);
    assert!(ui.read().sort_desc);
    assert_eq!(ui.read().selection, 1);
    assert!(result.messages.is_empty());
    assert_eq!(
        wired.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(market_ui_scope())
    );
}

#[rstest::rstest]
fn backspace_in_detail_mode_returns_to_the_table() {
    // Given detail mode with the filter unfocused.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When delete-backward arrives (the intent the composition keymap's
    // `bind_input_hook_scopes` mints for `<backspace>`).
    wired.handle(edit_intent("delete-backward", '\u{8}'));

    // Then the mode cleared and the popup stays open.
    assert_eq!(wired.ui().read().detail_model, None);
    assert_eq!(
        wired.state.frontend.scope(),
        jinn_slices::FocusScope::Dynamic(market_ui_scope())
    );
}

#[rstest::rstest]
fn backspace_in_table_mode_still_declines() {
    // Given table mode with the filter unfocused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When delete-backward arrives.
    let result = wired.handle(edit_intent("delete-backward", '\u{8}'));

    // Then the hook declined: the pin was never set, nothing published.
    assert!(result.scope_signal.is_none() && result.messages.is_empty());
    assert_eq!(wired.ui().read().detail_model, None);
}

#[rstest::rstest]
fn detail_navigation_moves_and_clamps() {
    // Given detail mode over the three-row detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When `j` and `<down>` fire, then `<pgdn>` far past the end.
    wired.handle(edit_intent("insert-char", 'j'));
    let after_j = wired.ui().read().detail_selection;
    wired.press("move-model-market-down", "down");
    let after_down = wired.ui().read().detail_selection;
    wired.press("page-model-market-down", "page dn");
    let after_page = wired.ui().read().detail_selection;

    // Then the highlight moved and clamped at the last row (index 2).
    assert_eq!(after_j, 1);
    assert_eq!(after_down, 2);
    assert_eq!(after_page, 2, "paging clamps at the projection's end");
    // And `k` steps back and clamps at 0.
    wired.handle(edit_intent("insert-char", 'k'));
    assert_eq!(wired.ui().read().detail_selection, 1);
    wired.handle(edit_intent("insert-char", 'k'));
    wired.handle(edit_intent("insert-char", 'k'));
    assert_eq!(wired.ui().read().detail_selection, 0, "`k` clamps at 0");
}

#[rstest::rstest]
fn detail_navigation_publishes_nothing() {
    // Given detail mode over a landed detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When every detail-mode navigation and sort key fires.
    let results = [
        wired.handle(edit_intent("insert-char", 'j')),
        wired.handle(edit_intent("insert-char", 'k')),
        wired.handle(edit_intent("insert-char", 'l')),
        wired.handle(edit_intent("insert-char", 'h')),
        wired.handle(edit_intent("insert-char", 's')),
        wired.press("move-model-market-down", "down"),
    ];

    // Then none of them publish a thing: the mode never mints fetches.
    for result in results {
        assert!(
            result.messages.is_empty(),
            "detail-mode keys publish nothing"
        );
    }
}

#[rstest::rstest]
fn detail_sort_cycles_the_eight_columns_and_the_highlight_follows() {
    // Given detail mode on its default sort (Proj ascending).
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When `l` fires once.
    wired.handle(edit_intent("insert-char", 'l'));
    let after_l = wired.ui().read().detail_sort_col;

    // Then the detail sort cycled to the next header column (Proj's
    // cycle successor is Lat) and the table's own sort is untouched.
    use jinn_model_market_msg::DetailSort;
    assert_eq!(after_l, DetailSort::Lat);
    assert_eq!(
        wired.ui().read().sort_col,
        SortColumn::In,
        "table sort untouched"
    );
    assert!(!wired.ui().read().detail_sort_desc);

    // And the render shows the sorted header cell wearing the focus
    // pairing with its arrow — `h` cycles back to Proj first, so the
    // sorted cell is PROJ again.
    wired.handle(edit_intent("insert-char", 'h'));
    assert_eq!(wired.ui().read().detail_sort_col, DetailSort::Proj);
    let buffer = render_to_buffer(&wired, 120, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let theme = jinn_theme::default_theme();
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");
    // Find "PROJ" as a run of styled cells, not a lone glyph: the
    // header row also carries "ENDPOINT", whose "P" would false-match.
    let header = row_text(&buffer, header_y, popup.width);
    let proj_x = header
        .find("PROJ")
        .map(|col| popup.x + col as u16)
        .expect("the PROJ header renders");
    let proj_cell = buffer.cell((proj_x, header_y)).expect("cell exists");
    assert_eq!(proj_cell.style().fg, Some(theme.gutter_bg));
    assert_eq!(proj_cell.style().bg, Some(theme.focus_accent));
    assert!(
        header[(proj_x - popup.x) as usize..].starts_with("PROJ \u{2191}"),
        "the ascending arrow rides the sorted cell, got: {header:?}"
    );
}

// ── Sort / navigation ────────────────────────────────────────────────

#[rstest::rstest]
fn sort_column_cycles_l_through_all_seven_columns() {
    // Given an open popup (PROJ by default).
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `l` fires six times (a hook case — the keymap's
    // insert-char catch-all owns it in this scope).
    for _ in 0..6 {
        wired.handle(edit_intent("insert-char", 'l'));
    }

    // Then the cycle walked PROJ → IN → OUT → CACHE → ECI → VALS →
    // MODEL.
    assert_eq!(wired.ui().read().sort_col, SortColumn::Model);
    // And `h` walks back.
    wired.handle(edit_intent("insert-char", 'h'));
    assert_eq!(wired.ui().read().sort_col, SortColumn::Vals);
}

#[rstest::rstest]
fn sort_direction_toggles_s() {
    // Given an open popup (ascending by default).
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `s` fires (a hook case).
    wired.handle(edit_intent("insert-char", 's'));

    // Then the direction flipped.
    assert!(wired.ui().read().sort_desc);
    // And again restores it.
    wired.handle(edit_intent("insert-char", 's'));
    assert!(!wired.ui().read().sort_desc);
}

#[rstest::rstest]
fn navigation_moves_and_clamps_at_the_table_end() {
    // Given a seeded two-row table.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When moving down twice.
    wired.press("move-model-market-down", "down");
    wired.press("move-model-market-down", "down");

    // Then the highlight sits on the last row.
    assert_eq!(wired.ui().read().selection, 1);
    // And a third move clamps instead of overflowing.
    wired.press("move-model-market-down", "down");
    assert_eq!(wired.ui().read().selection, 1);
    // And moving up steps back.
    wired.press("move-model-market-up", "up");
    assert_eq!(wired.ui().read().selection, 0);
    // And moving up at the top clamps.
    wired.press("move-model-market-up", "up");
    assert_eq!(wired.ui().read().selection, 0);
}

#[rstest::rstest]
fn j_clamps_against_the_live_filtered_count_not_the_viewport() {
    // Given a two-row table filtered down to both rows, with the
    // measured viewport (10) larger than the filtered list.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| s.results_viewport = 10);
    assert_eq!(wired.ui().read().results_viewport, 10);

    // When `j` fires five times — past the list's end.
    for _ in 0..5 {
        wired.handle(edit_intent("insert-char", 'j'));
    }

    // Then the selection clamped at the last filtered row.
    assert_eq!(wired.ui().read().selection, 1);

    // And the highlight renders at that clamped position.
    let buffer = render_to_buffer(&wired, 120, 40);
    let theme = jinn_theme::default_theme();
    let highlighted = buffer
        .content
        .iter()
        .filter(|c| c.bg == theme.picker_selected_bg)
        .count();
    assert!(
        highlighted > 0,
        "the clamped selection must still render highlighted"
    );
}

#[rstest::rstest]
fn navigation_publishes_nothing() {
    // Given a seeded table with no detail loaded.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When moving down.
    let result = wired.press("move-model-market-down", "down");

    // Then the highlight actually moved — the behavior that must
    // survive — and nothing published: table-mode motion fetches only
    // on drill-in ENTER.
    assert_eq!(wired.ui().read().selection, 1);
    assert!(result.messages.is_empty());
}

#[rstest::rstest]
fn hook_j_moves_without_publishing() {
    // Given a seeded table with no detail loaded, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When `j` arrives as the insert-char intent the keymap's catch-all
    // mints (route rows cannot own char keys in a hook scope).
    let result = wired.handle(edit_intent("insert-char", 'j'));

    // Then the highlight moved and nothing published.
    assert_eq!(wired.ui().read().selection, 1);
    assert!(result.messages.is_empty());
}

/// A two-row table whose highlighted row changes under every sort key:
/// `z/dear` is the cheapest projection, `a/cheap` the priciest — so
/// `h` (MODEL order), `l` (IN order), and `s` (direction flip) each
/// promote `a/cheap` over the default highlight.
fn reorder_fixture(wired: &mut Wired) {
    wired.data().update(|d| {
        d.rows = vec![
            row("openrouter/z/dear", "z", "dear", 0.000_005, 0.000_001, None),
            row(
                "openrouter/a/cheap",
                "a",
                "cheap",
                0.000_001,
                0.000_009,
                Some(0.000_009),
            ),
        ];
    });
}

#[rstest::rstest]
#[case('h')]
#[case('l')]
#[case('s')]
fn sort_keys_publish_nothing(#[case] key: char) {
    // Given a table whose default highlight (`z/dear`, cheapest PROJ)
    // every sort key reorders away, no detail loaded.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    reorder_fixture(&mut wired);

    // When the sort key arrives as the insert-char intent the keymap's
    // catch-all mints.
    let result = wired.handle(edit_intent("insert-char", key));

    // Then the highlight moved onto the row the reorder promoted — the
    // highlight index stays 0, the row under it changed — and nothing
    // published.
    let highlighted =
        jinn_model_market::ui_state::highlighted_row(&wired.data().read(), &wired.ui().read())
            .expect("the reordered projection still highlights a row");
    assert_eq!(highlighted.id, "openrouter/a/cheap", "a/cheap now leads");
    assert_eq!(wired.ui().read().selection, 0);
    assert!(result.messages.is_empty());
}

#[rstest::rstest]
fn typing_into_a_focused_filter_publishes_nothing() {
    // Given a seeded table with the filter focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| s.filter_focused = true);

    // When a character arrives as the insert-char intent.
    let result = wired.handle(edit_intent("insert-char", 's'));

    // Then nothing publishes: typing is not highlight motion.
    assert!(result.messages.is_empty());
}

// ── Help toggle (the hook's `?` case) ───────────────────────────────────

#[rstest::rstest]
fn question_mark_toggles_the_help_card() {
    // Given an open popup, table focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When `?` arrives.
    wired.handle(edit_intent("insert-char", '?'));

    // Then the help card is showing.
    assert!(wired.ui().read().help_visible);
    // And `?` again hides it.
    wired.handle(edit_intent("insert-char", '?'));
    assert!(!wired.ui().read().help_visible);
}

#[rstest::rstest]
fn question_mark_types_while_the_filter_is_focused() {
    // Given an open popup with the filter focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| s.filter_focused = true);

    // When `?` arrives.
    wired.handle(edit_intent("insert-char", '?'));

    // Then it typed into the filter instead of toggling the card.
    assert_eq!(wired.ui().read().filter, "?");
    assert!(!wired.ui().read().help_visible);
}

#[rstest::rstest]
fn question_mark_still_toggles_help_in_detail_mode() {
    // Given detail mode over a landed detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When `?` arrives.
    wired.handle(edit_intent("insert-char", '?'));

    // Then the help card is showing in detail mode too.
    assert!(wired.ui().read().help_visible);
    // And `?` again hides it.
    wired.handle(edit_intent("insert-char", '?'));
    assert!(!wired.ui().read().help_visible);
}

// ── Detail mode: f/i are inert ──────────────────────────────────────────

#[rstest::rstest]
fn f_in_detail_mode_does_not_focus_the_filter() {
    // Given detail mode over a landed detail projection (the drill-in
    // fixture's table state carries filter text).
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When `f` arrives as the insert-char intent.
    wired.handle(edit_intent("insert-char", 'f'));

    // Then the filter stayed unfocused and its text unchanged — the key
    // is inert in detail mode.
    let ui = wired.ui();
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().filter, "son");
    // And the mode is still open.
    assert!(ui.read().detail_model.is_some());
}

#[rstest::rstest]
fn i_in_detail_mode_does_not_focus_the_filter() {
    // Given detail mode over a landed detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When `i` arrives as the insert-char intent.
    wired.handle(edit_intent("insert-char", 'i'));

    // Then the filter stayed unfocused and its text unchanged.
    let ui = wired.ui();
    assert!(!ui.read().filter_focused);
    assert_eq!(ui.read().filter, "son");
    // And the mode is still open.
    assert!(ui.read().detail_model.is_some());
}

// ── Refresh ─────────────────────────────────────────────────────────────

#[rstest::rstest]
fn ctrl_r_publishes_refresh_all() {
    // Given an open popup.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When <c-r> fires.
    let result = wired.press("refresh-model-market", "refresh all");

    // Then the drop-everything command publishes.
    assert_eq!(result.message_names, vec!["RefreshAll"]);
}

// ── Render: the stamp clock (§5.3) ──────────────────────────────────────

#[rstest::rstest]
fn clock_renders_from_stamp_without_network() {
    // Given a data cell stamped two minutes in the past.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.data().update(|d| {
        d.models_fetched_at = Some(jiff::Timestamp::now() - jiff::Span::new().minutes(2));
    });

    // When the popup renders against a test backend (no bus, no HTTP).
    let theme = jinn_theme::default_theme();
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("test terminal");
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    terminal
        .draw(|frame| {
            let facts = jinn_slices::RenderFacts::new(theme.clone(), &wired.slices);
            jinn_model_market::view::render_market_popup(frame, popup, &facts);
        })
        .expect("draw");

    // Then the status line carries the stamp-derived age, computed from
    // the cell's stamp — no network involved.
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        text.contains("models updated 2m ago"),
        "status line must render the stamp age, got: {text:?}"
    );
    // And the seeded rows are on the table.
    assert!(
        text.contains("a/small"),
        "the seeded row must render, got: {text:?}"
    );
}

// ── Render: table shape, footer, filter bar ─────────────────────────────

/// Renders the current popup frame and returns the buffer's text plus a
/// per-row extractor. `Wired` must already have its cells seeded.
fn render_to_buffer(wired: &Wired, width: u16, height: u16) -> ratatui::buffer::Buffer {
    render_to_buffer_with_facts(wired, width, height, |facts| {
        facts.set_facts(std::iter::empty());
    })
}

/// Renders one frame with a hook for seeding extra render facts — the
/// usage numbers the mix toggle reads.
fn render_to_buffer_with_facts(
    wired: &Wired,
    width: u16,
    height: u16,
    seed: impl FnOnce(&mut jinn_slices::RenderFacts),
) -> ratatui::buffer::Buffer {
    let theme = jinn_theme::default_theme();
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("test terminal");
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    terminal
        .draw(|frame| {
            let mut facts = jinn_slices::RenderFacts::new(theme.clone(), &wired.slices);
            seed(&mut facts);
            jinn_model_market::view::render_market_popup(frame, popup, &facts);
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// A rendered row's text: the buffer row's cells joined.
fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .collect()
}

#[rstest::rstest]
fn table_headers_align_over_their_data_columns() {
    // Given a seeded table with distinct prices per column, table
    // focused.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.data().update(|d| {
        if let Some(first) = d.rows.first_mut() {
            first.has_overrides = true;
            first.is_free = false;
        }
    });

    // When the popup renders (wide enough for all eight columns).
    let buffer = render_to_buffer(&wired, 140, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 140, 40))
            .expect("frame fits the popup");

    // Then the header row — two rows below the popup's top border, the
    // filter line and its separator above it — carries every column
    // name in order, and PROJ is dollar-headed with no cent sign.
    let header = row_text(&buffer, popup.y + 3, popup.width);
    for name in ["PROJ", "IN/M", "OUT/M", "CACHE/M", "ECI", "MODEL", "FLAGS"] {
        assert!(
            header.contains(name),
            "header row must name {name}, got: {header:?}"
        );
    }
    assert!(
        !header.contains('\u{a2}'),
        "PROJ is dollar-headed, got: {header:?}"
    );
    // And the first data row shows each of the first listing's values
    // on the same line: PROJ dollars, $/M prices, the model, and the
    // FLAGS glyph for the pricing override.
    let data_row = row_text(&buffer, popup.y + 4, popup.width);
    assert!(
        data_row.contains('$'),
        "PROJ cell renders dollars, got: {data_row:?}"
    );
    assert!(
        data_row.contains("a/small"),
        "MODEL cell renders the name, got: {data_row:?}"
    );
    assert!(
        data_row.contains('\u{2020}'),
        "FLAGS cell renders the override dagger, got: {data_row:?}"
    );
    // And a row with no cache price shows the empty-cell placeholder
    // under CACHE/M.
    assert!(
        data_row.contains(jinn_model_market::format::EMPTY_CELL),
        "CACHE cell renders the empty cell when unpublished, got: {data_row:?}"
    );
}

#[rstest::rstest]
fn footer_renders_the_advertised_keys() {
    // Given an open popup.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then the popup's footer carries every advertised key and label
    // across its two rows, in MARKET_BINDINGS order split 7/6.
    let upper = row_text(&buffer, popup.y + popup.height - 3, popup.width);
    let lower = row_text(&buffer, popup.y + popup.height - 2, popup.width);
    let footer = format!("{upper}\n{lower}");
    assert!(
        footer.contains("<enter>"),
        "footer shows the confirm key, got: {footer:?}"
    );
    assert!(
        footer.contains("<esc>"),
        "footer shows the close key, got: {footer:?}"
    );
    assert!(
        footer.contains("<c-r>"),
        "footer shows the refresh key, got: {footer:?}"
    );
    assert!(
        footer.contains("<pgdn>"),
        "footer shows the page key, got: {footer:?}"
    );
}

#[rstest::rstest]
fn focused_filter_shows_a_visible_filter_line() {
    // Given an open popup with the filter focused and text typed.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.ui().update(|s| {
        s.filter_focused = true;
        s.filter = "son".to_owned();
    });
    seeded(&mut wired);

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);
    let title = row_text(&buffer, popup.y, popup.width);
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);

    // Then the filter lives on its own line — glyph, text plus cursor
    // block — and the title is always the popup's plain name.
    assert!(
        filter_line.contains(" > son"),
        "the focused filter line shows the glyph and filter, got: {filter_line:?}"
    );
    assert!(
        filter_line.contains('\u{2588}'),
        "the focused filter line shows a cursor, got: {filter_line:?}"
    );
    assert!(
        title.contains(" OpenRouter Model Market "),
        "the title stays plain, got: {title:?}"
    );
    assert!(
        !title.contains('>'),
        "the filter never renders into the titlebar, got: {title:?}"
    );
    // And a separator row of dashes sits under the filter line (the
    // row text includes the popup's side borders, so scan the inner
    // cell range).
    let separator = row_text(&buffer, popup.y + 2, popup.width);
    let inner: String = (popup.x + 1..popup.x + popup.width - 1)
        .filter_map(|x| buffer.cell((x, popup.y + 2)).map(Cell::symbol))
        .collect();
    let trimmed = inner.trim();
    assert!(
        !trimmed.is_empty() && trimmed.chars().all(|c| c == '\u{2500}'),
        "a separator sits under the filter line, got: {separator:?}"
    );
}

#[rstest::rstest]
fn status_line_shows_the_stamps_and_the_fixed_mix_no_score_stamps() {
    // Given an open popup over a seeded table with stamps two minutes
    // old.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.data().update(|d| {
        let stamp = jiff::Timestamp::now() - jiff::Span::new().minutes(2);
        d.models_fetched_at = Some(stamp);
        d.eci_fetched_at = Some(stamp);
        d.endpoints_fetched_at = Some(stamp);
    });

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);
    let status = row_text(&buffer, popup.y + popup.height - 4, popup.width);

    // Then the status line carries the models stamp.
    assert!(
        status.contains("models updated 2m ago"),
        "status shows the kept stamp, got: {status:?}"
    );
    // And the resolved mix — the fixed constant, since no usage facts
    // are seeded and the source is fixed.
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "status shows the fixed mix, got: {status:?}"
    );
    // And none of the retired indicators: no eci source, no eci
    // freshness stamp, no endpoints stamp, no m hint.
    assert!(
        !status.contains("endpoints "),
        "status shows no endpoints stamp, got: {status:?}"
    );
    assert!(
        !status.contains("[m] adjust"),
        "status shows no m hint, got: {status:?}"
    );
    assert!(
        !status.contains("eci: "),
        "status shows no eci source, got: {status:?}"
    );
    assert!(
        !status.contains("eci 2m ago"),
        "status shows no eci freshness stamp, got: {status:?}"
    );
}

#[rstest::rstest]
fn filter_matches_are_highlighted_in_the_model_column() {
    // Given an open popup filtered down to one row.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| s.filter = "small".to_owned());

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then only the matching row renders.
    let popup_rows: Vec<String> = (popup.y + 4..popup.y + 7)
        .map(|y| row_text(&buffer, y, popup.width))
        .collect();
    assert!(
        popup_rows.iter().any(|r| r.contains("a/small")),
        "the matching row renders: {popup_rows:?}"
    );
    assert!(
        !popup_rows.iter().any(|r| r.contains("b/big")),
        "the non-matching row is filtered out: {popup_rows:?}"
    );
    // And the matched glyphs carry the shared highlighter's background:
    // scanning the row's cells, at least one cell in the MODEL column
    // carries the theme's highlight bg (the filter's match spans).
    let theme = jinn_theme::default_theme();
    let y = (popup.y + 4..popup.y + popup.height - 4)
        .find(|y| row_text(&buffer, *y, popup.width).contains("a/small"))
        .expect("the matching row renders");
    let highlighted = (popup.x..popup.x + popup.width)
        .filter(|x| {
            buffer
                .cell((*x, y))
                .is_some_and(|c| c.style().bg == Some(theme.picker_highlight_bg))
        })
        .count();
    assert!(
        highlighted >= 1,
        "the filter's match spans carry the highlight bg, found {highlighted}"
    );
    // And the matched glyphs are the filter's letters ("s","m","a","l","l").
    let glyphs: String = (popup.x..popup.x + popup.width)
        .filter(|x| {
            buffer
                .cell((*x, y))
                .is_some_and(|c| c.style().bg == Some(theme.picker_highlight_bg))
        })
        .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
        .collect();
    assert_eq!(glyphs, "small");
}

#[rstest::rstest]
fn multi_term_filter_narrows_the_table() {
    // Given the seeded two-row table (`a/small`, `b/big`) and a
    // multi-term filter that only `a/small` can satisfy — typed through
    // the real hook: `f` focuses, then the letters.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.handle(edit_intent("insert-char", 'f'));
    assert!(wired.ui().read().filter_focused, "`f` focused the filter");
    for ch in "sm l".chars() {
        wired.handle(edit_intent("insert-char", ch));
    }

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then the AND-narrowed set renders: only `a/small` — `sm l` needs
    // `sm` AND `l`, and `b/big` has neither term fully.
    let popup_rows: Vec<String> = (popup.y + 4..popup.y + 8)
        .map(|y| row_text(&buffer, y, popup.width))
        .collect();
    assert!(
        popup_rows.iter().any(|r| r.contains("a/small")),
        "the AND-matching row renders: {popup_rows:?}"
    );
    assert!(
        !popup_rows.iter().any(|r| r.contains("b/big")),
        "the row missing a term is filtered out: {popup_rows:?}"
    );
    // And the filter line carries the typed terms verbatim.
    let filter_line = row_text(&buffer, popup.y + 1, popup.width);
    assert!(
        filter_line.contains("sm l"),
        "the filter line shows the multi-term query, got: {filter_line:?}"
    );
}

#[rstest::rstest]
fn footer_advertises_s_sort_m_mix_f_filter() {
    // Given an open popup.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When the popup renders (a wide frame — the footer is a long line
    // and the `m`/`f` entries sit past where 120 columns truncate it).
    let area = ratatui::layout::Rect::new(0, 0, 200, 50);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 200, 50);
    let footer = row_text(&buffer, popup.y + popup.height - 2, popup.width);

    // Then the direction entry reads `s sort`, not `s dir`, the mix
    // toggle is advertised, and the filter entry is present.
    assert!(
        footer.contains("s sort"),
        "the direction entry reads `s sort`, got: {footer:?}"
    );
    assert!(
        !footer.contains("s dir"),
        "the old `s dir` is gone, got: {footer:?}"
    );
    assert!(
        footer.contains("m mix"),
        "the mix toggle is advertised, got: {footer:?}"
    );
    assert!(
        footer.contains("f filter"),
        "the filter entry is advertised, got: {footer:?}"
    );
}

#[rstest::rstest]
fn m_toggles_the_mix_and_the_status_line_and_proj_reflect_it() {
    // Given an open popup over a seeded table — the fixed mix active,
    // its status display rendered.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let status_row = popup.y + popup.height - 4;
    let buffer = render_to_buffer(&wired, 120, 40);
    let status = row_text(&buffer, status_row, popup.width);
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "fixed mix shows before the toggle, got: {status:?}"
    );
    // When `m` toggles the source to usage (no usage facts seeded — the
    // projection degrades to the fixed constant) and the popup renders.
    wired.handle(edit_intent("insert-char", 'm'));
    let buffer = render_to_buffer(&wired, 120, 40);
    let status = row_text(&buffer, status_row, popup.width);

    // Then the source flag flipped (observable through the UI cell),
    // the status line still reads the fixed display (the degraded
    // resolve), and the PROJ values are unchanged.
    assert_eq!(
        wired.ui().read().mix_source,
        jinn_model_market_msg::MixSource::Usage,
        "the toggle flipped the source"
    );
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "no usage means the fixed display stands, got: {status:?}"
    );
    // And the PROJ header still reads PROJ (the column persists).
    let header = row_text(&buffer, popup.y + 3, popup.width);
    assert!(
        header.contains("PROJ"),
        "the PROJ column persists, got: {header:?}"
    );
}

/// Seeds the kernel's tree-usage facts the mix toggle reads.
fn seed_usage(
    facts: &mut jinn_slices::RenderFacts,
    sent: u64,
    received: u64,
    cached: u64,
    measured: u64,
) {
    facts.set_facts([
        jinn_slices::AppFact {
            key: "session.tree-effective-sent",
            value: sent.to_string(),
        },
        jinn_slices::AppFact {
            key: "session.tree-total-received",
            value: received.to_string(),
        },
        jinn_slices::AppFact {
            key: "session.tree-cached-total",
            value: cached.to_string(),
        },
        jinn_slices::AppFact {
            key: "session.tree-measured-sent",
            value: measured.to_string(),
        },
    ]);
}

#[rstest::rstest]
fn m_with_seeded_usage_projects_under_the_tree_mix() {
    // Given an open popup, `m` already flipped to the usage source, and
    // a seeded tree usage: 5M effective sent, 100k received, 4.9M of
    // 5.0M measured sent cached.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.handle(edit_intent("insert-char", 'm'));
    assert_eq!(
        wired.ui().read().mix_source,
        jinn_model_market_msg::MixSource::Usage,
        "the toggle flipped the source"
    );

    // When the popup renders with those usage facts seeded.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let status_row = popup.y + popup.height - 4;
    let buffer = render_to_buffer_with_facts(&wired, 120, 40, |facts| {
        seed_usage(facts, 5_000_000, 100_000, 4_900_000, 5_000_000);
    });
    let status = row_text(&buffer, status_row, popup.width);

    // Then the status line shows the tree mix: 5M input, 98% hit, 100k
    // output.
    assert!(
        status.contains("mix: tree 5M\u{b7}98%\u{b7}100k"),
        "the tree mix shows, got: {status:?}"
    );

    // And toggling back with the facts still seeded shows fixed again.
    wired.handle(edit_intent("insert-char", 'm'));
    let buffer = render_to_buffer_with_facts(&wired, 120, 40, |facts| {
        seed_usage(facts, 5_000_000, 100_000, 4_900_000, 5_000_000);
    });
    let status = row_text(&buffer, status_row, popup.width);
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "toggling back shows fixed, got: {status:?}"
    );
}

#[rstest::rstest]
fn m_toggles_from_detail_mode_and_the_status_line_reflects_it() {
    // Given detail mode over a landed detail projection — the fixed mix
    // active.
    let mut wired = Wired::new();
    drill_in(&mut wired);
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let status_row = popup.y + popup.height - 4;
    let buffer = render_to_buffer(&wired, 120, 40);
    let status = row_text(&buffer, status_row, popup.width);
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "fixed mix shows in detail mode before the toggle, got: {status:?}"
    );

    // When `m` fires in detail mode.
    wired.handle(edit_intent("insert-char", 'm'));

    // Then the source flipped — the same cell flag table mode toggles,
    // and the detail table still renders (its PROJ now re-projects
    // under the resolved mix, degraded to fixed with no usage facts).
    assert_eq!(
        wired.ui().read().mix_source,
        jinn_model_market_msg::MixSource::Usage,
        "the toggle flipped the source from detail mode"
    );
    let buffer = render_to_buffer(&wired, 120, 40);
    let header = row_text(&buffer, popup.y + 3, popup.width);
    assert!(
        header.contains("ENDPOINT") && header.contains("PROJ"),
        "the detail table still renders, got: {header:?}"
    );
}

#[rstest::rstest]
fn mix_source_word_wears_success_green_numbers_stay_muted() {
    // Given an open popup over a seeded table (fixed mix active).
    let (wired, popup, status_row, theme) = {
        let mut wired = Wired::new();
        wired.press("open-model-market", "search openrouter models");
        seeded(&mut wired);
        let area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let popup =
            jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
        let status_row = popup.y + popup.height - 4;
        let theme = jinn_theme::default_theme();
        (wired, popup, status_row, theme)
    };

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then the status row's "fixed" cells wear success green and the
    // number cells beside it do not. The word starts after `mix: `.
    let mix_x = (popup.x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, status_row))
                .is_some_and(|c| c.symbol() == "m")
        })
        .and_then(|x| {
            (x..popup.x + popup.width).find(|x2| {
                buffer
                    .cell((*x2, status_row))
                    .is_some_and(|c| c.symbol() == "i")
            })
        })
        .expect("the mix label renders");
    let word: Vec<(u16, ratatui::buffer::Cell)> = (mix_x..popup.x + popup.width)
        .filter_map(|x| buffer.cell((x, status_row)).map(|c| (x, c.clone())))
        .take_while(|(_, c)| c.symbol() != "\u{b7}")
        .collect();
    let fixed_start = word
        .iter()
        .position(|(_, c)| c.symbol() == "f")
        .expect("the source word renders");
    let cells: Vec<&ratatui::buffer::Cell> = word
        .iter()
        .skip(fixed_start)
        .map(|(_, c)| c)
        .take("fixed".len())
        .collect();
    assert_eq!(
        cells.iter().map(|c| c.symbol()).collect::<String>(),
        "fixed",
        "the source word renders contiguously"
    );
    for cell in &cells {
        assert_eq!(
            cell.style().fg,
            Some(theme.success),
            "the source word wears success green"
        );
    }
    // And a number cell after the word stays muted.
    let number_cell = word
        .iter()
        .skip(fixed_start + "fixed".len() + 1)
        .find(|(_, c)| c.symbol() != " ")
        .expect("the numbers render");
    assert_eq!(
        number_cell.1.style().fg,
        Some(theme.muted_text),
        "the numbers stay muted"
    );
}

#[rstest::rstest]
fn m_with_an_unmeasured_tree_degrades_to_the_fixed_mix() {
    // Given an open popup flipped to the usage source over a tree with
    // zero measured-sent tokens (a fresh session).
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.handle(edit_intent("insert-char", 'm'));

    // When the popup renders with the zero-usage facts.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let status_row = popup.y + popup.height - 4;
    let buffer = render_to_buffer_with_facts(&wired, 120, 40, |facts| {
        seed_usage(facts, 0, 0, 0, 0);
    });
    let status = row_text(&buffer, status_row, popup.width);

    // Then the fixed display stands: the resolve degraded, never a
    // $0.00 ranking.
    assert!(
        status.contains("mix: fixed 10M\u{b7}98%\u{b7}50k"),
        "the degraded resolve shows fixed, got: {status:?}"
    );
}

#[rstest::rstest]
fn sorted_header_cell_carries_the_focus_pairing() {
    // Given an open popup with the default sort (PROJ ascending).
    let (mut wired, popup, header_y, theme) = {
        let mut wired = Wired::new();
        wired.press("open-model-market", "search openrouter models");
        seeded(&mut wired);
        let area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let popup =
            jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
        let header_y = popup.y + 3;
        let theme = jinn_theme::default_theme();
        (wired, popup, header_y, theme)
    };

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then the PROJ header cells — name and arrow — wear the focus
    // pairing: fg gutter_bg on bg focus_accent.
    let proj_x = (popup.x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, header_y))
                .is_some_and(|c| c.symbol() == "P")
        })
        .expect("the PROJ header renders");
    let proj_cell = buffer.cell((proj_x, header_y)).expect("cell exists");
    assert_eq!(proj_cell.style().fg, Some(theme.gutter_bg));
    assert_eq!(proj_cell.style().bg, Some(theme.focus_accent));
    // And an unsorted header cell carries no background at all.
    let model_x = (proj_x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, header_y))
                .is_some_and(|c| c.symbol() == "M")
        })
        .expect("the MODEL header renders");
    let model_cell = buffer.cell((model_x, header_y)).expect("cell exists");
    assert_ne!(
        model_cell.style().bg,
        Some(theme.focus_accent),
        "an unsorted header carries no focus background"
    );

    // And after `l` the highlight moves to IN/M: the PROJ cell is
    // unstyled again and the IN/M cell wears the pairing.
    wired.handle(edit_intent("insert-char", 'l'));
    let buffer = render_to_buffer(&wired, 120, 40);
    let proj_cell = buffer.cell((proj_x, header_y)).expect("cell exists");
    assert_ne!(
        proj_cell.style().bg,
        Some(theme.focus_accent),
        "PROJ loses the pairing after `l`"
    );
    let in_x = (proj_x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, header_y))
                .is_some_and(|c| c.symbol() == "I")
        })
        .expect("the IN/M header renders");
    let in_cell = buffer.cell((in_x, header_y)).expect("cell exists");
    assert_eq!(in_cell.style().fg, Some(theme.gutter_bg));
    assert_eq!(in_cell.style().bg, Some(theme.focus_accent));
}

#[rstest::rstest]
fn footer_keys_wear_the_picker_pairing() {
    // Given an open popup.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);
    let footer_y = popup.y + popup.height - 3;
    let theme = jinn_theme::default_theme();

    // Then the `<enter>` key notation cells wear accent_action.
    let enter_x = (popup.x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, footer_y))
                .is_some_and(|c| c.symbol() == "<")
        })
        .expect("the footer's first key renders");
    let key_cell = buffer.cell((enter_x, footer_y)).expect("cell exists");
    assert_eq!(
        key_cell.style().fg,
        Some(theme.accent_action),
        "key notations render in accent_action"
    );
    // And a description cell — the label after the key — wears
    // muted_text.
    let desc_x = (enter_x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, footer_y))
                .is_some_and(|c| c.symbol() == "s")
        })
        .expect("the footer's first label renders");
    let desc_cell = buffer.cell((desc_x, footer_y)).expect("cell exists");
    assert_eq!(
        desc_cell.style().fg,
        Some(theme.muted_text),
        "descriptions render in muted_text"
    );
}

#[rstest::rstest]
fn highlight_on_a_long_list_never_parks_under_the_status_rows() {
    // Given a 50-row table with the selection walked deep past the
    // viewport — the auto-scroll territory the full-area rect used to
    // window wrongly.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.data().update(|d| {
        d.rows = (0..50)
            .map(|i| {
                row(
                    &format!("openrouter/m/model{i}"),
                    "m",
                    &format!("model{i}"),
                    1e-6 + f64::from(i) * 1e-8,
                    2e-6,
                    Some(1e-7),
                )
            })
            .collect();
    });
    for _ in 0..30 {
        wired.handle(edit_intent("insert-char", 'j'));
    }
    assert_eq!(wired.ui().read().selection, 30, "the walk landed deep");

    // When the popup renders.
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);
    let theme = jinn_theme::default_theme();
    let status_y = popup.y + popup.height - 4;

    // Then the highlighted row renders strictly above the status line,
    // and neither bottom row carries the selection background.
    let selected_y = (popup.y..popup.y + popup.height)
        .find(|y| {
            (popup.x..popup.x + popup.width).any(|x| {
                buffer
                    .cell((x, *y))
                    .is_some_and(|c| c.bg == theme.picker_selected_bg)
            })
        })
        .expect("the selection renders highlighted");
    assert!(
        selected_y < status_y,
        "the highlight ({selected_y}) must sit above the status line ({status_y})"
    );
    for y in status_y..popup.y + popup.height {
        let selected = (popup.x..popup.x + popup.width).any(|x| {
            buffer
                .cell((x, y))
                .is_some_and(|c| c.bg == theme.picker_selected_bg)
        });
        assert!(
            !selected,
            "the status/footer row {y} must never carry the selection bg"
        );
    }
}

// ── Detail mode: composition-level render facts ─────────────────────────

#[rstest::rstest]
fn detail_mode_renders_the_eight_column_headers() {
    // Given detail mode over a landed detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");
    let header = row_text(&buffer, header_y, popup.width);

    // Then the header row carries all eight names in order.
    let mut cursor = 0usize;
    for name in [
        "ENDPOINT", "IN /M", "OUT /M", "CACHE /M", "PROJ", "LAT p50", "TP p50", "UP 1d",
    ] {
        let at = header[cursor..]
            .find(name)
            .unwrap_or_else(|| panic!("header must carry {name} in order, got: {header:?}"));
        cursor += at + name.len();
    }
}

#[rstest::rstest]
fn the_default_row_is_first_and_labeled_default() {
    // Given detail mode over a landed detail projection.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let header_y = (popup.y..popup.y + popup.height)
        .find(|y| row_text(&buffer, *y, popup.width).contains("ENDPOINT"))
        .expect("the detail header renders");

    // Then the first data row reads `Default` and the provider
    // endpoints follow — under the default PROJ-ascending sort the
    // cheaper endpoint (Lambda) leads them.
    let first = row_text(&buffer, header_y + 1, popup.width);
    assert!(
        first.contains("Default"),
        "the sentinel is the first row, got: {first:?}"
    );
    let second = row_text(&buffer, header_y + 2, popup.width);
    assert!(
        second.contains("Lambda"),
        "the endpoints follow the sentinel, sorted, got: {second:?}"
    );
}

#[rstest::rstest]
fn detail_loading_renders_a_loading_line_and_no_header() {
    // Given detail mode pinned while the data cell holds detail for
    // another model.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired
        .ui()
        .update(|s| s.enter_detail("openrouter/a/small".to_owned()));
    wired.data().update(|d| {
        d.detail = Some(jinn_model_market_msg::MarketDetail {
            model_id: "openrouter/b/big".to_owned(),
            endpoints: vec![],
        });
    });

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);

    // Then the table area carries the muted loading line — no header,
    // no rows.
    let text: String = buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        text.contains("loading endpoints"),
        "the loading line renders, got: {text:?}"
    );
    assert!(
        !text.contains("ENDPOINT") && !text.contains("Azure"),
        "no header and no stale rows render, got: {text:?}"
    );
}

#[rstest::rstest]
fn title_in_detail_mode_carries_the_model_in_focus_accent() {
    // Given detail mode pinned to `openrouter/a/small`.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When the popup renders.
    let buffer = render_to_buffer(&wired, 120, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let theme = jinn_theme::default_theme();
    let title = row_text(&buffer, popup.y, popup.width);

    // Then the title reads the prefix and the model name, the
    // `openrouter/` prefix stripped.
    assert!(
        title.contains(" OpenRouter Model Market ") && title.contains("\u{b7} a/small"),
        "the title carries the model, got: {title:?}"
    );

    // And the model-name cells wear the focus accent while the prefix
    // cells wear the popup title color. The name starts after the `·`
    // separator and its following space.
    let separator_x = (popup.x..popup.x + popup.width)
        .find(|x| {
            buffer
                .cell((*x, popup.y))
                .is_some_and(|c| c.symbol() == "\u{b7}")
        })
        .expect("the separator dot renders");
    let model_cell = (separator_x + 1..popup.x + popup.width)
        .filter_map(|x| buffer.cell((x, popup.y)))
        .find(|c| c.symbol() != " ")
        .expect("the model name renders in the title");
    assert_eq!(
        model_cell.style().fg,
        Some(theme.focus_accent),
        "the model name renders in focus_accent"
    );
    let prefix_cell = buffer
        .cell((popup.x + 2, popup.y))
        .expect("prefix cell exists");
    assert_eq!(
        prefix_cell.style().fg,
        Some(theme.popup_title),
        "the prefix renders in popup_title"
    );
}

#[rstest::rstest]
fn filter_glyph_renders_gt_with_and_without_the_cursor() {
    // Given an open popup with filter text, once focused and once not.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    wired.ui().update(|s| {
        s.filter = "sonnet".to_owned();
        s.filter_focused = true;
    });

    // When rendering focused, then unfocused.
    let focused = render_to_buffer(&wired, 120, 40);
    wired.ui().update(|s| s.filter_focused = false);
    let unfocused = render_to_buffer(&wired, 120, 40);
    let row = |buffer: &ratatui::buffer::Buffer, y: u16| {
        (0..120)
            .filter_map(|x| buffer.cell((x, y)).map(Cell::symbol))
            .collect::<String>()
    };

    // Then the focused line reads ` > sonnet` with the cursor block and
    // the unfocused one reads ` > sonnet` without it.
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let focused_line = row(&focused, popup.y + 1);
    let unfocused_line = row(&unfocused, popup.y + 1);
    assert!(
        focused_line.contains(" > sonnet") && focused_line.contains('\u{2588}'),
        "focused reads the glyph, text, and cursor, got: {focused_line:?}"
    );
    assert!(
        unfocused_line.contains(" > sonnet") && !unfocused_line.contains('\u{2588}'),
        "unfocused reads the glyph and text, no cursor, got: {unfocused_line:?}"
    );
    // And the titlebar stays plain in both.
    for buffer in [&focused, &unfocused] {
        assert!(
            !row(buffer, popup.y).contains('>'),
            "the titlebar stays plain"
        );
    }
}

#[rstest::rstest]
fn footer_in_detail_mode_advertises_switch_pin() {
    // Given detail mode.
    let mut wired = Wired::new();
    drill_in(&mut wired);

    // When the popup renders (a wide frame — the footer is a long line
    // and `<enter> switch/pin` sits past where 120 columns truncate).
    let buffer = render_to_buffer(&wired, 160, 50);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 160, 50))
            .expect("frame fits the popup");
    let footer = format!(
        "{}\n{}",
        row_text(&buffer, popup.y + popup.height - 3, popup.width),
        row_text(&buffer, popup.y + popup.height - 2, popup.width)
    );

    // Then the enter entry reads `switch/pin` — truthful in both modes
    // — and no bare `switch` label stands alone.
    assert!(
        footer.contains("<enter> switch/pin"),
        "the footer advertises switch/pin, got: {footer:?}"
    );
    assert!(
        !footer.contains("switch "),
        "no bare `switch` label remains, got: {footer:?}"
    );
}

#[rstest::rstest]
fn table_mode_uses_the_full_height_after_the_pane_gone() {
    // Given an 80-row table with detail landed for the pinned row, then
    // the round trip back to table mode — enough rows to overflow the
    // viewport.
    let wired = {
        let mut wired = Wired::new();
        wired.press("open-model-market", "search openrouter models");
        wired.data().update(|d| {
            d.rows = (0..80)
                .map(|i| {
                    row(
                        &format!("openrouter/m/model{i}"),
                        "m",
                        &format!("model{i}"),
                        1e-6 + f64::from(i) * 1e-8,
                        2e-6,
                        Some(1e-7),
                    )
                })
                .collect();
            d.detail = Some(jinn_model_market_msg::MarketDetail {
                model_id: "openrouter/m/model0".to_owned(),
                endpoints: vec![jinn_model_market_msg::MarketEndpointRow {
                    tag: "azure/us-east".to_owned(),
                    provider_name: "Azure".to_owned(),
                    prompt_price: 1.2e-6,
                    completion_price: 4.5e-6,
                    cache_read_price: Some(0.9e-7),
                    latency_p50_ms: Some(320.0),
                    throughput_p50: Some(88.0),
                    uptime_1d: Some(0.992),
                    quantization: None,
                    overrides: Vec::new(),
                }],
            });
        });
        wired
            .ui()
            .update(|s| s.enter_detail("openrouter/m/model0".to_owned()));
        wired.ui().update(MarketUiState::leave_detail);
        wired
    };

    // When the popup renders in table mode.
    let buffer = render_to_buffer(&wired, 120, 40);
    let popup =
        jinn_model_market::view::market_overlay_rect(&ratatui::layout::Rect::new(0, 0, 120, 40))
            .expect("frame fits the popup");
    let status_y = popup.y + popup.height - 4;

    // Then rows render down to the row just above the status line: no
    // band sits reserved under the table.
    let found = (popup.y..status_y)
        .rev()
        .find(|y| row_text(&buffer, *y, popup.width).contains("m/model"))
        .expect("rows render");
    assert_eq!(
        found,
        status_y - 1,
        "a data row renders directly above the status line, got {found:?}"
    );
    // And no endpoint text renders anywhere: the pane is gone.
    let text: String = buffer
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(
        !text.contains("Azure"),
        "no pane text renders in table mode, got: {text:?}"
    );
}

#[rstest::rstest]
fn footer_renders_two_rows_at_a_narrow_width() {
    // Given an open popup rendered at the smallest overlay width the
    // geometry allows (a narrow terminal).
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    let area = ratatui::layout::Rect::new(0, 0, 48, 24);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 48, 24);

    // When both footer rows are read.
    let upper = row_text(&buffer, popup.y + popup.height - 3, popup.width);
    let lower = row_text(&buffer, popup.y + popup.height - 2, popup.width);

    // Then BOTH rows carry entries — the split is deterministic, not
    // width-dependent: seven entries above, six below.
    assert!(
        upper.contains("<enter>"),
        "the upper row opens with the confirm key, got: {upper:?}"
    );
    assert!(
        !upper.trim().is_empty() && !lower.trim().is_empty(),
        "both footer rows carry entries, got upper {upper:?} / lower {lower:?}"
    );
    // And the rows hold disjoint halves in display order: `<enter>` on
    // the upper row, `h` (the lower half's first entry) on the lower —
    // narrow widths truncate the tail, never reorder it.
    assert!(
        lower.contains("h col"),
        "the lower row opens with the eighth entry, got: {lower:?}"
    );
    assert!(
        !upper.contains("h col"),
        "the display order is preserved across the split, got: {upper:?}"
    );
}

#[rstest::rstest]
fn footer_renders_the_same_split_at_a_wide_width() {
    // Given an open popup rendered at a wide terminal.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    let area = ratatui::layout::Rect::new(0, 0, 220, 50);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 220, 50);

    // When both footer rows are read.
    let upper = row_text(&buffer, popup.y + popup.height - 3, popup.width);
    let lower = row_text(&buffer, popup.y + popup.height - 2, popup.width);

    // Then the same 7/6 split holds — width never moves an entry
    // between rows. `<enter>` stays upper, `?` stays lower.
    assert!(
        upper.contains("<enter>") && upper.contains("<c-r>"),
        "the upper row keeps its entries at width, got: {upper:?}"
    );
    assert!(
        lower.contains("? help"),
        "the lower row keeps the trailing entries at width, got: {lower:?}"
    );
}

#[rstest::rstest]
fn table_area_ends_above_the_two_footer_rows() {
    // Given an open popup over a seeded table.
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    seeded(&mut wired);
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let popup = jinn_model_market::view::market_overlay_rect(&area).expect("frame fits the popup");
    let buffer = render_to_buffer(&wired, 120, 40);

    // When the band between the header and the status line is scanned,
    // and the footer rows with it.
    let band: Vec<String> = ((popup.y + 4)..(popup.y + popup.height - 4))
        .map(|y| row_text(&buffer, y, popup.width))
        .collect();
    let footers: Vec<String> = ((popup.y + popup.height - 3)..(popup.y + popup.height - 1))
        .map(|y| row_text(&buffer, y, popup.width))
        .collect();

    // Then both rows render inside the band — the table's rect ends
    // above the status line and BOTH footer rows.
    assert!(
        band.iter().any(|t| t.contains("a/small")),
        "a data row renders in the band, got: {band:?}"
    );
    // And neither footer row carries table content: the last two rows
    // are the footer's, not the table's.
    for (idx, footer) in footers.iter().enumerate() {
        assert!(
            !footer.contains("a/small") && !footer.contains("b/big"),
            "footer row {idx} is never painted over by the table, got: {footer:?}"
        );
    }
}

#[rstest::rstest]
fn sorting_by_vals_orders_by_accuracy_with_unjoined_last() {
    // Given a table of three rows: high accuracy, low accuracy, and an
    // unjoined row (no vals).
    let mut wired = Wired::new();
    wired.press("open-model-market", "search openrouter models");
    wired.data().update(|d| {
        let mut high = row("openrouter/a/high", "a", "high", 1e-6, 2e-6, None);
        high.vals = Some(88.0);
        let mut low = row("openrouter/b/low", "b", "low", 1e-6, 2e-6, None);
        low.vals = Some(12.5);
        let unjoined = row("openrouter/c/none", "c", "none", 1e-6, 2e-6, None);
        d.rows = vec![low, unjoined, high];
    });

    // When the projection resolves under the vals sort, ascending.
    wired.ui().update(|s| s.sort_col = SortColumn::Vals);
    let sorted: Vec<MarketRow> = {
        let ui = wired.ui();
        jinn_model_market::ui_state::visible_rows(&wired.data().read(), &ui.read()).clone()
    };

    // Then ascending order is low → high, and the unjoined row sorts
    // last either way.
    let slugs: Vec<&str> = sorted.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(slugs, ["low", "high", "none"]);

    // When the direction flips to descending.
    wired.ui().update(|s| s.sort_desc = true);
    let sorted_desc: Vec<MarketRow> = {
        let ui = wired.ui();
        jinn_model_market::ui_state::visible_rows(&wired.data().read(), &ui.read()).clone()
    };

    // Then descending is high → low, and the unjoined row STILL sorts
    // last — a flip never surfaces the unmatched rows first.
    let slugs_desc: Vec<&str> = sorted_desc.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(slugs_desc, ["high", "low", "none"]);
}
