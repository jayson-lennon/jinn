//! Discord keybind routing — the slice's rows.
//!
//! Discord has no dynamic scope of its own (its UI surface is the
//! dashboard tab, which the dashboard slice owns), so its single user
//! action binds through [`BindSite::StaticScopes`] into the
//! composition scope where the command prefix lives: `Normal`.
//! The row's `scope` field is a nominal identity — required by
//! [`RouteRow`] for route resolution and diagnostics — but it is never
//! pushed onto the scope stack and never bound as an `OwnScope` key.
//!
//! The `gdc` sequence ("continue this session in a Discord thread")
//! resolves to the dynamic intent for this row; the action runs the
//! precondition chain in [`to_thread_intent`](super::to_thread_intent)
//! against the handler's own borrows.

use jinn_slices::SliceScopeId;

use super::to_thread_intent;
use jinn_domain::common::slices::key_routes::ActionFn;
use jinn_domain::common::slices::key_routes::BindSite;
use jinn_domain::common::slices::key_routes::KeyRoutes;
use jinn_domain::common::slices::key_routes::RouteOutcome;
use jinn_domain::common::slices::key_routes::RouteRow;
use jinn_domain::protocol::Intent;

/// Route ids for the discord slice's rows (composition resolution +
/// diagnostics).
pub mod route_ids {
    use jinn_domain::common::slices::key_routes::RouteId;

    /// Lift the active session into a Discord forum thread (`gdc`).
    pub const TO_THREAD: RouteId = RouteId::new("discord:to-thread");
}

/// Discord's nominal scope id.
///
/// Discord renders no UI of its own, so it declares no real dynamic
/// scope. [`RouteRow`] requires a scope for the route-table lookup key
/// (`Intent::Dynamic` carries it); this id is that identity only. It
/// is never entered, never registered as a tab, and its rows bind
/// exclusively via [`BindSite::StaticScopes`].
#[must_use]
pub fn discord_scope() -> SliceScopeId {
    SliceScopeId::new("discord", "actions")
}

/// Builds the dynamic intent for the slice's to-thread action.
#[must_use]
pub fn to_thread_intent_action() -> Intent {
    Intent::Dynamic(jinn_slices::DynamicIntent::new(
        discord_scope(),
        "to-thread",
        "continue in Discord thread",
    ))
}

/// Attaches the discord slice's route rows. Called once from the
/// slice's `activate()`; the action needs no captures — it receives
/// the handler's borrows ([`ActionCtx`](jinn_domain::common::slices::key_routes::ActionCtx))
/// at dispatch time.
pub fn attach_discord_rows(routes: &KeyRoutes) {
    routes.attach(RouteRow {
        route_id: route_ids::TO_THREAD,
        scope: discord_scope(),
        key: "gdc",
        category: "general",
        site: BindSite::StaticScopes(&["Normal"]),
        feature: "discord",
        outcome: RouteOutcome::Action {
            action: "to-thread",
            display: "continue in Discord thread",
            run: ActionFn::new(to_thread_intent::handle_to_discord_thread),
        },
    });
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code; a missing attached row is a hard failure"
    )]
    use super::attach_discord_rows;
    use super::discord_scope;
    use super::to_thread_intent_action;
    use crate::ConnectionState;
    use crate::discord_connection_slot;
    use jinn_domain::common::slices::key_routes::ActionCtx;
    use jinn_domain::common::slices::key_routes::BindSite;
    use jinn_domain::common::slices::key_routes::KeyRoutes;
    use jinn_domain::common::slices::key_routes::RouteOutcome;
    use jinn_domain::protocol::ChatEntryKind;
    use jinn_domain::protocol::Intent;

    fn routed() -> KeyRoutes {
        let routes = KeyRoutes::new();
        attach_discord_rows(&routes);
        routes
    }

    /// The row's dynamic intent, unwrapped from the composition-side
    /// `Intent` wrapper the keymap produces.
    fn to_thread_dynamic() -> jinn_slices::DynamicIntent {
        match to_thread_intent_action() {
            Intent::Dynamic(dynamic) => dynamic,
            other => panic!("to-thread action must be a dynamic intent, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn row_binds_through_a_static_scope_site() {
        // Given a route table with the discord rows attached.
        let routes = routed();

        // When enumerating the rows.
        let rows = routes.rows();

        // Then the `gdc` row binds via StaticScopes naming Normal.
        let row = rows
            .iter()
            .find(|row| row.key == "gdc")
            .expect("to-thread row attached");
        assert!(matches!(row.site, BindSite::StaticScopes(&["Normal"])));
        // And its outcome is the to-thread action.
        assert!(matches!(
            row.outcome,
            RouteOutcome::Action {
                action: "to-thread",
                ..
            }
        ));
    }

    #[rstest::rstest]
    #[test]
    fn action_pushes_error_entry_when_bot_disabled() {
        // Given a route table and a default state: no title, bot disabled.
        let routes = routed();
        let mut state = jinn_domain::common::app_state::AppState::default();
        let slices = jinn_domain::common::slices::Slices::new();

        // When dispatching the to-thread dynamic intent.
        let result = routes
            .action_for(
                &to_thread_dynamic(),
                ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("to-thread row attached");

        // Then no bus command was emitted.
        assert!(result.message_names.is_empty());
        // And the failure landed as an error entry in the active session.
        let entry = state
            .active_session()
            .history()
            .last()
            .expect("error entry pushed");
        assert!(
            matches!(entry.kind, ChatEntryKind::Error(_)),
            "expected an error entry; got {:?}",
            entry.kind
        );
    }

    #[rstest::rstest]
    #[test]
    fn action_emits_create_thread_command_when_preconditions_pass() {
        // Given a route table and a state meeting every precondition:
        // titled session, bot enabled, connection cell reporting connected.
        let routes = routed();
        let mut state = jinn_domain::common::app_state::AppState::default();
        state
            .active_session_mut()
            .set_title("My session".to_owned());
        let slices = jinn_domain::common::slices::Slices::new();
        slices.set_flag("discord", true);
        let cell = slices
            .register(
                discord_connection_slot(),
                ConnectionState {
                    connected: true,
                    detail: None,
                },
            )
            .expect("fresh registry");
        cell.update(|c| c.connected = true);

        // When dispatching the to-thread dynamic intent.
        let result = routes
            .action_for(
                &to_thread_dynamic(),
                ActionCtx {
                    state: &mut state,
                    slices: &slices,
                    key_bytes: Vec::new(),
                },
            )
            .expect("to-thread row attached");

        // Then the result carries the CreateThreadForSession bus command.
        assert_eq!(
            result.message_names,
            vec![std::any::type_name::<crate::CreateThreadForSession>()],
            "to-thread command name"
        );
    }

    #[rstest::rstest]
    #[test]
    fn rows_resolve_through_the_nominal_discord_scope() {
        // Given a route table with the discord rows attached.
        let routes = routed();

        // When dispatching an intent carrying the nominal discord scope.
        let result = routes.action_for(
            &to_thread_dynamic(),
            ActionCtx {
                state: &mut jinn_domain::common::app_state::AppState::default(),
                slices: &jinn_domain::common::slices::Slices::new(),
                key_bytes: Vec::new(),
            },
        );

        // Then the row resolved — proving the nominal scope is the
        // route-table key, not a real UI scope.
        assert!(result.is_some());
        // And the scope identity is `discord:actions`.
        assert_eq!(discord_scope().key(), "discord:actions");
    }
}
