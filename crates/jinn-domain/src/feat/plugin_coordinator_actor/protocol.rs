//! Plugin status events — published by the plugin coordinator as each
//! plugin's guest transitions through its lifecycle.
//!
//! No UI subscribes yet; the event exists so plugin health is observable
//! on the bus (and testable) from day one.

use serde::{Deserialize, Serialize};

/// Lifecycle state of one configured plugin's guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginPhase {
    /// The guest task spawned but the handshake has not completed.
    Starting,
    /// The handshake completed (`Hello` seen, `Welcome` sent).
    Running,
    /// The guest ended (crash, trap, or shutdown) or never came up.
    Dead,
    /// The guest lives but is flooding: the inbound channel filled and
    /// messages were dropped. Cleared back to `Running` when the channel
    /// drains.
    Unresponsive,
    /// The guest completed its work and exited cleanly after the
    /// handshake (run-to-completion plugins like the loaders). The host
    /// keeps the plugin's contributions cached after the guest ends.
    Done,
}

/// A lifecycle transition for one configured plugin.
///
/// Published by the plugin coordinator at every transition. Subscribers can
/// build a live view of every plugin process in the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    /// The configured plugin name (`jinn.toml` `[[plugin]].name`).
    pub name: String,
    /// The new phase.
    pub phase: PluginPhase,
}

impl crate::common::bus::BusMessage for PluginStatus {}

jinn_slices::crossing_schema!(PluginStatus, "PluginStatus",
trouper::schema::SchemaKind::Event,
description: "A lifecycle transition for one configured plugin.",
fields: ["name" => trouper::schema::FieldTy::Str]);

/// The validated subscription set a running plugin's guest declared in its
/// `Hello`.
///
/// Published by the plugin actor right after `PluginStatus::Running`. The
/// coordinator's event forwarder consumes it to know which host events to
/// write to that guest's stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSubscriptions {
    /// The configured plugin name (`jinn.toml` `[plugin.<name>]` key).
    pub name: String,
    /// The validated subscription kind tags (see
    /// `jinn_plugin_api::SUBSCRIPTION_KINDS`).
    pub kinds: Vec<String>,
}

impl crate::common::bus::BusMessage for PluginSubscriptions {}

jinn_slices::crossing_schema!(PluginSubscriptions, "PluginSubscriptions",
trouper::schema::SchemaKind::Event,
description: "A running plugin guest's declared event subscriptions.",
fields: ["name" => trouper::schema::FieldTy::Str]);

/// Internal self-addressed timer pulse for the plugin coordinator.
///
/// The coordinator is not a bus subscriber for this message — it is told
/// directly by a tokio interval task holding a clone of the actor ref (the
/// task dies with the actor). Each pulse is translated into a wire
/// [`jinn_plugin_api::TickEvent`] and forwarded to guests subscribed to
/// `"tick"`.
///
/// Guests are blocking stdin readers: they only wake when the host writes a
/// line, and during a quiet period no host event flows. Without this pulse a
/// guest could never act on elapsed time at all.
#[derive(Debug, Clone)]
pub struct Tick;
