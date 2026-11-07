//! The `[remote]` section — the external control surface's socket binding.
//!
//! One key: `bind`, a host:port socket address (`127.0.0.1:8899`). Port
//! `0` (the shipped default) asks the OS for an ephemeral port at
//! listener start, so two running jinn instances never collide;
//! instances are found through the instance registry instead of a fixed
//! port. The section ships active in `default_jinn.toml`; an absent
//! section reads as the default bind.
//!
//! The listener that binds it lives in `jinn-remote`. This module is
//! data plus the bind-string grammar — no listener, no I/O.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use wherror::Error;

use error_stack::Report;

/// The default bind: loopback, port 0 — an OS-assigned ephemeral port
/// per running jinn instance.
pub const DEFAULT_REMOTE_BIND: &str = "127.0.0.1:0";

/// The bind string failed to parse as a host:port socket address.
#[derive(Debug, Error)]
#[error(debug)]
pub struct RemoteBindError;

fn default_bind() -> String {
    DEFAULT_REMOTE_BIND.to_owned()
}

/// `[remote]` preferences.
///
/// # Behavior: Event-Dependent.
///
/// The external control surface owns the bound socket; a bus handler
/// rebinds it when the reload event lands.
///
/// Serialized as `[remote]` in `jinn.toml`. The single field is the
/// socket the external control surface binds; there is deliberately no
/// separate port key — the address carries the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Host:port the control surface binds, e.g. `"127.0.0.1:8899"`. A
    /// port of `0` binds an OS-assigned ephemeral port; the resolved
    /// port is discoverable through `jinn remote instances`.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl jinn_config::Configurable for RemoteConfig {
    const KEY: &'static str = "remote";
}

impl RemoteConfig {
    /// Parses the configured bind string into a socket address.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteBindError`] when the value is not a host:port
    /// socket address (`127.0.0.1:8899`, `[::1]:9000`). The caller
    /// decides whether that is fatal; the remote activation treats it
    /// as a stop-and-report configuration error.
    pub fn parse_bind(&self) -> Result<SocketAddr, Report<RemoteBindError>> {
        use error_stack::ResultExt as _;
        self.bind
            .parse::<SocketAddr>()
            .change_context(RemoteBindError)
            .attach(format!("invalid [remote] bind: {:?}", self.bind))
            .attach("expected a host:port socket address, e.g. 127.0.0.1:8899")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{DEFAULT_REMOTE_BIND, RemoteConfig};
    use serde::Deserialize;

    fn from_table(raw: &str) -> RemoteConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            remote: RemoteConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("remote table parses")
            .remote
    }

    #[rstest::rstest]
    #[test]
    fn absent_bind_deserializes_to_os_assigned_default() {
        // Given an empty `[remote]` table.
        // When deserializing.
        let config = from_table("[remote]\n");

        // Then the bind is the loopback port-0 default.
        assert_eq!(config.bind, DEFAULT_REMOTE_BIND);
        assert_eq!(config.bind, "127.0.0.1:0");
    }

    #[rstest::rstest]
    #[test]
    fn configured_bind_is_preserved_and_parses() {
        // Given a table with an explicit bind.
        let config = from_table("[remote]\nbind = \"127.0.0.1:7777\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("valid bind");

        // Then the value survives round-trip through the schema.
        assert_eq!(addr.to_string(), "127.0.0.1:7777");
    }

    #[rstest::rstest]
    #[test]
    fn port_zero_parses_as_os_assigned() {
        // Given a table whose bind names port 0.
        let config = from_table("[remote]\nbind = \"127.0.0.1:0\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("port 0 is a valid socket addr");

        // Then the parse keeps port 0; resolving it is serve-time behavior.
        assert_eq!(addr.port(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn non_loopback_bind_still_parses_at_the_schema_layer() {
        // Given a table binding a non-loopback host.
        let config = from_table("[remote]\nbind = \"192.168.1.10:8899\"\n");

        // When parsing the bind.
        let addr = config
            .parse_bind()
            .expect("schema layer parses any socket addr");

        // Then the parse succeeds — refusing non-loopback binds is the
        // activation path's policy, not the data's.
        assert_eq!(addr.to_string(), "192.168.1.10:8899");
    }

    #[rstest::rstest]
    #[test]
    fn malformed_bind_is_rejected_with_a_named_error() {
        // Given a table whose bind is not a host:port pair.
        let config = from_table("[remote]\nbind = \"localhost\"\n");

        // When parsing the bind.
        let result = config.parse_bind();

        // Then parsing fails (activation treats this as stop-and-report).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            remote: RemoteConfig,
        }

        // Given the default config.
        let config = RemoteConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: RemoteConfig = toml::from_str::<Repr>(&format!("[remote]\n{s}"))
            .expect("reparse")
            .remote;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
