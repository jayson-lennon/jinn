//! Mint: the single trust point where caps are constructed.
//!
//! Every cap originates here. `rg "mint_"` lists every mint site. Only actor
//! wiring (`actor_wiring.rs`) calls these functions.
//!
//! Caps are ZSTs with private constructors scoped `pub(in crate::common::tcaps)`,
//! so this module is the *only* code that can construct them.

use crate::common::tcaps::frontend::FrontendCap;
use crate::common::tcaps::intent_handler::IntentHandlerCap;
use crate::common::tcaps::plugins::PluginsCap;
use crate::common::tcaps::provider::ProviderCap;
use crate::common::tcaps::session::SessionCap;

/// Mint a [`PluginsCap`]. Called from actor wiring.
pub fn mint_plugins_cap() -> PluginsCap {
    PluginsCap::new()
}

/// Mint a [`ProviderCap`]. Called from actor wiring.
pub fn mint_provider_cap() -> ProviderCap {
    ProviderCap::new()
}

/// Mint a [`FrontendCap`]. Called from actor wiring.
pub fn mint_frontend_cap() -> FrontendCap {
    FrontendCap::new()
}

/// Mint a [`ContextCap`]. Called from actor wiring.
/// Mint a [`SessionCap`]. Called from actor wiring.
pub fn mint_session_cap() -> SessionCap {
    SessionCap::new()
}
/// Mint an [`IntentHandlerCap`]. Called from the platform layer (TUI app,
/// headless runner, discord bridge) where [`IntentHandler::handle`] runs
/// synchronously on the main thread. No actor receives this cap.
///
/// [`IntentHandler::handle`]: crate::feat::intent::IntentHandler::handle
pub fn mint_intent_handler_cap() -> IntentHandlerCap {
    IntentHandlerCap::new()
}
