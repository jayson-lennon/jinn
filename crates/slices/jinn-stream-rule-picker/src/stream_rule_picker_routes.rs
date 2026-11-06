//! The stream-rule picker's route rows and input hook.
//!
//! Fully slice-owned, like every picker: each key the picker responds to is
//! a [`RouteRow`] the slice attaches itself; the kernel contributes no
//! keybind, no scope variant, and no picker identifier.
//!
//! Two shapes are worth knowing:
//!
//! - **Actions** carry a closure that runs against the picker's own cell,
//!   and reach the configuration layer only through [`ActionCtx`] — the
//!   established seam.
//! - **The filter** is an *input hook* rather than rows, because `RouteRow`
//!   has no catch-all variant. Registering the hook makes the composition
//!   keymap synthesize the printable-character catch-all and the editing
//!   keys for this scope automatically, so typing keeps working.
//!
//! A cell guard is never held across a slow step: the confirm snapshots
//! the pending set, drops the guard, and hands the set to the persist
//! actor — the picker itself never reads a file after the open.

use std::sync::Arc;

use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::cell::TypedCell;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, EditIntent, InputHook, RouteOutcome, RouteRow, ScopeSignal,
};
use jinn_stream_rule_picker_msg::PersistStreamRuleDisabled;
use jinn_stream_rule_picker_msg::StreamRuleDisabledChange;
use jinn_stream_rule_picker_msg::StreamRulePickerState;
use jinn_stream_rule_picker_msg::stream_rule_picker_scope;

use crate::stream_rule_picker_actions;
use marked_path::{Absolute, MarkedPathBuf};

/// The picker's cell — the single home for everything it shows.
type StreamRulePickerCell = TypedCell<StreamRulePickerState>;

/// The picker's rows as data: the keys it binds, in footer order.
///
/// The footer renders from this, and a test asserts every entry here is a
/// key the picker actually binds — a footer that advertises a dead key is a
/// bug.
pub const STREAM_RULE_PICKER_BINDINGS: &[(&str, &str)] = &[
    ("<tab>", "toggle"),
    ("<enter>", "confirm"),
    ("<esc>", "cancel"),
];

/// Every key the picker binds in its own scope, for the wiring test.
#[must_use]
pub fn bound_keys() -> Vec<&'static str> {
    STREAM_RULE_PICKER_BINDINGS
        .iter()
        .map(|(key, _)| *key)
        .collect()
}

/// Wraps a picker action in an [`ActionFn`], handing it the dispatch
/// context, the cell, and the run's preferences path captured at activation.
fn action<F>(
    cell: &StreamRulePickerCell,
    preferences_path: &MarkedPathBuf<Absolute>,
    f: F,
) -> ActionFn
where
    F: Fn(&mut ActionCtx<'_>, &StreamRulePickerCell, &MarkedPathBuf<Absolute>) -> IntentResult
        + Send
        + Sync
        + 'static,
{
    let cell = cell.clone();
    let preferences_path = preferences_path.clone();
    ActionFn::new(move |mut ctx| f(&mut ctx, &cell, &preferences_path))
}

/// Builds one `Action` row binding `key` in the picker's own scope.
fn row(
    action_name: &'static str,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        route_id: RouteId::new(action_name),
        scope: stream_rule_picker_scope(),
        key,
        category,
        site: BindSite::OwnScope,
        feature: "stream-rule-picker",
        outcome: RouteOutcome::Action {
            action: action_name,
            display,
            run,
        },
    }
}

/// Attaches every row the stream-rule picker owns.
///
/// The opener binds in the `Normal` static scope rather than the picker's
/// own: a key that opens the picker cannot live inside the scope it opens.
/// The remaining rows are `OwnScope`, so they bind only while the picker is
/// on top and cannot shadow another slice's keys.
pub fn attach_stream_rule_picker_rows(
    routes: &KeyRoutes,
    cell: &StreamRulePickerCell,
    preferences_path: &MarkedPathBuf<Absolute>,
) {
    routes.attach(RouteRow {
        route_id: RouteId::new("stream-rule-picker:open"),
        scope: stream_rule_picker_scope(),
        key: "<leader>sx",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "stream-rule-picker",
        outcome: RouteOutcome::Action {
            action: "open-stream-rule-picker",
            display: "search stream rules",
            run: action(cell, preferences_path, open_stream_rule_picker),
        },
    });

    routes.attach(row(
        "toggle-stream-rule-picker",
        "<tab>",
        "general",
        "toggle the highlighted rule",
        action(cell, preferences_path, toggle_highlighted_rule),
    ));
    routes.attach(row(
        "confirm-stream-rule-picker",
        "<enter>",
        "general",
        "save the pending disabled set",
        action(cell, preferences_path, confirm_stream_rule_picker),
    ));
    routes.attach(row(
        "cancel-stream-rule-picker",
        "<esc>",
        "general",
        "close without changing any rule",
        action(cell, preferences_path, cancel_stream_rule_picker),
    ));

    attach_navigation_rows(routes, cell, preferences_path);
}

/// Attaches the four list-navigation rows.
///
/// They cannot be `StaticIntent` rows: composition's `static_intent` table
/// knows only shared-chrome intents, none of them picker navigation, and a
/// row naming an unknown id is silently dropped with a warning. So the
/// picker implements its own navigation, the same shape the skill picker
/// uses.
fn attach_navigation_rows(
    routes: &KeyRoutes,
    cell: &StreamRulePickerCell,
    preferences_path: &MarkedPathBuf<Absolute>,
) {
    for (name, key, display, step) in [
        ("move-stream-rule-picker-up", "<up>", "move up", Nav::Up),
        (
            "move-stream-rule-picker-down",
            "<down>",
            "move down",
            Nav::Down,
        ),
        (
            "page-stream-rule-picker-up",
            "<pgup>",
            "page up",
            Nav::PageUp,
        ),
        (
            "page-stream-rule-picker-down",
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
            action(cell, preferences_path, move |ctx, cell, _| {
                navigate(ctx, cell, step);
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

/// Moves the highlight within the measured viewport.
fn navigate(_ctx: &mut ActionCtx<'_>, cell: &StreamRulePickerCell, nav: Nav) {
    cell.update(|picker| {
        let viewport = picker.results_viewport;
        match nav {
            Nav::Up => picker.selection.move_up(viewport),
            Nav::Down => picker.selection.move_down(viewport),
            Nav::PageUp => picker.selection.page_up(viewport),
            Nav::PageDown => picker.selection.page_down(viewport),
        }
    });
}

/// Registers the picker's filter editing hook.
///
/// The composition keymap turns this registration into the scope's
/// printable-character catch-all plus the editing keys, which is why typing
/// in the filter needs no rows of its own.
pub fn register_stream_rule_picker_input_hook(routes: &KeyRoutes, cell: &StreamRulePickerCell) {
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
    routes.register_input_hook(&stream_rule_picker_scope(), hook);
}

// ── Actions ─────────────────────────────────────────────────────────────

/// Opens the picker: seed the rows from the assembled rules and push the
/// picker's scope.
///
/// The rule list is launch-global — one `jinn.toml`, every session — so the
/// seed reads the same assembly the matcher enforces: the embedded shipped
/// set when no user file exists, the user `stream_rules.toml` set when it
/// does, the jinn.toml entries, and the legacy conversions, deduped by
/// name. A listed rule is an enforced rule, which is what makes "toggle it
/// off" mean anything. The preferences path arrives captured at activation
/// — the same path the confirm writes through. The `disabled_rules` array
/// rides the same layer and seeds each row's pending state at the persisted
/// value, so reopening after a confirm shows what the file holds.
fn open_stream_rule_picker(
    ctx: &mut ActionCtx<'_>,
    cell: &StreamRulePickerCell,
    preferences_path: &MarkedPathBuf<Absolute>,
) -> IntentResult {
    let rules = jinn_preferences_config::assemble_stream_rules(ctx.config, preferences_path);
    let disabled_names: std::collections::HashSet<String> = ctx
        .config
        .get::<jinn_preferences_config::StreamRulesConfig>()
        .unwrap_or_default()
        .disabled_rules
        .into_iter()
        .collect();
    // The theme the rows were last painted with, so a reopen does not
    // recolor under the user's eyes; a first open falls back to the default.
    let theme = cell.read().theme.clone();

    cell.update(|picker| {
        stream_rule_picker_actions::open(picker, &rules, &disabled_names, &theme);
    });

    IntentResult::empty().with_scope_signal(ScopeSignal::Push(stream_rule_picker_scope()))
}

/// `<tab>`: flip the highlighted rule's pending state, then advance the
/// highlight one row down — the toggle-list walk, so N presses from the top
/// mark the first N rules; the advance clamps at the last row, where the
/// key keeps flipping it. The picker stays open and the file is untouched
/// — the write belongs to the confirm.
fn toggle_highlighted_rule(
    _ctx: &mut ActionCtx<'_>,
    cell: &StreamRulePickerCell,
    _preferences_path: &MarkedPathBuf<Absolute>,
) -> IntentResult {
    cell.update(stream_rule_picker_actions::toggle_highlighted);
    IntentResult::empty()
}

/// `<enter>`: hand the pending set to the persist actor and close.
///
/// The confirm performs **no write and no validation I/O**: it reads the
/// picker's own cell twice (snapshot and status line), clones the
/// preferences path, and returns carrying [`PersistStreamRuleDisabled`] —
/// session, preferences path, and every pending change — with the pop
/// signal, so the picker closes while the writes are still in flight.
/// Nothing on this path touches the config layer or the files: an
/// earlier revision pre-checked every pending name against the assembled
/// rule set here, one full assembly per pending row, and that read — not
/// the handoff — was the visible hitch on confirm.
///
/// Name validation therefore lives solely in the slice's persist actor,
/// against the same assembled set the open listed and the matcher
/// enforces. A name the files no longer hold closes nothing at confirm
/// time — the picker is already gone — and reports later as a chat error
/// (the shared `stream_rule_failure_line` wording) with no matcher
/// refresh: the TOCTOU boundary between open and write is the actor's
/// to report, not this picker's.
fn confirm_stream_rule_picker(
    ctx: &mut ActionCtx<'_>,
    cell: &StreamRulePickerCell,
    preferences_path: &MarkedPathBuf<Absolute>,
) -> IntentResult {
    // Snapshot the pending set and drop the guard; the only other cell
    // touch is the status line below. Everything past this point is
    // in-memory and O(rows).
    let pending: Vec<(String, bool)> =
        cell.update(|picker| stream_rule_picker_actions::pending_set(picker));

    cell.update(|picker| {
        picker.status = Some(confirmation_status(&pending));
    });
    let changes = pending
        .into_iter()
        .map(|(name, disabled)| StreamRuleDisabledChange { name, disabled })
        .collect();
    IntentResult::empty()
        .with_message(PersistStreamRuleDisabled {
            session_id: ctx.state.active_session_id(),
            preferences_path: preferences_path.clone(),
            changes,
        })
        .with_scope_signal(ScopeSignal::PopIf(stream_rule_picker_scope()))
}

/// The status line a confirmed set leaves: what the set now is, and
/// when it bites.
///
/// "Shortly", not "on restart": the confirm publishes the persist command,
/// and the persist actor publishes the matcher-reload event after the
/// writes land — the stream-rules slice's matcher actor recompiles then.
/// The refresh is eventual, so the wording claims immediacy only as far as
/// the code guarantees it.
fn confirmation_status(pending: &[(String, bool)]) -> String {
    let disabled: Vec<&str> = pending
        .iter()
        .filter(|(_, disabled)| *disabled)
        .map(|(name, _)| name.as_str())
        .collect();
    if disabled.is_empty() {
        "stream rules: all armed (takes effect shortly)".to_owned()
    } else {
        format!(
            "stream rules: disabled {} (takes effect shortly)",
            disabled.join(", ")
        )
    }
}

/// `<esc>`: close without writing anything.
///
/// No cell mutation at all: the pending set simply dies with the picker's
/// next open, which reseeds every row from the file. Cancel cannot leave a
/// partial write behind because it performs no write.
fn cancel_stream_rule_picker(
    _ctx: &mut ActionCtx<'_>,
    _cell: &StreamRulePickerCell,
    _preferences_path: &MarkedPathBuf<Absolute>,
) -> IntentResult {
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(stream_rule_picker_scope()))
}
