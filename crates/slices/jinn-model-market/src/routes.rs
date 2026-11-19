//! The model-market popup's route rows and input hook.
//!
//! Every key the popup responds to is either a [`RouteRow`] this slice
//! attaches or a hook case. The split follows the dispatch order the
//! kernel guarantees — **bound route rows resolve before the input
//! hook** — and, decisively, the popup lives in an input-hook scope:
//! every printable key is owned by the hook, so *any* character key
//! would be unreachable as a row inside the popup's own scope. The
//! non-character keys are rows:
//!
//! - `<enter>`/`<esc>`/`<c-r>` and the four navigation keys bind as
//!   rows (`<up>`/`<down>` plus their `j`/`k` twins);
//! - every printable character is a hook case: with the filter focused
//!   it types into the filter; with the table focused, `f`/`i` focus
//!   the filter, `?` toggles the help card, `O` opens the highlighted
//!   model's openrouter.ai page, and `h`/`l`/`s` run their sort
//!   actions. If those were rows, route dispatch would
//!   consume them before the hook and typing "mistral" into a focused
//!   filter would flip sorts instead of filtering.
//!
//! Focus and mode live *inside* the row actions: the rows fire wherever
//! focus is and whichever mode is active, so `<enter>` and `<esc>`
//! branch on [`MarketUiState::
//! filter_focused`](jinn_model_market_msg::MarketUiState::filter_focused)
//! and [`MarketUiState::
//! detail_model`](jinn_model_market_msg::MarketUiState::detail_model)
//! themselves — a focused filter turns ENTER into "accept the filter"
//! and ESC into "clear the filter"; detail mode turns ENTER into
//! "confirm the highlighted endpoint" and ESC into "back to the table".
//! The branch order inside every action is filter → detail → table.
//!
//! Fetches ride one key only: the table-mode ENTER drill-in. Highlight
//! motion, sorts, and filter accept/clear publish nothing — the detail
//! content fetches when the user drills into a row, never as a side
//! effect of moving the highlight.

use std::sync::Arc;

use jinn_core_types::model_selection::ModelSelection;
use jinn_kernel::common::app_state::AppState;
use jinn_model_market_msg::FetchEndpoints;
use jinn_model_market_msg::FetchModels;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketRow;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::OpenModelPage;
use jinn_model_market_msg::RefreshAll;
use jinn_model_market_msg::market_ui_scope;
use jinn_preferences_config::protocol::app_state_command::AppStateUpdate;
use jinn_preferences_config::protocol::app_state_command::UpdateAppState;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_provider_selection_msg::SetEndpointDefault;
use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::ActionCtx;
use jinn_slices::route::ActionFn;
use jinn_slices::route::BindSite;
use jinn_slices::route::EditIntent;
use jinn_slices::route::InputHook;
use jinn_slices::route::RouteOutcome;
use jinn_slices::route::RouteRow;
use jinn_slices::route::ScopeSignal;
use jinn_slices::route::SliceActionState;

use crate::ui_state;
use crate::ui_state::DetailRow;

/// The UI cell — the sync side's home for selection, filter, sort, mix.
type MarketUiCell = TypedCell<MarketUiState>;
/// The data cell — the actors' table; read-only from here.
type MarketDataCell = TypedCell<MarketDataState>;

/// The count the navigation keys clamp against: the visible projection's
/// length, computed through the same filter/sort the render pass shows.
fn visible_count(ui: &MarketUiCell, data: &MarketDataCell) -> usize {
    let d = data.read();
    let guard = ui.read();
    ui_state::visible_rows(&d, &guard).len()
}

/// The count the detail-mode navigation keys clamp against: the detail
/// projection's length (Default sentinel + endpoints), or zero while
/// the fetch is in flight.
fn detail_count(ui: &MarketUiCell, data: &MarketDataCell) -> usize {
    let d = data.read();
    let guard = ui.read();
    ui_state::detail_rows(&d, &guard).map_or(0, |rows| rows.len())
}

/// The highlighted row under the current filter/sort, if any.
fn highlighted(ui: &MarketUiCell, data: &MarketDataCell) -> Option<MarketRow> {
    let d = data.read();
    let guard = ui.read();
    ui_state::visible_rows(&d, &guard)
        .get(guard.selection)
        .cloned()
}

/// The `O` result: an `OpenModelPage` command for `model_id` when the
/// popup carries the session stamp the opener laid down, else nothing —
/// the press is consumed either way. Never a fabricated session id:
/// `SessionId::default()` mints a fresh UUID, and a failure report
/// aimed at a nonexistent session would vanish.
fn open_page_result(ui: &MarketUiCell, model_id: Option<String>) -> IntentResult {
    let session_id = {
        let guard = ui.read();
        guard.session_id.clone()
    };
    match (model_id, session_id) {
        (Some(model_id), Some(session_id)) => IntentResult::new_message(OpenModelPage {
            model_id,
            session_id,
        }),
        _ => IntentResult::empty(),
    }
}

/// The popup's keys as data: the keybind footer's content, in display
/// order.
///
/// The footer renders from this, and the composition tests assert every
/// entry here is a key the popup actually honors — a footer that
/// advertises a dead key is a bug. The arrow keys are deliberately
/// absent (`j`/`k` are the advertised twins; the arrows stay unadvertised
/// but bound), and the character keys name their hook-owned meanings:
/// the advertised *behavior* is what the popup honors for each key,
/// whichever mechanism binds it.
pub const MARKET_BINDINGS: &[(&str, &str)] = &[
    ("<enter>", "switch/pin"),
    ("<esc>", "close/back"),
    ("k", "up"),
    ("j", "down"),
    ("<pgup>", "page up"),
    ("<pgdn>", "page dn"),
    ("<c-r>", "refresh"),
    ("h", "col \u{2190}"),
    ("l", "col \u{2192}"),
    ("s", "sort"),
    ("m", "mix"),
    ("O", "web page"),
    ("f", "filter"),
    ("?", "help"),
];

/// The kernel's application state behind an [`ActionCtx`].
fn app<'a>(ctx: &'a mut ActionCtx<'_>) -> Option<&'a mut AppState> {
    ctx.state.as_any_mut()?.downcast_mut::<AppState>()
}

/// Wraps a popup action in an [`ActionFn`], handing it both cells.
fn action<F>(ui: &MarketUiCell, data: &MarketDataCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &MarketUiCell, &MarketDataCell) -> IntentResult
        + Send
        + Sync
        + 'static,
{
    let ui = ui.clone();
    let data = data.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &ui, &data))
}

/// Builds one own-scope row bound to a slice action.
fn row(
    route_id: &'static str,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        route_id: RouteId::new(route_id),
        scope: market_ui_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "model-market",
        outcome: RouteOutcome::Action {
            action: route_id,
            display,
            run,
        },
    }
}

/// Attaches every row the popup owns, plus the opener.
///
/// The opener binds in the `Normal` static scope rather than the popup's
/// own: a key that opens the popup cannot live inside the scope it
/// opens. The four sort/mix actions are deliberately *not* rows — they
/// are hook cases, so a focused filter can own their letters.
pub fn attach_market_rows(routes: &KeyRoutes, ui: &MarketUiCell, data: &MarketDataCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("model-market:open"),
        scope: market_ui_scope(),
        key: "<leader>so",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "model-market",
        outcome: RouteOutcome::Action {
            action: "open-model-market",
            display: "search openrouter models",
            run: action(ui, data, open_market),
        },
    });

    routes.attach(row(
        "confirm-model-market",
        "<enter>",
        "general",
        "switch to the highlighted model",
        action(ui, data, confirm_market),
    ));
    routes.attach(row(
        "close-model-market",
        "<esc>",
        "general",
        "back to the table, or close it",
        action(ui, data, escape_market),
    ));
    routes.attach(row(
        "refresh-model-market",
        "<c-r>",
        "general",
        "drop every cache and refetch",
        action(ui, data, refresh_market),
    ));

    attach_navigation_rows(routes, ui, data);
}

/// Attaches the six navigation rows: arrows and their `j`/`k` twins,
/// plus the two page keys.
///
/// None are `StaticIntent` rows: composition's `static_intent` table
/// knows only six route ids, none of them popup intents, and a row
/// naming an unknown id is silently dropped with a warning. `j`/`k`
/// are rows (not hook cases) because they are non-ambiguous under
/// either focus — but they are character keys, and the popup's scope is
/// an input-hook scope whose printable catch-all binds `j`/`k` to
/// insert-char intents first. The hook therefore re-serves them: a
/// table-focused `j`/`k` moves the selection through
/// [`edit_intent_nav`], a focused one types.
fn attach_navigation_rows(routes: &KeyRoutes, ui: &MarketUiCell, data: &MarketDataCell) {
    for (name, key, display, step) in [
        ("move-model-market-up", "<up>", "move up", Nav::Up),
        ("move-model-market-down", "<down>", "move down", Nav::Down),
        ("page-model-market-up", "<pgup>", "page up", Nav::PageUp),
        (
            "page-model-market-down",
            "<pgdn>",
            "page down",
            Nav::PageDown,
        ),
    ] {
        routes.attach(row(
            name,
            key,
            "navigation",
            display,
            action(ui, data, move |_ctx, ui, data| apply_nav(ui, data, step)),
        ));
    }
}

/// Applies one navigation step, moving the highlight only — no fetch
/// follows, in either mode: table mode fetches on drill-in ENTER
/// alone, detail mode fetches nothing at all.
fn apply_nav(ui: &MarketUiCell, data: &MarketDataCell, step: Nav) -> IntentResult {
    if ui.read().detail_model.is_some() {
        let count = detail_count(ui, data);
        match step {
            Nav::Up => ui.update(|s| {
                s.detail_selection = s.detail_selection.saturating_sub(1);
            }),
            Nav::Down => ui.update(|s| {
                s.detail_selection = (s.detail_selection + 1).min(count.saturating_sub(1));
            }),
            Nav::PageUp => ui.update(|s| {
                s.detail_selection = s.detail_selection.saturating_sub(s.results_viewport);
            }),
            Nav::PageDown => ui.update(|s| {
                s.detail_selection =
                    (s.detail_selection + s.results_viewport).min(count.saturating_sub(1));
            }),
        }
        return IntentResult::empty();
    }
    let count = visible_count(ui, data);
    match step {
        Nav::Up => ui.update(ui_state::move_up),
        Nav::Down => ui.update(|s| ui_state::move_down(s, count)),
        Nav::PageUp => ui.update(ui_state::page_up),
        Nav::PageDown => ui.update(|s| ui_state::page_down(s, count)),
    }
    IntentResult::empty()
}

/// Which navigation key was pressed.
#[derive(Debug, Clone, Copy)]
enum Nav {
    /// `<up>`/`k` — one row.
    Up,
    /// `<down>`/`j` — one row.
    Down,
    /// `<pgup>` — one viewport.
    PageUp,
    /// `<pgdn>` — one viewport.
    PageDown,
}

/// Registers the popup's input hook: the typing carve-out plus every
/// character-key action.
///
/// The composition keymap turns this registration into the scope's
/// printable-character catch-all plus the editing keys — and because
/// the keymap's bindings beat hook consults, `j`/`k` arrive here as
/// insert-char intents and the hook serves navigation for them (they
/// cannot be rows: a row would never win the binding race against the
/// catch-all, and removing the catch-all for two keys is not possible
/// per-scope). The hook:
///
/// - with the filter focused: types every character into the filter
///   (and serves the editing intents);
/// - with the table focused: `f`/`i` focus the filter, `?` toggles the
///   help card, `j`/`k` move the selection, `h`/`l` cycle the sort
///   column, `s` flips the sort direction, `O` opens the highlighted
///   model's openrouter.ai page;
/// - returns `None` otherwise — the key does nothing.
pub fn register_market_input_hook(routes: &KeyRoutes, ui: &MarketUiCell, data: &MarketDataCell) {
    let owned_ui = ui.clone();
    let owned_data = data.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        let focus = owned_ui.read().filter_focused;
        // Every character key classifies through one place, so the
        // "rows steal filter typing" trap is structurally impossible:
        // no character key is a route row.
        if let EditIntent::InsertChar(ch) = intent {
            return match table_char_action(&owned_ui, &owned_data, *ch) {
                CharAction::Acted(result) => Some(result),
                CharAction::Type => {
                    owned_ui.update(|s| {
                        s.filter.push(*ch);
                        s.selection = 0;
                    });
                    Some(IntentResult::empty())
                }
                CharAction::Unclaimed => None,
            };
        }
        match intent {
            EditIntent::Paste(text) if focus => {
                // Pasted text lands in the filter at its end (the filter
                // is a plain string, not a cursor buffer — the same
                // shape the insert/delete cases edit). Table-focused,
                // the arm does not match and the hook declines, so the
                // paste falls to the next surface in the routing chain.
                owned_ui.update(|s| {
                    s.filter.push_str(text);
                    s.selection = 0;
                });
                Some(IntentResult::empty())
            }
            EditIntent::DeleteBackward | EditIntent::DeleteForward if focus => {
                owned_ui.update(|s| {
                    s.filter.pop();
                    s.selection = 0;
                });
                Some(IntentResult::empty())
            }
            // Backspace with the filter unfocused and detail mode
            // active is "back to the table" — the hook arm the editing
            // guards leave open. Table mode keeps declining (the key
            // does nothing there), so focus behavior is bit-identical.
            EditIntent::DeleteBackward if owned_ui.read().detail_model.is_some() => {
                owned_ui.update(MarketUiState::leave_detail);
                Some(IntentResult::empty())
            }
            EditIntent::CursorLeft | EditIntent::CursorRight if focus => {
                // The filter is a plain string, not a cursor buffer: the
                // cursor keys do nothing but stay claimed so they cannot
                // leak into the table while typing.
                Some(IntentResult::empty())
            }
            _ => None,
        }
    });
    routes.register_input_hook(&market_ui_scope(), hook);
}

/// What the hook should do with a character.
#[derive(Debug)]
enum CharAction {
    /// An action fired; the keystroke is consumed. The payload is what
    /// the action publishes — empty for every table-mode action, since
    /// highlight motion and sort/filter keys publish nothing (only the
    /// drill-in ENTER fetches).
    Acted(IntentResult),
    /// The filter is focused: the character is typing.
    Type,
    /// Nobody wants this character; let the intent fall through.
    Unclaimed,
}

/// Classifies one character and runs its mode's action.
///
/// Table-focused `h`/`l`/`s` mutate the sort cell exactly as their
/// pre-hook-route ancestors did; `j`/`k` navigate because the
/// keymap's insert-char catch-all owns those keys in this scope. Every
/// table-mode arm that moves the highlighted row publishes nothing —
/// the only fetch site is the drill-in ENTER. Detail mode's arms mirror
/// them over the mode's own state (`detail_selection`,
/// `detail_sort_col`, `detail_sort_desc`) and publish nothing — the
/// mode never mints fetches, so `fetch_seq` sits still while it is
/// open.
fn table_char_action(ui: &MarketUiCell, data: &MarketDataCell, ch: char) -> CharAction {
    if ui.read().filter_focused {
        return CharAction::Type;
    }
    if ui.read().detail_model.is_some() {
        return detail_char_action(ui, data, ch);
    }
    match ch {
        'f' | 'i' => {
            ui.update(|s| s.filter_focused = true);
            CharAction::Acted(IntentResult::empty())
        }
        '?' => {
            ui.update(|s| s.help_visible = !s.help_visible);
            CharAction::Acted(IntentResult::empty())
        }
        'k' => {
            ui.update(ui_state::move_up);
            CharAction::Acted(IntentResult::empty())
        }
        'j' => {
            // Clamp against the live visible count, not the measured
            // viewport: a filter can shrink the list below the window,
            // and a highlight past the list end simply vanishes.
            let count = visible_count(ui, data);
            ui.update(|s| ui_state::move_down(s, count));
            CharAction::Acted(IntentResult::empty())
        }
        'h' => {
            ui.update(|s| {
                s.sort_col = s.sort_col.prev();
            });
            CharAction::Acted(IntentResult::empty())
        }
        'l' => {
            ui.update(|s| {
                s.sort_col = s.sort_col.next();
            });
            CharAction::Acted(IntentResult::empty())
        }
        's' => {
            ui.update(|s| s.sort_desc = !s.sort_desc);
            CharAction::Acted(IntentResult::empty())
        }
        'm' => {
            // The projection mix: shipped constant ⇄ the session tree's
            // measured usage. The render pass resolves the flag into the
            // active mix each frame (falling back to the constant when
            // the usage has no measured requests), and the PROJ column
            // and its sort re-derive from that.
            ui.update(MarketUiState::toggle_mix_source);
            CharAction::Acted(IntentResult::empty())
        }
        'O' => {
            // The highlighted model's openrouter.ai page: the command
            // carries the listing id and the session stamped at popup
            // open. Nothing here fetches, moves, or closes — the popup
            // and its state stand exactly as they were.
            let model_id = highlighted(ui, data).map(|row| row.id);
            CharAction::Acted(open_page_result(ui, model_id))
        }
        _ => CharAction::Unclaimed,
    }
}

/// Classifies one character in detail mode: `j`/`k` move the detail
/// highlight, `h`/`l` cycle the detail sort columns, `s` flips the
/// detail direction, `m` toggles the projection mix (the detail table's
/// PROJ reads the active mix), `?` toggles the help card, `O` opens the
/// pinned model's page. There is no filter in detail mode — `f`/`i` are
/// unclaimed here and fall through.
fn detail_char_action(ui: &MarketUiCell, data: &MarketDataCell, ch: char) -> CharAction {
    match ch {
        '?' => {
            ui.update(|s| s.help_visible = !s.help_visible);
            CharAction::Acted(IntentResult::empty())
        }
        'k' => {
            ui.update(|s| s.detail_selection = s.detail_selection.saturating_sub(1));
            CharAction::Acted(IntentResult::empty())
        }
        'j' => {
            let count = detail_count(ui, data);
            ui.update(|s| {
                s.detail_selection = (s.detail_selection + 1).min(count.saturating_sub(1));
            });
            CharAction::Acted(IntentResult::empty())
        }
        'h' => {
            ui.update(|s| {
                s.detail_sort_col = s.detail_sort_col.prev();
            });
            CharAction::Acted(IntentResult::empty())
        }
        'l' => {
            ui.update(|s| {
                s.detail_sort_col = s.detail_sort_col.next();
            });
            CharAction::Acted(IntentResult::empty())
        }
        's' => {
            ui.update(|s| s.detail_sort_desc = !s.detail_sort_desc);
            CharAction::Acted(IntentResult::empty())
        }
        'm' => {
            // The same mix toggle as table mode: the detail table's PROJ
            // column and its sort read the frame's active mix too, so
            // the flip re-projects the endpoints in place.
            ui.update(MarketUiState::toggle_mix_source);
            CharAction::Acted(IntentResult::empty())
        }
        'O' => {
            // Detail mode's "highlighted model" is the pin itself: the
            // model the user drilled into. Same command, same actor, no
            // fetch — the detail table stands.
            let model_id = ui.read().detail_model.clone();
            CharAction::Acted(open_page_result(ui, model_id))
        }
        _ => CharAction::Unclaimed,
    }
}

// ── Actions ─────────────────────────────────────────────────────────────

/// Opens the popup: reset the UI cell to the opener defaults, stamp the
/// session the popup opens over (the input hook reads it for `O` — the
/// hook's signature carries no `AppState`), publish `FetchModels` (the
/// actor serves warm cache or network), and push the scope.
fn open_market(ctx: &mut ActionCtx<'_>, ui: &MarketUiCell, _data: &MarketDataCell) -> IntentResult {
    ui.update(|s| {
        s.reset();
        if let Some(state) = app(ctx) {
            s.session_id = Some(state.active_session_id());
        }
    });
    IntentResult::new_message(FetchModels).with_scope_signal(ScopeSignal::Push(market_ui_scope()))
}

/// `<esc>`: a focused filter clears — text emptied, selection reset,
/// focus back on the table; the popup stays open. Nothing publishes —
/// the clear is not highlight motion worth fetching for. Detail mode
/// returns to the table — the pin clears, everything table-side exactly
/// as it was, the popup stays open. Table mode closes the popup.
fn escape_market(
    _ctx: &mut ActionCtx<'_>,
    ui: &MarketUiCell,
    _data: &MarketDataCell,
) -> IntentResult {
    if ui.read().filter_focused {
        ui.update(|s| {
            s.filter.clear();
            s.filter_focused = false;
            s.selection = 0;
        });
        return IntentResult::empty();
    }
    if ui.read().detail_model.is_some() {
        ui.update(MarketUiState::leave_detail);
        return IntentResult::empty();
    }
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(market_ui_scope()))
}

/// ENTER: a focused filter accepts — focus returns to the table with
/// the typed filter still applied and the popup still open; nothing
/// publishes. Detail mode confirms the highlighted detail row: the
/// Default sentinel is the model-only switch (`SetEndpointDefault` with
/// no tag — clearing any pin, OpenRouter auto-routes), a provider row is
/// the model switch plus that endpoint's pin; both close the popup
/// with the scope pop, exactly as table mode does. Table mode drills
/// in — the popup pins the highlighted row's model, requests its
/// endpoints once through the debounced fetch, and stays open.
fn confirm_market(
    ctx: &mut ActionCtx<'_>,
    ui: &MarketUiCell,
    data: &MarketDataCell,
) -> IntentResult {
    if ui.read().filter_focused {
        ui.update(|s| s.filter_focused = false);
        return IntentResult::empty();
    }
    if ui.read().detail_model.is_some() {
        return confirm_detail(ctx, ui, data);
    }
    let highlighted = highlighted(ui, data);
    let Some(row) = highlighted else {
        return IntentResult::empty();
    };
    // Drill in: pin the mode, request the endpoints once. Re-entering
    // a model whose detail is already cached publishes nothing — the
    // has-detail guard in `fetch_for_model` does that.
    let result = match fetch_for_model(ui, data, &row.id) {
        Some(fetch) => IntentResult::new_message(fetch),
        None => IntentResult::empty(),
    };
    ui.update(|s| s.enter_detail(row.id));
    result
}

/// Detail-mode ENTER: the confirm quad over the highlighted detail row
/// — `ProviderSwitch` first, then `UpdateAppState::SetLastModel`, then
/// `SetEndpointDefault` (`None` for the Default sentinel — clear any
/// pin, auto-route; `Some(tag)` for a provider row) — and the scope
/// pop. Model switch then pin land on the same actor mailbox in
/// publish order.
///
/// The row resolves through
/// [`ui_state::highlighted_detail_row`] — the *rendered* (sorted)
/// projection, the same function the view's row-by-index contract goes
/// through — so the pin lands on exactly the row the cursor sat on.
/// `detail_rows` (fetch order) would name a different endpoint whenever
/// the sort reorders the endpoints, which is the wrong-pin bug.
fn confirm_detail(
    ctx: &mut ActionCtx<'_>,
    ui: &MarketUiCell,
    data: &MarketDataCell,
) -> IntentResult {
    let (pinned_model, tag) = {
        let (d, guard) = (data.read(), ui.read());
        match ui_state::highlighted_detail_row(&d, &guard) {
            Some(DetailRow::Default) => (guard.detail_model.clone(), None),
            Some(DetailRow::Endpoint(endpoint)) => {
                (guard.detail_model.clone(), Some(endpoint.tag.clone()))
            }
            None => (None, None),
        }
    };
    let Some(pinned_model) = pinned_model else {
        return IntentResult::empty();
    };
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    // The trait surface a slice action may read; `active_session_id`
    // resolves through it, not through a kernel-only method.
    let session_id = state.active_session_id();
    let model = Some(ModelSelection::Single(pinned_model.clone()));

    IntentResult::new_message(ProviderSwitch {
        session_id,
        provider_id: ModelSelection::Single(pinned_model.clone()),
    })
    .with_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetLastModel(model)],
    })
    .with_message(SetEndpointDefault {
        model: pinned_model,
        tag,
    })
    .with_scope_signal(ScopeSignal::PopIf(market_ui_scope()))
}

/// `<c-r>`: refresh everything. The publish is the whole action — the
/// actor drops the caches, refetches, and reports failures as events.
fn refresh_market(
    _ctx: &mut ActionCtx<'_>,
    _ui: &MarketUiCell,
    _data: &MarketDataCell,
) -> IntentResult {
    IntentResult::new_message(RefreshAll)
}

/// Builds the `FetchEndpoints` request for one model when the data cell
/// does not already carry its detail, stamping the UI cell's sequence
/// for the actor's debounce.
///
/// The drill-in ENTER is the only caller — the one sync-side fetch
/// site. A model whose detail is already cached returns `None`:
/// re-entering a previously viewed model renders instantly, publishing
/// nothing.
fn fetch_for_model(
    ui: &MarketUiCell,
    data: &MarketDataCell,
    model_id: &str,
) -> Option<FetchEndpoints> {
    if has_detail_read(data, model_id) {
        return None;
    }
    let seq = ui.update(|s| {
        s.fetch_seq += 1;
        s.fetch_seq
    });
    Some(FetchEndpoints {
        model_id: model_id.to_owned(),
        seq,
    })
}

/// Whether the data cell already carries detail for `model_id`.
fn has_detail_read(data: &MarketDataCell, model_id: &str) -> bool {
    let d = data.read();
    has_detail(&d, model_id)
}

/// Whether a data snapshot already carries detail for `model_id`.
fn has_detail(data: &MarketDataState, model_id: &str) -> bool {
    data.detail.as_ref().is_some_and(|d| d.model_id == model_id)
}
