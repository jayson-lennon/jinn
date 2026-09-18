//! The context-assembly slice — a stateless assembly service.
//!
//! Assembling a system prompt + conversation messages is a PURE
//! function of the caller-provided [`assemble::AssemblyInputs`]: the
//! service never reads `AppState`. The kernel's queue/session dispatch
//! paths snapshot the session state they can see, send an
//! `AssembleContext` message to the `context-assembly` trouper actor,
//! and receive an `AssembledResponse` reply.

#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions on infallible registration"
    )
)]

pub mod assemble;
pub mod bridge;
pub mod service;
pub mod size_actor;

use trouper::schema::Schema;

use jinn_slices::SliceHost;

/// Installs the slice's actors on a trouper system: spawns the stateless
/// assembly service and the context-size actor, subscribing the latter to
/// the [`bridge::context_assembly_topic`] (the readiness point).
///
/// Split from composition so tests (and any composition that owns a bare
/// [`trouper::system::ActorSystem`]) can wire the actor fabric without
/// the kernel's `Services` + route staging.
///
/// # Panics
///
/// Panics if the size actor's topic subscription fails — a broken actor
/// system, not a caller bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn install_actors(
    system: &trouper::system::ActorSystem,
    state: jinn_domain::common::state::State,
    services: &jinn_domain::Services,
) {
    let _ = service::spawn(system);
    let path = size_actor::ContextSizeActor::spawn(system, state, services.clone());
    system
        .subscribe(&path, &bridge::context_assembly_topic(), None)
        .expect("context size actor subscribes to the context-assembly topic");
}

/// Stages the slice's crossing routes on the host: the kernel
/// context-affecting events forward onto
/// [`bridge::context_assembly_topic`]. Composition drains the staged
/// routes after activation (see [`bridge::drain_routes`]).
pub fn stage_routes(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let topic = bridge::context_assembly_topic();
    host.forward::<jinn_domain::feat::session::protocol::history_appended::HistoryAppended, _>(
        topic.clone(),
        || jinn_domain::feat::session::protocol::history_appended::HistoryAppended::schema_def(),
    );
    host.forward::<jinn_domain::feat::context::protocol::event::ContextOverrideChanged, _>(
        topic.clone(),
        || jinn_domain::feat::context::protocol::event::ContextOverrideChanged::schema_def(),
    );
    host.forward::<jinn_domain::protocol::system::ActiveSessionChanged, _>(
        topic.clone(),
        jinn_domain::protocol::system::ActiveSessionChanged::schema_def,
    );
    host.forward::<jinn_domain::feat::context::protocol::event::ChatEntryPinChanged, _>(
        topic.clone(),
        jinn_domain::feat::context::protocol::event::ChatEntryPinChanged::schema_def,
    );
    host.forward::<
        jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted,
        _,
    >(topic, || {
        jinn_domain::feat::session::protocol::session_load_completed::SessionLoadCompleted::schema_def()
    });
}
