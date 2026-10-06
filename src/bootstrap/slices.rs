//! The boot list: every slice activation, in one place, in order.
//!
//! This is the file to read to answer "what runs at launch, and in what
//! order". Each entry is one activation call. Adding a slice is one line
//! here.
//!
//! # The three blocks
//!
//! **Block 0 — the cell catalog.** Every slice cell in the workspace is
//! registered by one function, [`jinn_cell_catalog::register_all_cells`].
//! It runs before any slice activates, because every activation below
//! resolves its own cell by slot key — a slice that activated against an
//! unregistered cell would render nothing and panic.
//!
//! **Block 1 — producers.** These three register cells that later
//! activations resolve by slot key. They run first so a consumer can
//! never observe a missing cell.
//!
//! **Block 2 — independents.** Nothing below provides a value another
//! activation reads. Order within the block is free, subject only to the
//! constraints called out inline.
//!
//! # The ordering constraints that are behavioural
//!
//! Six orderings are load-bearing rather than cosmetic:
//!
//! 0. The cell catalog runs before every activation. Not an optimisation:
//!    it is the precondition for the whole list.
//! 1. The dashboard is first, always. Its canvas actor is subscribed to
//!    the actor-census schema, and `subscribe` is the readiness point —
//!    any actor spawned before it would be missing from the census
//!    entirely.
//! 2. Provider-selection precedes boot. Boot's provider-init actor
//!    writes the disk-loaded cache through the cell minted here.
//! 3. MCP-coordinator precedes the `EnvironmentLoaded` publish. That
//!    publish seeds the welcome session, which may publish
//!    `McpEnablementChanged` immediately, and the coordinator's
//!    subscription must already exist.
//! 4. Turn-dispatch, inference, and context-curation precede the
//!    session-actor spawn: those actors' subscriptions must exist before
//!    any dispatch, stream, `HistoryAppended`, or `TriggerCompaction`.
//! 5. `AllActorsSpawned` is published only after every subscription
//!    above exists, or the readiness signal races the wiring it means to
//!    certify.

use crate::bootstrap::Ctx;

/// Everything the boot list hands back to the `build` sequence.
pub struct Activated {
    /// The boot trio's handles: the env-init ask target and the
    /// main-thread readiness receiver.
    pub boot: jinn_boot::BootHandles,
    /// The discord slice's parked gateway channels and validated config.
    pub discord: jinn_discord::ActivatedDiscord,
}

/// Activates every slice, in boot order.
///
/// Split from the surrounding `build` sequence so the wiring is one
/// readable function rather than one interleaved with state setup and
/// the startup tail.
///
/// # Errors
///
/// Returns an error if a fallible slice activation fails. Callers abort
/// launch: a slice that cannot register its cell is a broken wiring, not
/// a degraded mode to run in.
pub async fn activate_all(ctx: &mut Ctx<'_>) -> Result<Activated, ActivateError> {
    // ── Block 0: the cell catalog ────────────────────────────────────
    // Every slice cell, registered in one place. No slice registers its
    // own cell from `activate()` — the activations below all resolve
    // their handles from the registry by slot key, which is why this
    // must run before any of them. See constraint 0.
    jinn_cell_catalog::register_all_cells(&ctx.services().slices);

    // ── Block 1: producers ───────────────────────────────────────────
    // These three register cells that later activations read by slot key.
    // They run before the consumers below resolve their cells.

    // provides: provider_state_slot
    //   consumed by: jinn_boot (provider-init) below, which reads it from
    //   the registry rather than receiving it from this call
    // provides: provider_picker_slot, endpoint_picker_slot
    //   consumed by: this slice's own pickers, wired just below
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    jinn_provider_selection::activate(&mut ctx.host(), &services_snapshot, state_snapshot);

    // The provider slice's three pickers. `activate` registered their
    // cells; the menus attach their overlay and key rows here. Without
    // these the keys resolve to nothing.
    jinn_provider_selection::activate_picker(&mut ctx.host());
    let provider_picker_cell = ctx.cell::<jinn_provider_selection_msg::ProviderPickerState>(
        jinn_provider_selection_msg::provider_picker_slot(),
    );
    jinn_provider_selection::activate_provider_picker(&mut ctx.host(), &provider_picker_cell);
    let endpoint_picker_cell = ctx
        .cell::<jinn_provider_selection_msg::endpoint::EndpointPickerState>(
            jinn_provider_selection_msg::endpoint_picker_slot(),
        );
    jinn_provider_selection::activate_endpoint_picker(&mut ctx.host(), &endpoint_picker_cell);

    // provides: token_cache_slot
    //   consumed by: jinn_session_turn (accumulation gate) and
    //   jinn_context_curation (prune workers), both below
    let state_snapshot = ctx.state().clone();
    jinn_token_count::activate(&mut ctx.host(), state_snapshot);

    // provides: session_picker_slot
    //   consumed by: jinn_session_lifecycle, which attaches the picker's
    //   overlay and keys against the cell this mints
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    jinn_session_store::activate(&mut ctx.host(), &services_snapshot, state_snapshot);

    // ── Block 2: independents ───────────────────────────────────────
    // Nothing here provides a value another activation reads. Order is
    // free within the block, subject to the constraints noted inline.

    // MUST BE FIRST in this block: the census subscription must exist
    // before any other actor spawns. See constraint 1.
    jinn_dashboard::activate(&mut ctx.host())?;

    jinn_scope_focus::activate(&mut ctx.host());
    jinn_chat_log_view::activate(&mut ctx.host());
    jinn_cwd::activate(&mut ctx.host());
    jinn_status_bar::activate(&mut ctx.host());
    jinn_quake_bar::activate(&mut ctx.host());
    // The work-time monitor folds `WorkStateChanged`, which the session actor
    // and the turn-dispatch queue actor both publish. Ordering is not
    // load-bearing against the two below — they are below — but it must
    // precede the first dispatch, or the opening edge of the first turn is
    // published to no subscriber and that turn is unmeasured.
    jinn_work_time::activate(&mut ctx.host());
    let services_snapshot = ctx.services().clone();
    jinn_citations::activate(&mut ctx.host(), services_snapshot.clone());
    jinn_skills::activate(&mut ctx.host());
    jinn_project::activate(&mut ctx.host());
    let state_snapshot = ctx.state().clone();
    jinn_sidebar::activate(&mut ctx.host(), state_snapshot);
    let state_snapshot = ctx.state().clone();
    jinn_export::activate(&mut ctx.host(), state_snapshot);
    let state_snapshot = ctx.state().clone();
    let services_snapshot = ctx.services().clone();
    jinn_watchdog::activate(&mut ctx.host(), &state_snapshot, services_snapshot);
    // The attendant trigger actor subscribes to `TurnCompleted`. Ordering
    // against turn-dispatch/inference below is not load-bearing: an attendant
    // fires on a completed turn, and no turn can complete before dispatch
    // exists, so the trigger actor cannot miss anything by spawning first.
    let state_snapshot = ctx.state().clone();
    let services_snapshot = ctx.services().clone();
    jinn_attendant::activate(&mut ctx.host(), state_snapshot, services_snapshot);
    let state_snapshot = ctx.state().clone();
    let services_snapshot = ctx.services().clone();
    jinn_turn_dispatch::activate(&mut ctx.host(), state_snapshot, services_snapshot);
    let services_snapshot = ctx.services().clone();
    jinn_inference::activate(&mut ctx.host(), services_snapshot.clone());
    // The stream-rules matcher must be installed before the first stream runs,
    // so a turn dispatched after boot is already guarded. Installing it after
    // inference's activation is fine — inference only *resolves* the matcher
    // when a stream starts, not at spawn — but it must precede any dispatch,
    // which the boot tail guarantees.
    let config_snapshot = ctx.services().config.clone();
    jinn_stream_rules::activate(&mut ctx.host(), &config_snapshot);
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    jinn_session_init::activate(&services_snapshot, state_snapshot)
        .map_err(|_report| ActivateError::SessionInit(jinn_session_init::SliceActivateError))?;

    // The term slice installs its overlay and key rows; its tab-mirrors
    // cell came from the catalog. The interactive-term coordinator that
    // owns the PTYs is spawned below with the MCP coordinator, since both
    // share that lifecycle shape.
    let state_snapshot = ctx.state().clone();
    jinn_term::activate(ctx.services_mut(), &state_snapshot);

    // Tools has no per-activation work: its registry cell came from the
    // catalog. The orchestrator spawn (below) and the picker (next) both
    // read that cell.
    let state_snapshot = ctx.state().clone();
    jinn_tools::activate(ctx.services_mut(), &state_snapshot);

    // The theme slice scans the theme directories and writes the result
    // through the cell the catalog registered; its picker is registered by
    // the same slice, after discovery.
    let services_snapshot = ctx.services().clone();
    jinn_theme_slice::activate(
        &mut ctx.host(),
        &services_snapshot.paths.themes_dir(),
        &services_snapshot.paths.system_themes_dir(),
    );
    jinn_theme_slice::activate_picker(&mut ctx.host());

    // The persona slice scans the persona directories and writes the
    // result through the cell the catalog registered; its picker is
    // registered by the same slice, after discovery. The scanned set is
    // resolved from the personas cell at the `PersonasLoaded` publish,
    // once every actor is spawned.
    let services_snapshot = ctx.services().clone();
    jinn_persona::activate(&mut ctx.host(), &services_snapshot.paths.personas_dir());
    jinn_persona::activate_picker(&mut ctx.host());

    // The chat input spawns the directory-lister actor behind `@path`.
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    jinn_chat_input::activate(
        &mut ctx.host(),
        jinn_kernel::common::actor_deps::ActorDeps {
            services: services_snapshot,
        },
        &state_snapshot,
    );

    // The preferences slice spawns the two persistence actors, which must
    // subscribe before the startup tail can publish `UpdateAppState`.
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    let trouper_system = services_snapshot.trouper_system.clone();
    jinn_preferences::activate(
        &mut ctx.host(),
        &trouper_system,
        services_snapshot,
        state_snapshot,
    );

    // ── The system-level actors ─────────────────────────────────────
    // These five have no cells of their own, so they are spawned here
    // rather than inside their owning slice's activation. The order
    // between them is behavioural: see constraints 3 and 4.

    // The boot trio. Receives the provider cell Block 1 minted — read
    // back from the registry by slot key, not handed over by the call
    // above.
    let provider_cell = ctx.cell::<jinn_provider_selection_msg::ProviderCell>(
        jinn_provider_selection_msg::provider_state_slot(),
    );
    let boot = jinn_boot::install_actors(
        &ctx.services().trouper_system,
        ctx.state().clone(),
        ctx.services(),
        provider_cell,
    );

    // Context assembly: the stateless service plus the context-size actor.
    jinn_context_assembly::install_actors(
        &ctx.services().trouper_system,
        ctx.state().clone(),
        ctx.services(),
    );

    // Context curation: the prune and compaction actors. MUST precede the
    // session-actor spawn — see constraint 4. The worker list is built
    // from config at the root because the config layer is the root's.
    let prune_workers = ctx.prune_workers();
    let compaction_deps = ctx.compaction_deps();
    jinn_context_curation::activate(&mut ctx.host(), prune_workers, compaction_deps);

    // The session-turn reducer. MUST follow turn-dispatch, inference, and
    // context-curation — see constraint 4.
    jinn_session_turn::activate(&ctx.services().trouper_system, ctx.session_actor_deps());

    // The tool orchestrator. Spawned after the session actor (it consumes
    // `SessionClosed`) and before the MCP coordinator, so MCP tool
    // registrations land in a running orchestrator.
    jinn_tools::ToolOrchestratorActor::spawn(
        &ctx.services().trouper_system,
        jinn_tools::ToolOrchestratorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: ctx.services().clone(),
            },
            state: ctx.state().clone(),
            services: ctx.services().clone(),
            builtin_filter: None,
        },
    );

    // The MCP coordinator. Its cell is registered first so no status or
    // log event can arrive before there is a writer — see constraint 3.
    let mcp_runtime = jinn_mcp_slice::activate_runtime(&ctx.services().slices);
    let mcp_coordinator_path = jinn_mcp_slice::coordinator::McpCoordinatorActor::spawn(
        &ctx.services().trouper_system,
        jinn_mcp_slice::coordinator::McpCoordinatorActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: ctx.services().clone(),
            },
            state: ctx.state().clone(),
            runtime: mcp_runtime,
        },
    )
    .await;
    let _ = ctx
        .services()
        .mcp_coordinator
        .set(jinn_mcp_slice::mcp_coordinator_handle(
            ctx.services().trouper_system.clone(),
            mcp_coordinator_path,
        ));

    // The interactive-term coordinator: owns PTY sessions across tool
    // calls. Same lifecycle shape as the MCP coordinator.
    let term_controls = jinn_term_msg::TermControls::default();
    let (term_coordinator_path, _controls) =
        jinn_term::interactive_term_actor::InteractiveTermActor::spawn(
            &ctx.services().trouper_system,
            jinn_term::interactive_term_actor::InteractiveTermActorDeps {
                bus: ctx.services().bus.clone(),
                controls: term_controls.clone(),
                state: ctx.state().clone(),
                config: ctx.services().config.clone(),
            },
        )
        .await;
    let _ = ctx.services().interactive_term.set(std::sync::Arc::new(
        crate::actor_wiring::ActorTermHandle::new(
            ctx.services().trouper_system.clone(),
            term_coordinator_path,
        ),
    ));
    let _ = jinn_term_msg::TERM_CONTROLS.set(term_controls);

    // The search-index maintenance actor: a message-driven reindex state
    // machine. The dashboard learns it exists from the runtime's spawn
    // announcement, not from a publish here.
    let _search_index = jinn_session_store::search_index_actor::SearchIndexActor::spawn(
        &ctx.services().trouper_system,
        jinn_session_store::search_index_actor::SearchIndexActorDeps {
            deps: jinn_kernel::common::actor_deps::ActorDeps {
                services: ctx.services().clone(),
            },
            interval: jinn_session_store::search_index_actor::REINDEX_INTERVAL,
            batch: jinn_session_store::search_index_actor::REINDEX_BATCH,
        },
    );

    // Session lifecycle: the arg-input overlay plus the actor that runs
    // setup and teardown. The session picker's cell was minted by
    // `jinn_session_store::activate` in Block 1.
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    let shell = ctx.shell();
    jinn_session_lifecycle::activate(
        &mut ctx.host(),
        &services_snapshot,
        state_snapshot,
        jinn_session_lifecycle_msg::BuiltinRegistry::new(),
        shell,
    );
    jinn_session_lifecycle::activate_picker(&mut ctx.host());
    // The session picker resolves its cell by slot key: `jinn_store` minted
    // it in Block 1, and the store actor publishes loaded rows into it.
    let session_picker_cell = ctx.cell::<jinn_session_store_msg::SessionPickerState>(
        jinn_session_store_msg::session_picker_slot(),
    );
    jinn_session_store::activate_session_picker(&mut ctx.host(), &session_picker_cell);

    // The remaining picker registrations, grouped with the slices that
    // own them. Each one is separate from its slice's `activate` because
    // it mints a cell that activation seeds its rows from.
    jinn_tools::activate_picker(&mut ctx.host());
    jinn_mcp_slice::activate_picker(&mut ctx.host());

    // The layout worker pool and the actor that ends a session load once
    // the chat log has been measured. Spawned so their subscriptions are
    // live before the first session can be loaded.
    jinn_chat_log_view::kernel_element::install_layout_actors(
        &ctx.services().trouper_system,
        ctx.state().clone(),
    );

    // The discord slice resolves `[discord]` (fail-fast on a malformed
    // section) and parks the gateway channels for the frontend spawn.
    let services_snapshot = ctx.services().clone();
    let state_snapshot = ctx.state().clone();
    let discord =
        jinn_discord::activate(&mut ctx.host(), &services_snapshot, state_snapshot).await?;

    Ok(Activated { boot, discord })
}

/// A slice activation that could not complete.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub enum ActivateError {
    /// The dashboard's cell or view slot was already taken.
    #[error(debug)]
    Dashboard(#[from] jinn_dashboard::ActivationError),
    /// The session-init partition set failed to install.
    #[error(debug)]
    SessionInit(jinn_session_init::SliceActivateError),
    /// The discord section was present but malformed.
    #[error(debug)]
    Discord(#[from] jinn_config::ConfigSectionError),
    /// A producer slice's cell was absent when a consumer resolved it.
    ///
    /// The three producers in Block 1 register these cells, so a missing
    /// one means the boot list's ordering was violated — a wiring bug, not
    /// a runtime condition.
    #[error(debug)]
    MissingCell(&'static str),
}
