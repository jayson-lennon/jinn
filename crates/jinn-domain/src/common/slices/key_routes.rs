//! Kernel re-exports of the slice keybind routing mechanics.
//!
//! The route table moved to [`jinn_slices::route`] so slice crates can
//! register rows without depending on the kernel; this module is the
//! historical import path inside `jinn-domain`. It also provides the
//! kernel-side glue the mechanics cannot own: the [`Intent`] →
//! [`EditIntent`] translation and [`AppState`]'s implementation of
//! [`SliceActionState`].

pub use jinn_slices::route::ActionCtx;
pub use jinn_slices::route::ActionFn;
pub use jinn_slices::route::BindSite;
use jinn_slices::route::EditIntent;
pub use jinn_slices::route::InputHook;
pub use jinn_slices::route::KeyRoutes;
pub use jinn_slices::route::PublishClosure;
pub use jinn_slices::route::RouteId;
pub use jinn_slices::route::RouteOutcome;
pub use jinn_slices::route::RouteResult;
pub use jinn_slices::route::RouteRow;
pub use jinn_slices::route::ScopeSignal;
pub use jinn_slices::route::SliceActionState;

use crate::common::app_state::AppState;
use crate::common::app_state::FocusScope;
use crate::protocol::intent::Intent;
use crate::protocol::intent::IntentResult;

impl SliceActionState for AppState {
    fn active_session_title(&self) -> Option<String> {
        self.active_session().title().map(str::to_owned)
    }

    fn active_session_id(&self) -> jinn_core_types::SessionId {
        self.session.active_session_id().clone()
    }

    fn push_session_error(&mut self, message: &str) {
        self.active_session_mut()
            .push_entry(crate::feat::session::chat_entry::ChatEntry::error(message));
    }
}

/// Translates the kernel's editing intents into the slice-hook
/// vocabulary.
///
/// `None` means the intent is not an editing surface action — hooks are
/// never consulted for it.
#[must_use]
pub fn as_edit_intent(intent: &Intent) -> Option<EditIntent> {
    match intent {
        Intent::InsertChar { ch } => Some(EditIntent::InsertChar(*ch)),
        Intent::DeleteGrapheme => Some(EditIntent::DeleteBackward),
        Intent::DeleteGraphemeForward => Some(EditIntent::DeleteForward),
        Intent::MoveCursorLeft => Some(EditIntent::CursorLeft),
        Intent::MoveCursorRight => Some(EditIntent::CursorRight),
        Intent::MoveCursorToStart => Some(EditIntent::CursorHome),
        Intent::MoveCursorToEnd => Some(EditIntent::CursorEnd),
        _ => None,
    }
}

/// Converts the kernel's [`IntentResult`] into the slice-level
/// [`RouteResult`] (they are the same shape; this erases the alias).
#[must_use]
pub fn into_route_result(result: IntentResult) -> RouteResult {
    RouteResult {
        messages: result.messages,
        message_names: result.message_names,
        scope_signal: result.scope_signal,
    }
}

/// Converts a slice-level [`RouteResult`] back into the kernel's
/// [`IntentResult`] alias.
#[must_use]
pub fn from_route_result(result: RouteResult) -> IntentResult {
    IntentResult {
        messages: result.messages,
        message_names: result.message_names,
        scope_signal: result.scope_signal,
    }
}

/// Applies a route action's scope transition to the scope stack.
///
/// The handler is the exempt scope-stack writer; this is the only
/// place a slice-requested transition lands.
pub fn apply_scope_signal(result: &mut IntentResult, state: &mut AppState) {
    let Some(signal) = result.scope_signal.take() else {
        return;
    };
    match signal {
        ScopeSignal::Push(id) => state.frontend.scope_push(FocusScope::Dynamic(id)),
        ScopeSignal::PopIf(id) => {
            if matches!(&state.frontend.scope(), FocusScope::Dynamic(cur) if *cur == id) {
                state.frontend.scope_pop();
            }
        }
    }
}
