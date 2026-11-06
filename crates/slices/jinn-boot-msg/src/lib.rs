//! Boot crossing contracts.
//!
//! The EXPORT surface of the boot slice: the readiness event published
//! by composition after every actor has spawned, and the env-config ask
//! path (`GetEnvironmentConfig` → `EnvironmentConfigReply`) composition
//! walks to kick off the startup chain.
//!
//! Kernel consumers of [`EnvironmentLoaded`]: the session actor
//! (session bootstrapping on config receipt). The ask path is consumed
//! by composition itself (`actor_wiring`'s startup tail).

use jinn_provider_config::ProvidersConfig;
use serde::{Deserialize, Serialize};

/// All actors have been spawned.
///
/// Emitted after the wiring code finishes spawning every actor. The
/// system-ready actor waits for this event, then releases the oneshot
/// that gates the TUI's first draw — it does no counting of its own.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "All actors have been spawned; the system is ready.")]
pub struct AllActorsSpawned;

impl jinn_slices::BusMessage for AllActorsSpawned {}

/// The environment has been loaded and API keys are available.
///
/// Emitted after the env init actor has populated `ApiKeysService`.
/// Published at runtime for environment reloads (not during startup).
/// Downstream actors should use `ask(GetEnvironmentConfig)` for initial config.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The environment has been loaded and API keys are available.")]
pub struct EnvironmentLoaded {
    /// The parsed provider configuration from `providers.toml`.
    pub config: ProvidersConfig,
}

impl jinn_slices::BusMessage for EnvironmentLoaded {}

/// Ask message to retrieve the loaded environment config.
///
/// Downstream actors use this during their `on_start` to pull config
/// directly from the EnvInitActor via the actor registry.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Ask the env-init actor for the parsed provider configuration.")]
pub struct GetEnvironmentConfig;

impl jinn_slices::BusMessage for GetEnvironmentConfig {}

/// The reply payload of the `GetEnvironmentConfig` ask (JSON-friendly twin
/// of `Option<ProvidersConfig>`).
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Reply payload for the GetEnvironmentConfig ask.")]
pub struct EnvironmentConfigReply {
    /// The loaded config, or `None` when the file is missing/unreadable.
    pub config: Option<ProvidersConfig>,
}

impl jinn_slices::BusMessage for EnvironmentConfigReply {}

/// The `jinn.toml` document has been reloaded from disk.
///
/// Published by the config-watch slice *after* the new snapshot has been
/// validated and swapped into the configuration layer, so every handler of
/// this event reads through the layer observes the post-reload values. A
/// refused edit (malformed TOML, a section that fails validation) never
/// publishes this — the old snapshot stays live.
///
/// Handlers re-resolve what they own from the layer: point-of-use readers
/// need no subscription at all, but a subscriber that snapped a value at
/// spawn (a bind address, a server roster, a shell resolution) uses this
/// event as its trigger to re-read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "The jinn.toml snapshot was reloaded from disk after validation.")]
pub struct JinnTomlReloaded;

impl jinn_slices::BusMessage for JinnTomlReloaded {}

/// The `providers.toml` document has been re-read from disk.
///
/// Published by the config-watch slice after the file parsed, so handlers
/// can rebuild what they snapshotted at startup: the provider registry,
/// the resolved API keys, the model cache, the picker entries. Unlike
/// [`JinnTomlReloaded`] this carries no layer swap — `providers.toml` is
/// read through its own storage boundary, not the layer — so handlers
/// re-read the file themselves and *refuse* on their own failure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "The providers.toml file changed on disk; handlers rebuild their snapshot.")]
pub struct ProvidersTomlReloaded;

impl jinn_slices::BusMessage for ProvidersTomlReloaded {}

/// A resource directory (themes, personas, prompts, skills, workflows)
/// changed on disk.
///
/// Published by the config-watch slice when a directory's fingerprint
/// changed — file added, removed, or rewritten. One event covers all
/// directories: a handler re-scans what it owns and dedupes by finding
/// nothing new. Carries no payload because the watcher cannot know which
/// slice cares about which directory; the fingerprint change is the whole
/// fact.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Event)]
#[schema(description = "A resource directory changed; handlers re-scan what they own.")]
pub struct ResourcesChanged;

impl jinn_slices::BusMessage for ResourcesChanged {}
