//! The slice-facing activation surface — one `SliceHost` per
//! `activate()` call.
//!
//! A slice's entire kernel integration is a sequence of registration
//! verbs on this host: mint a cell, spawn a service actor on trouper,
//! attach route rows, register an input hook, register views, tabs,
//! overlays, and read its config section. In-tree Rust slices call the
//! verbs imperatively from `activate(&mut SliceHost, …)`; a future WASM
//! guest host will perform the same verbs from validated manifest
//! messages. Naming the verbs once means the guest activation technique
//! arrives as a consumer of this surface, not a parallel system.
//!
//! The host borrows the kernel's registries for the duration of
//! activation and adds three of its own: the bridge route registry
//! ([`RouteRegistry`]), the input-hook registry ([`HookRegistry`]), and
//! the config-section set ([`SectionSet`]). All three stage during
//! activation and are drained by composition afterwards:
//!
//! - hooks install into the kernel's [`KeyRoutes`] once every slice has
//!   activated;
//! - sections convert against the kernel's user-preferences document at
//!   `finalize` time, where a missing section or malformed TOML aborts
//!   launch — activation-time fail-fast, concentrated in one checked
//!   step;
//! - routes are the manifest the bridge drain wiring consumes (see
//!   [`StagedRoutes`]; the drain actors themselves belong to the
//!   kernel's bridge task).
//!
//! The host is constructed per activation and never stored on
//! `Services`.

use std::sync::Arc;

use trouper::actor::ActorPath;
use trouper::actor::ServiceActor;
use trouper::system::ActorSystem;

use crate::overlay::OverlayViewFn;
use crate::overlay::OverlayViews;
use crate::route::EditIntent;
use crate::route::KeyRoutes;
use crate::route::RouteResult;
use crate::route::RouteRow;
use crate::slice_scope::SliceScopeId;
use crate::slices::Slices;
use crate::slices::SlotKey;
use crate::slices::SlotTaken;
use crate::view::Viewport;

pub mod host_config;
pub mod host_input;
pub mod host_routes;
pub mod host_view;

pub use host_config::ConfigSection;
pub use host_config::ConfigSectionError;
pub use host_config::DynamicConfigSection;
pub use host_config::SectionError;
pub use host_routes::Direction;
pub use host_routes::ForwardMessage;
pub use host_routes::ReverseMessage;
pub use host_routes::RouteConflict;
pub use host_routes::RouteEntry;
pub use host_routes::StagedRoutes;
pub use host_view::HostView;

/// The kernel registries a [`SliceHost`] borrows for one activation.
pub struct SliceHost<'a, C: 'static> {
    slices: &'a Slices,
    viewport: &'a mut Viewport,
    overlay_views: &'a OverlayViews<C>,
    key_routes: &'a KeyRoutes,
    system: &'a ActorSystem,
    routes: host_routes::RouteRegistry,
    hooks: host_input::HookRegistry,
    sections: host_config::SectionSet,
}

impl<'a, C: 'static> SliceHost<'a, C> {
    /// Assembles a host over the kernel's registries.
    #[must_use]
    pub fn new(
        slices: &'a Slices,
        viewport: &'a mut Viewport,
        overlay_views: &'a OverlayViews<C>,
        key_routes: &'a KeyRoutes,
        system: &'a ActorSystem,
    ) -> Self {
        Self {
            slices,
            viewport,
            overlay_views,
            key_routes,
            system,
            routes: host_routes::RouteRegistry::default(),
            hooks: host_input::HookRegistry::default(),
            sections: host_config::SectionSet::default(),
        }
    }

    /// The trouper actor system, for slice actors that spawn with
    /// custom builders (cell-injecting `start_with` overrides).
    #[must_use]
    pub fn system(&self) -> &'a ActorSystem {
        self.system
    }

    /// The kernel's key-route table, for slices that attach rows with
    /// captured cell handles (the row actions close over them).
    #[must_use]
    pub fn key_routes(&self) -> &'a KeyRoutes {
        self.key_routes
    }

    /// Mints the one write handle for a slice cell.
    ///
    /// # Errors
    ///
    /// Returns [`SlotTaken`] if the slot is already registered —
    /// double activation is a wiring bug.
    pub fn register_cell<T: Send + Sync + 'static>(
        &self,
        key: SlotKey,
        initial: T,
    ) -> Result<crate::cell::TypedCell<T>, SlotTaken> {
        self.slices.register(key, initial)
    }

    /// Spawns a trouper service actor at `path` with typed
    /// subscriptions. Subscribe is the readiness point: topic cursors
    /// register synchronously inside `start`, so publishes after this
    /// call cannot be missed.
    ///
    /// `build` constructs the actor instance, capturing whatever cell
    /// handles and channels it needs (they cannot ride trouper's JSON
    /// args).
    pub fn spawn_service<A, T, E>(&self, path: ActorPath, build: T) -> ActorPath
    where
        A: ServiceActor + Send + Sync + 'static,
        T: FnOnce() -> Result<A, E> + Send + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        trouper::builder::spawn_service_builder::<A>(self.system)
            .at(path)
            .start_with(move || {
                Box::pin(async move {
                    build().map_err(|err| {
                        error_stack::Report::new(SpawnError)
                            .attach(err.into())
                            .change_context(trouper::registry::RegistryError::InvalidSpec)
                    })
                })
            })
            .start()
    }

    /// Attaches route rows to the key-route table.
    pub fn attach_rows<R>(&self, rows: R)
    where
        R: IntoIterator<Item = RouteRow>,
    {
        for row in rows {
            self.key_routes.attach(row);
        }
    }

    /// Registers the synchronous input hook for a slice's scope: an
    /// editing-intent interceptor served while that scope is focused.
    /// Staged on the host; composition installs all hooks into
    /// [`KeyRoutes`] after activation.
    pub fn register_input_hook<F>(&mut self, scope: SliceScopeId, serve: F)
    where
        F: Fn(&EditIntent) -> Option<RouteResult> + Send + Sync + 'static,
    {
        self.hooks.register(scope, Arc::new(serve));
    }

    /// Registers a typed tab view; the view/slot pairing is verified
    /// immediately against the slices registry.
    ///
    /// # Errors
    ///
    /// Returns [`crate::view::ViewSlotError`] if the view's slot is
    /// unregistered or holds a different payload type — a wiring bug
    /// that must abort launch.
    pub fn register_view<V>(&mut self, view: V) -> Result<(), crate::view::ViewSlotError>
    where
        V: crate::view::SliceView + Send + Sync + 'static,
    {
        self.viewport.register(view, self.slices)
    }

    /// Declares a tab scope backed by a slot.
    pub fn register_tab_scope(&self, scope: SliceScopeId, slot: SlotKey) {
        self.slices.register_tab_scope(scope, slot);
    }

    /// Registers a slice overlay's geometry for its scope.
    pub fn register_overlay(&self, scope: SliceScopeId, overlay: crate::slices::OverlayFn) {
        self.slices.register_overlay(scope, overlay);
    }

    /// Declares the slot backing a scope's overlay content.
    pub fn register_overlay_slot(&self, scope: SliceScopeId, slot: SlotKey) {
        self.slices.register_overlay_slot(scope, slot);
    }

    /// Registers a scope's overlay renderer.
    /// Marks `scope`'s overlay rect as a selectable region (popups with
    /// focusable content).
    pub fn register_overlay_selectable(&self, scope: SliceScopeId) {
        self.slices.register_overlay_selectable(&scope);
    }

    pub fn register_overlay_view(&self, scope: SliceScopeId, view: OverlayViewFn<C>) {
        self.overlay_views.register(scope, view);
    }

    /// Sets the slice's feature flag in the registry (read model for
    /// route-action gates).
    pub fn set_flag(&self, slice: &str, enabled: bool) {
        self.slices.set_flag(slice, enabled);
    }

    /// Reads the slice's config section as a typed value. The section
    /// table is snapshotted now; conversion to `T` happens when the
    /// sections are applied — during activation via
    /// [`Self::apply_sections`] (a slice that needs its config value
    /// before returning calls this itself) or at composition's
    /// `finalize`. A missing table or malformed TOML aborts launch
    /// there.
    #[must_use]
    pub fn config_section<T>(&mut self, key: &str) -> ConfigSection<T>
    where
        T: serde::de::DeserializeOwned + Default + Send + 'static,
    {
        self.sections.add_typed::<T>(key)
    }

    /// Reads the slice's config section as a raw TOML table — the
    /// WASM-shaped dynamic face, snapshotted under the same gate.
    #[must_use]
    pub fn config_section_value(&mut self, key: &str) -> DynamicConfigSection {
        self.sections.add_dynamic(key)
    }

    /// Declares a forward bridge route: bus publishes of `M` cross to
    /// trouper `topic`.
    ///
    /// The `Schema` impl is supplied as a thunk because schema
    /// definitions belong to the message-owning crate; id equality is
    /// checked eagerly, the definition is taken verbatim.
    pub fn forward<M: ForwardMessage, S>(&mut self, topic: trouper::topics::Topic, schema: S)
    where
        S: FnOnce() -> trouper::schema::SchemaDef,
    {
        self.routes.forward::<M, S>(topic, schema);
    }

    /// Declares a reverse bridge route: trouper `topic` publishes of
    /// `M` cross to the bus.
    pub fn reverse<M: ReverseMessage, S>(&mut self, topic: trouper::topics::Topic, schema: S)
    where
        S: FnOnce() -> trouper::schema::SchemaDef,
    {
        self.routes.reverse::<M, S>(topic, schema);
    }

    /// Drains staged input hooks into the kernel's route table via
    /// `install`. Called by composition after all slices activate.
    pub fn install_hooks<I>(self, install: I)
    where
        I: FnMut(SliceScopeId, crate::route::InputHook),
    {
        self.hooks.install(install);
    }

    /// Resolves the staged config sections through `resolve` (the
    /// kernel's document lookup) so the slice's [`ConfigSection`]
    /// handles hold values immediately. Sections applied here are
    /// resolved again — harmlessly, overwriting the slot — at
    /// composition's `finalize`.
    ///
    /// # Errors
    ///
    /// Returns [`SectionError`] for a missing required section or a
    /// malformed table — the activation-time fail-fast gate, raised
    /// before the slice reads its value.
    pub fn apply_sections(
        &mut self,
        resolve: &dyn Fn(&str) -> Option<toml::Table>,
    ) -> Result<(), SectionError> {
        self.sections.apply(resolve)
    }

    /// Applies staged config sections through `sink` (composition
    /// provides the lookup into the user-preferences document) and
    /// returns the staged bridge routes for the drain wiring.
    ///
    /// # Errors
    ///
    /// Returns [`SectionError`] for a missing required section or a
    /// malformed table — the activation-time fail-fast gate.
    pub fn finalize(
        self,
        sink: &dyn Fn(&str) -> Option<toml::Table>,
    ) -> Result<StagedRoutes, SectionError> {
        let routes = self.routes;
        let sections = self.sections;
        sections.apply(sink)?;
        Ok(StagedRoutes::from_registry(routes))
    }
}

/// The build-failure error for [`SliceHost::spawn_service`] overrides.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct SpawnError;
