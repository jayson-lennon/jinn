//! jinn: a TUI agent harness with a component/actor system.

pub mod actor_wiring;
pub mod app;
pub mod bootstrap;
pub mod cli_paths;
pub mod config_path;
#[cfg(debug_assertions)]
pub mod headless;
pub mod remote_cli;
pub mod runner;
pub mod tracing;

pub use app::{App, AppError};
pub use config_path::{ConfigPathResolution, resolve_config_path};
#[cfg(debug_assertions)]
pub use headless::HeadlessApp;
pub use runner::Runner;

/// Install the process-wide rustls crypto provider (ring).
///
/// reqwest is built with `rustls-no-provider` (see the workspace `Cargo.toml`
/// comment), so no default provider exists — every TLS client would otherwise
/// panic with "No provider set". `install_default` is a no-op if some other
/// code got there first, making this safe to call from startup and tests alike.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Installs the rustls crypto provider in this crate's test binary. reqwest is
/// built with `rustls-no-provider` (see workspace `Cargo.toml`), so without a
/// process-wide default every `reqwest::Client` panics with "No provider set"
/// at construction. Test binaries never run `main()`, hence this hook.
#[cfg(test)]
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    crate::install_crypto_provider();
}
