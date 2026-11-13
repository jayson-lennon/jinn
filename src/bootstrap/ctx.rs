//! Everything the boot list needs beyond the registries themselves.
//!
//! The [`Ctx`] carries the shared state, the services container, and the
//! values a few slices cannot construct for themselves: the prune-worker
//! list (built from the config layer, which the root reads), the
//! handoff prompt, and the session actor's dependency bundle.
//!
//! It carries no producer state. The three values later activations read
//! are resolved from the registry by slot key, so list order expresses
//! the dependency rather than a field here.

use jinn_kernel::Services;
use jinn_kernel::State;

/// The activation-time inputs composition owns.
pub struct Ctx<'a> {
    state: &'a State,
    services: &'a mut Services,
    handoff_prompt: String,
    token_counter: jinn_llm_support::token_estimator::TiktokenCounter,
    image_converter: jinn_llm_support::image_convert::ImageConverterService,
}

impl<'a> Ctx<'a> {
    /// Assembles the context for one boot.
    ///
    /// `handoff_prompt` is read at startup and carried on the struct as
    /// the last-good fallback its point-of-use reload falls back to. The
    /// shell is not carried here: it was resolved at launch and rides on
    /// the services container, so every consumer reads the same resolved
    /// value.
    #[must_use]
    pub fn new(state: &'a State, services: &'a mut Services, handoff_prompt: String) -> Self {
        Self {
            state,
            services,
            handoff_prompt,
            token_counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::system(),
        }
    }

    /// The shared application state every actor writes through.
    pub(crate) const fn state(&self) -> &State {
        self.state
    }

    /// The services container, for slices that hand it to the actors they
    /// spawn.
    pub(crate) const fn services(&self) -> &Services {
        self.services
    }

    /// The services container, mutably: two slices register a cell
    /// through the raw container rather than the host.
    pub(crate) fn services_mut(&mut self) -> &mut Services {
        self.services
    }

    /// Assembles a host borrowing all five kernel registries.
    ///
    /// One borrow per activation: each statement gets a fresh host, so the
    /// boot list reads as a list rather than as repeated plumbing. A `Vec`
    /// of closures all borrowing one `&mut Ctx` would not compile, which
    /// is why the per-slice wrapper functions this replaces existed.
    #[must_use]
    pub fn host(&mut self) -> jinn_slices::SliceHost<'_, jinn_slices::RenderFacts> {
        jinn_slices::SliceHost::new(
            &self.services.slices,
            &mut self.services.viewport,
            &self.services.overlay_views,
            &self.services.key_routes,
            &self.services.trouper_system,
        )
    }

    /// Resolves a producer slice's cell by slot key, aborting if absent.
    ///
    /// The three producers at the top of the boot list register these
    /// cells, so a missing one means a consumer ran before its producer —
    /// a wiring bug, not a runtime condition.
    ///
    /// # Panics
    ///
    /// Panics when the cell was not registered at boot — a wiring bug,
    /// not a runtime condition, and one a silent `Option` would hide.
    #[expect(
        clippy::panic,
        reason = "a missing boot cell is a wiring bug; failing loudly here beats an Option every caller must unwrap"
    )]
    pub(crate) fn cell<T>(&self, slot: &jinn_slices::SlotKey) -> jinn_slices::cell::TypedCell<T>
    where
        T: Send + Sync + 'static,
    {
        match self.services.slices.reader::<T>(slot) {
            Some(cell) => cell,
            None => {
                panic!("cell not registered at boot: {slot}");
            }
        }
    }

    /// The prune-worker list context-curation runs.
    ///
    /// Every strategy is constructed unconditionally and reads its own
    /// subsection inside `evaluate`. Gating here instead would make a
    /// disabled strategy an ABSENT worker, and then a `reload` could only
    /// ever turn a strategy ON — there would be no worker left to turn
    /// off. Reading live makes enablement symmetric in both directions.
    pub(crate) fn prune_workers(
        &self,
    ) -> Vec<Box<dyn jinn_context_curation::worker::HistoryWorker>> {
        use jinn_context_curation::strategies::{
            AnchoredAssistantAutoPruneWorker, BrokenEditAutoPruneWorker,
            ConsecutiveReadsAutoPruneWorker, DoubleEditAutoPruneWorker, EditReadAutoPruneWorker,
            ReadEditAutoPruneWorker, RegexAutoPruneWorker, TodoAutoPruneWorker,
            ToolAgeWindowAutoPruneWorker, TrivialAssistantAutoPruneWorker,
        };
        use jinn_preferences_config::schemas::auto_prune::{
            AnchoredAssistantAutoPruneConfig, BrokenEditAutoPruneConfig,
            ConsecutiveReadsAutoPruneConfig, DoubleEditAutoPruneConfig, EditReadAutoPruneConfig,
            ReadEditAutoPruneConfig, TodoAutoPruneConfig, ToolAgeWindowAutoPruneConfig,
            TrivialAssistantAutoPruneConfig,
        };
        use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;

        let config = self.services.config.clone();
        let entry_token_cache = HistoryWorkerChatEntryTokenCache::default();

        vec![
            Box::new(ReadEditAutoPruneWorker {
                layer: config.clone(),
                config: ReadEditAutoPruneConfig::default(),
            }),
            Box::new(EditReadAutoPruneWorker {
                layer: config.clone(),
                config: EditReadAutoPruneConfig::default(),
            }),
            Box::new(BrokenEditAutoPruneWorker {
                layer: config.clone(),
                config: BrokenEditAutoPruneConfig::default(),
            }),
            Box::new(DoubleEditAutoPruneWorker {
                layer: config.clone(),
                config: DoubleEditAutoPruneConfig::default(),
            }),
            Box::new(ConsecutiveReadsAutoPruneWorker {
                layer: config.clone(),
                config: ConsecutiveReadsAutoPruneConfig::default(),
            }),
            Box::new(ToolAgeWindowAutoPruneWorker {
                layer: config.clone(),
                config: ToolAgeWindowAutoPruneConfig::default(),
            }),
            Box::new(TodoAutoPruneWorker {
                layer: config.clone(),
                config: TodoAutoPruneConfig::default(),
            }),
            Box::new(TrivialAssistantAutoPruneWorker {
                layer: config.clone(),
                config: TrivialAssistantAutoPruneConfig::default(),
                token_cache: entry_token_cache.clone(),
                counter: self.token_counter,
            }),
            Box::new(AnchoredAssistantAutoPruneWorker {
                // The anchor radius and the trivial-assistant floor are
                // both read live; this seed only describes the shape.
                layer: config.clone(),
                config: AnchoredAssistantAutoPruneConfig::default(),
                token_cache: entry_token_cache,
                counter: self.token_counter,
            }),
            Box::new(RegexAutoPruneWorker::new(config)),
        ]
    }

    /// The session-turn reducer's dependencies.
    ///
    /// The token cache is resolved from the cell `jinn_token_count`
    /// registered in Block 1 of the boot list, by slot key — the actor
    /// accumulates through the same instance the slice minted.
    pub(crate) fn session_actor_deps(
        &self,
    ) -> jinn_session_turn::session_actor::SessionPersistenceActorDeps {
        let token_cache = self
            .services
            .slices
            .reader::<jinn_token_count_msg::HistoryWorkerChatEntryTokenCache>(
            &jinn_token_count_msg::token_cache_slot(),
        );
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: self.services.clone(),
            },
            state: self.state.clone(),
            counter: self.token_counter,
            token_cache: token_cache.map_or_default(|cell| cell.read().clone()),
            image_converter: self.image_converter.clone(),
            handoff_prompt: self.handoff_prompt.clone(),
        }
    }
}
