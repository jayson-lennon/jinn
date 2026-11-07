//! The `[canvas]` section — the canvas server's socket binding.
//!
//! One key: `bind`, a host:port socket address (`127.0.0.1:8899`). Port
//! `0` (the shipped default) asks the OS for an ephemeral port at serve
//! time, so two running jinn instances never collide; the resolved URL
//! comes back in the `canvas` tool result. The section ships active in
//! `default_jinn.toml`; an absent section reads as the default bind.
//!
//! The server that binds it lives in `jinn-canvas`. This module is data
//! plus the bind-string grammar — no server, no I/O.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use wherror::Error;

/// The default bind: loopback, port 0 — an OS-assigned ephemeral port
/// per running jinn instance.
pub const DEFAULT_CANVAS_BIND: &str = "127.0.0.1:0";

/// The bind string failed to parse as a host:port socket address.
#[derive(Debug, Error)]
#[error(debug)]
pub struct CanvasBindError;

fn default_bind() -> String {
    DEFAULT_CANVAS_BIND.to_owned()
}

/// `[canvas]` preferences.
///
/// # Behavior: Event-Dependent.
///
/// The canvas server owns the bound socket; a bus handler rebinds it
/// when the reload event lands.
///
/// Serialized as `[canvas]` in `jinn.toml`. The single field is the
/// socket the canvas server binds; there is deliberately no separate
/// port key — the address carries the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasConfig {
    /// Host:port the canvas server binds, e.g. `"127.0.0.1:8899"`. A
    /// port of `0` binds an OS-assigned ephemeral port; the resolved
    /// URL comes back in the `canvas` tool result.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for CanvasConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl jinn_config::Configurable for CanvasConfig {
    const KEY: &'static str = "canvas";
}

impl CanvasConfig {
    /// Parses the configured bind string into a socket address.
    ///
    /// # Errors
    ///
    /// Returns [`CanvasBindError`] when the value is not a host:port
    /// socket address (`127.0.0.1:8899`, `[::1]:9000`). The caller decides
    /// whether that is fatal; the canvas serve path treats it as a
    /// stop-and-report configuration error.
    pub fn parse_bind(&self) -> Result<SocketAddr, Report<CanvasBindError>> {
        use error_stack::ResultExt as _;
        self.bind
            .parse::<SocketAddr>()
            .change_context(CanvasBindError)
            .attach(format!("invalid [canvas] bind: {:?}", self.bind))
            .attach("expected a host:port socket address, e.g. 127.0.0.1:8899")
    }
}

use error_stack::Report;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{CanvasConfig, DEFAULT_CANVAS_BIND};
    use serde::Deserialize;

    fn from_table(raw: &str) -> CanvasConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            canvas: CanvasConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("canvas table parses")
            .canvas
    }

    #[rstest::rstest]
    #[test]
    fn absent_bind_deserializes_to_os_assigned_default() {
        // Given an empty `[canvas]` table.
        // When deserializing.
        let config = from_table("[canvas]\n");

        // Then the bind is the loopback port-0 default.
        assert_eq!(config.bind, DEFAULT_CANVAS_BIND);
        assert_eq!(config.bind, "127.0.0.1:0");
    }

    #[rstest::rstest]
    #[test]
    fn configured_bind_is_preserved_and_parses() {
        // Given a table with an explicit bind.
        let config = from_table("[canvas]\nbind = \"127.0.0.1:7777\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("valid bind");

        // Then the value survives round-trip through the schema.
        assert_eq!(addr.to_string(), "127.0.0.1:7777");
    }

    #[rstest::rstest]
    #[test]
    fn port_zero_parses_as_os_assigned() {
        // Given a table whose bind names port 0.
        let config = from_table("[canvas]\nbind = \"127.0.0.1:0\"\n");

        // When parsing the bind.
        let addr = config.parse_bind().expect("port 0 is a valid socket addr");

        // Then the parse keeps port 0; resolving it is serve-time behavior.
        assert_eq!(addr.port(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn non_loopback_bind_still_parses_at_the_schema_layer() {
        // Given a table binding a non-loopback host.
        let config = from_table("[canvas]\nbind = \"192.168.1.10:8899\"\n");

        // When parsing the bind.
        let addr = config
            .parse_bind()
            .expect("schema layer parses any socket addr");

        // Then the parse succeeds — the schema layer is address-class
        // agnostic: a bind parses, whatever interface it names.
        assert_eq!(addr.to_string(), "192.168.1.10:8899");
    }

    #[rstest::rstest]
    #[test]
    fn malformed_bind_is_rejected_with_a_named_error() {
        // Given a table whose bind is not a host:port pair.
        let config = from_table("[canvas]\nbind = \"localhost\"\n");

        // When parsing the bind.
        let result = config.parse_bind();

        // Then parsing fails (serve treats this as stop-and-report).
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            canvas: CanvasConfig,
        }

        // Given the default config.
        let config = CanvasConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: CanvasConfig = toml::from_str::<Repr>(&format!("[canvas]\n{s}"))
            .expect("reparse")
            .canvas;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
