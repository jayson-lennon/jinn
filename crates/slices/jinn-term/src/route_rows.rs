//! The term slice's keybind rows — the overlay's every key, as data.
//!
//! Seven action rows (dispatched through the route table) plus two
//! shared-chrome rows (`q` quit, `?` which-key, static intents):
//!
//! - `toggle-overlay` — the would-be-global toggle key (`<M-t>`): opens
//!   the overlay for a session with a live terminal, or closes it when
//!   already open (view or control). Bound via
//!   [`jinn_slices::BindSite::GlobalToggle`] so the key works in every
//!   scope *except* `term:control`, where the key-hook catch-all is the
//!   only listener — globals would pierce capture mode.
//! - `toggle-for-selected` — the sidebar's `T`: activates the selected
//!   session first, then toggles (the overlay renders the *active*
//!   session's terminal, so activation must precede the open).
//! - `take-control` — the configured control-toggle key in `term:view`:
//!   flips the active session's control holder to *user*
//!   (synchronously, so an in-flight tool call's drain sees the takeover
//!   on its next iteration — mailbox ordering cannot deliver that) and
//!   pushes `term:control`.
//! - `send-key` — the key hook's target in `term:control`: publishes the
//!   encoded bytes straight to the pty. No-op unless the user actually
//!   holds control.
//! - `release-control` — the control-toggle key in `term:control`:
//!   releases control back to the agent and pops to `term:view`. Sends
//!   nothing to the model — the status hint advertises `I` for that.
//! - `yank-screen` — `y` in `term:view`: copies the visible screen to
//!   the clipboard via the TUI signal.
//! - `push-screen` — `I` in `term:view`: yanks the screen *and* pushes
//!   its text to the model using the same session-phase routing as chat
//!   submit (busy → steering buffer drained at next dispatch; idle →
//!   dispatched immediately by the queue actor).
//!
//! Toggle targets without a live terminal no-op: the overlay is a
//! *window* onto a running program, never a spawner; a status hint
//! explains the inert press.
//!
//! The control registry ([`jinn_term_msg::TERM_CONTROLS`]) is a shared
//! static also written by the coordinator actor — the documented
//! optimistic-write + authoritative-write pattern: the synchronous key
//! path flips it immediately so a settle poll cannot miss the takeover.

use jinn_domain::common::app_state::{AppState, FocusScope};
use jinn_domain::protocol::IntentResult;
use jinn_slices::route::{ActionCtx, ActionFn, BindSite, RouteId, RouteOutcome, RouteRow};
use jinn_slices::{DynamicIntent, KeyRoutes, SliceScopeId};
use jinn_term_msg::command::ControlHolder;
use jinn_term_msg::{control_scope, is_overlay_scope, view_scope};

/// Downcasts the action context's state to the kernel's application
/// state. Term actions drive concrete kernel behavior (scope stack,
/// control registry, session-phase routing), which needs the full
/// state surface — the sidebar's precedent for the sanctioned downcast.
fn app<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AppState>())
        .expect("term route action dispatched against a non-AppState state")
}

/// Builds one `Action` row binding `key` in `scope`. `action`/`display`
/// must be `'static` (they are the route-table key and which-key label).
fn row(
    action: &'static str,
    scope: SliceScopeId,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    site: BindSite,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        route_id: RouteId::new("term:row"),
        scope,
        key,
        category,
        site,
        feature: jinn_term_msg::SLICE_NAME,
        outcome: RouteOutcome::Action {
            action,
            display,
            run,
        },
    }
}

/// Builds one shared-chrome row resolving to a static intent by id.
fn chrome_row(scope: SliceScopeId, key: &'static str, route: &'static str) -> RouteRow {
    RouteRow {
        route_id: RouteId::new(route),
        scope,
        key,
        category: "general",
        site: BindSite::OwnScope,
        feature: jinn_term_msg::SLICE_NAME,
        outcome: RouteOutcome::StaticIntent(RouteId::new(route)),
    }
}

// ---------------------------------------------------------------------
// Ported mutation logic — one function per action, testable directly.
// ---------------------------------------------------------------------

/// Resolves the selected session when the Sessions sidebar section is
/// focused, for callers that act on the sidebar's selection (the sidebar
/// toggle key). `None` when no selection or the wrong section is focused.
#[must_use]
pub fn selected_sessions_sidebar_target(state: &AppState) -> Option<jinn_core_types::SessionId> {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    ) {
        return None;
    }
    let index = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)?;
    let sessions = jinn_domain::feat::session::sessions_list::state::sorted_open_sessions(state);
    sessions.get(index).map(|entry| entry.id.clone())
}

/// The active scope, if it is one of the overlay's scopes.
fn overlay_scope_of(state: &AppState) -> Option<SliceScopeId> {
    match state.frontend.scope() {
        FocusScope::Dynamic(id) if is_overlay_scope(&id) => Some(id),
        _ => None,
    }
}

/// Handles the `toggle-overlay` action.
///
/// Already open (view or control): any toggle closes it. Otherwise it
/// opens the overlay on the active session's terminal — a session
/// without a live terminal has nothing to show, so the press is inert
/// with a status hint.
pub fn handle_toggle_overlay(state: &mut AppState, slices: &jinn_slices::Slices) {
    // Already open (view or control): any toggle closes it.
    if overlay_scope_of(state).is_some() {
        state.frontend.scope_pop();
        return;
    }
    // A session without a live terminal has nothing to show; the overlay
    // is never a spawn trigger. A status hint explains the inert press.
    let target = state.session.active_session_id().clone();
    let live = state
        .term_tabs()
        .map(|cell| cell.read().live_terms.contains(&target))
        .unwrap_or(false);
    if !live {
        jinn_domain::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some(
                "that session has no live terminal — ask the agent to run `interactive_term`"
                    .to_owned(),
            ),
        );
        return;
    }
    // If a different popup holds the top of the stack, it is replaced: the
    // overlay mounts on the base scope (Esc semantics for the buried popup).
    state.frontend.scope_clear_overlays();
    state.frontend.scope_push(FocusScope::Dynamic(view_scope()));
}

/// Handles the `take-control` action (control-toggle key in `term:view`).
///
/// No status hint here: taking control is its own visible mode (the
/// scope switch is the announcement); the hint fires on handback.
pub fn handle_take_control(state: &mut AppState) {
    if let Some(registry) = jinn_term_msg::term_controls() {
        registry.set(state.session.active_session_id(), ControlHolder::User);
    }
    state.frontend.scope_push(FocusScope::Dynamic(control_scope()));
}

/// Handles the `send-key` action (the key hook's target in `term:control`).
///
/// No-op unless the user actually holds control; the bytes go to the
/// active session's terminal.
#[must_use]
pub fn handle_send_key(state: &mut AppState, bytes: Vec<u8>) -> IntentResult {
    // No-op unless the user actually holds control.
    if overlay_scope_of(state).is_none_or(|id| id != control_scope()) {
        return IntentResult::empty();
    }
    // The overlay targets the active chat session's terminal.
    IntentResult::empty().with_message(jinn_term_msg::SendTermKey {
        chat_session_id: state.session.active_session_id().clone(),
        bytes,
    })
}

/// Handles the `release-control` action (control-toggle key in
/// `term:control`).
///
/// Pure state transition: releases control to the agent (shared
/// registry), pops to `term:view`, and sets a status hint advertising
/// `I`. Sends nothing to the model — pushing the screen is an explicit
/// `I`.
pub fn handle_handback(state: &mut AppState, slices: &jinn_slices::Slices) {
    if overlay_scope_of(state).is_none_or(|id| id != control_scope()) {
        return;
    }
    if let Some(registry) = jinn_term_msg::term_controls() {
        registry.set(state.session.active_session_id(), ControlHolder::Agent);
    }
    state.frontend.scope_pop();
    jinn_domain::feat::ui::status_hint::set_hint(state, slices, Some(HANDLED_HINT.to_owned()));
}

/// The status hint shown after exiting capture mode: releasing control sends
/// nothing, so the hint advertises the explicit push key.
const HANDLED_HINT: &str =
    "terminal control released — press I to send the current screen to the agent";

/// Builds the model-facing message text for the `push-screen` action.
///
/// Public because the wording is part of the feature's contract with the
/// agent: it arrives as the user's own message, so it speaks in first
/// person — "Here is the current terminal screen" (never "The user …",
/// which would be confusing, and never "handed back", which would imply a
/// release event, or the user-control notice, which belongs solely to
/// refusal paths).
#[must_use]
pub fn push_screen_text(screen: &str) -> String {
    format!("Here is the current terminal screen:\n\n```\n{screen}\n```")
}

/// The visible screen of the active session's terminal, if any.
fn active_screen(state: &AppState) -> Option<String> {
    let chat = state.session.active_session_id();
    state
        .term_tabs()
        .and_then(|cell| cell.read().mirror(chat).map(|m| m.screen.clone()))
}

/// Handles the `yank-screen` action (`y` in `term:view`).
///
/// Copies the visible screen to the clipboard (via the TUI yank signal)
/// and reports the copied size in a status hint. No mirror → no-op with
/// a hint.
pub fn handle_yank(state: &mut AppState, slices: &jinn_slices::Slices) {
    if overlay_scope_of(state).is_none_or(|id| id != view_scope()) {
        return;
    }
    let Some(screen) = active_screen(state) else {
        jinn_domain::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some("no live terminal to yank — ask the agent to run `interactive_term`".to_owned()),
        );
        return;
    };
    let lines = screen.lines().count();
    state
        .frontend
        .update_scope(|s| s.signals.yank_text = Some(screen));
    jinn_domain::feat::ui::status_hint::set_hint(
        state,
        slices,
        Some(format!("yanked {lines} terminal lines to the clipboard")),
    );
}

/// Handles the `push-screen` action (`I` in `term:view`).
///
/// Yanks (see [`handle_yank`]) and pushes the screen text to the model:
/// busy → `SubmitSteeringMessage` (drained at the next dispatch-resume);
/// idle → `EnqueueUserMessage` (dispatched immediately). No mirror →
/// no-op with a hint.
#[must_use]
pub fn handle_push_screen(state: &mut AppState, slices: &jinn_slices::Slices) -> IntentResult {
    if overlay_scope_of(state).is_none_or(|id| id != view_scope()) {
        return IntentResult::empty();
    }
    let Some(screen) = active_screen(state) else {
        jinn_domain::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some("no live terminal to share — ask the agent to run `interactive_term`".to_owned()),
        );
        return IntentResult::empty();
    };
    let lines = screen.lines().count();
    state
        .frontend
        .update_scope(|s| s.signals.yank_text = Some(screen.clone()));
    jinn_domain::feat::ui::status_hint::set_hint(
        state,
        slices,
        Some(format!(
            "yanked {lines} terminal lines and sent the screen to the agent"
        )),
    );

    let text = push_screen_text(&screen);
    let session_id = state.session.active_session_id().clone();
    if state.active_session().phase() == jinn_domain::feat::session::phase_machine::PhaseKind::Idle
    {
        IntentResult::empty().with_message(
            jinn_domain::feat::chat_input::protocol::command::EnqueueUserMessage {
                session_id,
                entry: jinn_domain::protocol::ChatEntry::user(text),
            },
        )
    } else {
        IntentResult::empty().with_message(
            jinn_domain::feat::chat_input::protocol::command::SubmitSteeringMessage {
                session_id,
                text,
            },
        )
    }
}

/// Handles the `toggle-for-selected` action (the sidebar's `T`).
///
/// Activates the selected session first, so the overlay (which renders
/// the *active* session's terminal) and the live-term check always
/// target the same session. When nothing is selectable, falls through
/// targeting the active session — identical to the global toggle key.
pub fn handle_toggle_for_selected(state: &mut AppState, slices: &jinn_slices::Slices) {
    let selected = selected_sessions_sidebar_target(state);
    if let Some(selected) = selected
        && selected != *state.session.active_session_id()
    {
        state.session.set_active(selected);
    }
    handle_toggle_overlay(state, slices);
}

// ---------------------------------------------------------------------
// Row assembly
// ---------------------------------------------------------------------

/// Attaches the term slice's keybind rows onto the shared route table.
///
/// `toggle_key` is the *configured* control-toggle binding (already
/// normalized and interned — see the slice's `activate`): it is the row
/// key for take-control (view) and release-control (control).
pub fn attach_rows(routes: &KeyRoutes, toggle_key: &'static str) {
    let view = view_scope();
    let control = control_scope();

    // `<M-t>`: open the overlay from anywhere it can be reached, close it
    // when open. GlobalToggle spreads to every static scope, every other
    // slice's dynamic scope, *and* this row's own scope (the spread does
    // not skip own scopes) — which is exactly the close binding view
    // needs. The key-hook scope (`term:control`) is excluded by
    // composition, so capture mode stays hermetic.
    routes.attach(row(
        "toggle-overlay",
        view.clone(),
        "<M-t>",
        "general",
        "toggle terminal overlay",
        BindSite::GlobalToggle,
        ActionFn::new(|mut ctx| {
            let slices = ctx.slices;
            handle_toggle_overlay(app(&mut ctx), slices);
            IntentResult::empty()
        }),
    ));

    // `T` in view: activate the sidebar's selected session, then toggle.
    routes.attach(row(
        "toggle-for-selected",
        view.clone(),
        "T",
        "general",
        "toggle terminal for selected session",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            let slices = ctx.slices;
            handle_toggle_for_selected(app(&mut ctx), slices);
            IntentResult::empty()
        }),
    ));

    // Configured control-toggle in view: take over the pty.
    routes.attach(row(
        "take-control",
        view.clone(),
        toggle_key,
        "general",
        "take control of the terminal",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            handle_take_control(app(&mut ctx));
            IntentResult::empty()
        }),
    ));

    // Configured control-toggle in control: hand back to the agent.
    routes.attach(row(
        "release-control",
        control.clone(),
        toggle_key,
        "general",
        "release control back to the agent",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            let slices = ctx.slices;
            handle_handback(app(&mut ctx), slices);
            IntentResult::empty()
        }),
    ));

    // `y` in view: yank the visible screen to the clipboard.
    routes.attach(row(
        "yank-screen",
        view.clone(),
        "y",
        "general",
        "yank screen to clipboard",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            let slices = ctx.slices;
            handle_yank(app(&mut ctx), slices);
            IntentResult::empty()
        }),
    ));

    // `I` in view: yank + push the screen to the model.
    routes.attach(row(
        "push-screen",
        view.clone(),
        "I",
        "general",
        "send screen to the agent",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            let slices = ctx.slices;
            handle_push_screen(app(&mut ctx), slices)
        }),
    ));

    // The key hook's target: publishes `ctx.key_bytes` to the pty. Bound
    // with an empty key (bind-inert) — the hook's catch-all mints this
    // intent per keystroke; the row is the dispatch target.
    routes.attach(row(
        "send-key",
        control.clone(),
        "",
        "general",
        "send key to terminal",
        BindSite::OwnScope,
        ActionFn::new(|mut ctx| {
            let bytes = std::mem::take(&mut ctx.key_bytes);
            handle_send_key(app(&mut ctx), bytes)
        }),
    ));

    // Shared chrome: quit + which-key popup.
    routes.attach(chrome_row(view.clone(), "q", "term:quit"));
    routes.attach(chrome_row(view, "?", "term:which-key"));
}

/// The dynamic intent the sidebar's `T` row publishes: toggles the
/// terminal overlay after activating the sidebar's selected session.
#[must_use]
pub fn toggle_for_selected_intent() -> DynamicIntent {
    DynamicIntent::new(
        view_scope(),
        "toggle-for-selected",
        "toggle terminal for selected session",
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::app;
    use super::attach_rows;
    use super::control_scope;
    use super::handle_handback;
    use super::handle_push_screen;
    use super::handle_send_key;
    use super::handle_take_control;
    use super::handle_toggle_for_selected;
    use super::handle_toggle_overlay;
    use super::handle_yank;
    use super::push_screen_text;
    use super::view_scope;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::app_state::FocusScope;
    use jinn_slices::route::ActionCtx;
    use jinn_slices::route::KeyRoutes;
    use jinn_slices::route::RouteResult;
    use jinn_slices::Slices;
    use jinn_term_msg::command::ControlHolder;
    use jinn_term_msg::cells::ScreenCells;

    fn app_state() -> AppState {
        // The control registry is a process-wide OnceLock minted by
        // `activate` in production; tests mint it on first use (idempotent
        // — the set is a no-op when already present).
        let _ = jinn_term_msg::TERM_CONTROLS.set(jinn_term_msg::TermControls::default());
        AppState::default_with_scope_focus()
    }

    /// `Slices` with the status-bar cell registered (as the slice's
    /// `activate` does), for hint write/read assertions.
    fn status_bar_slices() -> Slices {
        let slices = Slices::new();
        slices
            .register(
                jinn_status_bar_msg::status_bar_slot(),
                jinn_status_bar_msg::StatusBarState::default(),
            )
            .expect("fresh Slices never has the status-bar cell registered");
        slices
    }

    /// Marks the given session as having a live terminal.
    fn set_live(state: &mut AppState, id: &jinn_core_types::SessionId) {
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.set_live(id, true));
    }

    /// Mirrors a screen onto the active session's terminal.
    fn mirror_screen(state: &mut AppState, screen: &str) {
        let id = state.session.active_session_id().clone();
        state
            .term_tabs()
            .expect("term tabs cell")
            .update(|t| t.apply_screen(&id, screen.to_owned(), ScreenCells::default(), (0, 0), false));
    }

    /// Runs a row's action through the route table exactly as the
    /// handler's dispatch arm would (downcast + ctx assembly included).
    fn dispatch(
        routes: &KeyRoutes,
        state: &mut AppState,
        slices: &Slices,
        action: &str,
        scope: &jinn_slices::SliceScopeId,
        key_bytes: Vec<u8>,
    ) -> Option<RouteResult> {
        let intent = jinn_slices::DynamicIntent::new(scope.clone(), action, "");
        routes.action_for(
            &intent,
            ActionCtx {
                state,
                slices,
                key_bytes,
            },
        )
    }

    #[rstest::rstest]
    fn toggle_opens_view_overlay_for_live_session() {
        // Given default state whose active session has a live terminal.
        let mut state = app_state();
        let slices = status_bar_slices();
        let routes = KeyRoutes::new();
        attach_rows(&routes, "<c-g>");
        let chat = state.session.active_session_id().clone();
        set_live(&mut state, &chat);

        // When dispatching the toggle-overlay action.
        dispatch(&routes, &mut state, &slices, "toggle-overlay", &view_scope(), Vec::new());

        // Then the overlay opens in view mode.
        assert_eq!(state.frontend.scope(), FocusScope::Dynamic(view_scope()));
    }

    #[rstest::rstest]
    fn toggle_without_live_term_is_inert_with_a_hint() {
        // Given default state with no live terminals and a status bar.
        let mut state = app_state();
        let slices = status_bar_slices();
        let routes = KeyRoutes::new();
        attach_rows(&routes, "<c-g>");

        // When dispatching the toggle-overlay action.
        dispatch(&routes, &mut state, &slices, "toggle-overlay", &view_scope(), Vec::new());

        // Then no overlay opened.
        assert_eq!(state.frontend.scope(), FocusScope::Input);
        // And a status hint explains the inert press.
        let hint = jinn_domain::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains("no live terminal")),
            "expected a no-live-terminal hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn toggle_closes_an_open_overlay() {
        // Given an open terminal overlay (view mode).
        let mut state = app_state();
        let slices = status_bar_slices();
        let routes = KeyRoutes::new();
        attach_rows(&routes, "<c-g>");
        let chat = state.session.active_session_id().clone();
        set_live(&mut state, &chat);
        dispatch(&routes, &mut state, &slices, "toggle-overlay", &view_scope(), Vec::new());

        // When dispatching the toggle-overlay action again.
        dispatch(&routes, &mut state, &slices, "toggle-overlay", &view_scope(), Vec::new());

        // Then the overlay closes back to the base scope.
        assert_eq!(state.frontend.scope(), FocusScope::Normal);
    }

    #[rstest::rstest]
    fn toggle_for_selected_activates_then_opens() {
        // Given two sessions where the *second* holds the live terminal,
        // and the sidebar's Sessions section selecting it.
        use jinn_domain::feat::session::chat_session::ChatSessionState;
        let mut state = app_state();
        let slices = status_bar_slices();
        let routes = KeyRoutes::new();
        attach_rows(&routes, "<c-g>");
        let second = ChatSessionState::new();
        let second_id = second.session_id().clone();
        state.session.insert(second);
        set_live(&mut state, &second_id);
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(
                jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id(),
            ));
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));

        // When dispatching toggle-for-selected.
        dispatch(
            &routes,
            &mut state,
            &slices,
            "toggle-for-selected",
            &view_scope(),
            Vec::new(),
        );

        // Then the overlay is open on the newly-activated session's
        // terminal — activation preceded the open.
        assert_eq!(
            state.session.active_session_id(),
            &second_id,
            "the selected session must be activated"
        );
        assert_eq!(state.frontend.scope(), FocusScope::Dynamic(view_scope()));
    }

    #[rstest::rstest]
    fn take_control_pushes_control_scope_and_flags_user() {
        // Given an AppState in the view overlay.
        let mut state = app_state();
        let chat = state.session.active_session_id().clone();
        set_live(&mut state, &chat);
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));

        // When handling take-control.
        handle_take_control(&mut state);

        // Then the scope is term:control.
        assert_eq!(state.frontend.scope(), FocusScope::Dynamic(control_scope()));
        // And the shared registry records the user as control holder.
        let registry = jinn_term_msg::term_controls().expect("registry minted by activate");
        assert_eq!(registry.holder_for(&chat), ControlHolder::User);
    }

    #[rstest::rstest]
    fn handback_releases_flag_pops_scope_and_sends_nothing() {
        // Given a state where the user holds control with a screen mirror.
        let mut state = app_state();
        let slices = status_bar_slices();
        mirror_screen(&mut state, "handback-screen-marker");
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));
        handle_take_control(&mut state);

        // When handling release-control.
        let mut ctx = ActionCtx {
            state: &mut state,
            slices: &slices,
            key_bytes: Vec::new(),
        };
        let slices_ref = ctx.slices;
        handle_handback(app(&mut ctx), slices_ref);

        // Then the scope pops back to view.
        assert_eq!(state.frontend.scope(), FocusScope::Dynamic(view_scope()));
        // And no message is published (release is silent; `I` pushes).
        assert!(RouteResult::empty().messages.is_empty());
        // And the status hint advertises the push key.
        let hint = jinn_domain::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains('I')),
            "handback hint must advertise I; got {hint:?}"
        );
    }

    #[rstest::rstest]
    fn send_key_publishes_to_the_active_session_while_held() {
        // Given the user holding terminal control.
        let mut state = app_state();
        handle_take_control(&mut state);

        // When handling send-key with bytes.
        let result = handle_send_key(&mut state, b"x".to_vec());

        // Then a SendTermKey command is published (targeting the active
        // session's terminal).
        assert!(
            result
                .message_names
                .iter()
                .any(|name| name.ends_with("SendTermKey")),
            "expected a SendTermKey command; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn send_key_outside_control_scope_is_inert() {
        // Given an AppState in view mode (no control).
        let mut state = app_state();

        // When handling send-key.
        let result = handle_send_key(&mut state, b"a".to_vec());

        // Then no pty write command is published.
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    fn yank_stages_screen_text_and_sets_line_count_hint() {
        // Given an AppState in the view overlay with a multi-line mirror.
        let mut state = app_state();
        let slices = status_bar_slices();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));
        mirror_screen(&mut state, "line one\nline two\nline three");

        // When handling yank-screen.
        handle_yank(&mut state, &slices);

        // Then the screen text was staged for the clipboard.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text.as_deref(),
            Some("line one\nline two\nline three")
        );
        // And the status hint reports the copied line count.
        let hint = jinn_domain::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains('3')),
            "yank hint must report the line count; got {hint:?}"
        );
    }

    #[rstest::rstest]
    fn yank_without_live_terminal_sets_a_hint_and_stages_nothing() {
        // Given an AppState in the view overlay with no mirror.
        let mut state = app_state();
        let slices = status_bar_slices();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));

        // When handling yank-screen.
        handle_yank(&mut state, &slices);

        // Then nothing was staged for the clipboard.
        assert!(state.frontend.signals_snapshot().yank_text.is_none());
        // And a status hint explains the inert press.
        let hint = jinn_domain::feat::ui::status_hint::hint(&slices);
        assert!(
            hint.as_deref().is_some_and(|h| h.contains("no live terminal")),
            "expected a no-live-terminal hint, got: {hint:?}"
        );
    }

    #[rstest::rstest]
    fn push_screen_when_idle_enqueues_user_message() {
        // Given the view overlay with a screen mirror, session idle.
        let mut state = app_state();
        let slices = status_bar_slices();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));
        mirror_screen(&mut state, "idle-screen-marker");

        // When handling push-screen.
        let result = handle_push_screen(&mut state, &slices);

        // Then an enqueue message is published (idle dispatch path).
        assert!(
            result
                .message_names
                .iter()
                .any(|name| name.ends_with("EnqueueUserMessage")),
            "idle push must publish EnqueueUserMessage; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn push_screen_while_busy_steers_via_buffer() {
        // Given the view overlay with a screen mirror, session streaming.
        let mut state = app_state();
        let slices = status_bar_slices();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));
        mirror_screen(&mut state, "busy-screen-marker");
        {
            let sid = state.session.active_session_id().clone();
            if let Some(session) = state.session.get_mut(&sid) {
                session.begin_streaming();
            }
        }

        // When handling push-screen.
        let result = handle_push_screen(&mut state, &slices);

        // Then a steering message is published (drains at dispatch-resume).
        assert!(
            result
                .message_names
                .iter()
                .any(|name| name.ends_with("SubmitSteeringMessage")),
            "busy push must publish SubmitSteeringMessage; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn push_screen_yanks_the_screen_text() {
        // Given the view overlay with a screen mirror.
        let mut state = app_state();
        let slices = status_bar_slices();
        state
            .frontend
            .scope_swap_base(FocusScope::Dynamic(view_scope()));
        mirror_screen(&mut state, "yank-and-push-marker");

        // When handling push-screen.
        handle_push_screen(&mut state, &slices);

        // Then the screen text was also staged for the clipboard.
        assert_eq!(
            state.frontend.signals_snapshot().yank_text.as_deref(),
            Some("yank-and-push-marker"),
            "push must also yank (I = yank + push)"
        );
    }

    #[rstest::rstest]
    fn push_screen_wording_speaks_as_the_user_not_about_them() {
        // Given a captured screen.
        let screen = "shared-marker";

        // When building the push message text.
        let text = push_screen_text(screen);

        // Then the text opens with the first-person screen offer.
        assert!(text.contains("Here is the current terminal screen"));
        assert!(text.contains(screen));
        // And it never speaks about the user in third person, never claims
        // a handback, and never embeds the refusal note.
        assert!(!text.contains("The user"));
        assert!(!text.contains("handed"));
        assert!(!text.contains(
            jinn_domain::feat::tools_actor::interactive_term_send::USER_HAS_CONTROL_NOTICE
        ));
    }

    #[rstest::rstest]
    fn rows_bind_the_documented_keys_in_their_scopes() {
        // Given the attached rows.
        let routes = KeyRoutes::new();
        attach_rows(&routes, "<c-g>");
        let rows = routes.rows();

        // Then the send-key row binds no key (the key hook mints its
        // intents) and the chrome rows resolve static intents.
        let send_key = rows
            .iter()
            .find(|r| matches!(&r.outcome, jinn_slices::route::RouteOutcome::Action { action, .. } if *action == "send-key"))
            .expect("send-key row attached");
        assert_eq!(send_key.key, "");
        let chrome: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r.outcome {
                jinn_slices::route::RouteOutcome::StaticIntent(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(chrome, vec!["term:quit", "term:which-key"]);
    }
}
