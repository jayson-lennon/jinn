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

//! Paste routing — deciding which surface claims pasted text.
//!
//! A paste is not a keystroke and no keymap row can produce one: the terminal
//! reports the event, the TUI mints the intent, and the code here decides which
//! surface claims the bytes. Resolution runs one fixed chain, most specific
//! first: the focused slice's input hook, then the focused scope's own paste
//! row, then the chat input box. The first two are allowed to decline, in which
//! case the next surface is tried; the chat input box is the terminal sink and
//! claims anything left over.
//!
//! Split out of [`super::handler`] because this is the only place that knows
//! about slice hooks and paste rows, and it answers a routing question rather
//! than dispatching an intent.

use super::handler::apply_scope_signal;
use crate::AppState;
use crate::IntentResult;
use crate::protocol::KernelIntent;

/// Dispatches the active dynamic scope's registered input hook.
///
/// A hit means the keystroke belonged to the slice's own input surface:
/// the hook performed the synchronous write and the intent is consumed.
/// Returns `None` outside dynamic scopes or when no hook is registered
/// (or the hook declines the intent) — the caller falls through to the
/// built-in arms.
pub(crate) fn try_slice_input_hook(
    intent: &KernelIntent,
    state: &mut AppState,
    routes: &jinn_slices::route::KeyRoutes,
) -> Option<IntentResult> {
    use jinn_slices::FocusScope;
    let FocusScope::Dynamic(scope) = &state.frontend.scope() else {
        return None;
    };
    let hook = routes.input_hook(scope)?;
    // Hooks speak the slice-level editing vocabulary, not the kernel's
    // full intent enum: translate, and skip hooks for non-editing
    // intents entirely.
    let edit = edit_intent_for(intent)?;
    hook(&edit)
}

/// Translates an intent into the slice-hook editing vocabulary.
///
/// Every editing key arrives as a dynamic intent the keymap minted for the
/// hook's scope, with a printable character carried in the byte payload.
/// `None` means the intent is not an editing surface action — hooks are
/// never consulted for it.
#[must_use]
fn edit_intent_for(intent: &KernelIntent) -> Option<jinn_slices::EditIntent> {
    let KernelIntent::Dynamic(dynamic) = intent else {
        return None;
    };
    Some(match dynamic.action.as_str() {
        "insert-char" => jinn_slices::EditIntent::InsertChar(
            std::str::from_utf8(&dynamic.bytes)
                .ok()
                .and_then(|text| text.chars().next())
                .unwrap_or_default(),
        ),
        "delete-backward" => jinn_slices::EditIntent::DeleteBackward,
        "delete-forward" => jinn_slices::EditIntent::DeleteForward,
        "move-cursor-left" => jinn_slices::EditIntent::CursorLeft,
        "move-cursor-right" => jinn_slices::EditIntent::CursorRight,
        "move-cursor-home" => jinn_slices::EditIntent::CursorHome,
        "move-cursor-end" => jinn_slices::EditIntent::CursorEnd,
        jinn_chat_input_msg::PASTE_TEXT_ACTION => jinn_slices::EditIntent::Paste(
            std::str::from_utf8(&dynamic.bytes)
                .map(std::borrow::ToOwned::to_owned)
                .unwrap_or_default(),
        ),
        _ => return None,
    })
}

/// Mints the dynamic intent a bracketed paste travels as.
///
/// A paste is not a keystroke and no keymap row can produce it: the
/// terminal reports the event, the TUI mints this intent, and the kernel's
/// paste branch decides which surface claims the text. A paste carries
/// nothing but its bytes, so `scope` is supplied by the routing step rather
/// than chosen here.
fn paste_intent(scope: jinn_slices::SliceScopeId, text: String) -> jinn_slices::DynamicIntent {
    jinn_slices::DynamicIntent::with_bytes(
        scope,
        jinn_chat_input_msg::PASTE_TEXT_ACTION,
        "paste text",
        text.into_bytes(),
    )
}

/// Reads a paste intent's payload back as text.
///
/// Invalid UTF-8 is dropped: a paste that cannot be decoded has nothing
/// well-defined to insert anywhere.
fn paste_text_of(intent: &KernelIntent) -> Option<String> {
    let KernelIntent::Dynamic(dynamic) = intent else {
        return None;
    };
    (dynamic.action == jinn_chat_input_msg::PASTE_TEXT_ACTION)
        .then(|| String::from_utf8_lossy(&dynamic.bytes).into_owned())
        .filter(|text| !text.is_empty())
}

/// Delivers a bracketed paste to the surface that holds focus.
///
/// Returns [`None`] when `intent` is not a paste, so the caller falls
/// through to ordinary dispatch. Otherwise it returns the paste's whole
/// outcome — a paste never falls through.
///
/// Resolution order, first match wins:
///
/// 1. The focused dynamic scope's input hook, as
///    [`jinn_slices::EditIntent::Paste`]. A hook that returns [`None`] has
///    *declined*: the paste is consumed and dropped there, never re-offered.
///    A surface that says "not mine" must not have its text land somewhere
///    the user is not looking. (A scope with no hook at all has not
///    declined — that is case 4, not this one.)
/// 2. The focused scope's own paste row, when that slice attached one.
///    The intent is re-minted against the focused scope so
///    [`jinn_slices::route::KeyRoutes::action_for`] — which matches on
///    `(row.scope, action)`, not on focus — finds it.
/// 3. The chat input box, when focus is not a dynamic slice scope: in
///    [`jinn_slices::FocusScope::Normal`] and
///    [`jinn_slices::FocusScope::Input`] the box is the surface under the
///    cursor. The box's own `paste-text` row performs the insert, so this is
///    the one path where the box is named — as a dispatch target, not as a
///    routing decision.
/// 4. Dropped, for a dynamic scope with neither a hook nor a row.
///
/// A paste is not a keypress, so it deliberately does not run
/// [`dismiss_unrelated_prompts`]'s dismissal: a confirmation prompt is
/// dismissed by the key that follows the paste, not by the paste itself.
pub(crate) fn route_paste(
    intent: &KernelIntent,
    state: &mut AppState,
    slices: &jinn_slices::Slices,
    routes: &jinn_slices::route::KeyRoutes,
    config: &jinn_config::ConfigLayer,
) -> Option<IntentResult> {
    use jinn_slices::FocusScope;
    let text = paste_text_of(intent)?;
    let bytes = text.clone().into_bytes();

    let focused = state.frontend.scope().clone();
    let Some(scope) = (match &focused {
        FocusScope::Dynamic(scope) => Some(scope.clone()),
        _ => None,
    }) else {
        // 3. Not a dynamic slice scope: the chat input box owns the paste.
        return Some(dispatch_paste_row(
            jinn_chat_input_msg::chat_input_scope(),
            text,
            state,
            slices,
            routes,
            config,
            bytes,
        ));
    };

    // 1. The focused scope's own input hook. A decline is final.
    let probe = KernelIntent::Dynamic(paste_intent(scope.clone(), text.clone()));
    if let Some(result) = try_slice_input_hook(&probe, state, routes) {
        return Some(result);
    }

    // 2 then 4. The focused scope's own paste row; dropped when there is none.
    Some(dispatch_paste_row(
        scope, text, state, slices, routes, config, bytes,
    ))
}

/// Runs the paste row attached to `scope`, if there is one.
///
/// A row reads its payload from the ctx rather than from the intent, so
/// the bytes travel twice: in the intent (the route-table key is
/// `(row.scope, action)`) and in the ctx (the action's input). No row
/// means no surface claimed the paste, and the result is empty.
fn dispatch_paste_row(
    scope: jinn_slices::SliceScopeId,
    text: String,
    state: &mut AppState,
    slices: &jinn_slices::Slices,
    routes: &jinn_slices::route::KeyRoutes,
    config: &jinn_config::ConfigLayer,
    bytes: Vec<u8>,
) -> IntentResult {
    routes
        .action_for(
            &paste_intent(scope, text),
            jinn_slices::route::ActionCtx {
                state,
                slices,
                config,
                key_bytes: bytes,
            },
        )
        .map_or_else(jinn_slices::route::RouteResult::empty, |mut result| {
            apply_scope_signal(&mut result, state, slices, routes, config);
            result
        })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::missing_docs_in_private_items,
        reason = "test code"
    )]
    use super::*;
    use jinn_slices::FocusScope;

    use crate::feat::intent::handler::IntentHandler;
    #[rstest::rstest]
    fn paste_reaches_the_focused_scope_input_hook() {
        // Given a dynamic scope focused whose input hook accepts pastes.
        let scope = jinn_slices::SliceScopeId::navigation("test", "paste-hook");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let routes = routes_with_paste_hook(&scope, &seen);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("hooked"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the focused hook received the pasted text.
        assert_eq!(
            seen.lock().expect("recording lock").as_slice(),
            ["hooked".to_owned()]
        );
    }

    #[rstest::rstest]
    fn paste_to_a_hook_leaves_the_chat_input_buffer_untouched() {
        // Given a dynamic scope focused whose input hook accepts pastes.
        let scope = jinn_slices::SliceScopeId::navigation("test", "paste-hook");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let routes = routes_with_paste_hook(&scope, &seen);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("hooked"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the paste did not fall through to the chat input box.
        assert_eq!(chat_input_text(&state), "");
    }

    #[rstest::rstest]
    fn paste_declined_by_a_hook_is_dropped() {
        // Given a dynamic scope focused whose input hook declines pastes.
        let scope = jinn_slices::SliceScopeId::navigation("test", "declining-hook");
        let routes = routes_with_declining_hook(&scope);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        let result = IntentHandler::handle(
            &paste_intent_for("declined"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then no message was published.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn paste_declined_by_a_hook_does_not_reach_the_chat_input_box() {
        // Given a dynamic scope focused whose input hook declines pastes.
        let scope = jinn_slices::SliceScopeId::navigation("test", "declining-hook");
        let routes = routes_with_declining_hook(&scope);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("declined"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the text was not re-offered to the chat input box.
        assert_eq!(chat_input_text(&state), "");
    }

    #[rstest::rstest]
    fn paste_reaches_the_focused_scopes_own_paste_row() {
        // Given a dynamic scope focused that attached a paste row of its
        // own and no input hook.
        let scope = jinn_slices::SliceScopeId::navigation("test", "paste-row");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let routes = routes_with_paste_row(&scope, &seen);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("rowed"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then that row received the pasted text.
        assert_eq!(
            seen.lock().expect("recording lock").as_slice(),
            ["rowed".to_owned()]
        );
    }

    #[rstest::rstest]
    fn paste_in_input_scope_fills_the_chat_input_buffer() {
        // Given focus in the static Input scope, with the chat box's own
        // paste row attached.
        let routes = empty_routes();
        attach_chat_input_paste_row(&routes);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_clear_overlays();

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("in the box"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the text landed in the chat input buffer.
        assert_eq!(chat_input_text(&state), "in the box");
    }

    #[rstest::rstest]
    fn paste_in_normal_scope_fills_the_chat_input_buffer() {
        // Given focus in the static Normal scope, with the chat box's own
        // paste row attached.
        let routes = empty_routes();
        attach_chat_input_paste_row(&routes);
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_swap_base(FocusScope::Normal);

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("normal scope"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the text landed in the chat input buffer.
        assert_eq!(chat_input_text(&state), "normal scope");
    }

    #[rstest::rstest]
    fn paste_in_a_scope_with_no_hook_and_no_row_is_dropped() {
        // Given a dynamic scope focused that attached neither an input
        // hook nor a paste row, and the chat box's row is attached.
        let routes = empty_routes();
        attach_chat_input_paste_row(&routes);
        let scope = jinn_slices::SliceScopeId::navigation("test", "no-surface");
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        let result = IntentHandler::handle(
            &paste_intent_for("nowhere"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then no message was published.
        assert_eq!(result.message_names, [] as [&str; 0]);
    }

    #[rstest::rstest]
    fn paste_in_a_scope_with_no_surface_does_not_fill_the_chat_input_buffer() {
        // Given a dynamic scope focused that attached neither an input
        // hook nor a paste row, and the chat box's row is attached.
        let routes = empty_routes();
        attach_chat_input_paste_row(&routes);
        let scope = jinn_slices::SliceScopeId::navigation("test", "no-surface");
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::Dynamic(scope));

        // When handling a bracketed paste.
        IntentHandler::handle(
            &paste_intent_for("nowhere"),
            &mut state,
            &empty_slices(),
            &routes,
            jinn_slices::empty_config_layer(),
        );

        // Then the text did not fall through to the chat input box.
        assert_eq!(chat_input_text(&state), "");
    }

    fn chat_input_text(state: &AppState) -> String {
        state.with_active_input(
            |input| jinn_chat_input_msg::ChatInputBoxState::text(input).to_owned(),
            String::new,
        )
    }

    fn paste_intent_for(text: &str) -> KernelIntent {
        KernelIntent::Dynamic(jinn_slices::DynamicIntent::with_bytes(
            jinn_chat_input_msg::chat_input_scope(),
            jinn_chat_input_msg::PASTE_TEXT_ACTION,
            "paste text",
            text.as_bytes().to_vec(),
        ))
    }

    fn routes_with_paste_hook(
        scope: &jinn_slices::SliceScopeId,
        seen: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> jinn_slices::route::KeyRoutes {
        let routes = empty_routes();
        let seen = std::sync::Arc::clone(seen);
        routes.register_input_hook(
            scope,
            std::sync::Arc::new(move |edit: &jinn_slices::EditIntent| {
                if let jinn_slices::EditIntent::Paste(text) = edit {
                    seen.lock().expect("recording lock").push(text.clone());
                    return Some(jinn_slices::route::RouteResult::empty());
                }
                None
            }),
        );
        routes
    }

    fn routes_with_declining_hook(
        scope: &jinn_slices::SliceScopeId,
    ) -> jinn_slices::route::KeyRoutes {
        let routes = empty_routes();
        routes.register_input_hook(
            scope,
            std::sync::Arc::new(|_: &jinn_slices::EditIntent| None),
        );
        routes
    }

    fn routes_with_paste_row(
        scope: &jinn_slices::SliceScopeId,
        seen: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> jinn_slices::route::KeyRoutes {
        use jinn_slices::route::{ActionFn, BindSite, RouteId, RouteOutcome, RouteRow};
        let routes = empty_routes();
        let seen = std::sync::Arc::clone(seen);
        routes.attach(RouteRow {
            route_id: RouteId::new("test:paste-row"),
            scope: scope.clone(),
            key: "",
            category: "test",
            site: BindSite::OwnScope,
            feature: "test",
            outcome: RouteOutcome::Action {
                action: jinn_chat_input_msg::PASTE_TEXT_ACTION,
                display: "paste text",
                run: ActionFn::new(move |mut ctx| {
                    let text =
                        String::from_utf8_lossy(&std::mem::take(&mut ctx.key_bytes)).into_owned();
                    seen.lock().expect("recording lock").push(text);
                    jinn_slices::route::RouteResult::empty()
                }),
            },
        });
        routes
    }

    fn attach_chat_input_paste_row(routes: &jinn_slices::route::KeyRoutes) {
        use jinn_slices::route::{ActionFn, BindSite, RouteId, RouteOutcome, RouteRow};
        routes.attach(RouteRow {
            route_id: RouteId::new("test:chat-input-paste"),
            scope: jinn_chat_input_msg::chat_input_scope(),
            key: "",
            category: "test",
            site: BindSite::StaticScopes(&["Input"]),
            feature: "test",
            outcome: RouteOutcome::Action {
                action: jinn_chat_input_msg::PASTE_TEXT_ACTION,
                display: "paste text",
                run: ActionFn::new(|mut ctx| {
                    let text =
                        String::from_utf8_lossy(&std::mem::take(&mut ctx.key_bytes)).into_owned();
                    let Some(state) = ctx
                        .state
                        .as_any_mut()
                        .and_then(|any| any.downcast_mut::<AppState>())
                    else {
                        return jinn_slices::route::RouteResult::empty();
                    };
                    state.update_active_input(|input| input.insert_text(&text));
                    jinn_slices::route::RouteResult::empty()
                }),
            },
        });
    }

    /// The slice registry with nothing registered: enough for dispatch tests
    /// that are not exercising composition.
    fn empty_slices() -> jinn_slices::Slices {
        jinn_slices::Slices::new()
    }

    fn empty_routes() -> jinn_slices::route::KeyRoutes {
        jinn_slices::route::KeyRoutes::new()
    }
}
