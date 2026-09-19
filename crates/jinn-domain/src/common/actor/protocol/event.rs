//! Actor lifecycle events.
//!
//! The three bus-travelling lifecycle events ([`ActorStarting`],
//! [`ActorStarted`], [`ActorShutdownCompleted`]) are defined in
//! `jinn-slices` ([`jinn_slices::fabric`]) and re-exported here:
//! kameo bus dispatch is by `TypeId`, so the kernel publishers and
//! every subscriber must share one Rust type. [`AllActorsSpawned`]
//! never crosses to a slice, so it stays kernel-resident.

pub use jinn_slices::fabric::ActorShutdownCompleted;
pub use jinn_slices::fabric::ActorStarted;
pub use jinn_slices::fabric::ActorStarting;

use serde::{Deserialize, Serialize};

/// All actors have been spawned.
///
/// Emitted after the wiring code finishes spawning every actor.
/// The system-ready actor waits for this event before checking whether
/// its running count of `ActorStarted` events matches the total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllActorsSpawned;

impl crate::common::bus::BusMessage for AllActorsSpawned {}

jinn_slices::crossing_schema!(AllActorsSpawned, "AllActorsSpawned",
trouper::schema::SchemaKind::Event,
description: "All actors have been spawned; the system is ready.",
fields: []);
