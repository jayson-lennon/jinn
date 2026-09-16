//! Sidebar keybind rows on the shared route table.
//!
//! Every sidebar key resolves here instead of a kernel keymap table:
//! entry keys bind in the composition scopes that can open the sidebar,
//! in-section keys bind in the section's own dynamic scope, and the
//! resize mode binds in `sidebar:resize`. A de-activated slice attaches
//! no rows, so the keys stay unbound — the sidebar is inert by
//! construction.

use jinn_domain::common::app_state::AppState;
use jinn_domain::protocol::IntentResult;
use jinn_slices::SliceScopeId;
use jinn_slices::route::{
    ActionCtx, ActionFn, BindSite, KeyRoutes, RouteId, RouteOutcome, RouteRow, ScopeSignal,
};

use crate::sections::intent as sidebar_intent;
use crate::sections::pins::pins_section as pins;
use crate::sections::rename_input::intent as rename;
use crate::sections::resize::intent as resize;
use crate::sections::section_trait::SidebarIntent;
use crate::sections::sessions;
use crate::sections::sidebar::{jump_to_section, navigate_sidebar};
use crate::sections::task_list_section as task_list;

/// The sidebar's resize-mode dynamic scope.
#[must_use]
pub fn resize_scope() -> SliceScopeId {
    jinn_sidebar_msg::SidebarSectionId::resize_scope_id()
}

/// The rename popup's dynamic scope (input-capturing).
#[must_use]
pub fn rename_scope() -> SliceScopeId {
    rename::rename_scope()
}

/// Downcasts the action context's state to the kernel's application
/// state. Sidebar actions drive concrete session behavior (activate,
/// close, archive), which needs the full state surface.
fn app<'a>(ctx: &'a mut ActionCtx<'_>) -> &'a mut AppState {
    ctx.state
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AppState>())
        .expect("sidebar route action dispatched against a non-AppState state")
}

/// Wraps a synchronous sidebar function into an [`ActionFn`].
fn sync<F>(f: F) -> ActionFn
where
    F: Fn(&mut AppState) -> IntentResult + Send + Sync + 'static,
{
    ActionFn::new(move |mut ctx| f(app(&mut ctx)))
}

/// Builds one `Action` row binding `key` in `scope`. `action`/`display`
/// must be `'static` (they are the route-table key and which-key label).
fn row(
    action: &'static str,
    scope: SliceScopeId,
    key: &'static str,
    category: &'static str,
    display: &'static str,
    run: ActionFn,
) -> RouteRow {
    RouteRow {
        // Route ids are only diagnostics/composition keys here; actions
        // dispatch by (scope, action).
        route_id: RouteId::new("sidebar:row"),
        scope,
        key,
        category,
        site: BindSite::OwnScope,
        feature: "sidebar",
        outcome: RouteOutcome::Action {
            action,
            display,
            run,
        },
    }
}

/// Attaches the sidebar's keybind rows onto the shared route table.
/// Called once from the slice's `activate()`.
pub fn attach_sidebar_rows(routes: &KeyRoutes) {
    let persona = jinn_sidebar_msg::SidebarSectionId::Persona.scope_id();
    let pins_scope = jinn_sidebar_msg::SidebarSectionId::Pins.scope_id();
    let task_list_scope = jinn_sidebar_msg::SidebarSectionId::TaskList.scope_id();
    let sessions_scope = jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id();
    let mcp = jinn_sidebar_msg::SidebarSectionId::McpServers.scope_id();
    let resize = resize_scope();
    let sections = [
        persona.clone(),
        pins_scope.clone(),
        task_list_scope.clone(),
        sessions_scope.clone(),
        mcp.clone(),
    ];

    // ---- Shared base keys (every section) ----
    for scope in &sections {
        routes.attach(row(
            "move-down",
            scope.clone(),
            "j",
            "navigation",
            "cursor down",
            sync(|state| {
                navigate_sidebar(&SidebarIntent::MoveDown, state);
                IntentResult::empty()
            }),
        ));
        routes.attach(row(
            "move-up",
            scope.clone(),
            "k",
            "navigation",
            "cursor up",
            sync(|state| {
                navigate_sidebar(&SidebarIntent::MoveUp, state);
                IntentResult::empty()
            }),
        ));
        routes.attach(row(
            "section-next",
            scope.clone(),
            "J",
            "navigation",
            "next section",
            sync(|state| {
                jump_to_section(&SidebarIntent::MoveDown, state);
                IntentResult::empty()
            }),
        ));
        routes.attach(row(
            "section-prev",
            scope.clone(),
            "K",
            "navigation",
            "previous section",
            sync(|state| {
                jump_to_section(&SidebarIntent::MoveUp, state);
                IntentResult::empty()
            }),
        ));
        routes.attach(row(
            "leave",
            scope.clone(),
            "<esc>",
            "general",
            "return to chat",
            sync(|state| sidebar_intent::handle_sidebar_leave(state)),
        ));
        routes.attach(row(
            "leave-chat",
            scope.clone(),
            "<c-h>",
            "navigation",
            "return to chat",
            sync(|state| sidebar_intent::handle_sidebar_leave(state)),
        ));
        routes.attach(RouteRow {
            route_id: RouteId::new("sidebar:quit"),
            scope: scope.clone(),
            key: "q",
            category: "general",
            site: BindSite::OwnScope,
            feature: "sidebar",
            outcome: RouteOutcome::StaticIntent(RouteId::new("sidebar:quit")),
        });
        routes.attach(RouteRow {
            route_id: RouteId::new("sidebar:ctrl-clear"),
            scope: scope.clone(),
            key: "<c-c>",
            category: "general",
            site: BindSite::OwnScope,
            feature: "sidebar",
            outcome: RouteOutcome::StaticIntent(RouteId::new("sidebar:ctrl-clear")),
        });
        routes.attach(RouteRow {
            route_id: RouteId::new("sidebar:which-key"),
            scope: scope.clone(),
            key: "?",
            category: "general",
            site: BindSite::OwnScope,
            feature: "sidebar",
            outcome: RouteOutcome::StaticIntent(RouteId::new("sidebar:which-key")),
        });
        routes.attach(row(
            "resize-enter",
            scope.clone(),
            "<c-w>",
            "navigation",
            "resize sidebar",
            sync(move |state| {
                let mut result = resize::handle_resize_enter(state);
                result.scope_signal = Some(ScopeSignal::Push(resize_scope()));
                result
            }),
        ));
    }

    // ---- Persona section ----
    routes.attach(row(
        "persona-edit",
        persona,
        "c",
        "general",
        "change persona",
        sync(|state| {
            let pickers = jinn_domain::feat::picker::registry::build_picker_registry();
            pins::handle_sidebar_persona_edit(state, &pickers)
        }),
    ));

    // ---- Pins section ----
    routes.attach(row(
        "unpin",
        pins_scope.clone(),
        "u",
        "general",
        "unpin entry",
        sync(|state| pins::handle_pins_unpin(state)),
    ));
    routes.attach(row(
        "pin-top",
        pins_scope.clone(),
        "t",
        "general",
        "pin to top",
        sync(|state| pins::handle_pins_pin(state, jinn_domain::protocol::PinPosition::Top)),
    ));
    routes.attach(row(
        "pin-bottom",
        pins_scope.clone(),
        "b",
        "general",
        "pin to bottom",
        sync(|state| pins::handle_pins_pin(state, jinn_domain::protocol::PinPosition::Bottom)),
    ));
    routes.attach(row(
        "pin-relative",
        pins_scope.clone(),
        "r",
        "general",
        "pin above/below",
        sync(|state| pins::handle_pins_pin(state, jinn_domain::protocol::PinPosition::Relative)),
    ));
    routes.attach(row(
        "pin-cycle",
        pins_scope.clone(),
        "m",
        "general",
        "cycle pin position",
        sync(|state| pins::handle_pins_pin_cycle(state)),
    ));
    routes.attach(row(
        "leave-enter",
        pins_scope.clone(),
        "<enter>",
        "general",
        "return to chat",
        sync(|state| sidebar_intent::handle_sidebar_leave(state)),
    ));

    // ---- Sessions section ----
    routes.attach(row(
        "session-close",
        sessions_scope.clone(),
        "x",
        "general",
        "close session",
        sync(|state| sessions::handle_session_close_arm(state)),
    ));
    routes.attach(row(
        "session-teardown-tree",
        sessions_scope.clone(),
        "X",
        "general",
        "teardown+archive tree",
        sync(|state| {
            sessions::handle_session_tree_action_arm(
                state,
                sessions::TreePromptAction::TeardownAndArchive,
            )
        }),
    ));
    routes.attach(row(
        "session-teardown",
        sessions_scope.clone(),
        "t",
        "general",
        "run teardown",
        sync(|state| sessions::handle_session_teardown(state)),
    ));
    routes.attach(row(
        "session-confirm",
        sessions_scope.clone(),
        "<enter>",
        "general",
        "activate session",
        sync(|state| sessions::handle_session_activate(state)),
    ));
    routes.attach(row(
        "session-new",
        sessions_scope.clone(),
        "n",
        "general",
        "new session",
        sync(|_state| IntentResult::new_message(jinn_domain::Intent::SessionNew)),
    ));
    routes.attach(row(
        "session-new-lifecycle",
        sessions_scope.clone(),
        "N",
        "general",
        "new session (setup)",
        sync(|_state| IntentResult::new_message(jinn_domain::Intent::SessionNewWithLifecycle)),
    ));
    routes.attach(row(
        "session-rename",
        sessions_scope.clone(),
        "r",
        "general",
        "rename session",
        // The enter handler pushes the popup's dynamic scope itself.
        sync(|state| rename::handle_rename_session_enter(state)),
    ));

    routes.attach(row(
        "rename-confirm",
        rename_scope(),
        "<enter>",
        "input",
        "rename the session",
        sync(|state| rename::handle_rename_session_confirm(state)),
    ));
    routes.attach(row(
        "rename-leave",
        rename_scope(),
        "<esc>",
        "general",
        "cancel rename",
        sync(|state| rename::handle_rename_session_leave(state)),
    ));
    routes.attach(row(
        "session-archive",
        sessions_scope.clone(),
        "a",
        "general",
        "archive session",
        sync(|state| sessions::handle_session_archive(state)),
    ));
    routes.attach(row(
        "session-archive-tree",
        sessions_scope.clone(),
        "A",
        "general",
        "archive subtree",
        sync(|state| {
            sessions::handle_session_tree_action_arm(state, sessions::TreePromptAction::Archive)
        }),
    ));
    routes.attach(row(
        "session-continue",
        sessions_scope.clone(),
        "c",
        "general",
        "continue session",
        sync(|state| sessions::handle_session_continue(state)),
    ));
    routes.attach(row(
        "session-rerun-setup",
        sessions_scope.clone(),
        "s",
        "general",
        "rerun setup",
        sync(|state| {
            jinn_domain::feat::session_lifecycle::intent::handle_session_rerun_setup(state)
        }),
    ));
    routes.attach(row(
        "session-terminal",
        sessions_scope.clone(),
        "T",
        "general",
        "toggle terminal",
        sync(|_state| {
            IntentResult::new_message(jinn_domain::Intent::ToggleTerminalOverlayForSelected)
        }),
    ));
    routes.attach(row(
        "session-insert",
        sessions_scope.clone(),
        "i",
        "general",
        "activate + insert",
        sync(|state| sessions::handle_session_activate_insert(state)),
    ));

    // ---- Task list section ----
    routes.attach(row(
        "task-open-picker",
        task_list_scope.clone(),
        "s",
        "general",
        "browse task list",
        sync(move |state| {
            let pickers = jinn_domain::feat::picker::registry::build_picker_registry();
            jinn_domain::feat::picker::intent::handle_open_picker(
                state,
                jinn_domain::feat::picker::PickerKind::TaskList,
                &pickers,
            )
        }),
    ));
    routes.attach(row(
        "task-preview-up",
        task_list_scope.clone(),
        "<pgup>",
        "navigation",
        "preview up",
        sync(|state| task_list::handle_preview_scroll_up(state)),
    ));
    routes.attach(row(
        "task-preview-down",
        task_list_scope.clone(),
        "<pgdn>",
        "navigation",
        "preview down",
        sync(|state| task_list::handle_preview_scroll_down(state)),
    ));

    // ---- Resize mode ----
    routes.attach(row(
        "resize-expand",
        resize.clone(),
        "h",
        "general",
        "widen sidebar",
        sync(|state| resize::handle_resize_expand(state)),
    ));
    routes.attach(row(
        "resize-contract",
        resize.clone(),
        "l",
        "general",
        "narrow sidebar",
        sync(|state| resize::handle_resize_contract(state)),
    ));
    routes.attach(row(
        "resize-leave",
        resize.clone(),
        "<esc>",
        "general",
        "leave resize",
        sync(move |state| {
            let mut result = resize::handle_resize_leave(state);
            result.scope_signal = Some(ScopeSignal::PopIf(resize_scope()));
            result
        }),
    ));
    routes.attach(RouteRow {
        route_id: RouteId::new("sidebar:resize-mode"),
        scope: resize,
        key: "<c-c>",
        category: "general",
        site: BindSite::OwnScope,
        feature: "sidebar",
        outcome: RouteOutcome::StaticIntent(RouteId::new("sidebar:quit")),
    });

    // ---- Entry keys (Normal + Input scopes) ----
    routes.attach(RouteRow {
        route_id: RouteId::new("sidebar:focus"),
        scope: sessions_scope.clone(),
        key: "<c-l>",
        category: "navigation",
        site: BindSite::StaticScopes(&["Normal", "Input"]),
        feature: "sidebar",
        outcome: RouteOutcome::Action {
            action: "focus",
            display: "focus sidebar",
            run: sync(sidebar_intent::handle_sidebar_focus),
        },
    });
    routes.attach(RouteRow {
        route_id: RouteId::new("sidebar:focus-sessions"),
        scope: sessions_scope,
        key: "<M-s>",
        category: "navigation",
        site: BindSite::StaticScopes(&["Normal", "Input"]),
        feature: "sidebar",
        outcome: RouteOutcome::Action {
            action: "focus-sessions",
            display: "focus session list",
            run: sync(sidebar_intent::handle_sidebar_focus_sessions),
        },
    });
    routes.attach(RouteRow {
        route_id: RouteId::new("sidebar:resize-mode"),
        scope: resize_scope(),
        key: "<c-w>",
        category: "navigation",
        site: BindSite::StaticScopes(&["Normal", "Input"]),
        feature: "sidebar",
        outcome: RouteOutcome::Action {
            action: "resize-mode",
            display: "resize sidebar",
            run: sync(move |state| {
                let mut result = resize::handle_resize_enter(state);
                result.scope_signal = Some(ScopeSignal::Push(resize_scope()));
                result
            }),
        },
    });
}

/// Registers the rename popup's input hook on the shared route table:
/// within the rename scope, ordinary typing and paste edit the popup's
/// in-progress text directly in the sections cell — no kernel state
/// access needed.
pub fn register_rename_input_hook(
    routes: &KeyRoutes,
    cell: &jinn_slices::cell::TypedCell<jinn_sidebar_msg::SidebarSections>,
) {
    use jinn_slices::route::{EditIntent, InputHook};

    let cell = cell.clone();
    let hook: InputHook = std::sync::Arc::new(move |intent: &EditIntent| {
        let cell = cell.clone();
        let result = match intent {
            EditIntent::InsertChar(ch) => {
                cell.update(|s| rename::insert_char(&mut s.rename_input, *ch));
                IntentResult::empty()
            }
            EditIntent::DeleteBackward => {
                cell.update(|s| rename::delete(&mut s.rename_input));
                IntentResult::empty()
            }
            EditIntent::DeleteForward => {
                cell.update(|s| rename::delete_forward(&mut s.rename_input));
                IntentResult::empty()
            }
            EditIntent::CursorLeft => {
                cell.update(|s| rename::cursor_left(&mut s.rename_input));
                IntentResult::empty()
            }
            EditIntent::CursorRight => {
                cell.update(|s| rename::cursor_right(&mut s.rename_input));
                IntentResult::empty()
            }
            EditIntent::Paste(text) => {
                cell.update(|s| rename::paste(&mut s.rename_input, text));
                IntentResult::empty()
            }
            EditIntent::CursorHome | EditIntent::CursorEnd => return None,
        };
        Some(result)
    });
    routes.register_input_hook(&rename_scope(), hook);
}
