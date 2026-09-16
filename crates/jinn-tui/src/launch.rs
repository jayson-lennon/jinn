//! Single bootstrap point for [`TuiApp`] construction.
//!
//! Both the real launch path (the binary's `Commands::Tui` / `Commands::Bench`
//! arms) and the test builder ([`crate::TuiAppBuilder`]) delegate to
//! [`launch`], so keymap setup happens in exactly one place. This is what
//! prevents test/prod divergence in keymap binding.

use std::path::Path;

use error_stack::{Report, ResultExt};
use jinn_domain::common::system_resource::load_system_resource;
use jinn_domain::{AppCore, AppUiRegistry, State};
use jinn_sidebar::sections::register_sections;
use jinn_sidebar::sections::sidebar::Sidebar;
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
pub fn launch(
    core: AppCore,
    mut services: jinn_domain::Services,
) -> Result<TuiApp, Report<LaunchError>> {
    let paths = &services.paths;
    let intent_handler_cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
    load_compaction_prompt(
        &core.state,
        &paths.prompts_dir(),
        &paths.system_prompts_dir(),
        &intent_handler_cap,
    )?;
    load_theme(
        &core.state,
        &paths.themes_dir(),
        &paths.system_themes_dir(),
        &intent_handler_cap,
    );

    // Resolve mouse-selection config from environment.
    let mouse_selection = !matches!(std::env::var("JINN_MOUSE_SELECTION"), Ok(val) if val.eq_ignore_ascii_case("false") || val == "0");
    let tui_config = TuiConfig::new(mouse_selection);

    // The single keymap-bootstrap site. Production and tests reach this
    // via the same path. The terminal control-toggle binding comes from
    // `[interactive_term]` prefs, validated by the same parser the keymap
    // binds through (falls back to the default, loudly).
    let configured = core
        .state
        .read()
        .frontend
        .preferences
        .interactive_term
        .control_toggle_key
        .clone();
    let control_toggle = jinn_domain::feat::interactive_term::prefs::normalize_control_toggle_key(
        &configured,
    )
    .unwrap_or_else(|| {
        tracing::warn!(
            configured = %configured,
            default = jinn_domain::feat::interactive_term::prefs::DEFAULT_CONTROL_TOGGLE_KEY,
            "invalid [interactive_term] control_toggle_key; falling back to the default"
        );
        jinn_domain::feat::interactive_term::prefs::DEFAULT_CONTROL_TOGGLE_KEY.to_owned()
    });

    let mut ui_registry = AppUiRegistry::new();
    jinn_domain::register_all_ui_elements(&mut ui_registry);
    // The status-bar slice's element (the slice's cell is minted in the
    // actor-system bootstrap). Registered here because the kernel cannot
    // reference slice crates, and jinn-tui's registry assembly is the
    // composition point for display chrome.
    jinn_status_bar::register(&mut ui_registry);

    // Generated keymap bindings from the slice route rows attached
    // during actor-system bootstrap (single keymap bootstrap site).
    let mut keymap = keymap::init_with_control_toggle(&control_toggle);
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
        config: tui_config,
        sidebar: {
            let mut s = Sidebar::new();
            register_sections(&mut s);
            s
        },
        intent_handler_cap,
    })
}

/// Loads the compaction system prompt from user or system prompts directory.
///
/// Searches the user prompts directory first, then the system prompts directory.
///
/// # Errors
///
/// Returns an error if the compaction prompt is missing from both directories
/// or cannot be read. This is a fatal error - the application cannot run without it.
pub fn load_compaction_prompt(
    state: &State,
    user_dir: &Path,
    system_dir: &Path,
    cap: &jinn_domain::common::tcaps::IntentHandlerCap,
) -> Result<(), Report<LaunchError>> {
    let prompt =
        load_system_resource("_compaction.md", user_dir, system_dir).change_context(LaunchError)?;
    tracing::info!("loaded compaction prompt");
    state.write(cap).context.compaction_prompt = prompt;
    Ok(())
}

/// Loads the theme from user preferences into application state.
///
/// Searches the user themes directory first, then the system themes directory.
/// If the preferred theme cannot be loaded, falls back to the default theme.
/// Failures are logged but not fatal.
pub fn load_theme(
    state: &State,
    user_dir: &Path,
    system_dir: &Path,
    cap: &jinn_domain::common::tcaps::IntentHandlerCap,
) {
    let theme_name = {
        let guard = state.read();
        guard.frontend.app_state.theme_name.clone()
    };
    match jinn_domain::feat::theme::resolve_theme(theme_name.as_deref(), user_dir, system_dir) {
        Ok(theme) => {
            tracing::info!(theme = ?theme_name, "loaded theme");
            state.write(cap).frontend.theme = theme;
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
    services: &mut jinn_domain::Services,
    keymap: &mut ratatui_which_key::Keymap<
        jinn_domain::KeyEvent,
        Scope,
        jinn_domain::Intent,
        KeyCategory,
    >,
) {
    // Slice activation happens in the actor-system bootstrap
    // (`actor_wiring`), which is the async context kameo spawns need and
    // the only place that can put the dashboard first in spawn order.
    // This function runs after it, so every slice's rows exist by now.
    crate::keymap_gen::bind_route_rows(&services.key_routes, keymap);
    // Picker spec rows: keymap bindings for migrated pickers derive from
    // the specs themselves (same data the footer/geometry use).
    crate::keymap_gen::bind_picker_spec_rows(&services.picker_registry, keymap);
}
