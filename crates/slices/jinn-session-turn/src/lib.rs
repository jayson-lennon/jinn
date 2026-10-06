//! The session-turn slice.
//!
//! This slice owns the one coordinated reducer that advances session turns:
//! enqueue, streaming, tool continuation, retry, context mutation, and
//! turn-path persistence. The implementation is activated on the shared actor
//! system at the established `session` path.

pub mod phase_actor;
pub mod session_actor;

use session_actor::{SessionPersistenceActor, SessionPersistenceActorDeps};
use trouper::system::ActorSystem;

/// Activates the session-turn reducer at the shared static actor path.
///
/// The path is not returned: the actor is registered with the trouper
/// system for the life of the process, and nothing in composition reads
/// the address back.
///
/// # Panics
///
/// Panics if the reducer path is already occupied or its subscriptions fail.
/// Either condition is a composition error and must abort launch.
pub fn activate(system: &ActorSystem, deps: SessionPersistenceActorDeps) {
    // The phase actor: the sole phase writer. It must exist before any
    // publisher asks it (the queue actor, the session actor's handlers,
    // the kernel's synchronous cancel paths) — spawning it here, beside
    // the session actor, keeps that ordering in one place. The bus
    // handle comes from the same Services the session actor was built
    // with: one Services, one bus, one fabric.
    let _phase =
        phase_actor::ensure_spawned(system, deps.state.clone(), deps.deps.services.bus.clone());
    let _session = SessionPersistenceActor::spawn(system, deps);
}
