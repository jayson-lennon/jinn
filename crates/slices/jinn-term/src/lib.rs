//! Interactive terminal sessions — drive TUI programs from the agent.
//!
//! The term slice owns the whole PTY actor family: spawn, emulation,
//! settle detection, realtime screen mirroring, and the coordinator
//! that keeps one terminal per chat session. The kernel's tools speak
//! to the coordinator through the `TermHandle` trait; the TUI reads
//! the `term/tabs` cell and renders the slice's overlay views.
//!
//! Keybinds are slice data: [`activate`] attaches the route rows (see
//! [`route_rows`]) and the capture key hook (see [`key_hook`]) plus the
//! overlay geometry/renderer (see [`overlay`]). Deleting this crate
//! deletes the feature's entire keymap footprint.

pub mod emulator;
pub mod interactive_term_actor;
pub mod key_hook;
pub mod overlay;
pub mod pty_session;
pub mod query_responder;
pub mod route_rows;
pub mod screen_task;
pub mod settle;

/// Activates the term slice over the kernel's services: registers the
/// `term/tabs` cell (idempotent), attaches the keybind rows and the
/// capture key hook, and registers the overlay geometry + renderer for
/// both overlay scopes.
///
/// The configured control-toggle key is read from the snapshot's
/// `[interactive_term]` preferences, normalized (falling back to the
/// default, loudly), and interned once — row keys are `&'static str`,
/// and this runs exactly once per launch over a bounded set of config
/// values, so the leaked allocation is deliberate and bounded.
///
/// Does **not** mint the shared control registry
/// ([`jinn_term_msg::TERM_CONTROLS`]): actor wiring owns that
/// set-once static so it can share the minted registry with the
/// coordinator actor it spawns afterwards.
pub fn activate(
    services: &mut jinn_domain::common::services::Services,
    state: &jinn_domain::common::state::State,
) {
    // The cell is registered by composition today (actor_wiring) so the
    // spawn order matches the tools-registry cell; re-registering is a
    // no-op error we ignore deliberately.
    let _ = services.slices.register(
        jinn_term_msg::term_tabs_slot(),
        jinn_term_msg::TerminalTabState::default(),
    );

    // The configured control-toggle binding, validated by the same parser
    // the keymap binds through (falls back to the default, loudly).
    let configured = {
        let snapshot = state.read();
        snapshot
            .frontend
            .preferences
            .interactive_term
            .control_toggle_key
            .clone()
    };
    let toggle_key = jinn_term_msg::prefs::normalize_control_toggle_key(&configured)
        .unwrap_or_else(|| {
            tracing::warn!(
                configured = %configured,
                default = jinn_term_msg::prefs::DEFAULT_CONTROL_TOGGLE_KEY,
                "invalid [interactive_term] control_toggle_key; falling back to the default"
            );
            jinn_term_msg::prefs::DEFAULT_CONTROL_TOGGLE_KEY.to_owned()
        });
    // Row keys are `&'static str`; the normalized key is interned once
    // per launch (bounded, config-derived — a deliberate leak).
    let toggle_key: &'static str = Box::leak(toggle_key.into_boxed_str());

    route_rows::attach_rows(&services.key_routes, toggle_key);
    key_hook::register(&services.key_routes);
    overlay::register_views(&services.slices, &services.overlay_views);

    tracing::debug!("term slice activated");
}
