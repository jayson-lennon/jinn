//! The alt-i toggle row — the dictation slice's keybind.
//!
//! One row on the [`BindSite::GlobalToggle`] site: alt-i works from any
//! static scope and any other slice's dynamic scope, dispatching through
//! the route table as a dynamic intent carrying no payload.
//!
//! The action is the press matrix's synchronous half. It reads the state
//! cell (the actor's machine truth), acts on the current phase:
//!
//! - **Offline / Fetching / Loading / Arming / Error** — push the
//!   state-name hint (idempotent on repeat presses: the hint write is
//!   unconditional, and the every-intent clear prologue has already
//!   run, so the hint survives its own dispatch), and publish
//!   [`DictationToggle`] for the actor to fold (a no-op for every busy
//!   phase, the actor's idempotence seam).
//! - **Ready** — publish [`DictationToggle`]: the actor arms the mic and
//!   moves to Recording. No hint — the meter is the signal.
//! - **Recording** — publish [`DictationToggle`]: the actor disarms,
//!   force-finalizes the trailing audio, and appends it through the
//!   shared paste-append helper. No hint.
//!
//! A disabled slice reads Offline in the cell, so the disabled gate is
//! the Offline arm: the hint tells the user how to enable, the meter
//! never paints (the state cell never says Recording), and the actor
//! stays Offline because its init refused to run.

use jinn_dictation_msg::DictationPhase;
use jinn_dictation_msg::DictationState;
use jinn_dictation_msg::DictationToggle;
use jinn_dictation_msg::dictation_scope;
use jinn_dictation_msg::dictation_state_slot;
use jinn_slices::KeyRoutes;
use jinn_slices::RouteId;
use jinn_slices::RouteResult;
use jinn_slices::route::ActionCtx;
use jinn_slices::route::ActionFn;
use jinn_slices::route::BindSite;
use jinn_slices::route::RouteOutcome;
use jinn_slices::route::RouteRow;
use jinn_status_bar_msg::StatusBarState;
use jinn_status_bar_msg::status_bar_slot;

/// The route id for the dictation toggle.
pub const TOGGLE_ROUTE_ID: &str = "dictation:toggle";

/// The action name the row's dynamic intent carries.
pub const TOGGLE_ACTION: &str = "toggle";

/// The alt-i key, in keymap display form.
pub const TOGGLE_KEY: &str = "<M-i>";

/// Builds the dictation toggle row.
#[must_use]
pub fn toggle_row() -> RouteRow {
    RouteRow {
        route_id: RouteId::new(TOGGLE_ROUTE_ID),
        scope: dictation_scope(),
        key: TOGGLE_KEY,
        category: "general",
        site: BindSite::GlobalToggle,
        feature: "dictation",
        outcome: RouteOutcome::Action {
            action: TOGGLE_ACTION,
            display: "dictate",
            run: ActionFn::new(toggle_action),
        },
    }
}

/// Attaches the slice's rows.
pub fn attach_all(routes: &KeyRoutes) {
    routes.attach(toggle_row());
}

/// The press matrix, run synchronously on the state cell's truth.
///
/// The cell read goes through [`SliceActionState::as_any_mut`] — the
/// sanctioned downcast hook — because the dictation cells live in the
/// slice registry, not on the state; the registry arrives on the ctx.
fn toggle_action(mut ctx: ActionCtx<'_>) -> RouteResult {
    let phase = ctx
        .slices
        .reader::<DictationState>(&dictation_state_slot())
        .map_or(DictationPhase::Offline, |cell| cell.read().phase);

    match phase {
        // Armed cycle: publish; no hint (the meter is the signal).
        DictationPhase::Ready | DictationPhase::Recording => {
            RouteResult::new_message(DictationToggle)
        }
        // Busy (mid-arm, mid-download, mid-load), disabled, or failed:
        // the state-name hint, idempotent on repeat presses, plus the
        // toggle command for the actor's idempotent fold.
        DictationPhase::Arming
        | DictationPhase::Offline
        | DictationPhase::Fetching
        | DictationPhase::Loading
        | DictationPhase::Error => {
            let hint = ctx
                .slices
                .reader::<DictationState>(&dictation_state_slot())
                .map_or_else(
                    || "dictation unavailable".to_owned(),
                    |cell| {
                        cell.read()
                            .press_hint()
                            .unwrap_or_else(|| "dictation unavailable".to_owned())
                    },
                );
            push_hint(&mut ctx, hint);
            RouteResult::new_message(DictationToggle)
        }
    }
}

/// Pushes the transient hint onto the status bar's cell.
///
/// The existing slot, never a parallel channel: the IntentHandler's
/// every-intent prologue clears the hint before this action runs, so a
/// hint set here survives its own dispatch and clears on the next
/// keypress — the specified transient-hint semantics.
fn push_hint(ctx: &mut ActionCtx<'_>, hint: String) {
    if let Some(status) = ctx.slices.reader::<StatusBarState>(&status_bar_slot()) {
        status.update(|state| state.hint = Some(hint));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]
    #![allow(
        clippy::unwrap_used,
        reason = "test assertions on infallible registration"
    )]

    use super::TOGGLE_ACTION;
    use super::attach_all;
    use jinn_dictation_msg::DictationPhase;
    use jinn_dictation_msg::DictationState;
    use jinn_dictation_msg::dictation_scope;
    use jinn_dictation_msg::dictation_state_slot;
    use jinn_slices::DynamicIntent;
    use jinn_slices::Slices;
    use jinn_status_bar_msg::StatusBarState;
    use jinn_status_bar_msg::status_bar_slot;

    /// A registry seeded with the two dictation cells and the row.
    fn seeded() -> (Slices, jinn_slices::route::KeyRoutes) {
        let slices = Slices::new();
        slices
            .register(dictation_state_slot(), DictationState::default())
            .unwrap();
        slices
            .register(status_bar_slot(), StatusBarState::default())
            .unwrap();
        let routes = jinn_slices::route::KeyRoutes::new();
        attach_all(&routes);
        (slices, routes)
    }

    /// Dispatches the toggle intent through the row.
    fn dispatch(
        slices: &Slices,
        routes: &jinn_slices::route::KeyRoutes,
    ) -> jinn_slices::route::RouteResult {
        let mut state = jinn_app_state::AppState::default();
        let intent = DynamicIntent::new(dictation_scope(), TOGGLE_ACTION, "dictate");
        routes
            .action_for(
                &intent,
                jinn_slices::route::ActionCtx {
                    state: &mut state,
                    slices,
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .expect("the toggle row serves its own intent")
    }

    #[rstest::rstest]
    #[test]
    fn offline_press_pushes_the_disabled_hint_and_publishes() {
        // Given a registry whose state cell reads Offline (the default).
        let (slices, routes) = seeded();

        // When the toggle intent dispatches.
        let result = dispatch(&slices, &routes);

        // Then the transient hint names the disabled gate.
        let hint = slices
            .reader::<StatusBarState>(&status_bar_slot())
            .unwrap()
            .read()
            .hint
            .clone();
        assert_eq!(
            hint.as_deref(),
            Some("dictation disabled — set [dictation] enabled = true and restart")
        );
        // And the toggle command was published for the actor to fold.
        assert_eq!(result.message_names, ["DictationToggle"]);
    }

    #[rstest::rstest]
    #[test]
    fn ready_press_publishes_without_a_hint() {
        // Given a registry whose state cell reads Ready.
        let (slices, routes) = seeded();
        slices
            .reader::<DictationState>(&dictation_state_slot())
            .unwrap()
            .update(|state| state.phase = DictationPhase::Ready);

        // When the toggle intent dispatches.
        let result = dispatch(&slices, &routes);

        // Then the command was published and no hint was pushed: the
        // meter is the signal on arm.
        assert_eq!(result.message_names, ["DictationToggle"]);
        assert_eq!(
            slices
                .reader::<StatusBarState>(&status_bar_slot())
                .unwrap()
                .read()
                .hint,
            None
        );
    }

    #[rstest::rstest]
    #[test]
    fn fetching_press_repushes_the_download_hint_idempotently() {
        // Given a registry mid-fetch.
        let (slices, routes) = seeded();
        let cell = slices
            .reader::<DictationState>(&dictation_state_slot())
            .unwrap();
        cell.update(|state| {
            state.phase = DictationPhase::Fetching;
            state.model_name = "parakeet-tdt-0.6b-v2".to_owned();
        });

        // When the toggle intent dispatches twice.
        let _ = dispatch(&slices, &routes);
        let _ = dispatch(&slices, &routes);

        // Then the hint is the state name both times (idempotent).
        assert_eq!(
            slices
                .reader::<StatusBarState>(&status_bar_slot())
                .unwrap()
                .read()
                .hint
                .as_deref(),
            Some("downloading parakeet-tdt-0.6b-v2")
        );
    }

    #[rstest::rstest]
    #[test]
    fn error_press_carries_the_fault_description() {
        // Given a registry whose state cell reads Error with a fault.
        let (slices, routes) = seeded();
        slices
            .reader::<DictationState>(&dictation_state_slot())
            .unwrap()
            .update(|state| {
                state.phase = DictationPhase::Error;
                state.error = Some("fetching: disk full".to_owned());
            });

        // When the toggle intent dispatches.
        let _ = dispatch(&slices, &routes);

        // Then the hint carries the fault.
        assert_eq!(
            slices
                .reader::<StatusBarState>(&status_bar_slot())
                .unwrap()
                .read()
                .hint
                .as_deref(),
            Some("dictation failed: fetching: disk full")
        );
    }

    #[rstest::rstest]
    #[test]
    fn arming_press_pushes_the_arming_hint_and_publishes() {
        // Given a registry whose state cell reads Arming — a press is
        // already running the device open.
        let (slices, routes) = seeded();
        let cell = slices
            .reader::<DictationState>(&dictation_state_slot())
            .unwrap();
        cell.update(|state| {
            state.phase = DictationPhase::Arming;
            state.model_name = "parakeet-tdt-0.6b-v2".to_owned();
        });

        // When the toggle intent dispatches twice.
        let first = dispatch(&slices, &routes);
        let second = dispatch(&slices, &routes);

        // Then both presses publish the toggle for the actor's
        // idempotent fold, and the hint names the model both times —
        // a mid-arm press is an acknowledgement, not a state change.
        assert_eq!(first.message_names, ["DictationToggle"]);
        assert_eq!(second.message_names, ["DictationToggle"]);
        assert_eq!(
            slices
                .reader::<StatusBarState>(&status_bar_slot())
                .unwrap()
                .read()
                .hint
                .as_deref(),
            Some("arming parakeet-tdt-0.6b-v2")
        );
    }
}
