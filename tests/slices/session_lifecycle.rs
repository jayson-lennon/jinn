//! Session-lifecycle slice-composition tests.
#![expect(
    clippy::unused_async_trait_impl,
    clippy::unused_async,
    reason = "trouper trait impls and harness constructors are async by contract even when nothing awaits"
)]

use jinn_kernel::common::state::State;
use jinn_session_lifecycle_msg::arg_input_scope;
use jinn_session_lifecycle_msg::arg_input_slot;
use jinn_slices::SliceHost;
use jinn_slices::route::EditIntent;
use jinn_slices::view::Viewport;
use marked_path::MarkedPathAccess;

#[rstest::rstest]
#[tokio::test]
async fn activation_registers_argument_popup_surfaces() {
    // Given fresh shared slice registries and lifecycle state.
    let slices = jinn_slices::Slices::new();
    // Production boot seeds every slice cell here, before any activation.
    jinn_cell_catalog::register_all_cells(&slices);
    let mut viewport = Viewport::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let routes = jinn_slices::KeyRoutes::new();
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let services = jinn_kernel::Services::new_fake().await;
    let state = State::new(jinn_kernel::AppState::default());
    let mut host = SliceHost::new(&slices, &mut viewport, &overlay_views, &routes, &system);

    // When the lifecycle slice activates.
    jinn_session_lifecycle::activate(
        &mut host,
        &services,
        state,
        jinn_session_lifecycle_msg::BuiltinRegistry::new(),
        "/bin/sh".to_owned(),
    );

    // Then the cell, input hook, overlay geometry, view, and route actions resolve.
    let scope = arg_input_scope();
    assert!(
        slices
            .reader::<jinn_session_lifecycle_msg::ArgInputState>(&arg_input_slot())
            .is_some()
    );
    assert!(routes.input_hook(&scope).is_some());
    assert!(slices.overlay(&scope).is_some());
    assert!(slices.overlay_slot(&scope).is_some());
    assert!(slices.overlay_selectable(&scope));
    assert!(overlay_views.view(&scope).is_some());
    assert!(routes.rows().iter().any(|row| {
        row.scope == scope
            && matches!(&row.outcome, jinn_slices::RouteOutcome::Action { action, .. } if *action == "confirm-lifecycle-args")
    }));
}

#[rstest::rstest]
fn registered_argument_hook_dispatches_through_dynamic_intent() {
    // Given a popup cell and a shared route table carrying lifecycle actions.
    let slices = jinn_slices::Slices::new();
    // Production boot seeds every slice cell here, before any activation.
    jinn_cell_catalog::register_all_cells(&slices);
    let cell = slices
        .reader::<jinn_session_lifecycle_msg::ArgInputState>(&arg_input_slot())
        .expect("the cell catalog registers the lifecycle argument slot");
    let routes = jinn_slices::KeyRoutes::new();
    jinn_session_lifecycle::arg_input::attach_rows(&routes, &cell);
    jinn_session_lifecycle::arg_input::register_input_hook(&routes, &cell);

    // When a Home intent reaches the registered hook.
    cell.update(|state| state.text.set("héllo".to_owned()));
    let hook = routes
        .input_hook(&arg_input_scope())
        .expect("activation registered the lifecycle argument hook");
    let result = hook(&EditIntent::CursorHome);

    // Then the input surface consumes the key and applies the cursor edit.
    assert!(result.is_some());
    assert_eq!(cell.read().text.cursor_pos, 0);
}

// ── The project picker's `<c-enter>` opens this picker ───────────────────

/// The project directory both cross-slice tests create a session at, rendered
/// into the fixture document the pickers activate against. The directory is
/// derived per-test from the test's own tempdir root.
///
/// The document is built through `toml::Value` and serialized, so the path is
/// escaped by the TOML writer rather than pasted into a literal: a Windows path
/// carries backslashes, and `\U` is an invalid TOML unicode escape — a raw
/// format! produced a document that failed to parse.
fn cross_slice_document(root: &std::path::Path) -> String {
    let project_path = root.join("jinn-cross-slice");
    let document = toml::Value::Table({
        let mut table = toml::Table::new();

        // `[[project.entry]]` — an array of tables, one entry per project.
        let mut entry = toml::Table::new();
        entry.insert(
            "path".to_owned(),
            toml::Value::String(project_path.display().to_string()),
        );
        let mut project = toml::Table::new();
        project.insert(
            "entry".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(entry)]),
        );
        table.insert("project".to_owned(), toml::Value::Table(project));

        // `[[session_lifecycle.script]]` — likewise an array of tables.
        let mut script = toml::Table::new();
        script.insert("name".to_owned(), toml::Value::String("dev".to_owned()));
        script.insert(
            "setup_command".to_owned(),
            toml::Value::String("/bin/true".to_owned()),
        );
        let mut lifecycle = toml::Table::new();
        lifecycle.insert(
            "script".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(script)]),
        );
        table.insert(
            "session_lifecycle".to_owned(),
            toml::Value::Table(lifecycle),
        );

        table
    });

    toml::to_string(&document).expect("fixture document serializes")
}

/// The project picker open, both pickers activated, and a scoped app state.
///
/// Both slices activate over the *same* registries — the two openers only meet
/// in production because they share one route table and one cell registry.
struct CrossSlice {
    /// Backs the fixture project path; alive for the whole test.
    _dir: tempfile::TempDir,
    slices: jinn_slices::Slices,
    routes: jinn_slices::KeyRoutes,
    config: jinn_config::ConfigLayer,
    state: jinn_kernel::AppState,
}

impl CrossSlice {
    /// Activates the project and lifecycle pickers side by side, with the
    /// project picker already open and a row highlighted.
    async fn new() -> Self {
        let dir = jinn_testutil::tempdir();
        let slices = jinn_slices::Slices::new();
        // Production boot seeds every slice cell here, before any activation.
        jinn_cell_catalog::register_all_cells(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let routes = jinn_slices::KeyRoutes::new();
        let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let config = jinn_config::testutil::config_layer(&cross_slice_document(
            dir.path.as_marked_path().as_path(),
        ));
        // `default_with_scope_focus`, not `default`: a scope push is a no-op
        // without the shared scope cell, and `<c-enter>` only acts on a
        // highlighted row.
        let state = jinn_kernel::AppState::default_with_scope_focus();
        state.frontend.attach_slices(slices.clone());
        {
            let mut host = jinn_slices::SliceHost::new(
                &slices,
                &mut viewport,
                &overlay_views,
                &routes,
                &system,
            );
            jinn_project::activate(&mut host);
            jinn_session_lifecycle::activate_picker(&mut host);
        }
        let mut this = Self {
            _dir: dir.dir,
            slices,
            routes,
            config,
            state,
        };
        this.open_project_picker();
        this
    }

    /// Opens the project picker through the kernel, so its rows are seeded.
    fn open_project_picker(&mut self) {
        let result = self.dispatch(
            "open-project-picker",
            jinn_project_msg::project_picker_scope(),
        );
        if let Some(jinn_slices::ScopeSignal::Push(id)) = result.scope_signal {
            self.state
                .frontend
                .scope_push(jinn_slices::FocusScope::Dynamic(id));
        }
    }

    /// Runs one dynamic intent through the kernel.
    ///
    /// The kernel is the only place a scope signal is applied and a
    /// scope-enter hook fires, so a test that dispatched the route action
    /// directly would never observe the picker seeding itself.
    fn dispatch(
        &mut self,
        action: &str,
        scope: jinn_slices::SliceScopeId,
    ) -> jinn_kernel::IntentResult {
        jinn_kernel::IntentHandler::handle(
            &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
                scope, action, action,
            )),
            &mut self.state,
            &self.slices,
            &self.routes,
            &self.config,
        )
    }

    /// The project picker's `<c-enter>`: chain into the lifecycle picker.
    fn press_control_enter(&mut self) -> jinn_kernel::IntentResult {
        self.dispatch(
            "new-session-with-lifecycle",
            jinn_project_msg::project_picker_scope(),
        )
    }

    /// The lifecycle picker's cell.
    fn lifecycle_cell(
        &self,
    ) -> jinn_slices::cell::TypedCell<jinn_session_lifecycle_msg::SessionLifecyclePickerState> {
        self.slices
            .reader(&jinn_session_lifecycle_msg::session_lifecycle_picker_slot())
            .expect("the lifecycle picker registers its cell at activation")
    }

    /// The lifecycle names the filter currently shows, in display order.
    fn visible_lifecycles(&self) -> Vec<String> {
        let cell = self.lifecycle_cell();
        let guard = cell.read();
        (0..guard.selection.filtered_count())
            .filter_map(|i| guard.selection.filtered_item(i))
            .map(|item| item.entry().name.clone())
            .collect()
    }

    /// The lifecycle picker's filter text.
    fn lifecycle_filter(&self) -> String {
        self.lifecycle_cell().read().selection.filter().to_owned()
    }

    /// Types into the lifecycle picker's filter through its input hook.
    fn type_into_lifecycle_filter(&self, ch: char) {
        let hook = self
            .routes
            .input_hook(&jinn_session_lifecycle_msg::session_lifecycle_picker_scope())
            .expect("the lifecycle picker registers a filter hook");
        hook(&EditIntent::InsertChar(ch)).expect("the filter hook always consumes the edit");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn project_control_enter_populates_the_lifecycle_picker() {
    // Given both pickers activated on a boot where the lifecycle picker has
    // never been opened.
    let mut cross = CrossSlice::new().await;
    assert!(
        cross.visible_lifecycles().is_empty(),
        "the lifecycle picker starts unpopulated — that is the bug this guards"
    );

    // When the project picker's `<c-enter>` runs.
    cross.press_control_enter();

    // Then the lifecycle picker lists blank plus the configured lifecycle.
    assert_eq!(
        cross.visible_lifecycles(),
        vec!["blank".to_owned(), "dev".to_owned()]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn project_control_enter_clears_a_stale_filter() {
    // Given a lifecycle picker already visited and filtered.
    let mut cross = CrossSlice::new().await;
    cross.press_control_enter();
    cross.type_into_lifecycle_filter('z');
    assert_eq!(
        cross.lifecycle_filter(),
        "z",
        "the filter is stale before the reopen"
    );

    // When the project picker's `<c-enter>` runs again.
    cross.press_control_enter();

    // Then the filter is empty.
    assert_eq!(cross.lifecycle_filter(), "");
}
