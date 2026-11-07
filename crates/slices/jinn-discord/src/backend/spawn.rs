//! Gateway spawn entry point.
//!
//! The discord slice owns its wiring (channels, actors, rows) via
//! `crate::activate`; this crate is the *frontend* — the one piece that
//! cannot live in the slice: the poise websocket task. Composition
//! (`app.rs`) attaches [`gateway_spawner`] to the activation's
//! supervisor as its spawn hook; the supervisor calls it to start the
//! gateway (at attach time for an enabled activation, on a live
//! enablement flip otherwise) and after every disable → enable round
//! trip.
//!
//! [`Services`]: jinn_kernel::Services

use std::sync::Arc;

use tokio::sync::Notify;

use crate::GatewayConfigHandle;
use crate::backend::gateway;
use jinn_kernel::Services;

/// Re-exported so callers name one crate for the pool type.
pub use daow::Pool as SessionPool;

/// Builds the spawn hook composition attaches to the supervisor.
///
/// Each invocation spawns one gateway task: it clones the parked
/// channel halves, snapshots the config through the handle (the reload
/// path may have changed enablement or the token since the last spawn),
/// resolves the token (env var first, then the config), and runs until
/// the supervisor's shutdown signal fires — the same signal the
/// disable-at-idle path notifies, so a deferred disable lands here.
///
/// The pool is cloned per invocation (an `Arc` increment inside daow),
/// so every gateway generation shares the session database.
pub fn gateway_spawner(
    handle: &tokio::runtime::Handle,
    core: &jinn_kernel::AppCore,
    services: &Services,
    session_pool: SessionPool,
    parked: crate::DiscordGatewayChannels,
) -> Box<crate::GatewaySpawnHook> {
    let runtime_handle = handle.clone();
    let services = services.clone();
    let state = core.state.clone();
    let bridge = core.bridge.clone();
    Box::new(
        move |config_handle: GatewayConfigHandle, stop_signal: Arc<Notify>| {
            let config = config_handle.read();
            if !config.enabled {
                return;
            }
            // Per-spawn clones: an `Fn` hook runs many times over the
            // process lifetime (each enable after a disable is another
            // generation), and nothing moved in may be consumed.
            let state = state.clone();
            let bridge = bridge.clone();
            let services = services.clone();
            let session_pool = session_pool.clone();
            let bridge_rx = parked.bridge_rx.clone();
            let gateway_rx = parked.gateway_rx.clone();
            let status_tx = parked.status_tx.clone();
            let token = std::env::var("DISCORD_BOT_TOKEN")
                .ok()
                .or_else(|| config.bot_token.clone())
                .unwrap_or_default();
            runtime_handle.spawn(gateway::run(
                gateway::BotData {
                    state,
                    bridge,
                    thread_map: crate::DiscordThreadMap::new(session_pool),
                    // The gateway reads through the swappable handle:
                    // content-only reloads (authorize list, forum
                    // channel) swap the slot and every reader of this
                    // arc sees the swap without a restart.
                    config: config_handle.into_arc(),
                    services,
                },
                token,
                bridge_rx,
                gateway_rx,
                status_tx,
                stop_signal,
            ));
        },
    )
}
