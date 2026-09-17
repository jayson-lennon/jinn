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

use jinn_domain::AppCore;
use jinn_sidebar::sections::register_sections;
use jinn_sidebar::sections::sidebar::Sidebar;
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
pub async fn launch_for_test(core: AppCore, mut services: jinn_domain::Services) -> TuiApp {
    let mut ui_registry = jinn_domain::AppUiRegistry::new();
    jinn_domain::register_all_ui_elements(&mut ui_registry);
    jinn_status_bar::register(&mut ui_registry);

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
        // Forward-bridge routes drain per slice: each relay registers on
        // the bus in its own on_start, so spawns may land before or after
        // the activations they serve. The dashboard's canvas actor
        // consumes the fabric + nav topics through these relays.
        jinn_dashboard::bridge::drain_routes(&services).await;
        drain_quake_bar_routes(&services).await;
        let activated = jinn_dashboard::activate(&mut jinn_dashboard::SliceCtx {
            slices: &services.slices,
            key_routes: &services.key_routes,
            viewport: &mut services.viewport,
            trouper_system: &services.trouper_system,
        });
        if let Err(error) = activated {
            panic!("dashboard slice activation failed: {error}");
        }
        activate_quake_bar(&mut services);
        activate_status_bar(&mut services);
        activate_scope_focus(&mut services);
        activate_chat_input(&mut services);
        activate_cwd(&mut services);
        activate_sidebar(&mut services);
        activate_theme(&mut services);
        activate_persona(&mut services);
        activate_token_count(&mut services);
        core.state
            .write_test_no_cap()
            .frontend
            .attach_slices(services.slices.clone());
        activate_session_init(&mut services, &core).await;
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
        config: TuiConfig::default(),
        sidebar: {
            let mut s = Sidebar::new();
            register_sections(&mut s);
            s
        },
        intent_handler_cap: jinn_domain::common::tcaps::mint::mint_intent_handler_cap(),
    }
}

/// Activates the quake-bar slice over the kernel's registries.
///
/// The slice crate is kernel-free, so composition assembles the
/// `SliceHost` borrows and hands them over.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn activate_quake_bar(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_quake_bar::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("quake-bar slice finalize failed: {error}");
    }
}

#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
fn activate_scope_focus(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_scope_focus::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("scope-focus slice finalize failed: {error}");
    }
}

fn activate_chat_input(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_chat_input::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("chat-input slice finalize failed: {error}");
    }
}

fn activate_status_bar(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_status_bar::activate(&mut host);
    let staged = host.finalize(&|_key| None);
    if let Err(error) = staged {
        panic!("status-bar slice finalize failed: {error}");
    }
}

/// Drains the quake-bar slice's staged forward route into its relay.
async fn drain_quake_bar_routes(services: &jinn_domain::Services) {
    jinn_domain::common::trouper_bridge::spawn_one::<jinn_quake_bar::SubmitQuakeBarCommand>(
        services,
        &jinn_slices::host::RouteEntry {
            schema_id:
                <jinn_quake_bar::SubmitQuakeBarCommand as trouper::schema::Schema>::schema_id(),
            name: "quake-bar",
            topic: jinn_quake_bar::command::quake_bar_topic(),
            direction: jinn_slices::host::Direction::Forward,
        },
    )
    .await;
}

/// Activates the session-init slice over the kernel's registries and
/// drains its crossing routes.
///
/// Session-init attaches no route rows (headless discovery), so the
/// harness call is exactly the production pairing: activate, then
/// drain. The drain must complete before the first trigger publishes —
/// `launch_for_test` composes before any session exists, so ordering
/// holds by construction here.
#[expect(
    clippy::panic,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
async fn activate_session_init(services: &mut jinn_domain::Services, core: &jinn_domain::AppCore) {
    // `Services` is cheap to clone (Arc fields); the clone side-steps
    // the host's mutable viewport borrow for the activation call.
    let services_snapshot = services.clone();
    let state = core.state.clone();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    if let Err(error) = jinn_session_init::activate(&mut host, &services_snapshot, state) {
        panic!("session-init slice activation failed: {error}");
    }
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("session-init slice finalize failed: {error}");
    }
    jinn_session_init::bridge::drain_routes(services).await;
}

/// A composed [`TuiApp`]: fake services plus every slice activated.
///
/// # Panics
///
/// Panics if slice activation fails — see [`launch_for_test`].
pub async fn test_app() -> TuiApp {
    let services = jinn_domain::Services::new_fake().await;
    let state = jinn_domain::AppState::default();
    let core = AppCore {
        state: jinn_domain::State::new(state),
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
pub fn composition_routes() -> jinn_domain::common::slices::key_routes::KeyRoutes {
    let routes = jinn_domain::common::slices::key_routes::KeyRoutes::new();
    jinn_dashboard::attach_dashboard_rows(&routes);
    // The quake rows' submit/scroll actions capture a cell handle; the
    // seam mints a detached one (never registered into a live `Slices`)
    // since only row *shape* matters for keymap tests.
    let slices = jinn_domain::common::slices::Slices::new();
    #[expect(
        clippy::expect_used,
        reason = "test seam: a fresh Slices never has the quake cell registered"
    )]
    let cell = slices
        .register(
            jinn_quake_bar::quake_bar_slot(),
            jinn_quake_bar::QuakeBarState::default(),
        )
        .expect("fresh Slices never has the quake cell registered");
    jinn_quake_bar::attach_quake_bar_rows(&routes, &cell);
    jinn_quake_bar::register_quake_input_hook(&routes, &cell);
    jinn_discord::attach_discord_rows(&routes);
    // The term slice's rows + capture key hook (the hook's scope shape
    // matters for hermeticity tests; the hook fn only encodes keys).
    jinn_term::route_rows::attach_rows(&routes, "<c-g>");
    jinn_term::key_hook::register(&routes);
    routes
}

/// A composed keymap: built-in scope bindings + every slice's rows.
///
/// The test twin of the composed bootstrap; tests that exercise slice
/// keys query this. The per-dynamic-scope `<M-t>` toggle is spread by
/// `bind_route_rows` itself — production parity, no manual chrome here.
#[must_use]
pub fn composed_keymap() -> ratatui_which_key::Keymap<
    jinn_domain::KeyEvent,
    jinn_tui::Scope,
    jinn_domain::Intent,
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
pub fn plain(ch: char) -> jinn_domain::KeyEvent {
    jinn_domain::KeyEvent {
        key: jinn_domain::Key::Char(ch),
        modifiers: jinn_domain::Modifiers::none(),
    }
}

/// Activates the sidebar slice on the harness services.
pub fn activate_sidebar(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_sidebar::activate(&mut host);
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("sidebar slice finalize failed: {error}");
    }
}

/// Activates the theme slice on the harness services, scanning the real
/// user/system theme directories when they exist.
pub fn activate_token_count(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    let _cache = jinn_token_count::activate(&mut host);
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("token-count slice finalize failed: {error}");
    }
}

pub fn activate_persona(services: &mut jinn_domain::Services) {
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
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("persona slice finalize failed: {error}");
    }
}

pub fn activate_theme(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_theme_slice::activate(
        &mut host,
        &services.paths.themes_dir(),
        &services.paths.system_themes_dir(),
    );
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("theme slice finalize failed: {error}");
    }
}

/// Activates the cwd slice on the harness services.
pub fn activate_cwd(services: &mut jinn_domain::Services) {
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut services.viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_cwd::activate(&mut host);
    if let Err(error) = host.finalize(&|_key| None) {
        panic!("cwd slice finalize failed: {error}");
    }
}

#[cfg(test)]
mod term_keybinds_spot_check {
    use super::composed_keymap;
    use jinn_domain::{Intent, Key, KeyEvent, Modifiers};
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

    #[rstest::rstest]
    fn capture_hermeticity_and_view_binds_hold_in_composition() {
        // In capture (term:control): <M-t> stays hermetic — no overlay
        // intent fires; the key forwards to the pty like any other
        // (encoded as the ESC-prefix bytes a program expects).
        let intent = wk(Scope::Dynamic(jinn_term_msg::control_scope())).handle_key(alt_t());
        assert!(
            !matches!(&intent, Some(Intent::Dynamic(d)) if d.action == "toggle-overlay"),
            "<M-t> must not toggle in capture: {intent:?}"
        );
        assert!(
            matches!(&intent, Some(Intent::Dynamic(d)) if d.bytes == vec![0x1b, b't']),
            "<M-t> in capture must forward as ESC+t: {intent:?}"
        );
        // ...printable keys forward with bytes...
        let intent = wk(Scope::Dynamic(jinn_term_msg::control_scope())).handle_key(KeyEvent {
            key: Key::Char('a'),
            modifiers: Modifiers::none(),
        });
        let Some(Intent::Dynamic(d)) = &intent else {
            panic!("capture must forward: {intent:?}");
        };
        assert_eq!(d.bytes, b"a".to_vec());
        // ...<c-c> forwards as ETX...
        let intent = wk(Scope::Dynamic(jinn_term_msg::control_scope())).handle_key(KeyEvent {
            key: Key::Char('c'),
            modifiers: Modifiers::ctrl(),
        });
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.bytes == vec![0x03]));
        // ...f-keys forward...
        let intent = wk(Scope::Dynamic(jinn_term_msg::control_scope())).handle_key(KeyEvent {
            key: Key::F(4),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.bytes == b"\x1bOS".to_vec()));
        // ...and the configured toggle beats the catch-all (handback).
        let intent = wk(Scope::Dynamic(jinn_term_msg::control_scope())).handle_key(KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        });
        let Some(Intent::Dynamic(d)) = &intent else {
            panic!("toggle must beat catch-all: {intent:?}");
        };
        assert_eq!(d.action, "release-control");

        // In view (term:view): toggle, yank, push, chrome, T resolve.
        let view = || Scope::Dynamic(jinn_term_msg::view_scope());
        let intent = wk(view()).handle_key(alt_t());
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.action == "toggle-overlay"));
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('y'),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.action == "yank-screen"));
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('I'),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.action == "push-screen"));
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('T'),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(&intent, Some(Intent::Dynamic(d)) if d.action == "toggle-for-selected"));
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('q'),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(intent, Some(Intent::Quit)));
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('?'),
            modifiers: Modifiers::none(),
        });
        assert!(matches!(intent, Some(Intent::ToggleWhichkey)));

        // Deliberately unbound in view: <M-`> and `i`.
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('`'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        });
        assert!(
            intent.is_none(),
            "<M-`> must stay unbound in view: {intent:?}"
        );
        let intent = wk(view()).handle_key(KeyEvent {
            key: Key::Char('i'),
            modifiers: Modifiers::none(),
        });
        assert!(
            intent.is_none(),
            "`i` must stay unbound in view: {intent:?}"
        );
    }
}
