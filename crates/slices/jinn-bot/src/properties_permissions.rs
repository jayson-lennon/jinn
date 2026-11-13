//! The permission engine behind the properties form's per-field setting.
//!
//! Each of a bot's tool and skill filters is a set of names and globs,
//! and the form's job is to let the user move one entry in or out of that set
//! with a direction and a set-mode. This owns that: which field a row is
//! setting, what the next set-mode is in the current direction, what the
//! bot may actually reach right now, and whether the filter it is about
//! to write already holds a pattern.
//!
//! It is separate from the rendering and from the save path because it is the
//! only part that answers questions about the bot's *current* reach,
//! and that reach is derived rather than stored: a name the bot cannot
//! use is not part of what a freeze is meant to preserve, so the answer is
//! computed from the registry and the enablement filters each time rather than
//! read from a snapshot that could disagree with them.

use std::collections::BTreeSet;

use jinn_bot_msg::{PickDirection, PopupStatus, PropertyField, SetField, SetMode};
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::route::ActionCtx;

use super::properties_overlay::{BotPropertiesCell, app};

/// The `h`/`l` action: acts on whichever row the form cursor is on.
///
/// Five of the seven rows are decided entirely from the popup's own cell, so
/// they go straight to [`BotPropertiesState::pick`]. The two set rows
/// are the exception: freezing one is a statement about the bot's
/// present capabilities, and reading those means reaching `AppState` — the
/// same way [`save_bot`] and [`commit_pending_to_session`] do.
///
/// The key walks the row rather than toggling it. Every other choice row in
/// the form moves one position per press and stops at its ends, and a row
/// that reads identically to those should not behave differently under the
/// same key: `h` on the leftmost choice is a no-op rather than a flip to the
/// rightmost, and `l` on the rightmost is a no-op rather than a re-freeze
/// that would recapture against a state the user has since changed.
///
/// The direction is still consumed by the *outcome*, which is what a
/// two-state row cannot avoid: moving onto Frozen is the freeze, whatever
/// came from.
pub(super) fn pick_on(
    ctx: &mut ActionCtx<'_>,
    cell: &BotPropertiesCell,
    direction: PickDirection,
) -> IntentResult {
    let Some(field) = set_field_of(cell.read().focus) else {
        cell.update(|popup| popup.pick(direction));
        return IntentResult::empty();
    };
    let Some(bot_id) = cell.read().session_id.clone() else {
        return IntentResult::empty();
    };
    let current = cell.read().set_mode_of(field);
    let Some(next) = next_set_mode(current, direction) else {
        // Already at the end the key points at. Unlike the cell-only rows,
        // this leaves the status line alone: there is nothing to report
        // about a key that walked to the end of its row, and the row's
        // current value is still on it.
        return IntentResult::empty();
    };
    let thawing = next == SetMode::Live;
    // A thaw has nothing to read: the capture is discarded and the
    // bot's filter is left for the commit to leave alone.
    let permitted = if thawing {
        BTreeSet::new()
    } else {
        permitted_now(ctx, &bot_id, field)
    };
    // A capture that came back empty is a set frozen to nothing, and the
    // commit writes it: an allow list naming nothing is a filter that
    // withholds every name, so nothing has to be reported about it. A glob
    // in the bot's filter is the one thing a flip does silently
    // change, so that is what the line is for.
    let dropped = !thawing && contains_glob(ctx, &bot_id, field);
    cell.update(|popup| {
        popup.set_mode(field, next, &permitted);
        if dropped {
            popup.report(PopupStatus::GlobDropped { field });
        }
    });
    IntentResult::empty()
}

/// The mode one position along a set row from `current`, or `None` when the
/// key points past the end.
///
/// The two choices run left to right — `live` then `frozen` — so the row is
/// a window with two positions in it and the keys move within it. Stopping
/// at the ends is what makes the row honest: `h` says "left", and on the
/// leftmost row there is nothing to its left.
pub(super) fn next_set_mode(current: SetMode, direction: PickDirection) -> Option<SetMode> {
    match (direction, current) {
        (PickDirection::Right, SetMode::Live) => Some(SetMode::Frozen),
        (PickDirection::Left, SetMode::Frozen) => Some(SetMode::Live),
        _ => None,
    }
}

/// The set row a form field names, or `None` for the five rows that are
/// decided from the cell alone.
pub(super) fn set_field_of(field: PropertyField) -> Option<SetField> {
    match field {
        PropertyField::ToolSet => Some(SetField::Tool),
        PropertyField::SkillSet => Some(SetField::Skill),
        PropertyField::Trigger
        | PropertyField::Behavior
        | PropertyField::PrepMode
        | PropertyField::Model
        | PropertyField::SeedTemplate => None,
    }
}

/// The names the bot currently permits for `field`.
///
/// The sources are the ones a picker seeds its rows from, narrowed by the
/// bot's own filter and — for tools — by the provider gate, because a
/// name that is refused at dispatch however the filter is written is not
/// worth freezing in: it would make `jinn.toml` longer to read and change
/// nothing.
///
/// This is the same conjunction the context assembler applies, and it is
/// deliberately derived rather than read from a registry: a name the
/// bot cannot use is not part of what freezing is meant to preserve.
pub(super) fn permitted_now(
    ctx: &mut ActionCtx<'_>,
    bot_id: &jinn_core_types::SessionId,
    field: SetField,
) -> BTreeSet<String> {
    let Some(state) = app(ctx) else {
        return BTreeSet::new();
    };
    let Some(session) = state.session.get(bot_id) else {
        return BTreeSet::new();
    };
    match field {
        SetField::Skill => session
            .discovered_skills()
            .iter()
            .filter(|skill| session.is_skill_enabled(&skill.name))
            .map(|skill| skill.name.clone())
            .collect(),
        SetField::Tool => {
            let provider = session.model_selection().provider_name().to_owned();
            state.tool_registry().map_or_default(|registry| {
                registry
                    .read()
                    .tools_for_session(bot_id)
                    .into_iter()
                    .filter(|def| session.is_tool_enabled(&def.name))
                    .filter(|def| def.available_for_provider(&provider))
                    .map(|def| def.name)
                    .collect()
            })
        }
    }
}

/// Whether the bot's own filter for `field` holds a glob pattern.
///
/// Only the bot's *own* filter is asked. An inherited parent pattern
/// is not the user's, and reporting a drop they did not make — or dropping
/// a pattern that never applied to this bot — would put a message on
/// the status line about an edit that was not made.
pub(super) fn contains_glob(
    ctx: &mut ActionCtx<'_>,
    bot_id: &jinn_core_types::SessionId,
    field: SetField,
) -> bool {
    let Some(state) = app(ctx) else {
        return false;
    };
    let Some(session) = state.session.get(bot_id) else {
        return false;
    };
    let filter = match field {
        SetField::Tool => session.tool_filter(),
        SetField::Skill => session.skill_filter(),
    };
    filter.is_some_and(|filter| filter.names.iter().any(|pattern| is_glob(pattern)))
}

/// Whether `pattern` is a glob rather than a plain name.
///
/// A pattern that would not compile as a glob is a literal — the same
/// reading `NameFilter` gives it when matching — so an uncompilable pattern
/// is not reported as one.
fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}
