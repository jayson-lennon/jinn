//! Builder for constructing a [`TuiApp`] with sensible defaults for tests.

use jinn_domain::AppCore;

use crate::TuiApp;
use crate::app::WhichKeyInstance;
use crate::app::scope_for_focus;
use crate::config::TuiConfig;
use crate::keymap;
use crate::selection::{SelectableRects, SelectionState};
use crate::suspend::Suspend;
use crate::{AppStatus, MsgHandler};
use jinn_sidebar::sections::register_sections;
use jinn_sidebar::sections::sidebar::Sidebar;

/// Builder for constructing a [`TuiApp`] with sensible defaults for tests.
///
/// All fields default to fake/noop implementations. Override only what the
/// test needs.
///
/// The built app is **slice-free**: the keymap carries only the built-in
/// scope bindings (`keymap::init`). Tests exercising slice keys, cells, or
/// actors live in the root crate's `tests/` integration targets, where a
/// slice-activating harness composes the real system.
///
/// # Panics
///
/// Panics if scope resolution for the default state fails — unreachable
/// for the default scope stack.
#[derive(Default)]
pub struct TuiAppBuilder {
    /// Optional services override (defaults to fake services).
    services: Option<jinn_domain::Services>,
    /// Optional app state override (defaults to default state).
    state: Option<jinn_domain::AppState>,
}

impl TuiAppBuilder {
    /// Override the default services.
    #[must_use]
    pub fn services(mut self, services: jinn_domain::Services) -> Self {
        self.services = Some(services);
        self
    }

    /// Override the default app state.
    #[must_use]
    pub fn state(mut self, state: jinn_domain::AppState) -> Self {
        self.state = Some(state);
        self
    }

    /// Build the `TuiApp` with the configured overrides.
    ///
    /// Mirrors [`crate::launch::launch`]'s assembly with the fatal
    /// bootstrap steps skipped (no on-disk prompt/theme files) and no
    /// slice wiring: route rows are a composition concern, and this
    /// builder never touches slice crates.
    pub async fn build(self) -> TuiApp {
        let services = match self.services {
            Some(s) => s,
            None => jinn_domain::Services::new_fake().await,
        };
        let state = self.state.unwrap_or_default();

        // Scope-focus cell: activate + attach so the FrontendState
        // facade reads/writes real storage (kernel-free: the cell is
        // minted through the shared Slices registry directly).
        {
            let slices = services.slices.clone();
            let _ = slices.register(
                jinn_slices::scope_focus_slot(),
                jinn_slices::ScopeFocusState::default(),
            );
            state.frontend.attach_slices(slices);
        }

        let core = AppCore {
            state: jinn_domain::State::new(state),
            bridge: services.bridge.clone(),
        };

        let mut ui_registry = jinn_domain::AppUiRegistry::new();
        jinn_domain::register_all_ui_elements(&mut ui_registry);

        let keymap = keymap::init();
        let initial_scope = scope_for_focus(&core.state.read().frontend.scope());

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
}
