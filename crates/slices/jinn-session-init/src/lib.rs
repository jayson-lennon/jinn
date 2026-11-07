//! The session-init slice — per-session environment discovery on the
//! trouper fabric.
//!
//! A keyed-actor topology: one supervisor
//! translates session-lifecycle triggers and manual rescan commands
//! into keyed commands, and a partition set activates one discovery
//! worker per session, owning that session's skills, prompt,
//! context-file, and workflow scans plus settle coalescing. A notifier
//! actor posts the settled summary entry into the session's chat log.
//!
//! Discovery results broadcast as `SkillsLoaded`,
//! `PromptTemplatesLoaded`, `ContextFilesLoaded`, and `WorkflowsLoaded`
//! events, so kernel consumers — the session actor and the subagent
//! task-settle listener — are unchanged.

pub mod commands;
pub mod contracts;
pub mod notifier;
pub mod scans;
pub mod supervisor;
pub mod worker;

pub use commands::RescanContext;
pub use commands::RescanPrompts;
pub use commands::RescanSkills;
pub use commands::RescanWorkflows;
pub use commands::RunDiscovery;
pub use contracts::DiscoverySnapshot;
pub use contracts::SessionDiscoverySettled;

use jinn_kernel::common::state::State;
use wherror::Error;

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

/// Error activating the session-init slice.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SliceActivateError;

/// Activates the session-init slice: installs the discovery partition
/// set and spawns the supervisor + notifier on the trouper fabric (their
/// `.subscribe` declarations are the readiness point).
///
/// Must precede the readiness `EnvironmentLoaded` publish so no trigger
/// is missed.
///
/// # Errors
///
/// Returns [`SliceActivateError`] when the partition set install
/// fails — its shard-key declaration is validated at install.
pub fn activate(
    services: &jinn_kernel::Services,
    state: State,
) -> Result<(), error_stack::Report<SliceActivateError>> {
    let system = services.trouper_system.clone();
    install_actors(&system, services.paths.clone(), state)?;

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
    paths: jinn_kernel::common::app_paths::AppPaths,
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
    system.register_schema::<crate::commands::RescanWorkflows>();
    // The supervisor also handles this event (live resource edits re-run
    // every live session's discovery), so the schema must exist before
    // the supervisor's spawn resolves its subscription.
    system.register_schema::<jinn_boot_msg::ResourcesChanged>();

    // Partition set before any send: the supervisor addresses its
    // public path, and install validates the shard-key contract.
    system
        .install_partition_set(partition_spec(system, &paths, &state))
        .change_context(SliceActivateError)
        .attach("installing the jinn.discovery partition set")?;

    supervisor::SessionInitSupervisor::spawn(system, state.clone());
    notifier::DiscoveryNotifier::spawn(system, state);

    Ok(())
}

/// Builds the `jinn.discovery` partition spec: one
/// [`worker::SessionDiscoveryWorker`] entity per session id, activated
/// on demand by the kernel from this shared factory.
///
/// The factory closure captures `AppPaths` (the scan inputs are
/// launch-wide) and clones `State` per activation.
///
/// `args_template` is merged with the entity `"key"` at activation;
/// production passes `{}` and tests may inject e.g. a shortened
/// settle budget ([`worker::SETTLE_BUDGET_ARG`]).
fn partition_spec_with_args(
    system: &trouper::system::ActorSystem,
    paths: &jinn_kernel::common::app_paths::AppPaths,
    state: &State,
    args_template: trouper::json::Json,
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
        args_template: Some(args_template),
        opts: trouper::system::SpawnOpts::default(),
    }
}

/// Builds the partition spec with the production args template.
fn partition_spec(
    system: &trouper::system::ActorSystem,
    paths: &jinn_kernel::common::app_paths::AppPaths,
    state: &State,
) -> trouper::pool::PartitionSpec {
    partition_spec_with_args(system, paths, state, trouper::json::Json::default())
}

/// Installs the discovery partition set with a custom entity args
/// template (test seam: inject a shortened settle budget).
///
/// Registers the worker's command schemas first — the install
/// validates the shard-key contract against the registry's schema
/// table.
///
/// # Errors
///
/// Returns an error when the partition set install fails.
pub fn install_partition_set_with_args(
    system: &trouper::system::ActorSystem,
    paths: &jinn_kernel::common::app_paths::AppPaths,
    state: &State,
    args_template: trouper::json::Json,
) -> Result<(), error_stack::Report<SliceActivateError>> {
    use error_stack::ResultExt;

    system.register_schema::<crate::commands::RunDiscovery>();
    system.register_schema::<crate::commands::RescanSkills>();
    system.register_schema::<crate::commands::RescanPrompts>();
    system.register_schema::<crate::commands::RescanContext>();
    system.register_schema::<crate::commands::RescanWorkflows>();
    system
        .install_partition_set(partition_spec_with_args(
            system,
            paths,
            state,
            args_template,
        ))
        .change_context(SliceActivateError)
        .attach("installing the jinn.discovery partition set")?;
    Ok(())
}

/// The supervision budget for one discovery entity: the actors'
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

/// The supervised spawn closure type trouper's [`trouper::supervision::ActorSpec`] carries.
type ChildSpawnFn = std::sync::Arc<
    dyn Fn(&trouper::system::ActorSystem, &trouper::actor::ActorPath, &trouper::json::Json)
        + Send
        + Sync,
>;

/// The entity's supervised spawn closure: re-runs the factory spawn
/// (a full spawn — slot insert included) at the same path. Registered
/// on the spec at activation and reused by the supervision engine's
/// restart path.
///
/// The merged genesis args may carry a `settle_budget_ms` override
/// ([`worker::SETTLE_BUDGET_ARG`]); the template is `{}` in production,
/// so the default budget applies there.
fn entity_spawn_fn(
    system: &trouper::system::ActorSystem,
    paths: &jinn_kernel::common::app_paths::AppPaths,
    state: &State,
) -> ChildSpawnFn {
    let paths = paths.clone();
    let state = state.clone();
    let system_handle = system.clone();
    std::sync::Arc::new(
        move |system: &trouper::system::ActorSystem,
              path: &trouper::actor::ActorPath,
              args: &trouper::json::Json| {
            let session_id = entity_key(args);
            let settle_budget = settle_budget_from_args(args);
            let deps = worker::WorkerDeps::for_session_with_budget(
                &system_handle,
                &state,
                &paths,
                session_id,
                settle_budget,
            );
            worker::SessionDiscoveryWorker::spawn(system, path.clone(), deps);
        },
    )
}

/// Reads the optional `settle_budget_ms` genesis-arg override.
fn settle_budget_from_args(args: &trouper::json::Json) -> Option<std::time::Duration> {
    args.get(worker::SETTLE_BUDGET_ARG)
        .and_then(serde_json::Value::as_u64)
        .map(std::time::Duration::from_millis)
}

/// Registers one discovery entity under the supervisor: the spec goes
/// to the supervision engine (crash → restart → escalate) and the
/// spawn closure runs once for this activation.
fn supervise_entity(
    system: &trouper::system::ActorSystem,
    path: &trouper::actor::ActorPath,
    args: &trouper::json::Json,
    parent: &trouper::actor::ActorPath,
    spawn: ChildSpawnFn,
) {
    system.spawn(trouper::supervision::ActorSpec {
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
fn entity_key(args: &trouper::json::Json) -> jinn_core_types::SessionId {
    args.get("key")
        .and_then(serde_json::Value::as_str)
        .and_then(jinn_core_types::SessionId::try_from_string)
        .unwrap_or_default()
}
