//! The session-init slice — per-session environment discovery on the
//! trouper fabric.
//!
//! Replaces the kameo scan trio + discovery coordinator + discovery
//! notifier with a true keyed-actor topology: one supervisor
//! translates session-lifecycle triggers and manual rescan commands
//! into keyed commands, and a partition set activates one discovery
//! worker per session, owning that session's skills, prompt, and
//! context-file scans plus the settle coalescing the kameo coordinator
//! used to do across four actors. A notifier actor posts the settled
//! summary entry into the session's chat log.
//!
//! Discovery results return to the kameo bus via reverse relays
//! (`SkillsLoaded`, `PromptTemplatesLoaded`, `ContextFilesLoaded`) so
//! kernel consumers — the session actor and the subagent task-settle
//! listener — are unchanged.

pub mod bridge;
pub mod commands;
pub mod contracts;
pub mod notifier;
pub mod scans;
pub mod supervisor;
pub mod worker;

pub use commands::RescanContext;
pub use commands::RescanPrompts;
pub use commands::RescanSkills;
pub use commands::RunDiscovery;
pub use contracts::DiscoverySnapshot;
pub use contracts::SessionDiscoverySettled;

use jinn_domain::common::state::State;
use jinn_slices::AppSliceHost;
use trouper::schema::Schema;
use trouper::topics::Topic;
use wherror::Error;

/// The trouper topic session-lifecycle triggers and manual rescans
/// cross on (kameo bus → supervisor).
#[must_use]
pub fn session_init_topic() -> Topic {
    Topic::new("jinn.session-init")
}

/// The public path of the discovery partition set. Keyed commands are
/// addressed here; the kernel resolves `<public>/<session_id>` and
/// activates the per-session worker entity on demand.
pub const DISCOVERY_PATH: &str = "jinn.discovery";

/// The discovery partition set's key: a session id.
pub const DISCOVERY_KEY_FIELD: &str = "session_id";

/// The supervisor actor's static path.
pub const SUPERVISOR_PATH: &str = "session-init-supervisor";

/// The discovery notifier's static path.
pub const NOTIFIER_PATH: &str = "discovery-notifier";

/// The trouper topic the settled event crosses on (worker → notifier).
#[must_use]
pub fn settled_topic() -> Topic {
    Topic::new("SessionDiscoverySettled")
}

/// Stages the slice's crossing routes on the host.
///
/// The 8 triggers forward onto the shared [`session_init_topic`]; the 3
/// loaded events return via reverse relays, each publishing onto its
/// schema-named topic — the exact topics [`bridge::drain_routes`]
/// subscribes its relays to.
fn stage_routes(host: &mut AppSliceHost<'_>) {
    let topic = session_init_topic();

    host.forward::<jinn_domain::init::env_init_actor::EnvironmentLoaded, _>(topic.clone(), || {
        jinn_domain::init::env_init_actor::EnvironmentLoaded::schema_def()
    });
    host.forward::<jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated, _>(topic.clone(), || {
        jinn_domain::feat::session_lifecycle::protocol::event::SessionCreated::schema_def()
    });
    host.forward::<jinn_session_msg::SessionSetupCompleted, _>(topic.clone(), || {
        jinn_session_msg::SessionSetupCompleted::schema_def()
    });
    host.forward::<jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted, _>(topic.clone(), || {
        jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted::schema_def()
    });
    host.forward::<jinn_domain::feat::session_lifecycle::protocol::event::SessionCwdChanged, _>(topic.clone(), || {
        jinn_domain::feat::session_lifecycle::protocol::event::SessionCwdChanged::schema_def()
    });
    host.forward::<crate::commands::RunDiscovery, _>(topic.clone(), || {
        crate::commands::RunDiscovery::schema_def()
    });
    host.forward::<crate::commands::RescanSkills, _>(topic.clone(), || {
        crate::commands::RescanSkills::schema_def()
    });
    host.forward::<crate::commands::RescanPrompts, _>(topic.clone(), || {
        crate::commands::RescanPrompts::schema_def()
    });
    host.forward::<crate::commands::RescanContext, _>(topic, || {
        crate::commands::RescanContext::schema_def()
    });

    host.reverse::<jinn_domain::feat::skills::skills_scan_actor::SkillsLoaded, _>(
        Topic::new("SkillsLoaded"),
        jinn_domain::feat::skills::skills_scan_actor::SkillsLoaded::schema_def,
    );
    host.reverse::<jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded, _>(
        Topic::new("PromptTemplatesLoaded"),
        jinn_domain::feat::provider::protocol::event::PromptTemplatesLoaded::schema_def,
    );
    host.reverse::<jinn_domain::feat::context::protocol::event::ContextFilesLoaded, _>(
        Topic::new("ContextFilesLoaded"),
        jinn_domain::feat::context::protocol::event::ContextFilesLoaded::schema_def,
    );
}

/// Error activating the session-init slice.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SliceActivateError;

/// Activates the session-init slice: install the discovery partition
/// set, spawn the supervisor and notifier on the trouper fabric, and
/// stage this slice's crossing routes (the forward triggers on
/// [`session_init_topic`], the reverse results back onto the kameo bus).
///
/// Composition drains the staged routes after activation (see
/// [`bridge::drain_routes`]); both call sites must precede the
/// readiness `EnvironmentLoaded` publish so no trigger is missed.
///
/// # Errors
///
/// Returns [`SliceActivateError`] when the partition set install
/// fails — its shard-key declaration is validated at install.
pub fn activate(
    host: &mut AppSliceHost<'_>,
    services: &jinn_domain::Services,
    state: State,
) -> Result<(), error_stack::Report<SliceActivateError>> {
    let system = services.trouper_system.clone();
    install_actors(&system, services.paths.clone(), state)?;

    stage_routes(host);

    Ok(())
}

/// Installs the slice's actors on a trouper system: registers the
/// worker's command schemas, installs the discovery partition set, and
/// spawns the supervisor + notifier.
///
/// Split from [`activate`] so tests (and any composition that owns a
/// bare [`ActorSystem`]) can wire the actor fabric without the kernel's
/// `Services` + route staging.
///
/// # Errors
///
/// Returns an error when the partition set install fails — its
/// shard-key declaration is validated at install.
pub fn install_actors(
    system: &trouper::system::ActorSystem,
    paths: jinn_domain::common::app_paths::AppPaths,
    state: State,
) -> Result<(), error_stack::Report<SliceActivateError>> {
    use error_stack::ResultExt;

    // The partition install validates the shard-key contract against
    // the registry's schema table (refuse-to-lie), so every command
    // schema the worker handles must be registered first.
    system.register_schema::<crate::commands::RunDiscovery>();
    system.register_schema::<crate::commands::RescanSkills>();
    system.register_schema::<crate::commands::RescanPrompts>();
    system.register_schema::<crate::commands::RescanContext>();

    // Partition set before any send: the supervisor addresses its
    // public path, and install validates the shard-key contract.
    system
        .install_partition_set(partition_spec(system, &paths, &state))
        .change_context(SliceActivateError)
        .attach("installing the jinn.discovery partition set")?;

    supervisor::SessionInitSupervisor::spawn(system, state.clone(), paths);
    notifier::DiscoveryNotifier::spawn(system, state);

    Ok(())
}

/// Builds the `jinn.discovery` partition spec: one
/// [`worker::SessionDiscoveryWorker`] entity per session id, activated
/// on demand by the kernel from this shared factory.
///
/// The factory closure captures `AppPaths` (the scan inputs are
/// launch-wide) and clones `State` per activation; each entity mints
/// its own write authorities in [`worker::WorkerDeps::for_session`].
fn partition_spec(
    system: &trouper::system::ActorSystem,
    paths: &jinn_domain::common::app_paths::AppPaths,
    state: &State,
) -> trouper::pool::PartitionSpec {
    let paths = paths.clone();
    let state = state.clone();
    trouper::pool::PartitionSpec {
        public: trouper::actor::ActorPath::new(DISCOVERY_PATH),
        key_field: DISCOVERY_KEY_FIELD.to_owned(),
        system: system.clone(),
        factory: {
            let supervisor = trouper::actor::ActorPath::new(SUPERVISOR_PATH);
            let spawn = entity_spawn_fn(system, &paths, &state);
            std::sync::Arc::new(
                move |system: &trouper::system::ActorSystem,
                      path: &trouper::actor::ActorPath,
                      args| {
                    supervise_entity(system, path, args, &supervisor, spawn.clone());
                },
            )
        },
        args_template: Some(serde_json::json!({})),
        opts: trouper::system::SpawnOpts::default(),
    }
}

/// The supervision budget for one discovery entity: the kameo actors'
/// convention — restart on crash until the restart budget (5 within a
/// 10 s sliding window) is exhausted, then escalate to the supervisor.
fn entity_restart_budget() -> trouper::supervision::RestartBudget {
    trouper::supervision::RestartBudget::per(5, std::time::Duration::from_secs(10))
}

/// The backoff between entity restarts: 5 ms doubling to 20 ms —
/// restarts are local, so the delay stays short.
fn entity_backoff() -> trouper::supervision::Backoff {
    trouper::supervision::Backoff {
        base: std::time::Duration::from_millis(5),
        max: std::time::Duration::from_millis(20),
        factor: 2.0,
    }
}

/// The supervised spawn closure type trouper's [`ChildSpec`] carries.
type ChildSpawnFn = std::sync::Arc<
    dyn Fn(&trouper::system::ActorSystem, &trouper::actor::ActorPath, &serde_json::Value)
        + Send
        + Sync,
>;

/// The entity's supervised spawn closure: re-runs the factory spawn
/// (a full spawn — slot insert included) at the same path. Passed to
/// both `spawn_child` at activation and the supervision engine's
/// restart path.
fn entity_spawn_fn(
    system: &trouper::system::ActorSystem,
    paths: &jinn_domain::common::app_paths::AppPaths,
    state: &State,
) -> ChildSpawnFn {
    let paths = paths.clone();
    let state = state.clone();
    let system_handle = system.clone();
    std::sync::Arc::new(
        move |system: &trouper::system::ActorSystem,
              path: &trouper::actor::ActorPath,
              args: &serde_json::Value| {
            let session_id = entity_key(args);
            let deps = worker::WorkerDeps::for_session(&system_handle, &state, &paths, session_id);
            worker::SessionDiscoveryWorker::spawn(system, path.clone(), deps);
        },
    )
}

/// Registers one discovery entity under the supervisor: the spec goes
/// to the supervision engine (crash → restart → escalate) and the
/// spawn closure runs once for this activation.
fn supervise_entity(
    system: &trouper::system::ActorSystem,
    path: &trouper::actor::ActorPath,
    args: &serde_json::Value,
    parent: &trouper::actor::ActorPath,
    spawn: ChildSpawnFn,
) {
    system.spawn_child(trouper::supervision::ChildSpec {
        path: path.clone(),
        parent: Some(parent.clone()),
        restart: trouper::supervision::RestartPolicy::Permanent,
        budget: entity_restart_budget(),
        backoff: entity_backoff(),
        args: args.clone(),
        spawn,
    });
}

/// Extracts the entity key (a session id string) from the merged
/// genesis args the kernel passes the factory.
///
/// Unparsable keys fall back to a fresh id: the entity would serve a
/// session that cannot exist (no state entry, every scan gated), so a
/// placeholder is safer than a panic inside the kernel's activation
/// path.
fn entity_key(args: &serde_json::Value) -> jinn_core_types::SessionId {
    args.get("key")
        .and_then(serde_json::Value::as_str)
        .and_then(jinn_core_types::SessionId::try_from_string)
        .unwrap_or_default()
}
