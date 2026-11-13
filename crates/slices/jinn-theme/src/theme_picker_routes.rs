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

//! The theme picker's route rows and input hook.
//!
//! Every key the theme picker responds to is a [`RouteRow`] this slice
//! attaches itself; the kernel contributes no keybind, no scope variant, and
//! no picker identifier.
//!
//! Two shapes are worth knowing:
//!
//! - **Actions** carry a closure that runs against the picker's own cell and
//!   reach app state only through [`SliceActionState::as_any_mut`] — the
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

use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteOutcome, RouteRow, ScopeSignal,
};

use crate::theme_picker_actions;
use jinn_theme_msg::{ThemePickerState, theme_entries_slot, theme_picker_scope};

/// The picker's cell — the single home for everything it shows.
type ThemePickerCell = TypedCell<ThemePickerState>;

/// The picker's rows as data: the keys it binds, in footer order.
///
/// The footer renders from this, and a test asserts every entry is a key the
/// picker actually binds — a footer advertising a dead key is a bug.
pub const THEME_PICKER_BINDINGS: &[(&str, &str)] = &[("<enter>", "apply"), ("<esc>", "cancel")];

/// The kernel's application state behind an [`ActionCtx`].
///
/// The preview, confirm, and escape actions must change the app's theme, so
/// they downcast. This is the established slice-side seam; when the state is
/// not the kernel's (a test double), the caller gets `None` and the action
/// declines rather than panicking.
fn app<'a>(ctx: &'a mut ActionCtx<'_>) -> Option<&'a mut jinn_app_state::AppState> {
    ctx.state
        .as_any_mut()?
        .downcast_mut::<jinn_app_state::AppState>()
}

/// Wraps a picker action in an [`ActionFn`], handing it both the dispatch
/// context and the cell.
fn action<F>(cell: &ThemePickerCell, f: F) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &ThemePickerCell) -> IntentResult + Send + Sync + 'static,
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
        scope: theme_picker_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "theme",
        outcome: RouteOutcome::Action {
            action: route_id,
            display,
            run,
        },
    }
}

/// Attaches every row the theme picker owns.
///
/// The opener binds in the `Normal` static scope rather than the picker's own:
/// a key that opens the picker cannot live inside the scope it opens.
pub fn attach_theme_picker_rows(routes: &KeyRoutes, cell: &ThemePickerCell) {
    routes.attach(RouteRow {
        route_id: RouteId::new("theme:open"),
        scope: theme_picker_scope(),
        key: "<leader>sh",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "theme",
        outcome: RouteOutcome::Action {
            action: "open-theme-picker",
            display: "search themes",
            run: action(cell, open_theme_picker),
        },
    });

    routes.attach(row(
        "confirm-theme-picker",
        "<enter>",
        "general",
        "apply the highlighted theme and close",
        action(cell, confirm_theme_picker),
    ));
    routes.attach(row(
        "cancel-theme-picker",
        "<esc>",
        "general",
        "close and restore the theme in force when the picker opened",
        action(cell, cancel_theme_picker),
    ));
    routes.attach(row(
        "new-session-from-theme-picker",
        "<c-n>",
        "general",
        "start a new session",
        action(cell, new_session),
    ));
    routes.attach(row(
        "clear-filter-or-leave-theme-picker",
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
fn attach_navigation_rows(routes: &KeyRoutes, cell: &ThemePickerCell) {
    for (name, key, display, step) in [
        ("move-theme-picker-up", "<up>", "move up", Nav::Up),
        ("move-theme-picker-down", "<down>", "move down", Nav::Down),
        ("page-theme-picker-up", "<pgup>", "page up", Nav::PageUp),
        (
            "page-theme-picker-down",
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
            action(cell, move |ctx, cell| {
                let Some(state) = app(ctx) else {
                    return IntentResult::empty();
                };
                // Snapshot the previewed theme out of the cell, then drop the
                // guard before writing app state.
                let mut highlighted = None;
                cell.update(|picker| {
                    let viewport = picker.results_viewport;
                    match step {
                        Nav::Up => picker.selection.move_up(viewport),
                        Nav::Down => picker.selection.move_down(viewport),
                        Nav::PageUp => picker.selection.page_up(viewport),
                        Nav::PageDown => picker.selection.page_down(viewport),
                    }
                    highlighted = theme_picker_actions::highlighted_theme(picker).cloned();
                });
                // Live preview: the theme under the highlight takes effect the
                // moment the highlight lands on it, and every theme-sensitive
                // cache is dropped so nothing keeps rendering the old colors.
                if let Some(theme) = highlighted {
                    state.frontend.theme = theme;
                    state.invalidate_theme_caches();
                }
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
pub fn register_theme_picker_input_hook(routes: &KeyRoutes, cell: &ThemePickerCell) {
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
    routes.register_input_hook(&theme_picker_scope(), hook);
}

// ── Actions ─────────────────────────────────────────────────────────────

/// Opens the theme picker from anywhere in the app.
///
/// Pushing the scope is enough: the render pass fills the rows from the
/// theme-entries cell, so a caller needs no picker registry and no knowledge
/// of the picker's contents.
#[must_use]
pub fn open_from_scope(state: &mut jinn_app_state::AppState) -> IntentResult {
    state
        .frontend
        .scope_push(jinn_slices::FocusScope::Dynamic(theme_picker_scope()));
    IntentResult::empty()
}

/// Opens the picker: seed the rows from the theme slice's own cell, snapshot
/// the current theme, then push the picker's scope.
fn open_theme_picker(ctx: &mut ActionCtx<'_>, cell: &ThemePickerCell) -> IntentResult {
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    let Some(slices) = state.frontend.slices() else {
        return IntentResult::empty();
    };
    let scanned = slices
        .reader::<jinn_theme_msg::ThemeEntries>(&theme_entries_slot())
        .map_or_default(|themes| themes.read().entries.clone());
    let theme = state.frontend.theme.clone();
    let persisted = state.frontend.app_state.theme_name.clone();

    cell.update(|picker| {
        theme_picker_actions::open(picker, &scanned, &theme, persisted.as_deref());
    });

    state
        .frontend
        .scope_push(jinn_slices::FocusScope::Dynamic(theme_picker_scope()));
    IntentResult::empty()
}

/// Enter: the highlighted theme is already applied (live preview); persist its
/// name and close.
fn confirm_theme_picker(ctx: &mut ActionCtx<'_>, cell: &ThemePickerCell) -> IntentResult {
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    let mut outcome = None;
    cell.update(|picker| {
        outcome = theme_picker_actions::confirm(picker).map(|name| {
            let theme = theme_picker_actions::highlighted_theme(picker).cloned();
            (name, theme)
        });
    });
    let Some((theme_name, theme)) = outcome else {
        return IntentResult::empty();
    };

    // Apply the confirmed theme rather than trusting the preview. Movement
    // previews live, but a confirm that never moved the cursor would otherwise
    // persist a name for a theme that was never put into force.
    if let Some(theme) = theme {
        state.frontend.theme = theme;
        state.invalidate_theme_caches();
    }

    IntentResult::new_message(UpdateAppState {
        updates: vec![AppStateUpdate::SetTheme(Some(theme_name))],
    })
    .with_scope_signal(ScopeSignal::PopIf(theme_picker_scope()))
}

/// Escape: put back the theme that was in force when the picker opened.
///
/// This is the revert path and never the confirm path — a picker's own Enter
/// clears the snapshot, so there is nothing left here to restore.
fn cancel_theme_picker(ctx: &mut ActionCtx<'_>, cell: &ThemePickerCell) -> IntentResult {
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    let mut original = None;
    cell.update(|picker| original = theme_picker_actions::cancel(picker));
    if let Some(original) = original {
        state.frontend.theme = original;
        state.invalidate_theme_caches();
    }
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(theme_picker_scope()))
}

/// Starts a new session, as the key does from any picker.
fn new_session(ctx: &mut ActionCtx<'_>, _cell: &ThemePickerCell) -> IntentResult {
    let config = ctx.config;
    let Some(state) = app(ctx) else {
        return IntentResult::empty();
    };
    jinn_app_state::session_creation::intent::handle_session_new(state, config)
}

/// Clears a non-empty filter, or closes the picker when the filter is empty.
fn clear_filter_or_leave(ctx: &mut ActionCtx<'_>, cell: &ThemePickerCell) -> IntentResult {
    let mut leave = false;
    cell.update(|picker| {
        if picker.selection.filter().is_empty() {
            leave = true;
        } else {
            picker.selection.clear_filter();
        }
    });
    if leave {
        return cancel_theme_picker(ctx, cell);
    }
    IntentResult::empty()
}
