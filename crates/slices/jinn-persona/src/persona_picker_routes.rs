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

//! The persona picker's route rows and input hook.
//!
//! Every key the persona picker responds to is a [`RouteRow`] this slice
//! attaches itself; the kernel contributes no keybind, no scope variant, and
//! no picker identifier. The structure mirrors the skills picker's, which is
//! what makes adding a thirteenth picker a folder-local change.
//!
//! Two shapes are worth knowing:
//!
//! - **Actions** carry a closure that runs against the picker's own cell and
//!   reach session state only through [`SliceActionState::as_any_mut`] — the
//!   same seam `jinn-sidebar`, `jinn-term`, `jinn-project`, and
//!   `jinn-session-lifecycle` already use.
//! - **The filter** is an *input hook* rather than rows, because `RouteRow` has
//!   no catch-all variant. Registering the hook makes the composition keymap
//!   synthesize the printable-character catch-all and the editing keys for
//!   this scope, so typing keeps working.
//!
//! A cell guard is never held across an await: every action snapshots what it
//! needs, mutates, and drops the guard before it returns.

use std::sync::Arc;

use jinn_core_types::DEFAULT_PERSONA_NAME;
use jinn_persona_msg::{PersonaPickerState, persona_picker_scope, personas_slot};
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_session_msg::MarkSessionInteracted;
use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteOutcome, RouteRow, ScopeEnterHook,
    ScopeSignal,
};

use crate::persona_picker_actions;

/// The picker's cell — the single home for everything it shows.
type PersonaPickerCell = TypedCell<PersonaPickerState>;

/// The picker's rows as data: the keys it binds, in footer order.
///
/// The footer renders from this, and a test asserts every entry is a key the
/// picker actually binds — a footer advertising a dead key is a bug.
pub const PERSONA_PICKER_BINDINGS: &[(&str, &str)] = &[("<enter>", "apply"), ("<esc>", "cancel")];

/// The kernel's application state behind an [`ActionCtx`].
///
/// The open and confirm actions must change session state, so they downcast.
/// This is the established slice-side seam; when the state is not the kernel's
/// (a test double), the caller gets `None` and the action declines rather than
/// panicking.
fn app<'a>(ctx: &'a mut ActionCtx<'_>) -> Option<&'a mut jinn_app_state::AppState> {
    ctx.state
        .as_any_mut()?
        .downcast_mut::<jinn_app_state::AppState>()
}

/// Wraps a picker action in an [`ActionFn`], handing it both the dispatch
/// context and the cell.
fn action<F>(cell: &PersonaPickerCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &PersonaPickerCell) -> IntentResult + Send + Sync + 'static,
{
    let cell = cell.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &cell))
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
        scope: persona_picker_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "persona",
        outcome: RouteOutcome::Action {
            action: route_id,
            display,
            run,
        },
    }
}

/// Attaches every row the persona picker owns.
///
/// The opener binds in the `Normal` static scope rather than the picker's own:
/// a key that opens the picker cannot live inside the scope it opens.
pub fn attach_persona_picker_rows(routes: &KeyRoutes, cell: &PersonaPickerCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("persona:open"),
        scope: persona_picker_scope(),
        key: "<leader>se",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "persona",
        outcome: RouteOutcome::Action {
            action: "open-persona-picker",
            display: "search personas",
            run: action(cell, open_persona_picker),
        },
    });

    routes.attach(row(
        "confirm-persona-picker",
        "<enter>",
        "general",
        "apply the highlighted persona and close",
        action(cell, confirm_persona_picker),
    ));
    routes.attach(row(
        "cancel-persona-picker",
        "<esc>",
        "general",
        "close without changing the active persona",
        action(cell, cancel_persona_picker),
    ));
    routes.attach(row(
        "new-session-from-persona-picker",
        "<c-n>",
        "general",
        "start a new session",
        action(cell, new_session),
    ));
    routes.attach(row(
        "clear-filter-or-leave-persona-picker",
        "<c-c>",
        "general",
        "clear the filter, or close when already empty",
        action(cell, clear_filter_or_leave),
    ));

    attach_navigation_rows(routes, cell);
}

/// Attaches the four list-navigation rows.
///
/// They cannot be `StaticIntent` rows: composition's `static_intent` table
/// knows only six route ids, none of them picker intents, and a row naming an
/// unknown id is silently dropped with a warning. So the picker implements its
/// own navigation.
fn attach_navigation_rows(routes: &KeyRoutes, cell: &PersonaPickerCell) {
    for (name, key, display, step) in [
        ("move-persona-picker-up", "<up>", "move up", Nav::Up),
        ("move-persona-picker-down", "<down>", "move down", Nav::Down),
        ("page-persona-picker-up", "<pgup>", "page up", Nav::PageUp),
        (
            "page-persona-picker-down",
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
            action(cell, move |_ctx, cell| {
                cell.update(|picker| {
                    let viewport = picker.results_viewport;
                    match step {
                        Nav::Up => picker.selection.move_up(viewport),
                        Nav::Down => picker.selection.move_down(viewport),
                        Nav::PageUp => picker.selection.page_up(viewport),
                        Nav::PageDown => picker.selection.page_down(viewport),
                    }
                });
                IntentResult::empty()
            }),
        ));
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
/// printable-character catch-all plus the editing keys, which is why typing in
/// the filter needs no rows of its own.
pub fn register_persona_picker_input_hook(routes: &KeyRoutes, cell: &PersonaPickerCell) {
    // The hook outlives this call, so it owns the cell rather than borrowing it.
    let owned = cell.clone();
    let hook: InputHook = Arc::new(move |intent: &EditIntent| {
        owned.update(|picker| match intent {
            EditIntent::InsertChar(ch) => picker.selection.insert_char(*ch),
            EditIntent::DeleteBackward | EditIntent::DeleteForward => {
                picker.selection.backspace();
            }
            EditIntent::CursorLeft => picker.selection.move_cursor_left(),
            EditIntent::CursorRight => picker.selection.move_cursor_right(),
            // The filter is a single-line box: home/end have no meaning here.
            EditIntent::CursorHome | EditIntent::CursorEnd => {}
            EditIntent::Paste(text) => picker.selection.insert_text(text),
        });
        Some(IntentResult::empty())
    });
    routes.register_input_hook(&persona_picker_scope(), hook);
}

/// Registers the picker's scope-enter hook: the one place its per-open
/// state is built.
///
/// Every opener — this slice's `<leader>se` row and the sidebar's persona
/// section key — does nothing but request the transition, so both land on the
/// same fresh menu: filter cleared, highlight on the active session's
/// persona, rows rebuilt from the personas cell. The rows come from the
/// personas this slice already owns, so entering needs no loader command and
/// no kernel involvement.
pub fn register_persona_picker_enter_hook(routes: &KeyRoutes, cell: &PersonaPickerCell) {
    // The hook outlives this call, so it owns the cell rather than borrowing it.
    let owned = cell.clone();
    let hook: ScopeEnterHook = Arc::new(move |mut ctx: ActionCtx<'_>| {
        // Only the theme falls back when the state is not the kernel's (a test
        // double); the rows must be seeded either way.
        let (theme, active) = app(&mut ctx).map_or_else(
            || (jinn_theme::default_theme(), DEFAULT_PERSONA_NAME.to_owned()),
            |state| {
                (
                    state.frontend.theme.clone(),
                    state.active_session().persona_name().to_owned(),
                )
            },
        );
        // Snapshot the personas into an owned list before touching the picker:
        // the personas cell's guard and the picker's guard must not overlap.
        let entries = ctx
            .slices
            .reader::<jinn_persona_msg::Personas>(&personas_slot())
            .map_or_else(Vec::new, |personas| personas.read().entries.clone());
        owned.update(|picker| {
            persona_picker_actions::open(picker, &entries, Some(&active), &theme);
        });
    });
    routes.register_scope_enter_hook(&persona_picker_scope(), hook);
}

// ── Actions ─────────────────────────────────────────────────────────────

/// Opens the picker from anywhere in the app.
///
/// Requesting the transition is all an opener does: the picker's scope-enter
/// hook builds the rows and clears the filter, so every opener shows the same
/// fresh menu.
fn open_persona_picker(_ctx: &mut ActionCtx<'_>, _cell: &PersonaPickerCell) -> IntentResult {
    IntentResult::empty().with_scope_signal(ScopeSignal::Push(persona_picker_scope()))
}

/// Enter: bind the highlighted persona to the active session, persist it as
/// the last-persona seed, and close.
///
/// The active session's profile persona is the only persona authority: the
/// binding is the whole effect. The persisted `SetPersona` seeds the startup
/// session on the next launch, exactly like `last_model`.
fn confirm_persona_picker(ctx: &mut ActionCtx<'_>, cell: &PersonaPickerCell) -> IntentResult {
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    let Some(name) = cell
        .read()
        .selection
        .selected_item()
        .map(|item| item.entry().name.clone())
    else {
        return IntentResult::empty();
    };

    // Bind it to the active session.
    let session_id = state.session.active_session_id().clone();
    state.active_session_mut().set_persona_name(name.clone());

    IntentResult::new_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetPersona(Some(name))],
    })
    .with_message(MarkSessionInteracted { session_id })
    .with_scope_signal(ScopeSignal::PopIf(persona_picker_scope()))
}

/// Escape: close without changing anything.
///
/// The persona picker keeps no snapshot — it has no staged edit to undo, since
/// a persona only changes on confirm.
fn cancel_persona_picker(_ctx: &mut ActionCtx<'_>, _cell: &PersonaPickerCell) -> IntentResult {
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(persona_picker_scope()))
}

/// Starts a new session, as the key does from any picker.
fn new_session(ctx: &mut ActionCtx<'_>, _cell: &PersonaPickerCell) -> IntentResult {
    let config = ctx.config;
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    jinn_app_state::session_creation::intent::handle_session_new(state, config)
}

/// Clears a non-empty filter, or closes the picker when the filter is empty.
fn clear_filter_or_leave(ctx: &mut ActionCtx<'_>, cell: &PersonaPickerCell) -> IntentResult {
    let mut leave = false;
    cell.update(|picker| {
        if picker.selection.filter().is_empty() {
            leave = true;
        } else {
            picker.selection.clear_filter();
        }
    });
    if leave {
        return cancel_persona_picker(ctx, cell);
    }
    IntentResult::empty()
}
