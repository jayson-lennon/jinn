//! Dashboard keybind routing — the feature's contribution to [`KeyRoutes`].
//!
//! The four navigation keys are no longer handled by arms in the central
//! intent handler and have no central intent variants: this module
//! registers route rows mapping each key in the dashboard's dynamic
//! scope to a [`DashboardNav`] message for the dashboard actor. The
//! router consults the table (via `KeyRoutes::action_for`) and emits
//! the message onto the bus; the dashboard actor — the slice's single
//! writer — updates the cell.
//!
//! Registering rows here (rather than hard-coding arms) is what makes
//! the slice removable: no `activate` call, no rows, no bindings.

use jinn_slices::SliceScopeId;

use crate::DashboardNav;
use jinn_slices::ActionFn;
use jinn_slices::BindSite;
use jinn_slices::KeyRoutes;
use jinn_slices::RouteOutcome;
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::RouteRow;

/// The dashboard tab's dynamic scope.
///
/// The slice's identity in the focus stack and route table: pushed by
/// `SwitchTab` (which swaps the base scope to this id) and consumed by
/// every registration this module performs.
#[must_use]
pub fn dashboard_scope() -> SliceScopeId {
    SliceScopeId::new("dashboard", "tab")
}

/// Route ids for the dashboard's rows (composition resolution +
/// diagnostics).
pub mod route_ids {
    use jinn_slices::RouteId;

    /// Move selection up one row (`k`).
    pub const NAV_UP: RouteId = RouteId::new("dashboard:nav-up");
    /// Move selection down one row (`j`).
    pub const NAV_DOWN: RouteId = RouteId::new("dashboard:nav-down");
    /// Jump to the first row (`g`).
    pub const NAV_FIRST: RouteId = RouteId::new("dashboard:nav-first");
    /// Jump to the last row (`G`).
    pub const NAV_LAST: RouteId = RouteId::new("dashboard:nav-last");
    /// Switch back to the chat tab (`<Tab>`/`<esc>`).
    pub const SWITCH_TAB: RouteId = RouteId::new("dashboard:switch-tab");
    /// Quit the application (`q`).
    pub const QUIT: RouteId = RouteId::new("dashboard:quit");
    /// Toggle the which-key popup (`?`).
    pub const TOGGLE_WHICHKEY: RouteId = RouteId::new("dashboard:toggle-whichkey");
}

/// Registers the dashboard feature's keybind rows onto the shared route
/// table. Called once from the slice's `activate()`.
pub fn attach_dashboard_rows(routes: &KeyRoutes) {
    let scope = dashboard_scope();

    routes.attach(RouteRow {
        route_id: route_ids::NAV_UP,
        scope: scope.clone(),
        key: "k",
        category: "navigation",
        site: BindSite::OwnScope,
        feature: "dashboard",
        outcome: RouteOutcome::Action {
            action: "nav-up",
            display: "move up",
            run: ActionFn::new(|_ctx| IntentResult::new_message(DashboardNav::Up)),
        },
    });
    routes.attach(RouteRow {
        route_id: route_ids::NAV_DOWN,
        scope: scope.clone(),
        key: "j",
        category: "navigation",
        site: BindSite::OwnScope,
        feature: "dashboard",
        outcome: RouteOutcome::Action {
            action: "nav-down",
            display: "move down",
            run: ActionFn::new(|_ctx| IntentResult::new_message(DashboardNav::Down)),
        },
    });
    routes.attach(RouteRow {
        route_id: route_ids::NAV_FIRST,
        scope: scope.clone(),
        key: "g",
        category: "navigation",
        site: BindSite::OwnScope,
        feature: "dashboard",
        outcome: RouteOutcome::Action {
            action: "nav-first",
            display: "move to first",
            run: ActionFn::new(|_ctx| IntentResult::new_message(DashboardNav::First)),
        },
    });
    routes.attach(RouteRow {
        route_id: route_ids::NAV_LAST,
        scope: scope.clone(),
        key: "G",
        category: "navigation",
        site: BindSite::OwnScope,
        feature: "dashboard",
        outcome: RouteOutcome::Action {
            action: "nav-last",
            display: "move to last",
            run: ActionFn::new(|_ctx| IntentResult::new_message(DashboardNav::Last)),
        },
    });

    // Shared-chrome keys resolve to static intents via composition.
    for (route_id, key, category) in [
        (route_ids::SWITCH_TAB, "<Tab>", "general"),
        (route_ids::SWITCH_TAB, "<esc>", "general"),
        (route_ids::QUIT, "q", "general"),
        (route_ids::TOGGLE_WHICHKEY, "?", "general"),
    ] {
        routes.attach(RouteRow {
            route_id,
            scope: scope.clone(),
            key,
            category,
            site: BindSite::OwnScope,
            feature: "dashboard",
            outcome: RouteOutcome::StaticIntent(route_id),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test code; a missing attached row is a hard failure"
    )]
    use crate::attach_dashboard_rows;
    use crate::dashboard_scope;
    use jinn_slices::ActionCtx;
    use jinn_slices::DynamicIntent;
    use jinn_slices::KeyRoutes;
    use jinn_slices::RouteOutcome;

    #[rstest::rstest]
    #[case::up("nav-up")]
    #[case::down("nav-down")]
    #[case::first("nav-first")]
    #[case::last("nav-last")]
    fn dynamic_intent_produces_nav_message_for_registered_action(#[case] action: &'static str) {
        // Given a route table with the dashboard rows attached.
        let routes = KeyRoutes::new();
        attach_dashboard_rows(&routes);

        // When dispatching a dynamic intent for one of the actions.
        let intent = DynamicIntent::new(dashboard_scope(), action, "dashboard nav");
        let mut state = TestState::default();
        let slices = jinn_slices::Slices::new();
        let result = routes
            .action_for(
                &intent,
                ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("row is attached");

        // Then the result carries a DashboardNav message.
        assert_eq!(
            result.message_names,
            vec![std::any::type_name::<super::DashboardNav>()],
            "dashboard nav message name"
        );
    }

    #[rstest::rstest]
    #[test]
    fn shared_chrome_rows_resolve_to_static_intent_ids() {
        // Given a route table with the dashboard rows attached.
        let routes = KeyRoutes::new();
        attach_dashboard_rows(&routes);

        // When enumerating the rows.
        let rows = routes.rows();

        // Then the `q` row names the quit route id as a static intent.
        let quit = rows
            .iter()
            .find(|row| row.key == "q")
            .expect("quit row attached");
        assert!(matches!(
            quit.outcome,
            RouteOutcome::StaticIntent(super::route_ids::QUIT)
        ));
    }

    #[rstest::rstest]
    #[test]
    fn unbound_dynamic_intent_yields_no_route() {
        // Given a route table with the dashboard rows attached.
        let routes = KeyRoutes::new();
        attach_dashboard_rows(&routes);

        // When dispatching a dynamic intent for an unknown action.
        let intent = DynamicIntent::new(dashboard_scope(), "nonexistent", "nothing");
        let mut state = TestState::default();
        let slices = jinn_slices::Slices::new();
        let result = routes.action_for(
            &intent,
            ActionCtx {
                state: &mut state,
                slices: &slices,
                key_bytes: Vec::new(),
            },
        );

        // Then no route serves it.
        assert!(result.is_none());
    }
    impl jinn_slices::SliceActionState for TestState {
        fn active_session_title(&self) -> Option<String> {
            None
        }

        fn active_session_id(&self) -> jinn_core_types::SessionId {
            jinn_core_types::SessionId::new()
        }

        fn push_session_error(&mut self, message: &str) {
            self.errors.push(message.to_owned());
        }

        fn active_session_cwd(&self) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }

        fn publish_session_cwd(
            &self,
            _session_id: jinn_core_types::SessionId,
            _cwd: std::path::PathBuf,
        ) -> jinn_slices::PublishClosure {
            Box::new(|_bus| {})
        }
    }

    /// A minimal [`jinn_slices::SliceActionState`] stand-in for the row tests.
    #[derive(Default)]
    struct TestState {
        errors: Vec<String>,
    }
}
