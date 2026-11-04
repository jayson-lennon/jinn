//! The apps picker's route rows, hooks, and activation wiring.
//!
//! Every key the apps picker responds to is a [`RouteRow`] this slice
//! attaches itself; the kernel contributes no keybind, no scope variant,
//! and no picker identifier. The structure mirrors the persona and
//! workflows pickers.
//!
//! Keys:
//!
//! - `<leader>sa` opens (a `StaticScopes` row in `Normal` — a key that
//!   opens a scope cannot live inside it). The opener publishes
//!   [`RescanApps`](jinn_apps_msg::RescanApps) *and* pushes the scope:
//!   the async rescan is the discovery model, so the first frame shows
//!   the previous cell contents and the store actor's republish
//!   repaints in place a few ms later (the accepted one-frame-stale
//!   open).
//! - `ENTER` publishes [`OpenApp`](jinn_apps_msg::OpenApp): the store
//!   actor ensures the server is bound (lazy bind on the first open),
//!   builds the URL, and launches the OS browser. Route actions are
//!   synchronous, so the ensure-and-launch lives in the actor.
//! - `<c-r>` pushes the rename popup. A modified key, deliberately: the
//!   filter's input hook captures bare letters, so an unmodified `r`
//!   must stay a filter keystroke.
//! - `<c-d>` publishes [`DeleteApp`](jinn_apps_msg::DeleteApp); the
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

use jinn_apps_msg::{AppsPickerState, DeleteApp, OpenApp, RescanApps, apps_picker_scope};
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
pub type AppsPickerCell = TypedCell<AppsPickerState>;

/// The picker's rows as data: the keys it binds, in footer order.
///
/// The footer renders from this, and a test asserts every entry is a key
/// the picker actually binds — a footer advertising a dead key is a bug.
pub const APPS_PICKER_BINDINGS: &[(&str, &str)] = &[
    ("<enter>", "open in browser"),
    ("<c-r>", "rename"),
    ("<c-d>", "delete"),
    ("<esc>", "close"),
];

/// Every key the picker binds in its own scope, for the wiring test.
#[must_use]
pub fn bound_keys() -> Vec<&'static str> {
    APPS_PICKER_BINDINGS.iter().map(|(key, _)| *key).collect()
}

/// Wraps a picker action in an [`ActionFn`], handing it the picker cell.
fn action<F>(picker: &AppsPickerCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &AppsPickerCell) -> IntentResult + Send + Sync + 'static,
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
        scope: apps_picker_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "apps",
        outcome: RouteOutcome::Action {
            action: route_id,
            display,
            run,
        },
    }
}

/// Attaches every row the apps picker owns.
///
/// The opener binds in the `Normal` static scope rather than the picker's
/// own: a key that opens the picker cannot live inside the scope it opens.
/// The remaining rows are `OwnScope`, so they bind only while the picker
/// is on top and cannot shadow another slice's keys.
pub fn attach_apps_picker_rows(routes: &KeyRoutes, picker: &AppsPickerCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("apps:open"),
        scope: apps_picker_scope(),
        key: "<leader>sa",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "apps",
        outcome: RouteOutcome::Action {
            action: "open-apps-picker",
            display: "search apps",
            run: ActionFn::new(move |_ctx| {
                // Publish the rescan and push the scope in one action:
                // the scope signal is applied before the messages
                // publish, so the enter hook's reset from the cell has
                // already run when the store's republish lands.
                IntentResult::new_message(RescanApps)
                    .with_scope_signal(ScopeSignal::Push(apps_picker_scope()))
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
        "cancel-apps-picker",
        "<esc>",
        "general",
        "close the apps picker",
        action(picker, cancel_apps_picker),
    ));

    attach_navigation_rows(routes, picker);
}

/// Attaches the four list-navigation rows.
fn attach_navigation_rows(routes: &KeyRoutes, picker: &AppsPickerCell) {
    for (name, key, display, step) in [
        ("move-apps-picker-up", "<up>", "move up", Nav::Up),
        ("move-apps-picker-down", "<down>", "move down", Nav::Down),
        ("page-apps-picker-up", "<pgup>", "page up", Nav::PageUp),
        (
            "page-apps-picker-down",
            "<pgdn>",
            "page down",
            Nav::PageDown,
        ),
    ] {
        let picker = picker.clone();
        routes.attach(RouteRow {
            route_id: RouteId::new(name),
            scope: apps_picker_scope(),
            key,
            category: "navigation",
            site: BindSite::OwnScope,
            feature: "apps",
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
pub fn register_apps_picker_input_hook(routes: &KeyRoutes, picker: &AppsPickerCell) {
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
    routes.register_input_hook(&apps_picker_scope(), hook);
}

/// Registers the picker's scope-enter hook: the one place its per-open
/// state is reset — synchronously, from the picker cell only.
///
/// Every opener lands on the same fresh menu: filter cleared, highlight
/// on row 0. The rows themselves come from the picker cell as the
/// republisher last rebuilt them; no disk read happens here — the hook's
/// contract is sync-only. The opener's `RescanApps` publish is what
/// brings fresh rows, via the store actor and the republisher's
/// in-place repaint.
pub fn register_apps_picker_enter_hook(routes: &KeyRoutes, picker: &AppsPickerCell) {
    let owned = picker.clone();
    let hook: ScopeEnterHook = Arc::new(move |_ctx: ActionCtx<'_>| {
        owned.update(AppsPickerState::reset);
    });
    routes.register_scope_enter_hook(&apps_picker_scope(), hook);
}

// ── Actions ─────────────────────────────────────────────────────────────

/// `ENTER`: publish the open command for the highlighted app.
///
/// The store actor does the ensure-bind, the URL build, and the browser
/// launch; the picker's action only snapshots the highlighted id and
/// publishes. An empty picker (nothing highlighted) is a no-op.
fn open_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppsPickerCell) -> IntentResult {
    let Some(id) = picker.read().highlighted_id() else {
        return IntentResult::empty();
    };
    IntentResult::new_message(OpenApp { id })
}

/// `<c-r>`: push the rename popup, seeded with the highlighted title.
///
/// A no-op on an empty picker.
fn rename_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppsPickerCell) -> IntentResult {
    picker_actions::begin_rename(picker)
}

/// `<c-d>`: delete the highlighted app — publish the delete command.
///
/// The store actor removes the directory and the row, and the
/// republisher repaints the still-open picker. A no-op on an empty
/// picker.
fn delete_highlighted_app(_ctx: &mut ActionCtx<'_>, picker: &AppsPickerCell) -> IntentResult {
    let Some(id) = picker.read().highlighted_id() else {
        return IntentResult::empty();
    };
    IntentResult::new_message(DeleteApp { id })
}

/// `<esc>`: close without changing anything.
fn cancel_apps_picker(_ctx: &mut ActionCtx<'_>, _picker: &AppsPickerCell) -> IntentResult {
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(apps_picker_scope()))
}

/// Repaints the picker's rows from a catalog.
///
/// Called by the republisher actor when a store event announces a
/// change. Rebuilds the rows from the catalog in its order (the store
/// owns recency ordering), keeping the highlight on the app the user
/// was looking at when it survived the change — and keeping the filter,
/// which the user typed and a background repaint must not clear.
pub fn republish_from_catalog(cell: &AppsPickerCell, catalog: &jinn_apps_msg::AppCatalog) {
    cell.update(|state| {
        let highlight = state.highlighted_id();
        let theme = state.theme.clone();
        let items = jinn_apps_msg::build_app_entries(&catalog.entries, &theme);
        state.selection.set_items(items);
        if let Some(id) = highlight {
            restore_highlight(state, &id);
        }
    });
}

/// Puts the highlight back on `id` after a repaint, if it survived.
fn restore_highlight(state: &mut AppsPickerState, id: &str) {
    if let Some(index) = state
        .selection
        .items()
        .iter()
        .position(|item| item.entry().id == id)
    {
        state.selection.set_selection(index);
    }
}

/// Activates the apps picker: registers its overlay, view, slot, and
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
    picker: &AppsPickerCell,
) {
    let scope = apps_picker_scope();
    host.register_overlay(
        scope.clone(),
        std::sync::Arc::new(crate::picker_render::apps_picker_overlay_rect),
    );
    host.register_overlay_selectable(&scope);
    host.register_overlay_slot(scope.clone(), jinn_apps_msg::apps_picker_slot());
    host.register_overlay_view(
        scope,
        std::sync::Arc::new(crate::picker_render::render_apps_picker),
    );

    // The picker's keys, its filter's input hook, and the scope-enter
    // hook that resets the menu are all this slice's own.
    attach_apps_picker_rows(host.key_routes(), picker);
    register_apps_picker_input_hook(host.key_routes(), picker);
    register_apps_picker_enter_hook(host.key_routes(), picker);

    // The rename popup: its own overlay (the session-rename geometry),
    // its rows, and its input hook, on its own dynamic scope.
    let rename = crate::rename_popup::rename_scope();
    host.register_overlay(
        rename.clone(),
        std::sync::Arc::new(crate::rename_popup::rename_overlay_rect),
    );
    host.register_overlay_selectable(&rename);
    host.register_overlay_slot(rename.clone(), jinn_apps_msg::apps_picker_slot());
    host.register_overlay_view(
        rename,
        std::sync::Arc::new(crate::rename_popup::render_rename_overlay),
    );
    crate::rename_popup::attach_rename_rows(host.key_routes(), picker);
}
