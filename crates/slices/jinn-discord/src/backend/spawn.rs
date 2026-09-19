//! Gateway spawn entry point.
//!
//! The discord slice owns its wiring (channels, actors, rows) via
//! `crate::activate`; this crate is the *frontend* —
//! the one piece that cannot live in the slice: the poise websocket task.
//! Composition (`app.rs`) calls [`spawn_gateway`] once per process with
//! the activated slice's parked channels + validated config.
//!
//! [`Services`]: jinn_domain::Services

use crate::backend::gateway;
use jinn_domain::Services;
use tokio::task::JoinHandle;

/// Re-exported so callers name one crate for the pool type.
pub use daow::Pool as SessionPool;

/// Spawns the Discord gateway task when the slice activated enabled.
///
/// `activated` is the slice activation's output: the parked channel
/// halves and the validated `[discord]` section (the enablement gate,
/// decided exactly once at activation). When disabled, nothing is
/// spawned and the parked channels stay untouched.
///
/// `session_pool` backs the thread-map DAO; `intent_handler_cap` grants
/// the gateway its God-mode state writes.
pub fn spawn_gateway(
    handle: &tokio::runtime::Handle,
    core: &jinn_domain::AppCore,
    services: &Services,
    session_pool: SessionPool,
    activated: crate::ActivatedDiscord,
    intent_handler_cap: &jinn_domain::common::tcaps::IntentHandlerCap,
) -> Option<JoinHandle<()>> {
    let crate::ActivatedDiscord { parked, config } = activated;
    if !config.enabled {
        return None;
    }

    let channels = &parked;
    let services = services.clone();
    let state = core.state.clone();
    let bridge = core.bridge.clone();
    let intent_handler_cap = *intent_handler_cap;
    let bridge_rx = channels.bridge_rx.clone();
    let gateway_rx = channels.gateway_rx.clone();
    let status_tx = channels.status_tx.clone();
    let token = std::env::var("DISCORD_BOT_TOKEN")
        .ok()
        .or_else(|| config.bot_token.clone())
        .unwrap_or_default();

    Some(handle.spawn(async move {
        if let Err(report) = gateway::run(
            gateway::BotData {
                state,
                bridge,
                thread_map: crate::DiscordThreadMap::new(session_pool),
                config: std::sync::Arc::new(config),
                services,
                intent_handler_cap,
            },
            token,
            bridge_rx,
            gateway_rx,
            status_tx,
        )
        .await
        {
            tracing::error!("discord gateway terminated: {report:?}");
        }
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::*;
    use jinn_domain::common::bridge::Bridge;
    use jinn_domain::common::state::State;

    /// A throwaway in-memory pool; the disabled path never touches it.
    fn detached_pool() -> SessionPool {
        daow::Pool::builder()
            .path(":memory:")
            .build()
            .expect("in-memory pool")
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawn_gateway_noops_when_disabled() {
        // Given a disabled slice activation output (the gate decided at
        // activation).
        let services = jinn_domain::Services::new_fake().await;
        let core = jinn_domain::AppCore {
            state: State::new(jinn_domain::common::app_state::AppState::default()),
            bridge: Bridge::new(services.bus.clone()),
        };
        let activated = crate::ActivatedDiscord {
            parked: crate::DiscordGatewayChannels::detached(),
            config: crate::DiscordConfig {
                enabled: false,
                ..crate::DiscordConfig::default()
            },
        };
        let cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();

        // When spawning the gateway.
        let handle = spawn_gateway(
            &tokio::runtime::Handle::current(),
            &core,
            &services,
            detached_pool(),
            activated,
            &cap,
        );

        // Then no task was spawned.
        assert!(handle.is_none(), "disabled config must not spawn");
    }
}
