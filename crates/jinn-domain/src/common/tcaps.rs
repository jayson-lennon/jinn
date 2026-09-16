//! TCaps: Token-based Capability System.
//!
//! Write access to shared state is gated by unforgeable ZST tokens (caps).
//! Each cap projects a narrow borrowed view. Reads remain a full snapshot.
//!
//! This module is the root for the `tcaps/` subtree. Caps, views, and traits
//! live one-per-file under `tcaps/`; `mint.rs` is the single trust point where
//! caps are constructed.

pub mod frontend;
pub mod intent_handler;
pub mod mint;
pub mod plugins;
pub mod provider;
pub mod session;

#[cfg(test)]
mod tests;

pub use frontend::FrontendCap;
pub use intent_handler::IntentHandlerCap;
pub use plugins::PluginsCap;
pub use provider::ProviderCap;
pub use session::SessionCap;
