//! The config-watch slice — `jinn.toml` live, without a restart.
//!
//! Watches the run's `jinn.toml` (the resolved `--config` path, which may
//! live outside the standard config dir), the standard config dir's
//! `providers.toml` and `stream_rules.toml`, and the resource directories
//! (themes, personas, prompts, skills, workflows). An external edit — or
//! jinn's own write — settles through a 250 ms debounce and a
//! content-hash diff, so a no-op write publishes nothing. What actually
//! changed is then handled in order:
//!
//! - `jinn.toml` re-enters the system through the configuration layer's
//!   validate-gated reload ([`jinn_config::ConfigLayer::reload_validated`]):
//!   a malformed or invalid edit is **refused** — the previously installed
//!   snapshot stays live, and the refusal is surfaced on the dashboard row
//!   and as a transient chat entry. A valid edit swaps the snapshot before
//!   [`jinn_boot_msg::JinnTomlReloaded`] is published, so every subscriber
//!   observes post-reload values.
//! - `providers.toml` parses through its storage boundary; on success
//!   [`jinn_boot_msg::ProvidersTomlReloaded`] fans out to the boot actors
//!   that rebuild the registry, the API keys, the model cache, and the
//!   picker. A broken file is refused before broadcast.
//! - `stream_rules.toml` publishes
//!   [`jinn_stream_rules_msg::StreamRulesReloaded`], the event the
//!   stream-rule matcher actor already answers (its one declarant); the
//!   matcher's refuse-and-keep-old covers a bad file.
//! - A resource fingerprint change publishes
//!   [`jinn_boot_msg::ResourcesChanged`], which theme, persona, and
//!   session-init re-scan on.
//!
//! The pipeline feeds the three behaviors every configuration section
//! declares (see the schemas module's glossary): **Snapshot Read**
//! sections need nothing from this slice — the validate-gated swap is
//! all they ride. **Event-Dependent** sections exist for the event
//! stage: their owning actor's handler rebuilds the artifact a socket,
//! a process, or a registry stands for. **Frozen** sections ignore the
//! pipeline entirely: their session-profile copies change only at
//! session creation.
//!
//! The slice contributes one actor ([`watch_actor::ConfigWatchActor`]),
//! no cell, no routes, no view — the watchdog precedent. Point-of-use
//! readers of the configuration layer need no subscription at all: the
//! layer is already live, and this slice is what feeds it.

pub mod signature;
pub mod watch_actor;

use jinn_kernel::Services;
use jinn_kernel::common::state::State;
use jinn_slices::RenderFacts;
use jinn_slices::SliceHost;

/// Activates the slice: spawns the watch actor with the notify watches
/// established and the signature ledger seeded from disk.
///
/// Call late in the boot list — after every slice whose re-resolution the
/// watcher triggers has activated, so a settle landing during startup
/// finds its subscriber already spawned. A watch-establishment failure is
/// not fatal: the actor still spawns and its row says live reload is off.
///
/// # Errors
///
/// Returns the trouper registration error if the actor cannot spawn —
/// a broken wiring, not a degraded mode.
pub fn activate(
    host: &mut SliceHost<'_, RenderFacts>,
    services: &Services,
    state: &State,
) -> Result<(), error_stack::Report<trouper::registry::RegistryError>> {
    watch_actor::ConfigWatchActor::spawn(host.system(), services, state)?;
    Ok(())
}

#[cfg(test)]
mod watch_actor_tests;
