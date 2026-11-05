//! Single bootstrap point for [`TuiApp`] construction.
//!
//! Both the real launch path (the binary's `Commands::Tui` / `Commands::Bench`
//! arms) and the test builder ([`crate::TuiAppBuilder`]) delegate to
//! [`launch`], so keymap setup happens in exactly one place. This is what
//! prevents test/prod divergence in keymap binding.

use error_stack::{Report, ResultExt};
use jinn_kernel::common::system_resource::load_system_resource;
use jinn_kernel::{AppCore, AppUiRegistry, State};
use marked_path::{Absolute, MarkedPath};
use wherror::Error;

use crate::app::WhichKeyInstance;
use crate::config::TuiConfig;
use crate::keymap;
use crate::keymap::KeyCategory;
use crate::scope::Scope;
use crate::selection::{SelectableRects, SelectionState};
use crate::suspend::Suspend;
use crate::{AppStatus, MsgHandler, TuiApp};

/// Error returned by [`launch`] when TUI bootstrap fails.
///
/// The only currently-fatal bootstrap step is loading the compaction prompt:
/// the application cannot run without it. Theme and prompt-template failures
/// degrade to defaults and are logged at `warn`.
#[derive(Debug, Error)]
#[error(debug)]
pub struct LaunchError;

/// Construct a fully-bootstrapped [`TuiApp`] from the supplied core, services,
/// actor host.
///
/// Owns the full TUI bootstrap sequence:
/// 1. Load prompt templates, the compaction prompt, and the theme into `core.state`.
/// 2. Read `JINN_MOUSE_SELECTION` to resolve mouse-selection behavior.
/// 3. Build the keymap via [`keymap::init`] — this is the single site
///    that does so, shared by production and tests.
/// 4. Register all UI elements and sidebar sections.
/// 5. Assemble and return the [`TuiApp`].
///
/// # Errors
///
/// Returns `Err` if the compaction prompt cannot be loaded (the application
/// cannot run without it).
/// Assembles the TUI around an already-wired actor system.
///
/// `ui_registry` is built by the composition root (see
/// `bootstrap::ui::build_ui_registry`) and passed in: the four elements
/// whose display widgets live in slice crates are registered there,
/// because the kernel's registry cannot reference slice crates and this
/// crate is the wrong place to know which slices exist.
pub fn launch(
    core: AppCore,
    mut services: jinn_kernel::Services,
    ui_registry: AppUiRegistry,
) -> Result<TuiApp, Report<LaunchError>> {
    let paths = &services.paths;
    load_compaction_prompt(&paths.prompts_dir().as_marked_path())?;
    load_theme(&core.state, &paths.themes_dir().as_marked_path());

    // Resolve mouse-selection config from environment.
    let mouse_selection = !matches!(std::env::var("JINN_MOUSE_SELECTION"), Ok(val) if val.eq_ignore_ascii_case("false") || val == "0");
    let tui_config = TuiConfig::new(mouse_selection);

    // The single keymap-bootstrap site. Production and tests reach this
    // via the same path. (The terminal control-toggle binding is a slice
    // route row — attached from `jinn_term::activate` during actor-system
    // bootstrap, not here.)

    // Generated keymap bindings from the slice route rows attached
    // during actor-system bootstrap (single keymap bootstrap site).
    let mut keymap = keymap::init();
    register_slice_wiring(&mut services, &mut keymap);
    let which_key = WhichKeyInstance::new(keymap, Scope::Normal);

    Ok(TuiApp {
        core,
        services,
        ui_registry,
        events: MsgHandler::new(),
        which_key,
        suspend: Suspend::new(),
        event_thread: None,
        status: AppStatus::Starting,
        selection: SelectionState::Idle,
        selectable_rects: SelectableRects::default(),
        pending_clipboard: false,
        clipboard_writer: crate::clipboard_writer::ClipboardWriter::arboard(),
        config: tui_config,
    })
}

/// Loads the compaction system prompt from the user prompts directory.
///
/// # Errors
///
/// Returns an error if the compaction prompt is missing from the user prompts
/// directory or cannot be read. This is a fatal error - the application cannot
/// run without it.
pub fn load_compaction_prompt(
    user_dir: &MarkedPath<Absolute>,
) -> Result<String, Report<LaunchError>> {
    let prompt = load_system_resource("_compaction.md", user_dir).change_context(LaunchError)?;
    tracing::info!("loaded compaction prompt");
    Ok(prompt)
}

/// Loads the theme from user preferences into application state.
///
/// Resolves the theme from the user themes directory. If the preferred theme
/// cannot be loaded, falls back to the default theme.
/// Failures are logged but not fatal.
pub fn load_theme(state: &State, user_dir: &MarkedPath<Absolute>) {
    let theme_name = {
        let guard = state.read();
        guard.frontend.app_state.theme_name.clone()
    };
    match jinn_theme::resolve_theme(theme_name.as_deref(), user_dir) {
        Ok(theme) => {
            tracing::info!(theme = ?theme_name, "loaded theme");
            state.write().frontend.theme = theme;
        }
        Err(e) => {
            tracing::warn!(err = ?e, "failed to load theme, using default");
        }
    }
}

/// Generates slice keymap bindings from the attached route rows.
///
/// Slice activation (cells, actors, views, tab/overlay descriptors)
/// happens in the actor-system bootstrap (`actor_wiring::build`) for the
/// production path. This function runs after it, on the freshly built
/// keymap, so slice bindings land in the same tree as the built-in scope
/// bindings. A slice whose activate is commented out leaves no keymap,
/// scope, or which-key residue: removability is automatic.
fn register_slice_wiring(
    services: &mut jinn_kernel::Services,
    keymap: &mut ratatui_which_key::Keymap<
        jinn_kernel::KeyEvent,
        Scope,
        jinn_kernel::KernelIntent,
        KeyCategory,
    >,
) {
    // Slice activation happens in the actor-system bootstrap
    // (`actor_wiring`), which is the async context actor spawns need and
    // the only place that can put the dashboard first in spawn order.
    // This function runs after it, so every slice's rows exist by now.
    crate::keymap_gen::bind_route_rows(&services.key_routes, keymap);
    // Picker spec rows: keymap bindings for migrated pickers derive from
    // the specs themselves (same data the footer/geometry use).
}
