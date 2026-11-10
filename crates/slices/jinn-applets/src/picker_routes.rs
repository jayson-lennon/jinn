//! The applets picker's route rows, hooks, and activation wiring.
//!
//! Every key the applets picker responds to is a [`RouteRow`] this slice
//! attaches itself; the kernel contributes no keybind, no scope variant,
//! and no picker identifier. The structure mirrors the persona and
//! workflows pickers.
//!
//! Keys:
//!
//! - `<leader>sa` opens (a `StaticScopes` row in `Normal` — a key that
//!   opens a scope cannot live inside it). The opener publishes
//!   [`RescanApplets`](jinn_applets_msg::RescanApplets) and
//!   [`WakeAppletsServer`](jinn_applets_msg::WakeAppletsServer) *and*
//!   pushes the scope: the async rescan is the discovery model, so the
//!   first frame shows the previous cell contents and the store actor's
//!   republish repaints in place a few ms later (the accepted
//!   one-frame-stale open), and the wake binds the server so the
//!   footer's status line can show the resolved address on a fresh run.
//! - `ENTER` publishes [`OpenApplet`](jinn_applets_msg::OpenApplet): the store
//!   actor ensures the server is bound (lazy bind on the first open),
//!   builds the URL, and launches the OS browser. Route actions are
//!   synchronous, so the ensure-and-launch lives in the actor.
//! - `<c-r>` pushes the rename popup. A modified key, deliberately: the
//!   filter's input hook captures bare letters, so an unmodified `r`
//!   must stay a filter keystroke.
//! - `<c-d>` publishes [`DeleteApplet`](jinn_applets_msg::DeleteApplet); the
//!   store actor removes the directory and the row, and the republisher
//!   repaints the still-open picker.
//! - `<esc>` leaves the picker to the prior scope.
//!
//! The filter is an *input hook*, not rows. The scope-enter hook resets
//! the picker from the picker cell — synchronously, no IO: the hook's
//! contract. Disk reads happen only inside the store actor's async
//! handler.
//!
//! A cell guard is never held across an await: every action snapshots
//! what it needs, mutates, and drops the guard before it returns.

use std::sync::Arc;

use jinn_applets_msg::{
    AppletsPickerState, DeleteApplet, OpenApplet, RescanApplets, WakeAppletsServer,
    applets_picker_scope,
};
use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteOutcome, RouteRow, ScopeEnterHook,
    ScopeSignal,
};

use crate::picker_actions;

/// The picker's cell — the single home for everything it shows.
pub type AppletsPickerCell = TypedCell<AppletsPickerState>;

/// The picker's rows as data: the keys it binds, in footer order.
///
/// The footer renders from this, and a test asserts every entry is a key
/// the picker actually binds — a footer advertising a dead key is a bug.
pub const APPLETS_PICKER_BINDINGS: &[(&str, &str)] = &[
    ("<enter>", "open in browser"),
    ("<c-r>", "rename"),
    ("<c-d>", "delete"),
    ("<esc>", "close"),
];

/// Every key the picker binds in its own scope, for the wiring test.
#[must_use]
pub fn bound_keys() -> Vec<&'static str> {
    APPLETS_PICKER_BINDINGS
        .iter()
        .map(|(key, _)| *key)
        .collect()
}

/// Wraps a picker action in an [`ActionFn`], handing it the picker cell.
fn action<F>(picker: &AppletsPickerCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &AppletsPickerCell) -> IntentResult + Send + Sync + 'static,
{
    let picker = picker.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &picker))
}

/// Builds an own-scope row bound to a slice action.
fn row(
    route_id: &'static str,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        route_id: RouteId::new(route_id),
        scope: applets_picker_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "applets",
        outcome: RouteOutcome::Action {
            action: route_id,
            display,
            run,
        },
    }
}

/// Attaches every row the applets picker owns.
///
/// The opener binds in the `Normal` static scope rather than the picker's
/// own: a key that opens the picker cannot live inside the scope it opens.
/// The remaining rows are `OwnScope`, so they bind only while the picker
/// is on top and cannot shadow another slice's keys.
pub fn attach_applets_picker_rows(routes: &KeyRoutes, picker: &AppletsPickerCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("applets:open"),
        scope: applets_picker_scope(),
        key: "<leader>sa",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "applets",
        outcome: RouteOutcome::Action {
            action: "open-applets-picker",
            display: "search applets",
            run: ActionFn::new(move |_ctx| {
                // Publish the rescan, the server wake, and the scope
                // push in one action: the scope signal is applied
                // before the messages publish, so the enter hook's
                // reset from the cell has already run when the store's
                // republish lands. The wake binds the server on the
                // first open, so the footer's status line shows the
                // resolved address without opening an applet first;
                // its fact comes back through the republisher's fold.
                IntentResult::new_message(RescanApplets)
                    .with_message(WakeAppletsServer)
                    .with_scope_signal(ScopeSignal::Push(applets_picker_scope()))
            }),
        },
    });

    routes.attach(row(
        "open-highlighted-app",
        "<enter>",
        "general",
        "open the highlighted app in your browser",
        action(picker, open_highlighted_app),
    ));
    routes.attach(row(
        "rename-highlighted-app",
        "<c-r>",
        "general",
        "rename the highlighted app",
        action(picker, rename_highlighted_app),
    ));
    routes.attach(row(
        "delete-highlighted-app",
        "<c-d>",
        "general",
        "delete the highlighted app",
        action(picker, delete_highlighted_app),
    ));
    routes.attach(row(
        "cancel-applets-picker",
        "<esc>",
        "general",
        "close the applets picker",
        action(picker, cancel_apps_picker),
    ));

    attach_navigation_rows(routes, picker);
}

/// Attaches the four list-navigation rows.
fn attach_navigation_rows(routes: &KeyRoutes, picker: &AppletsPickerCell) {
    for (name, key, display, step) in [
        ("move-applets-picker-up", "<up>", "move up", Nav::Up),
        ("move-applets-picker-down", "<down>", "move down", Nav::Down),
        ("page-applets-picker-up", "<pgup>", "page up", Nav::PageUp),
        (
            "page-applets-picker-down",
            "<pgdn>",
            "page down",
            Nav::PageDown,
        ),
    ] {
        let picker = picker.clone();
        routes.attach(RouteRow {
            route_id: RouteId::new(name),
            scope: applets_picker_scope(),
            key,
            category: "navigation",
            site: BindSite::OwnScope,
            feature: "applets",
            outcome: RouteOutcome::Action {
                action: name,
                display,
                run: ActionFn::new(move |_ctx| {
                    picker.update(|state| {
                        let viewport = state.results_viewport;
                        match step {
                            Nav::Up => state.selection.move_up(viewport),
                            Nav::Down => state.selection.move_down(viewport),
                            Nav::PageUp => state.selection.page_up(viewport),
                            Nav::PageDown => state.selection.page_down(viewport),
                        }
                    });
                    IntentResult::empty()
                }),
            },
        });
    }
}

/// Which list-navigation key was pressed.
#[derive(Debug, Clone, Copy)]
enum Nav {
    /// `<up>` — one row.
    Up,
    /// `<down>` — one row.
    Down,
    /// `<pgup>` — half the visible window.
    PageUp,
    /// `<pgdn>` — half the visible window.
    PageDown,
}

/// Registers the picker's filter editing hook.
///
/// The composition keymap turns this registration into the scope's
/// printable-character catch-all plus the editing keys, which is why
/// typing in the filter needs no rows of its own — and why `<c-r>` and
/// `<c-d>` must be modified keys: a bare `r` or `d` lands here as a
/// filter character.
pub fn register_applets_picker_input_hook(routes: &KeyRoutes, picker: &AppletsPickerCell) {
    let owned = picker.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        owned.update(|state| match intent {
            EditIntent::InsertChar(ch) => state.selection.insert_char(*ch),
            EditIntent::DeleteBackward | EditIntent::DeleteForward => {
                state.selection.backspace();
            }
            EditIntent::CursorLeft => state.selection.move_cursor_left(),
            EditIntent::CursorRight => state.selection.move_cursor_right(),
            // The filter is a single-line box: home/end have no meaning here.
            EditIntent::CursorHome | EditIntent::CursorEnd => {}
            EditIntent::Paste(text) => state.selection.insert_text(text),
        });
        Some(IntentResult::empty())
    });
    routes.register_input_hook(&applets_picker_scope(), hook);
}

/// Registers the picker's scope-enter hook: the one place its per-open
/// state is reset — synchronously, from the picker cell only.
///
/// Every opener lands on the same fresh menu: filter cleared, highlight
/// on row 0. The rows themselves come from the picker cell as the
/// republisher last rebuilt them; no disk read happens here — the hook's
/// contract is sync-only. The opener's `RescanApplets` publish is what
/// brings fresh rows, via the store actor and the republisher's
/// in-place repaint.
pub fn register_applets_picker_enter_hook(routes: &KeyRoutes, picker: &AppletsPickerCell) {
    let owned = picker.clone();
    let hook: ScopeEnterHook = Arc::new(move |_ctx: ActionCtx<'_>| {
        owned.update(AppletsPickerState::reset);
    });
    routes.register_scope_enter_hook(&applets_picker_scope(), hook);
}

// ── Actions ─────────────────────────────────────────────────────────────

/// `ENTER`: publish the open command for the highlighted app and close
/// the picker.
///
/// The store actor does the ensure-bind, the URL build, and the browser
/// launch; the picker's action only snapshots the highlighted id,
/// publishes, and pops. An empty picker (nothing highlighted) is a
/// no-op — nothing opens, so the picker stays.
fn open_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppletsPickerCell) -> IntentResult {
    let Some(id) = picker.read().highlighted_id() else {
        return IntentResult::empty();
    };
    IntentResult::new_message(OpenApplet { id })
        .with_scope_signal(ScopeSignal::PopIf(applets_picker_scope()))
}

/// `<c-r>`: push the rename popup, seeded with the highlighted title.
///
/// A no-op on an empty picker.
fn rename_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppletsPickerCell) -> IntentResult {
    picker_actions::begin_rename(picker)
}

/// `<c-d>`: delete the highlighted app — publish the delete command.
///
/// The store actor removes the directory and the row, and the
/// republisher repaints the still-open picker. A no-op on an empty
/// picker.
fn delete_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppletsPickerCell) -> IntentResult {
    let Some(id) = picker.read().highlighted_id() else {
        return IntentResult::empty();
    };
    IntentResult::new_message(DeleteApplet { id })
}

/// `<esc>`: close without changing anything.
fn cancel_apps_picker(_ctx: &mut ActionCtx<'_>, _picker: &AppletsPickerCell) -> IntentResult {
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(applets_picker_scope()))
}

/// Repaints the picker's rows from a catalog.
///
/// Called by the republisher actor when a store event announces a
/// change. Rebuilds the rows from the catalog in its order (the store
/// owns recency ordering), keeping the highlight on the applet the user
/// was looking at when it survived the change — and keeping the filter,
/// which the user typed and a background repaint must not clear.
pub fn republish_from_catalog(
    cell: &AppletsPickerCell,
    catalog: &jinn_applets_msg::AppletsCatalog,
) {
    cell.update(|state| {
        let highlight = state.highlighted_id();
        let theme = state.theme.clone();
        let items = jinn_applets_msg::build_applet_entries(&catalog.entries, &theme);
        state.selection.set_items(items);
        if let Some(id) = highlight {
            restore_highlight(state, &id);
        }
    });
}

/// Puts the highlight back on `id` after a repaint, if it survived.
fn restore_highlight(state: &mut AppletsPickerState, id: &str) {
    if let Some(index) = state
        .selection
        .items()
        .iter()
        .position(|item| item.entry().id == id)
    {
        state.selection.set_selection(index);
    }
}

/// Activates the applets picker: registers its overlay, view, slot, and
/// selectable geometry; attaches its rows, filter hook, and enter hook;
/// and attaches the rename popup's rows and input hook on its own
/// dynamic scope.
///
/// Split from the slice's `activate` because it needs the same host but
/// is not part of actor spawning, and because the tests drive it with a
/// cell directly. The picker's cell is not minted here: the shared cell
/// catalog registers every slice cell in one place, before any slice
/// activates, so this resolves the handle the same way every other
/// consumer does.
pub fn activate_picker(
    host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>,
    picker: &AppletsPickerCell,
) {
    let scope = applets_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(crate::picker_render::applets_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_applets_msg::applets_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(crate::picker_render::render_applets_picker),
    );

    // The picker's keys, its filter's input hook, and the scope-enter
    // hook that resets the menu are all this slice's own.
    attach_applets_picker_rows(host.key_routes(), picker);
    register_applets_picker_input_hook(host.key_routes(), picker);
    register_applets_picker_enter_hook(host.key_routes(), picker);

    // The rename popup: its own overlay (the session-rename geometry),
    // its rows, and its input hook, on its own dynamic scope.
    let rename = crate::rename_popup::rename_scope();
    host.register_overlay(
        rename.clone(),
        std::sync::Arc::new(crate::rename_popup::rename_overlay_rect),
    );
    host.register_overlay_selectable(&rename);
    host.register_overlay_slot(rename.clone(), jinn_applets_msg::applets_picker_slot());
    host.register_overlay_view(
        rename,
        std::sync::Arc::new(crate::rename_popup::render_rename_overlay),
    );
    crate::rename_popup::attach_rename_rows(host.key_routes(), picker);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_app_state::AppState;
    use jinn_applets_msg::{AppletEntry, AppletsPickerState, applets_picker_slot};
    use jinn_slices::ScopeSignal;
    use jinn_slices::cell::TypedCell;

    use super::applets_picker_scope;

    /// The picker wired exactly as composition wires it: `activate_picker`
    /// over a catalog-seeded registry, so a test cannot pass against
    /// wiring production never runs.
    fn wired() -> (
        jinn_slices::Slices,
        jinn_slices::KeyRoutes,
        TypedCell<AppletsPickerState>,
    ) {
        let slices = jinn_slices::Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        let cell = slices
            .reader::<AppletsPickerState>(&applets_picker_slot())
            .expect("the catalog registers the picker cell the activation wires");
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let routes = jinn_slices::KeyRoutes::new();
        let system = trouper::system::ActorSystem::test().0;
        let mut host =
            jinn_slices::SliceHost::new(&slices, &mut viewport, &overlay_views, &routes, &system);
        super::activate_picker(&mut host, &cell);
        (slices, routes, cell)
    }

    /// Fires the opener action the `<leader>sa` row binds.
    fn fire_opener(
        slices: &jinn_slices::Slices,
        routes: &jinn_slices::KeyRoutes,
        config: &jinn_config::ConfigLayer,
    ) -> jinn_slices::RouteResult {
        let mut state = AppState::default();
        routes
            .action_for(
                &jinn_slices::DynamicIntent::new(
                    applets_picker_scope(),
                    "open-applets-picker",
                    "open-applets-picker",
                ),
                jinn_slices::ActionCtx {
                    state: &mut state,
                    slices,
                    config,
                    key_bytes: Vec::new(),
                },
            )
            .expect("the opener row attaches an action named open-applets-picker")
    }

    #[rstest::rstest]
    #[test]
    fn opening_the_picker_publishes_the_rescan_and_the_wake() {
        // Given the picker wired the way activation wires it.
        let (slices, routes, _cell) = wired();
        let config = jinn_config::empty_config_layer().clone();

        // When firing the opener.
        let result = fire_opener(&slices, &routes, &config);

        // Then the result carries both messages — the rescan for fresh
        // rows and the wake that binds the server on a fresh run.
        assert_eq!(result.message_names, ["RescanApplets", "WakeAppletsServer"]);
    }

    #[rstest::rstest]
    #[test]
    fn opening_the_picker_pushes_its_scope() {
        // Given the picker wired the way activation wires it.
        let (slices, routes, _cell) = wired();
        let config = jinn_config::empty_config_layer().clone();

        // When firing the opener.
        let result = fire_opener(&slices, &routes, &config);

        // Then the result pushes the picker's scope — applied by the
        // handler before the messages publish.
        assert_eq!(
            result.scope_signal,
            Some(ScopeSignal::Push(applets_picker_scope()))
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_enter_hook_resets_the_menu_but_keeps_the_serving_phase() {
        // Given the wired picker whose cell already carries a serving
        // phase and a filter the user typed.
        let (slices, routes, cell) = wired();
        let config = jinn_config::empty_config_layer().clone();
        cell.update(|state| {
            state.apply_serving(true, "127.0.0.1:1");
            state.selection.insert_char('x');
        });

        // When the opener's scope push lands (the enter hook runs).
        let hook = routes
            .scope_enter_hook(&applets_picker_scope())
            .expect("activation registers the picker's enter hook");
        let mut state = AppState::default();
        hook(jinn_slices::ActionCtx {
            state: &mut state,
            slices: &slices,
            config: &config,
            key_bytes: Vec::new(),
        });

        // Then the menu is reset but the serving phase survived — no
        // offline flash between the open and the wake event's fold.
        let guard = cell.read();
        assert_eq!(guard.selection.filter(), "");
        assert!(
            guard.serving.is_some(),
            "the reset preserved the serving phase"
        );
    }

    /// One catalog row, for the repaint test.
    fn alpha_entry() -> AppletEntry {
        AppletEntry {
            id: "a".to_owned(),
            title: "Alpha".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn republish_rebuilds_rows_without_touching_the_serving_phase() {
        // Given the wired picker serving at a resolved bind, empty rows.
        let (_slices, _routes, cell) = wired();
        cell.update(|state| {
            state.apply_serving(true, "127.0.0.1:41234");
        });

        // When a catalog change republishes the rows.
        let catalog = jinn_applets_msg::AppletsCatalog {
            entries: vec![alpha_entry()],
        };
        super::republish_from_catalog(&cell, &catalog);

        // Then the row landed and the serving phase is untouched — a
        // catalog repaint owns rows, never the footer's phase.
        let guard = cell.read();
        assert_eq!(guard.selection.items().len(), 1);
        assert_eq!(
            guard
                .selection
                .items()
                .first()
                .map(|item| item.entry().id.as_str()),
            Some("a")
        );
        assert!(guard.serving.is_some());
    }
}
