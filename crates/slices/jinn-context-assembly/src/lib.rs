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
pub mod service;
pub mod size_actor;
