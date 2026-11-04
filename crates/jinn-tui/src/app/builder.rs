//! Builder for constructing a [`TuiApp`] with sensible defaults for tests.

use jinn_kernel::AppCore;

use crate::TuiApp;
use crate::app::WhichKeyInstance;
use crate::app::scope_for_focus;
use crate::config::TuiConfig;
use crate::keymap;
use crate::selection::{SelectableRects, SelectionState};
use crate::suspend::Suspend;
use crate::{AppStatus, MsgHandler};

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
    services: Option<jinn_kernel::Services>,
    /// Optional app state override (defaults to default state).
    state: Option<jinn_kernel::AppState>,
}

impl TuiAppBuilder {
    /// Override the default services.
    #[must_use]
    pub fn services(mut self, services: jinn_kernel::Services) -> Self {
        self.services = Some(services);
        self
    }

    /// Override the default app state.
    #[must_use]
    pub fn state(mut self, state: jinn_kernel::AppState) -> Self {
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
            None => jinn_kernel::Services::new_fake().await,
        };
        let state = self.state.unwrap_or_default();

        // Every slice cell the render pass reads, registered by the same
        // catalog production boot uses, then attached so the FrontendState
        // facades read and write real storage. This builder runs no slice
        // activation, so before the catalog a popup whose cell it happened
        // not to name rendered nothing while the tests around it stayed
        // green.
        {
            let slices = services.slices.clone();
            jinn_cell_catalog::register_all_cells(&slices);
            // Both halves, from the same registry: a chat-input draft is
            // written through the session facade and read through the
            // frontend one, and `attach_slices` writes a `OnceLock`, so
            // attaching only one half means the write and its read resolve
            // different registries. Production wiring attaches both.
            state.frontend.attach_slices(slices.clone());
            state.session.attach_slices(slices);
        }

        let core = AppCore {
            state: jinn_kernel::State::new(state),
            bridge: services.bridge.clone(),
        };

        // The in-flight subagent registry: the same handle `Services`
        // carries, so the confirmed-cancel cascade reads what the `spawn_subagent`
        // tool writes (production wiring mirrors this in actor_wiring).
        core.state.write().task_spawns = services.task_spawns.clone();

        // The same element list the app runs with, so a test app draws
        // every element a real app draws. The chat input box is fetched
        // with `if let Some(..)`, so a missing registration would fail
        // silently — the box just never draws.
        let ui_registry = crate::ui_elements::build_ui_registry();

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
            clipboard_writer: crate::clipboard_writer::ClipboardWriter::arboard(),
            config: TuiConfig::default(),
        }
    }
}
