//! The `[apps]` section — the saved-apps server's socket binding.
//!
//! One key: `bind`, a host:port socket address (`127.0.0.1:0`). Port
//! `0` (the shipped default) asks the OS for an ephemeral port the first
//! time an app is opened, so two running jinn instances never collide;
//! the resolved port is what the opened URL carries. The section ships
//! active in `default_jinn.toml`; an absent section reads as the default
//! bind.
//!
//! The server that binds it lives in `jinn-apps`. This module is data
//! plus the bind-string grammar — no server, no I/O, and deliberately no
//! loopback check: refusing non-loopback binds is the serve path's
//! policy, so the schema layer accepts any host:port (the canvas
//! schema's tests pin that same layering).

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use wherror::Error;

/// The default bind: loopback, port 0 — an OS-assigned ephemeral port
/// per running jinn instance.
pub const DEFAULT_APPS_BIND: &str = "127.0.0.1:0";

/// The bind string failed to parse as a host:port socket address.
#[derive(Debug, Error)]
#[error(debug)]
pub struct AppsBindError;

fn default_bind() -> String {
    DEFAULT_APPS_BIND.to_owned()
}

/// `[apps]` preferences.
///
/// Serialized as `[apps]` in `jinn.toml`. The single field is the
/// socket the saved-apps server binds; there is deliberately no separate
/// port key — the address carries the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppsConfig {
    /// Host:port the saved-apps server binds, e.g. `"127.0.0.1:8123"`. A
    /// port of `0` binds an OS-assigned ephemeral port; the resolved
    /// port is what the picker's opened URL carries.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for AppsConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl jinn_config::Configurable for AppsConfig {
    const KEY: &'static str = "apps";
}

impl AppsConfig {
    /// Parses the configured bind string into a socket address.
    ///
    /// # Errors
    ///
    /// Returns [`AppsBindError`] when the value is not a host:port
    /// socket address (`127.0.0.1:8123`, `[::1]:9000`). The caller
    /// decides whether that is fatal; the apps activation path treats it
    /// as launch-fatal, the same severity the canvas bind gets.
    pub fn parse_bind(&self) -> Result<SocketAddr, Report<AppsBindError>> {
        use error_stack::ResultExt as _;
        self.bind
            .parse::<SocketAddr>()
            .change_context(AppsBindError)
            .attach(format!("invalid [apps] bind: {:?}", self.bind))
            .attach("expected a host:port socket address, e.g. 127.0.0.1:8123")
    }
}

use error_stack::Report;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{AppsConfig, DEFAULT_APPS_BIND};
    use serde::Deserialize;

    fn from_table(raw: &str) -> AppsConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            apps: AppsConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("apps table parses")
            .apps
    }

    #[rstest::rstest]
    #[test]
    fn absent_bind_deserializes_to_os_assigned_default() {
        // Given an empty `[apps]` table.
        // When deserializing.
        let config = from_table("[apps]\n");

        // Then the bind is the loopback port-0 default.
        assert_eq!(config.bind, DEFAULT_APPS_BIND);
        assert_eq!(config.bind, "127.0.0.1:0");
    }

    #[rstest::rstest]
    #[test]
    fn configured_bind_is_preserved_and_parses() {
        // Given a table with an explicit bind.
        let config = from_table("[apps]\nbind = \"127.0.0.1:8123\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("valid bind");

        // Then the value survives round-trip through the schema.
        assert_eq!(addr.to_string(), "127.0.0.1:8123");
    }

    #[rstest::rstest]
    #[test]
    fn port_zero_parses_as_os_assigned() {
        // Given a table whose bind names port 0.
        let config = from_table("[apps]\nbind = \"127.0.0.1:0\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("port 0 is a valid socket addr");

        // Then the parse keeps port 0; resolving it is serve-time behavior.
        assert_eq!(addr.port(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn non_loopback_bind_still_parses_at_the_schema_layer() {
        // Given a table binding a non-loopback host.
        let config = from_table("[apps]\nbind = \"192.168.1.10:8123\"\n");

        // When parsing the bind.
        let addr = config
            .parse_bind()
            .expect("schema layer parses any socket addr");

        // Then the parse succeeds — refusing non-loopback binds is the
        // serve path's policy, not the data's.
        assert_eq!(addr.to_string(), "192.168.1.10:8123");
    }

    #[rstest::rstest]
    #[test]
    fn malformed_bind_is_rejected_with_a_named_error() {
        // Given a table whose bind is not a host:port pair.
        let config = from_table("[apps]\nbind = \"localhost\"\n");

        // When parsing the bind.
        let result = config.parse_bind();

        // Then parsing fails (activation treats this as launch-fatal).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            apps: AppsConfig,
        }

        // Given the default config.
        let config = AppsConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: AppsConfig = toml::from_str::<Repr>(&format!("[apps]\n{s}"))
            .expect("reparse")
            .apps;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
