//! Shared harness for the slice-composition integration tests (`tests/slices`).
//!
//! Tests here compose the real system: real slice activation over the
//! kernel's registries, real route rows, real cells and actors. Everything
//! the production launch path does — minus the terminal itself.
//!
//! This harness exists in the root crate's `tests/` (not inside a slice or
//! the tui crate) because that is the only place a test may depend on
//! several slice crates at once: `just check` and IDE analysis never
//! compile integration targets, so the tui crate stays slice-free.

#![allow(clippy::expect_used, clippy::panic, reason = "test harness")]

use jinn_kernel::AppCore;
use jinn_kernel::common::render_test::seed_cell;
use jinn_tui::TuiApp;
use jinn_tui::app::WhichKeyInstance;
use jinn_tui::config::TuiConfig;
use jinn_tui::keymap;
use jinn_tui::selection::{SelectableRects, SelectionState};
use jinn_tui::suspend::Suspend;
use jinn_tui::{AppStatus, MsgHandler};

/// Builds a `TuiApp` over a **fully activated** slice system.
///
/// Drains the slices' forward-bridge routes, activates the dashboard and
/// quake-bar slices, and generates the composed keymap from every
/// attached row — the test twin of the production bootstrap
/// (`actor_wiring::build` + `launch`).
///
/// # Panics
///
/// Panics if slice activation fails — the harness cannot compose without
/// the slices it exists to test.
pub async fn launch_for_test(core: AppCore, mut services: jinn_kernel::Services) -> TuiApp {
    let mut ui_registry = jinn_kernel::AppUiRegistry::new();
    jinn_kernel::register_all_ui_elements(&mut ui_registry);
    jinn_status_bar::register(&mut ui_registry);
    jinn_chat_input::register(&mut ui_registry);

    // Slice activation on the ambient runtime (test path is async).
    // `Services` itself is mutated: the viewport is the render-side view
    // registry and `Viewport::clone` is an empty shell by design, so
    // views must register into the instance that reaches `TuiApp`.
    let mut keymap = keymap::init();
    // The two activate calls below cannot panic directly, but the keymap
    // bootstrap after them must abort launch on a broken pairing.
    #[expect(
        clippy::panic,
        reason = "bootstrap assertion: a broken pairing must abort launch, not render blank"
    )]
    {
        let activated = jinn_dashboard::activate(&mut jinn_slices::SliceHost::new(
            &services.slices,
            &mut services.viewport,
            &services.overlay_views,
            &services.key_routes,
            &services.trouper_system,
        ));
        if let Err(error) = activated {
            panic!("dashboard slice activation failed: {error}");
        }
        activate_quake_bar(&mut services);
        activate_status_bar(&mut services);
        activate_scope_focus(&mut services);
        activate_chat_input(&mut services);
        activate_cwd(&mut services);
        activate_project(&mut services);
        activate_preferences(&mut services);
        activate_sidebar(&mut services, core.state.clone()).await;
        activate_theme(&mut services);
        activate_persona(&mut services);
        activate_token_count(&mut services, core.state.clone()).await;
        activate_turn_dispatch(&mut services, core.state.clone()).await;
        activate_inference(&mut services).await;
        activate_watchdog(&mut services, &core.state).await;
        activate_citations(&mut services).await;
        // The work-time monitor. Must precede the turn-dispatch and session
        // actors' first publish, or the opening edge of the first turn
        // reaches no subscriber.
        activate_work_time(&mut services);
        // Every slice-owned picker, in the same order as `actor_wiring`.
        activate_every_picker(&mut services);
        jinn_tools::activate(&mut services, &core.state);
        core.state
            .write()
            .frontend
            .attach_slices(services.slices.clone());
        activate_session_init(&mut services, &core).await;
        // Provider-selection: mints the provider cell + spawns the
        // provider/discover actors (production wiring calls the same
        // activation before the boot trio, whose init actor writes the
        // disk-loaded cache through the returned cell).
        activate_provider_selection(&mut services, &core.state);
        // The layout pool and its supervisor — the component that owns the
        // preview deadline. Production wiring installs these in
        // `actor_wiring::build`; without them here, nothing in the composed test
        // app can ever time a preview out, so any test of that path would hang
        // for reasons that have nothing to do with the code under test.
        jinn_chat_log_view::kernel_element::install_layout_actors(
            &services.trouper_system,
            core.state.clone(),
        );
        // Bindings generate after all activations so every slice's rows exist.
        jinn_tui::keymap_gen::bind_route_rows(&services.key_routes, &mut keymap);
    }

    let initial_scope = jinn_tui::app::scope_for_focus(&core.state.read().frontend.scope());

    TuiApp {
        core,
        services,
        ui_registry,
        events: MsgHandler::new(),
        which_key: WhichKeyInstance::new(keymap, initial_scope),
        suspend: Suspend::new(),
        event_thread: None,
        status: AppStatus::Starting,
        selection: SelectionState::Idle,
        selectable_rects: SelectableRects::default(),
        pending_clipboard: false,
        clipboard_writer: jinn_tui::clipboard_writer::ClipboardWriter::arboard(),
        config: TuiConfig::default(),
    }
}

/// Activates the quake-bar slice over the kernel's registries.
///
/// The slice crate is kernel-free, so composition assembles the
/// `SliceHost` borrows and hands them over.
fn activate_quake_bar(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_quake_bar::activate(&mut host);
}

fn activate_scope_focus(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_scope_focus::activate(&mut host);
}

fn activate_chat_input(services: &mut jinn_kernel::Services) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps the
    // host's mutable viewport borrow, matching the production wiring.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let deps = jinn_kernel::common::actor_deps::ActorDeps {
        services: services_snapshot,
    };
    let state = jinn_kernel::common::state::State::new(jinn_kernel::AppState::default());
    jinn_chat_input::activate(&mut host, deps, &state);
}

fn activate_status_bar(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_status_bar::activate(&mut host);
}

/// Activates the session-init slice over the kernel's registries and
/// drains its crossing routes.
///
/// Session-init attaches no route rows (headless discovery), so the
/// harness call is exactly the production pairing: activate, then
/// drain. The drain must complete before the first trigger publishes —
/// `launch_for_test` composes before any session exists, so ordering
/// holds by construction here.
/// Activates the provider-selection slice: mints the provider cell and
/// spawns the provider + discover actors over the same `State` and
/// trouper system the harness wires.
fn activate_provider_selection(services: &mut jinn_kernel::Services, state: &jinn_kernel::State) {
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let handles = jinn_provider_selection::activate(&mut host, &services_snapshot, state.clone());
    // The three pickers this slice owns, registered in the same order as
    // `actor_wiring`. The reasoning picker mints its own cell; the other two
    // reuse the cells `activate` already registered (the provider actor
    // publishes fetches into them).
    jinn_provider_selection::activate_picker(&mut host);
    jinn_provider_selection::activate_provider_picker(&mut host, &handles.provider_picker_cell);
    jinn_provider_selection::activate_endpoint_picker(&mut host, &handles.endpoint_picker_cell);
}

async fn activate_session_init(services: &mut jinn_kernel::Services, core: &jinn_kernel::AppCore) {
    let state = core.state.clone();
    if let Err(error) = jinn_session_init::activate(services, state) {
        panic!("session-init slice activation failed: {error}");
    }
}

/// A composed [`TuiApp`]: fake services plus every slice activated.
///
/// # Panics
///
/// Panics if slice activation fails — see [`launch_for_test`].
pub async fn test_app() -> TuiApp {
    let services = jinn_kernel::Services::new_fake().await;
    let state = jinn_kernel::AppState::default();
    let core = AppCore {
        state: jinn_kernel::State::new(state),
        bridge: services.bridge.clone(),
    };
    launch_for_test(core, services).await
}

/// A `KeyRoutes` pre-seeded with every slice's rows, mirroring what
/// composition produces at launch (all `activate()` calls made).
///
/// # Panics
///
/// Panics if the detached quake cell cannot be minted (a fresh
/// `Slices` never has it registered, so this is unreachable).
#[must_use]
pub fn composition_routes() -> jinn_slices::route::KeyRoutes {
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_dashboard::attach_dashboard_rows(&routes);
    // The quake rows' submit/scroll actions capture a cell handle; the
    // seam mints a detached one (never registered into a live `Slices`)
    // since only row *shape* matters for keymap tests.
    let slices = jinn_slices::Slices::new();
    let cell = seed_cell(
        &slices,
        &jinn_quake_bar::quake_bar_slot(),
        jinn_quake_bar::QuakeBarState::default(),
    );
    jinn_quake_bar::attach_quake_bar_rows(&routes, &cell);
    jinn_quake_bar::register_quake_input_hook(&routes, &cell);
    jinn_discord::attach_discord_rows(&routes);
    // The term slice's rows + capture key hook (the hook's scope shape
    // matters for hermeticity tests; the hook fn only encodes keys).
    jinn_term::route_rows::attach_rows(&routes, "<c-g>");
    jinn_term::key_hook::register(&routes);
    jinn_sidebar::key_routes::attach_sidebar_rows(&routes);
    jinn_chat_log_view::routes::attach_all(&routes);
    routes
}

/// A composed keymap: built-in scope bindings + every slice's rows.
///
/// The test twin of the composed bootstrap; tests that exercise slice
/// keys query this. The per-dynamic-scope `<M-t>` toggle is spread by
/// `bind_route_rows` itself — production parity, no manual chrome here.
#[must_use]
pub fn composed_keymap() -> ratatui_which_key::Keymap<
    jinn_kernel::KeyEvent,
    jinn_tui::Scope,
    jinn_kernel::KernelIntent,
    jinn_tui::KeyCategory,
> {
    let routes = composition_routes();
    let mut keymap = keymap::init();
    jinn_tui::keymap_gen::bind_route_rows(&routes, &mut keymap);
    keymap
}

/// Waits (bounded) for `predicate` to hold, polling the async runtime.
///
/// Slice actors apply routed messages asynchronously; tests must wait
/// for the observable effect instead of sleeping a fixed duration.
///
/// # Panics
///
/// Panics after ~2s when the predicate never holds — the failure message
/// names what the test was waiting for.
#[expect(
    clippy::panic,
    reason = "bounded wait: a never-true predicate is a test failure"
)]
pub async fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// `wait_for` with a caller-chosen budget for tests that legitimately
/// process large message batches (the default 2s window is sized for
/// single-event propagation).
///
/// # Panics
///
/// Panics after `secs` when the predicate never holds.
#[expect(
    clippy::panic,
    reason = "bounded wait: a never-true predicate is a test failure"
)]
pub async fn wait_for_bounded(what: &str, secs: u64, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Builds a plain (unmodified) character `KeyEvent`.
#[must_use]
pub fn plain(ch: char) -> jinn_kernel::KeyEvent {
    jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Char(ch),
        modifiers: jinn_kernel::Modifiers::none(),
    }
}

/// Activates the sidebar slice on the harness services. Async because
/// the drain awaits relay startup on the ambient tokio runtime.
pub async fn activate_sidebar(services: &mut jinn_kernel::Services, state: jinn_kernel::State) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_sidebar::activate(&mut host, state);
}

/// Activates the token-count slice on the harness services. Async because
/// the drain awaits relay startup on the ambient tokio runtime.
pub async fn activate_token_count(services: &mut jinn_kernel::Services, state: jinn_kernel::State) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_token_count::activate(&mut host, state);
}

/// Activates the work-time slice on the harness services (the monitor actor
/// bound to the catalogued interval cell). The cell comes from the same
/// catalog production boot uses, so the monitor and any reader resolve the
/// identical instance rather than a private one that records into nothing.
pub fn activate_work_time(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_work_time::activate(&mut host);
}

/// Activates the turn-dispatch slice on the harness services (the queue
/// actor + its crossing routes). Must run before anything publishes an
/// `Idle` phase event or a `DispatchTurn` — subscribe is the readiness
/// point. Async because the drain awaits relay startup on the ambient
/// tokio runtime.
pub async fn activate_turn_dispatch(
    services: &mut jinn_kernel::Services,
    state: jinn_kernel::State,
) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_turn_dispatch::activate(&mut host, state, services_snapshot);
}

/// Activates the inference slice: spawns the inference actor (trouper
/// ServiceActor) and stages its crossing routes, then drains them.
pub async fn activate_inference(services: &mut jinn_kernel::Services) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_inference::activate(
        &mut host,
        jinn_kernel::State::new(jinn_kernel::AppState::default()),
        services_snapshot,
    );
}

/// Activates the watchdog slice: spawns the stall + tool-call watchdog
/// actors (trouper ServiceActors) on the kernel's trouper system.
///
/// Watchdog knobs are read once from the `State` snapshot (the term-slice
/// precedent); a test that wants a fast trip writes a smaller
/// `[stall_watchdog].timeout_secs` into the snapshot's preferences
/// *before* calling this helper.
pub async fn activate_watchdog(services: &mut jinn_kernel::Services, state: &jinn_kernel::State) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_watchdog::activate(&mut host, state, services_snapshot);
    eprintln!("DIAG16 watchdog activation ran");
}

/// Activates the citations slice: spawns the citations actor (trouper
/// ServiceActor) on the kernel's trouper system.
pub async fn activate_citations(services: &mut jinn_kernel::Services) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_citations::activate(&mut host, services_snapshot);
}

pub fn activate_persona(services: &mut jinn_kernel::Services) {
    // `Services::new_fake*` pre-seeds the personas cell the way production
    // wiring does; re-activating would trip the once-only slot invariant.
    if services
        .slices
        .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        .is_some()
    {
        return;
    }
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let _scanned = jinn_persona::activate(&mut host, &services.paths.personas_dir());
}

pub fn activate_theme(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_theme_slice::activate(&mut host, &services.paths.themes_dir().as_marked_path());
    jinn_theme_slice::activate_picker(&mut host);
}

/// Activates the cwd slice on the harness services.
pub fn activate_cwd(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_cwd::activate(&mut host);
}

/// Activates the project slice on the harness services.
pub fn activate_project(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_project::activate(&mut host);
}

/// Activates the preferences slice on the harness services.
pub fn activate_preferences(services: &mut jinn_kernel::Services) {
    let system = services.trouper_system.clone();
    let services_handle = services.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_preferences::activate(
        &mut host,
        &system,
        services_handle,
        jinn_kernel::common::state::State::new(
            jinn_kernel::common::app_state::AppState::default_with_scope_focus(),
        ),
    );
}

/// Activates the remaining slice-owned pickers over the harness services.
///
/// The provider-selection pickers are activated by
/// [`activate_provider_selection`], which owns the cells they share.
///
/// Several pickers here register their own cell, and more than one activation
/// path calls in (persona's pre-seeded-cell branch calls this too). Registering
/// a slot twice is a wiring error, so each registration is attempted once and
/// the picker activated only when the registration succeeded.
pub fn activate_every_picker(services: &mut jinn_kernel::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );

    // Every picker below is activated unconditionally. It used to be guarded
    // by "is the slot still free?", a dance that existed only because each
    // picker minted its own cell and more than one activation path could
    // reach it. The shared cell catalog registers all of them first, so the
    // cell is always present and the guard had become a silent way to skip
    // attaching a picker's rows entirely — which is exactly the failure
    // `every_picker_scope_owns_rows_in_the_test_composition` exists to catch.
    jinn_persona::activate_picker(&mut host);

    let session_cell = services
        .slices
        .reader::<jinn_session_store_msg::SessionPickerState>(
            &jinn_session_store_msg::session_picker_slot(),
        )
        .expect("the cell catalog registers the session picker slot");
    jinn_session_store::activate_session_picker(&mut host, &session_cell);

    // Also activates the task-list picker this slice owns.
    jinn_tools::activate_picker(&mut host);
    jinn_skills::activate(&mut host);
    jinn_mcp_slice::activate_picker(&mut host);
    jinn_session_lifecycle::activate_picker(&mut host);
}

#[cfg(test)]
mod term_keybinds_spot_check {
    use super::composed_keymap;

    use jinn_kernel::{KernelIntent, Key, KeyEvent, Modifiers};
    use jinn_tui::Scope;
    use jinn_tui::app::WhichKeyInstance;

    fn wk(scope: Scope) -> WhichKeyInstance {
        WhichKeyInstance::new(composed_keymap(), scope)
    }

    fn alt_t() -> KeyEvent {
        KeyEvent {
            key: Key::Char('t'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        }
    }

    /// A `KeyEvent` for a printable character with exactly `modifiers` set.
    fn char_event(c: char, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            key: Key::Char(c),
            modifiers,
        }
    }

    /// The modifier set of a bare <c-X> chord.
    fn ctrl_only() -> Modifiers {
        Modifiers {
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// The capture (term:control) and view (term:view) scopes of the
    /// terminal slice's keymap spot check.
    ///
    /// Two scopes, two invariants: capture is hermetic — nothing toggles the
    /// overlay there, every key forwards to the pty as the bytes a program
    /// expects — and view binds its own actions while deliberately leaving
    /// <M-`> and `i` unbound.
    /// The terminal slice's two keymap scopes, as a spot check over the
    /// composed keymap.
    ///
    /// Two invariants, deliberately exercised through the one keymap the app
    /// actually composes: capture is hermetic — nothing there toggles the
    /// overlay, every key forwards to the pty as the bytes a program expects
    /// — and view binds its own actions while deliberately leaving <M-`> and
    /// `i` unbound.
    #[rstest::rstest]
    fn capture_hermeticity_and_view_binds_hold_in_composition() {
        // Given the terminal slice's capture scope, queried in the composed
        // keymap.
        let capture = || Scope::Dynamic(jinn_term_msg::control_scope());

        // When each of its forwarding keys is pressed there.
        let alt_t_intent = wk(capture()).handle_key(alt_t());
        let printable_intent = wk(capture()).handle_key(char_event('a', Modifiers::none()));
        let ctrl_c_intent = wk(capture()).handle_key(char_event('c', Modifiers::ctrl()));
        let f4_intent = wk(capture()).handle_key(KeyEvent {
            key: Key::F(4),
            modifiers: Modifiers::none(),
        });
        let ctrl_g_intent = wk(capture()).handle_key(char_event('g', ctrl_only()));

        // Then <M-t> stays hermetic: no overlay intent fires, and the key
        // forwards to the pty like any other (the ESC-prefix bytes a program
        // expects).
        assert!(
            !matches!(&alt_t_intent, Some(KernelIntent::Dynamic(d)) if d.action == "toggle-overlay"),
            "<M-t> must not toggle in capture: {alt_t_intent:?}"
        );
        assert!(
            matches!(&alt_t_intent, Some(KernelIntent::Dynamic(d)) if d.bytes == vec![0x1b, b't']),
            "<M-t> in capture must forward as ESC+t: {alt_t_intent:?}"
        );
        // And printable keys forward with bytes.
        let Some(KernelIntent::Dynamic(printable)) = &printable_intent else {
            panic!("capture must forward: {printable_intent:?}");
        };
        assert_eq!(printable.bytes, b"a".to_vec());
        // And <c-c> forwards as ETX.
        assert!(matches!(&ctrl_c_intent, Some(KernelIntent::Dynamic(d)) if d.bytes == vec![0x03]));
        // And f-keys forward.
        assert!(
            matches!(&f4_intent, Some(KernelIntent::Dynamic(d)) if d.bytes == b"\x1bOS".to_vec())
        );
        // And the configured toggle beats the catch-all (handback).
        let Some(KernelIntent::Dynamic(handback)) = &ctrl_g_intent else {
            panic!("toggle must beat catch-all: {ctrl_g_intent:?}");
        };
        assert_eq!(handback.action, "release-control");
    }

    /// The same spot check, read from the terminal slice's view scope: its
    /// own actions resolve, and two keys stay deliberately unbound.
    #[rstest::rstest]
    fn view_binds_hold_in_composition() {
        // Given the terminal slice's view scope, queried in the composed
        // keymap.
        let view = || Scope::Dynamic(jinn_term_msg::view_scope());

        // When its bound keys are pressed there.
        let alt_t_intent = wk(view()).handle_key(alt_t());
        let yank_intent = wk(view()).handle_key(char_event('y', Modifiers::none()));
        let push_intent = wk(view()).handle_key(char_event('I', Modifiers::none()));
        let toggle_for_selected = wk(view()).handle_key(char_event('T', Modifiers::none()));
        let quit_intent = wk(view()).handle_key(char_event('q', Modifiers::none()));
        let whichkey_intent = wk(view()).handle_key(char_event('?', Modifiers::none()));

        // Then toggle, yank, push, chrome and T all resolve.
        assert!(
            matches!(&alt_t_intent, Some(KernelIntent::Dynamic(d)) if d.action == "toggle-overlay")
        );
        assert!(
            matches!(&yank_intent, Some(KernelIntent::Dynamic(d)) if d.action == "yank-screen")
        );
        assert!(
            matches!(&push_intent, Some(KernelIntent::Dynamic(d)) if d.action == "push-screen")
        );
        assert!(
            matches!(&toggle_for_selected, Some(KernelIntent::Dynamic(d)) if d.action == "toggle-for-selected")
        );
        // And `q` quits.
        assert!(matches!(quit_intent, Some(KernelIntent::Quit)));
        // And `?` opens which-key.
        assert!(matches!(
            whichkey_intent,
            Some(KernelIntent::ToggleWhichkey)
        ));
    }

    /// The view scope's two deliberate gaps: <M-`> and `i` bind nothing.
    #[rstest::rstest]
    fn view_leaves_alt_backtick_and_i_unbound() {
        // Given the terminal slice's view scope, queried in the composed
        // keymap.
        let view = || Scope::Dynamic(jinn_term_msg::view_scope());

        // When <M-`> and `i` are pressed there.
        let alt_backtick = wk(view()).handle_key(KeyEvent {
            key: Key::Char('`'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        });
        let plain_i = wk(view()).handle_key(char_event('i', Modifiers::none()));

        // Then <M-`> stays unbound.
        assert!(
            alt_backtick.is_none(),
            "<M-`> must stay unbound in view: {alt_backtick:?}"
        );
        // And `i` stays unbound.
        assert!(
            plain_i.is_none(),
            "`i` must stay unbound in view: {plain_i:?}"
        );
    }

    #[rstest::rstest]
    fn sidebar_t_resolves_to_the_session_terminal_row() {
        // Given the composed keymap queried in the sidebar sessions scope.
        let sessions = jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id();
        let mut wk = wk(Scope::Dynamic(sessions));

        // When pressing 'T'.
        let intent = wk.handle_key(KeyEvent {
            key: Key::Char('T'),
            modifiers: Modifiers::none(),
        });

        // Then the session-terminal row resolves.
        assert!(
            matches!(&intent, Some(KernelIntent::Dynamic(d)) if d.action == "session-terminal"),
            "sidebar T must resolve the session-terminal row, got {intent:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn session_terminal_row_publishes_nothing() {
        // Given the sidebar's session-terminal row dispatching in its scope.
        let routes = jinn_slices::route::KeyRoutes::new();
        jinn_sidebar::key_routes::attach_sidebar_rows(&routes);
        let sessions = jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id();
        let mut state = jinn_kernel::AppState::default_with_scope_focus();

        // When firing the row.
        let result = routes
            .action_for(
                &jinn_slices::DynamicIntent::new(sessions, "session-terminal", "toggle terminal"),
                jinn_slices::route::ActionCtx {
                    state: &mut state,
                    slices: &jinn_slices::Slices::new(),
                    config: jinn_slices::empty_config_layer(),
                    key_bytes: Vec::new(),
                },
            )
            .expect("session-terminal row must dispatch");

        // Then it publishes nothing. This row used to publish a
        // `KernelIntent::Dynamic` naming the term slice's action, on the
        // assumption the message would be routed back into dispatch. It is
        // not: a published message goes to the bus, and no actor subscribes
        // to `KernelIntent`, so `T` did nothing. A `RouteResult` carries no
        // local-dispatch channel, so the row now calls the term slice's
        // handler directly -- which is why the overlay opens, and why there
        // is no message here.
        assert!(
            result.messages.is_empty(),
            "session-terminal must not publish, got {:?}",
            result.message_names
        );
    }
}
